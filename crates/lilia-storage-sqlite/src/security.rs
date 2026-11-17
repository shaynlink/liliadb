use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use lilia_core::security::{MaskFunction, MaskingRule, NamedPolicy, Policy, SecurityContext};
use lilia_core::{
    ErrorCode, FtsFuzzyHit, FtsHit, JsonEntry, KvEntry, LiliaError, MutationResult, Result,
    SqlQueryResult,
};
use lilia_sqlite_native::NativeConnection;

use crate::storage::now_ms;
use crate::Database;

thread_local! {
    static CURRENT_SECURITY_CONTEXT: RefCell<Option<SecurityContext>> = const { RefCell::new(None) };
}

/// Retrieve the active security context on the current thread, if any.
#[must_use]
pub fn get_active_context() -> Option<SecurityContext> {
    CURRENT_SECURITY_CONTEXT.with(|c| c.borrow().clone())
}

/// Set or clear the active security context on the current thread.
pub fn set_active_context(ctx: Option<SecurityContext>) {
    CURRENT_SECURITY_CONTEXT.with(|c| *c.borrow_mut() = ctx);
}

/// RAII guard that restores the previous security context when dropped.
#[derive(Debug)]
pub struct SecurityContextGuard {
    previous: Option<SecurityContext>,
}

impl Drop for SecurityContextGuard {
    fn drop(&mut self) {
        set_active_context(self.previous.take());
    }
}

/// In-memory cache of Row-Level Security policies and Dynamic Data Masking rules.
#[derive(Debug, Default)]
pub struct SecurityRegistry {
    policies: RwLock<HashMap<String, NamedPolicy>>,
    masking_rules: RwLock<HashMap<String, Vec<MaskingRule>>>,
}

impl SecurityRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            policies: RwLock::new(HashMap::new()),
            masking_rules: RwLock::new(HashMap::new()),
        }
    }

    /// Load persisted policies and masking rules from SQLite metadata tables.
    pub fn load_from_connection(&self, conn: &NativeConnection) -> Result<()> {
        // 1. Load RLS policies
        let mut stmt = conn.prepare(
            "SELECT name, table_name, condition, action, created_at_ms FROM _lilia_security_policies;",
        )?;
        let mut loaded_policies = HashMap::new();
        while stmt.step()? {
            let name = stmt.column_text(0).to_string();
            let table = stmt.column_text(1).to_string();
            let condition = stmt.column_text(2).to_string();
            let action_str = stmt.column_text(3);
            let action = lilia_core::security::PolicyAction::parse(action_str)
                .unwrap_or(lilia_core::security::PolicyAction::All);
            let created_at_ms = u64::try_from(stmt.column_int64(4)).unwrap_or_default();

            loaded_policies.insert(
                name.clone(),
                NamedPolicy {
                    name,
                    policy: Policy {
                        table,
                        condition,
                        action,
                    },
                    created_at_ms,
                },
            );
        }
        drop(stmt);

        // 2. Load masking rules
        let mut stmt_mask = conn.prepare(
            "SELECT table_name, path, mask_type, exempt_condition FROM _lilia_masking_rules;",
        )?;
        let mut loaded_masking: HashMap<String, Vec<MaskingRule>> = HashMap::new();
        while stmt_mask.step()? {
            let table = stmt_mask.column_text(0).to_string();
            let path = stmt_mask.column_text(1).to_string();
            let mask_type_str = stmt_mask.column_text(2);
            let mask: MaskFunction =
                serde_json::from_str(mask_type_str).unwrap_or(MaskFunction::Redact);
            let exempt_condition = if stmt_mask.column_is_null(3) {
                None
            } else {
                Some(stmt_mask.column_text(3).to_string())
            };

            loaded_masking
                .entry(table.clone())
                .or_default()
                .push(MaskingRule::new(table, path, mask, exempt_condition));
        }
        drop(stmt_mask);

        let mut p_guard = self
            .policies
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *p_guard = loaded_policies;

        let mut m_guard = self
            .masking_rules
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *m_guard = loaded_masking;

        Ok(())
    }

    pub fn insert_policy(&self, named: NamedPolicy) {
        let mut guard = self
            .policies
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.insert(named.name.clone(), named);
    }

    pub fn remove_policy(&self, name: &str) -> Option<NamedPolicy> {
        let mut guard = self
            .policies
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.remove(name)
    }

    #[must_use]
    pub fn get_policy(&self, name: &str) -> Option<Policy> {
        let guard = self
            .policies
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(name).map(|np| np.policy.clone())
    }

    #[must_use]
    pub fn list_policies(&self) -> Vec<NamedPolicy> {
        let guard = self
            .policies
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut out: Vec<NamedPolicy> = guard.values().cloned().collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    #[must_use]
    pub fn policies_for_table(&self, table: &str) -> Vec<NamedPolicy> {
        let guard = self
            .policies
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .values()
            .filter(|np| np.policy.table == table || np.policy.table == "*")
            .cloned()
            .collect()
    }

    pub fn insert_masking_rule(&self, rule: MaskingRule) {
        let mut guard = self
            .masking_rules
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let rules = guard.entry(rule.table.clone()).or_default();
        rules.retain(|r| !r.matches_path(&rule.path));
        rules.push(rule);
    }

    pub fn remove_masking_rule(&self, table: &str, path: &str) -> bool {
        let mut guard = self
            .masking_rules
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(rules) = guard.get_mut(table) {
            let before = rules.len();
            rules.retain(|r| !r.matches_path(path));
            return rules.len() < before;
        }
        false
    }

    #[must_use]
    pub fn list_masking_rules(&self) -> Vec<MaskingRule> {
        let guard = self
            .masking_rules
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut out = Vec::new();
        for rules in guard.values() {
            out.extend(rules.clone());
        }
        out.sort_by(|a, b| (&a.table, &a.path).cmp(&(&b.table, &b.path)));
        out
    }

    #[must_use]
    pub fn masking_rules_for_table(&self, table: &str) -> Vec<MaskingRule> {
        let guard = self
            .masking_rules
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(table).cloned().unwrap_or_default()
    }
}

/// Security management handle returned by `db.security()`.
#[derive(Debug, Clone)]
pub struct SecurityManager<'a> {
    pub(crate) db: &'a Database,
}

impl<'a> SecurityManager<'a> {
    #[must_use]
    pub const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Registers and persists a new Row-Level Security (RLS) policy.
    pub fn create_policy(&self, name: impl Into<String>, policy: Policy) -> Result<()> {
        let name_str = name.into();
        crate::storage::validate_name("policy_name", &name_str)?;
        if policy.table.is_empty() {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "policy table cannot be empty",
                false,
            ));
        }

        // Validate syntax of the condition expression
        lilia_core::parse_expr(&policy.condition)?;

        let created_at_ms = now_ms();
        let named = NamedPolicy {
            name: name_str.clone(),
            policy: policy.clone(),
            created_at_ms: u64::try_from(created_at_ms.max(0)).unwrap_or(0),
        };

        let mut writer = self.db.lock_writer()?;
        let tx = writer.transaction()?;
        let mut stmt = tx.prepare(
            "INSERT INTO _lilia_security_policies(name, table_name, condition, action, created_at_ms) \
             VALUES(?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(name) DO UPDATE SET table_name=excluded.table_name, condition=excluded.condition, action=excluded.action;",
        )?;
        stmt.bind_text_static(1, &name_str)?;
        stmt.bind_text_static(2, &policy.table)?;
        stmt.bind_text_static(3, &policy.condition)?;
        stmt.bind_text_static(4, policy.action.as_str())?;
        stmt.bind_int64(5, created_at_ms)?;
        stmt.step()?;
        drop(stmt);
        tx.commit()?;

        self.db.security_registry.insert_policy(named);
        Ok(())
    }

    /// Removes a previously registered RLS policy.
    pub fn drop_policy(&self, name: &str) -> Result<bool> {
        let mut writer = self.db.lock_writer()?;
        let tx = writer.transaction()?;
        let mut stmt = tx.prepare("DELETE FROM _lilia_security_policies WHERE name = ?1;")?;
        stmt.bind_text_static(1, name)?;
        stmt.step()?;
        drop(stmt);
        tx.commit()?;

        Ok(self.db.security_registry.remove_policy(name).is_some())
    }

    /// Looks up a registered RLS policy by name.
    #[must_use]
    pub fn get_policy(&self, name: &str) -> Option<Policy> {
        self.db.security_registry.get_policy(name)
    }

    /// Lists all registered RLS policies.
    #[must_use]
    pub fn list_policies(&self) -> Vec<NamedPolicy> {
        self.db.security_registry.list_policies()
    }

    /// Registers and persists a Dynamic Data Masking rule.
    pub fn create_masking_rule(&self, rule: MaskingRule) -> Result<()> {
        crate::storage::validate_name("table", &rule.table)?;
        if rule.path.is_empty() {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "masking path cannot be empty",
                false,
            ));
        }

        if let Some(ref exempt) = rule.exempt_condition {
            lilia_core::parse_expr(exempt)?;
        }

        let rule_id = format!("{}:{}", rule.table, rule.path);
        let mask_json = serde_json::to_string(&rule.mask)
            .map_err(|e| LiliaError::new(ErrorCode::InvalidInput, e.to_string(), false))?;
        let created_at_ms = now_ms();

        let mut writer = self.db.lock_writer()?;
        let tx = writer.transaction()?;
        let mut stmt = tx.prepare(
            "INSERT INTO _lilia_masking_rules(id, table_name, path, mask_type, exempt_condition, created_at_ms) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(id) DO UPDATE SET mask_type=excluded.mask_type, exempt_condition=excluded.exempt_condition;",
        )?;
        stmt.bind_text_static(1, &rule_id)?;
        stmt.bind_text_static(2, &rule.table)?;
        stmt.bind_text_static(3, &rule.path)?;
        stmt.bind_text_static(4, &mask_json)?;
        if let Some(ref exempt) = rule.exempt_condition {
            stmt.bind_text_static(5, exempt)?;
        } else {
            stmt.bind_null(5)?;
        }
        stmt.bind_int64(6, created_at_ms)?;
        stmt.step()?;
        drop(stmt);
        tx.commit()?;

        self.db.security_registry.insert_masking_rule(rule);
        Ok(())
    }

    /// Drops a masking rule for the specified table and path.
    pub fn drop_masking_rule(&self, table: &str, path: &str) -> Result<bool> {
        let rule_id = format!("{table}:{path}");
        let mut writer = self.db.lock_writer()?;
        let tx = writer.transaction()?;
        let mut stmt = tx.prepare("DELETE FROM _lilia_masking_rules WHERE id = ?1;")?;
        stmt.bind_text_static(1, &rule_id)?;
        stmt.step()?;
        drop(stmt);
        tx.commit()?;

        Ok(self.db.security_registry.remove_masking_rule(table, path))
    }

    /// Lists all registered Dynamic Data Masking rules.
    #[must_use]
    pub fn list_masking_rules(&self) -> Vec<MaskingRule> {
        self.db.security_registry.list_masking_rules()
    }

    /// Set thread-local security context.
    pub fn set_context(&self, ctx: Option<SecurityContext>) {
        set_active_context(ctx);
    }

    /// Enters a scoped security context, returning an RAII guard that restores previous context on drop.
    #[must_use]
    pub fn enter_context(&self, ctx: &SecurityContext) -> SecurityContextGuard {
        let previous = get_active_context();
        set_active_context(Some(ctx.clone()));
        SecurityContextGuard { previous }
    }

    /// Executes closure with the given security context active.
    pub fn with_context<F, R>(&self, ctx: &SecurityContext, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let _guard = self.enter_context(ctx);
        f()
    }

    /// Gets current thread security context.
    #[must_use]
    pub fn current_context(&self) -> Option<SecurityContext> {
        get_active_context()
    }
}

/// A scoped database handle that automatically applies a pre-bound [`SecurityContext`] to all operations.
#[derive(Debug, Clone)]
pub struct SecuredDatabase<'a> {
    pub db: &'a Database,
    pub context: SecurityContext,
}

impl<'a> SecuredDatabase<'a> {
    #[must_use]
    pub const fn new(db: &'a Database, context: SecurityContext) -> Self {
        Self { db, context }
    }

    pub fn json_get(&self, space: &str, id: &str) -> Result<Option<JsonEntry>> {
        let _guard = self.db.security().enter_context(&self.context);
        self.db.json_get(space, id)
    }

    pub fn json_scan(
        &self,
        space: &str,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JsonEntry>> {
        let _guard = self.db.security().enter_context(&self.context);
        self.db.json_scan(space, after, limit)
    }

    pub fn json_find(
        &self,
        space: &str,
        query: &serde_json::Map<String, serde_json::Value>,
        after_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JsonEntry>> {
        let _guard = self.db.security().enter_context(&self.context);
        self.db.json_find(space, query, after_id, limit)
    }

    pub fn json_extract(
        &self,
        space: &str,
        id: &str,
        path: &str,
    ) -> Result<Option<serde_json::Value>> {
        let _guard = self.db.security().enter_context(&self.context);
        self.db.json_extract(space, id, path)
    }

    pub fn json_extract_str(&self, space: &str, id: &str, path: &str) -> Result<Option<String>> {
        let _guard = self.db.security().enter_context(&self.context);
        self.db.json_extract_str(space, id, path)
    }

    pub fn json_extract_i64(&self, space: &str, id: &str, path: &str) -> Result<Option<i64>> {
        let _guard = self.db.security().enter_context(&self.context);
        self.db.json_extract_i64(space, id, path)
    }

    pub fn json_extract_f64(&self, space: &str, id: &str, path: &str) -> Result<Option<f64>> {
        let _guard = self.db.security().enter_context(&self.context);
        self.db.json_extract_f64(space, id, path)
    }

    pub fn json_extract_bool(&self, space: &str, id: &str, path: &str) -> Result<Option<bool>> {
        let _guard = self.db.security().enter_context(&self.context);
        self.db.json_extract_bool(space, id, path)
    }

    pub fn json_put(
        &self,
        space: &str,
        id: &str,
        doc: &serde_json::Value,
    ) -> Result<MutationResult> {
        let _guard = self.db.security().enter_context(&self.context);
        let res = self.db.batch(&[lilia_core::BatchOperation::JsonPut {
            space: space.to_string(),
            id: id.to_string(),
            value: doc.clone(),
            if_version: None,
        }])?;
        res.into_iter()
            .next()
            .ok_or_else(|| LiliaError::new(ErrorCode::Storage, "empty result from json_put", false))
    }

    pub fn json_delete(&self, space: &str, id: &str) -> Result<bool> {
        let _guard = self.db.security().enter_context(&self.context);
        let res = self.db.batch(&[lilia_core::BatchOperation::JsonDelete {
            space: space.to_string(),
            id: id.to_string(),
            if_version: None,
        }])?;
        let first = res.into_iter().next().ok_or_else(|| {
            LiliaError::new(ErrorCode::Storage, "empty result from json_delete", false)
        })?;
        Ok(first.deleted)
    }

    pub fn kv_get(&self, namespace: &str, key: &[u8]) -> Result<Option<KvEntry>> {
        let _guard = self.db.security().enter_context(&self.context);
        self.db.kv_get(namespace, key)
    }

    pub fn kv_put(&self, namespace: &str, key: &[u8], value: &[u8]) -> Result<MutationResult> {
        let _guard = self.db.security().enter_context(&self.context);
        let res = self.db.batch(&[lilia_core::BatchOperation::KvSet {
            namespace: namespace.to_string(),
            key: key.to_vec(),
            value: value.to_vec(),
            if_version: None,
            expires_at_ms: None,
        }])?;
        res.into_iter()
            .next()
            .ok_or_else(|| LiliaError::new(ErrorCode::Storage, "empty result from kv_put", false))
    }

    pub fn kv_delete(&self, namespace: &str, key: &[u8]) -> Result<bool> {
        let _guard = self.db.security().enter_context(&self.context);
        let res = self.db.batch(&[lilia_core::BatchOperation::KvDelete {
            namespace: namespace.to_string(),
            key: key.to_vec(),
            if_version: None,
        }])?;
        let first = res.into_iter().next().ok_or_else(|| {
            LiliaError::new(ErrorCode::Storage, "empty result from kv_delete", false)
        })?;
        Ok(first.deleted)
    }

    pub fn kv_scan(
        &self,
        namespace: &str,
        after: Option<&[u8]>,
        limit: u32,
    ) -> Result<Vec<KvEntry>> {
        let _guard = self.db.security().enter_context(&self.context);
        self.db.kv_scan(namespace, after, limit)
    }

    pub fn sql_query(&self, sql: &str, params: &[serde_json::Value]) -> Result<SqlQueryResult> {
        let _guard = self.db.security().enter_context(&self.context);
        self.db.sql_query(sql, params)
    }

    pub fn fts_search(&self, collection: &str, query: &str, limit: u32) -> Result<Vec<FtsHit>> {
        let _guard = self.db.security().enter_context(&self.context);
        self.db.fts_search(collection, query, limit)
    }

    pub fn fts_fuzzy_search(
        &self,
        collection: &str,
        query: &str,
        max_distance: u8,
        limit: u32,
    ) -> Result<Vec<FtsFuzzyHit>> {
        let _guard = self.db.security().enter_context(&self.context);
        self.db
            .fts_fuzzy_search(collection, query, max_distance, limit)
    }
}

/// An owned `Arc<Database>` wrapper for a secured session.
#[derive(Debug, Clone)]
pub struct SecuredDatabaseArc {
    pub db: Arc<Database>,
    pub context: SecurityContext,
}

impl SecuredDatabaseArc {
    #[must_use]
    pub const fn new(db: Arc<Database>, context: SecurityContext) -> Self {
        Self { db, context }
    }

    pub fn json_get(&self, space: &str, id: &str) -> Result<Option<JsonEntry>> {
        let secured = SecuredDatabase::new(&self.db, self.context.clone());
        secured.json_get(space, id)
    }

    pub fn json_scan(
        &self,
        space: &str,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JsonEntry>> {
        let secured = SecuredDatabase::new(&self.db, self.context.clone());
        secured.json_scan(space, after, limit)
    }

    pub fn json_find(
        &self,
        space: &str,
        query: &serde_json::Map<String, serde_json::Value>,
        after_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JsonEntry>> {
        let secured = SecuredDatabase::new(&self.db, self.context.clone());
        secured.json_find(space, query, after_id, limit)
    }

    pub fn sql_query(&self, sql: &str, params: &[serde_json::Value]) -> Result<SqlQueryResult> {
        let secured = SecuredDatabase::new(&self.db, self.context.clone());
        secured.sql_query(sql, params)
    }
}
