use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

pub use lilia_core::rules::{
    eval_rule_condition_kv, eval_rule_condition_value, infer_target_collection, interpolate_string,
    interpolate_value, Action, Rule, RuleBuilder, RuleContext, SlidingWindow, TriggerMode,
    WindowCondition,
};
use lilia_core::{parse_expr, BatchOperation, ErrorCode, Expr, LiliaError, Result};
use lilia_sqlite_native::{NativeConnection, NativeTransaction};
use serde_json::Value;

use crate::engine::Database;
use crate::storage::{now_ms, validate_name};

#[derive(Debug, Clone, Copy, Default)]
pub struct WindowStats {
    pub count: u64,
    pub sum: Option<f64>,
    pub avg: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// A compiled rule with parsed AST and indexing metadata.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub name: String,
    pub rule: Rule,
    pub compiled_expr: Expr,
    pub target: String,
    pub window: Option<SlidingWindow>,
    pub then: Action,
}

impl CompiledRule {
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn compile(name: &str, rule: Rule) -> Result<Self> {
        let compiled_expr = parse_expr(&rule.when)?;
        let target = if let Some(ref t) = rule.target {
            t.clone()
        } else {
            infer_target_collection(&compiled_expr).unwrap_or_default()
        };

        Ok(Self {
            name: name.to_string(),
            window: rule.window.clone(),
            then: rule.then.clone(),
            target,
            compiled_expr,
            rule,
        })
    }
}

/// Internal thread-safe engine managing active rules and in-transaction execution.
#[derive(Debug, Default)]
pub struct RuleEngine {
    rules: RwLock<HashMap<String, Arc<CompiledRule>>>,
    target_index: RwLock<HashMap<String, Vec<Arc<CompiledRule>>>>,
    wildcard_rules: RwLock<Vec<Arc<CompiledRule>>>,
    active_targets: RwLock<HashSet<String>>,
}

impl RuleEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.read().map_or(true, |r| r.is_empty())
    }

    /// Load persisted rules from `_lilia_rules`.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn load_from_connection(&self, conn: &NativeConnection) -> Result<()> {
        let mut stmt =
            conn.prepare("SELECT name, definition FROM _lilia_rules WHERE enabled = 1;")?;

        let mut loaded = Vec::new();
        while stmt.step()? {
            let name = stmt.column_text(0).to_string();
            let def_str = stmt.column_text(1);
            if let Ok(rule) = serde_json::from_str::<Rule>(def_str) {
                if let Ok(compiled) = CompiledRule::compile(&name, rule) {
                    loaded.push(compiled);
                }
            }
        }
        drop(stmt);

        for compiled in loaded {
            self.insert_compiled(compiled);
        }
        Ok(())
    }

    /// Register and persist a rule.
    ///
    /// # Errors
    /// Returns `InvalidInput` errors detected here, and propagates errors from the
    /// operations it performs.
    pub fn register(&self, conn: &NativeConnection, name: &str, rule: &Rule) -> Result<()> {
        validate_name("rule_name", name)?;
        let compiled = CompiledRule::compile(name, rule.clone())?;
        let def_json = serde_json::to_string(rule).map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("invalid rule json: {e}"),
                false,
            )
        })?;

        let mut stmt = conn.prepare(
            "INSERT INTO _lilia_rules(name, definition, created_at_ms, enabled) \
             VALUES(?1, ?2, ?3, 1) \
             ON CONFLICT(name) DO UPDATE SET definition = excluded.definition, enabled = 1;",
        )?;
        stmt.bind_text_static(1, name)?;
        stmt.bind_text_static(2, &def_json)?;
        stmt.bind_int64(3, now_ms())?;
        stmt.step()?;
        stmt.reset()?;

        self.insert_compiled(compiled);
        Ok(())
    }

    /// Unregister a rule and remove its CEP window history.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn unregister(&self, conn: &NativeConnection, name: &str) -> Result<bool> {
        let mut stmt = conn.prepare("DELETE FROM _lilia_rules WHERE name = ?1;")?;
        stmt.bind_text_static(1, name)?;
        stmt.step()?;
        stmt.reset()?;
        let deleted = conn.changes() > 0;

        if deleted {
            let mut cep_del =
                conn.prepare("DELETE FROM _lilia_cep_window_events WHERE rule_name = ?1;")?;
            cep_del.bind_text_static(1, name)?;
            let _ = cep_del.step();
        }

        self.remove_compiled(name);
        Ok(deleted)
    }

    /// Retrieve a registered rule definition by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Rule> {
        self.rules.read().ok()?.get(name).map(|c| c.rule.clone())
    }

    /// List all registered rules.
    #[must_use]
    pub fn list(&self) -> Vec<(String, Rule)> {
        self.rules
            .read()
            .map_or_default(|r| r.iter().map(|(k, v)| (k.clone(), v.rule.clone())).collect())
    }

    /// Clear all rules.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn clear(&self, conn: &NativeConnection) -> Result<usize> {
        conn.exec("DELETE FROM _lilia_rules;")?;
        let count = usize::try_from(conn.changes()).unwrap_or_default();
        conn.exec("DELETE FROM _lilia_cep_window_events;")?;

        if let Ok(mut r) = self.rules.write() {
            r.clear();
        }
        if let Ok(mut t) = self.target_index.write() {
            t.clear();
        }
        if let Ok(mut w) = self.wildcard_rules.write() {
            w.clear();
        }
        if let Ok(mut a) = self.active_targets.write() {
            a.clear();
        }

        Ok(count)
    }

    fn insert_compiled(&self, compiled: CompiledRule) {
        let name = compiled.name.clone();
        let target = compiled.target.clone();
        let arc = Arc::new(compiled);

        if let Ok(mut rules) = self.rules.write() {
            rules.insert(name.clone(), Arc::clone(&arc));
        }

        if target.is_empty() {
            if let Ok(mut wildcards) = self.wildcard_rules.write() {
                wildcards.retain(|r| r.name != name);
                wildcards.push(arc);
            }
        } else {
            if let Ok(mut targets) = self.target_index.write() {
                let entry = targets.entry(target.clone()).or_default();
                entry.retain(|r| r.name != name);
                entry.push(Arc::clone(&arc));
            }
            if let Ok(mut active) = self.active_targets.write() {
                active.insert(target);
            }
        }
    }

    fn remove_compiled(&self, name: &str) {
        let removed = self.rules.write().ok().and_then(|mut r| r.remove(name));

        if let Some(arc) = removed {
            if arc.target.is_empty() {
                if let Ok(mut w) = self.wildcard_rules.write() {
                    w.retain(|r| r.name != name);
                }
            } else if let Ok(mut t) = self.target_index.write() {
                if let Some(list) = t.get_mut(&arc.target) {
                    list.retain(|r| r.name != name);
                }
            }
        }
    }

    /// Retrieve candidates matching collection / namespace.
    fn candidates_for(&self, collection: &str) -> Vec<Arc<CompiledRule>> {
        let mut candidates = Vec::new();
        if let Ok(targets) = self.target_index.read() {
            if let Some(list) = targets.get(collection) {
                candidates.extend(list.iter().cloned());
            }
        }
        if let Ok(wildcards) = self.wildcard_rules.read() {
            candidates.extend(wildcards.iter().cloned());
        }
        candidates
    }

    /// In-transaction evaluation of rules on a mutation.
    ///
    /// Evaluates both simple reactive rules and temporal sliding window CEP patterns.
    /// Returns synthesized [`BatchOperation`]s to be applied in the same transaction.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    #[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
    pub fn evaluate_in_transaction(
        &self,
        tx: &mut NativeTransaction<'_>,
        op: &BatchOperation,
        now: i64,
    ) -> Result<Vec<BatchOperation>> {
        if self.is_empty() {
            return Ok(Vec::new());
        }

        let (collection, key, id, json_val, raw_bytes) = match op {
            BatchOperation::JsonPut {
                space, id, value, ..
            }
            | BatchOperation::JsonPutHlc {
                space, id, value, ..
            } => (space.as_str(), id.as_str(), id.as_str(), Some(value), None),
            BatchOperation::JsonDelete { space, id, .. } => {
                (space.as_str(), id.as_str(), id.as_str(), None, None)
            }
            BatchOperation::KvSet {
                namespace,
                key,
                value,
                ..
            } => {
                let k_str = std::str::from_utf8(key).unwrap_or("");
                (
                    namespace.as_str(),
                    k_str,
                    k_str,
                    None,
                    Some((key.as_slice(), value.as_slice())),
                )
            }
            BatchOperation::KvDelete { namespace, key, .. } => {
                let k_str = std::str::from_utf8(key).unwrap_or("");
                (namespace.as_str(), k_str, k_str, None, None)
            }
            _ => return Ok(Vec::new()),
        };

        let candidates = self.candidates_for(collection);
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let mut triggered_ops = Vec::new();

        for rule in candidates {
            if let Some(ref window) = rule.window {
                // Temporal CEP Sliding Window Evaluation
                let matches_condition = if let Some(val) = json_val {
                    eval_rule_condition_value(&rule.compiled_expr, collection, val).unwrap_or(false)
                } else if let Some((k, v)) = raw_bytes {
                    eval_rule_condition_kv(&rule.compiled_expr, collection, k, v)
                } else {
                    false
                };

                if !matches_condition {
                    continue;
                }

                // Partition key resolution
                let partition_key = match &window.partition_by {
                    Some(expr) if expr == "$key" || expr == "$id" || expr == "key" => {
                        key.to_string()
                    }
                    Some(expr) => {
                        if let Some(val) = json_val {
                            let clean = expr
                                .trim_start_matches("$event.")
                                .trim_start_matches(&format!("{collection}."));
                            val.get(clean)
                                .and_then(|v| v.as_str())
                                .unwrap_or(key)
                                .to_string()
                        } else {
                            key.to_string()
                        }
                    }
                    None => key.to_string(),
                };

                // Extract numeric value if needed
                let event_val = json_val.and_then(|v| match &window.condition {
                    WindowCondition::SumGreaterThan { field, .. }
                    | WindowCondition::AvgGreaterThan { field, .. }
                    | WindowCondition::MaxGreaterThan { field, .. }
                    | WindowCondition::MinLessThan { field, .. } => {
                        v.get(field).and_then(Value::as_f64)
                    }
                    _ => None,
                });

                // Query window state prior to insertion for edge-triggering
                let prev_stats =
                    query_window_stats(tx, &rule.name, &partition_key, now, window.duration_ms)?;

                let prev_satisfied = evaluate_window_condition(&window.condition, prev_stats);

                // Insert event into CEP window state
                insert_cep_event(tx, &rule.name, &partition_key, now, event_val, json_val)?;

                // Prune expired events
                prune_cep_events(tx, &rule.name, &partition_key, now, window.duration_ms)?;

                // Query window state after insertion
                let new_stats =
                    query_window_stats(tx, &rule.name, &partition_key, now, window.duration_ms)?;

                let new_satisfied = evaluate_window_condition(&window.condition, new_stats);

                let should_fire = match window.trigger_mode {
                    TriggerMode::OnThresholdCrossed => !prev_satisfied && new_satisfied,
                    TriggerMode::Always => new_satisfied,
                };

                if should_fire {
                    let mut ctx = RuleContext::new(now, collection, &partition_key);
                    ctx.id = id.to_string();
                    if let Some(val) = json_val {
                        ctx = ctx.with_payload(val.clone());
                    }
                    ctx = ctx.with_window_stats(new_stats.count, new_stats.sum, new_stats.avg);

                    if let Some(synthesized) = action_to_batch_operation(&rule.then, &ctx, tx)? {
                        triggered_ops.push(synthesized);
                    }
                }
            } else {
                // Simple in-transaction reactive rule
                let is_match = if let Some(val) = json_val {
                    eval_rule_condition_value(&rule.compiled_expr, collection, val).unwrap_or(false)
                } else if let Some((k, v)) = raw_bytes {
                    eval_rule_condition_kv(&rule.compiled_expr, collection, k, v)
                } else {
                    false
                };

                if is_match {
                    let mut ctx = RuleContext::new(now, collection, id);
                    if let Some(val) = json_val {
                        ctx = ctx.with_payload(val.clone());
                    }

                    if let Some(synthesized) = action_to_batch_operation(&rule.then, &ctx, tx)? {
                        triggered_ops.push(synthesized);
                    }
                }
            }
        }

        Ok(triggered_ops)
    }
}

fn query_window_stats(
    tx: &mut NativeTransaction<'_>,
    rule_name: &str,
    partition_key: &str,
    now: i64,
    duration_ms: u64,
) -> Result<WindowStats> {
    let cutoff = now.saturating_sub(i64::try_from(duration_ms).unwrap_or(i64::MAX));
    let mut stmt = tx.conn().prepare(
        "SELECT COUNT(*), SUM(event_value), AVG(event_value), MIN(event_value), MAX(event_value) \
         FROM _lilia_cep_window_events \
         WHERE rule_name = ?1 AND partition_key = ?2 AND event_timestamp_ms >= ?3;",
    )?;
    stmt.bind_text_static(1, rule_name)?;
    stmt.bind_text_static(2, partition_key)?;
    stmt.bind_int64(3, cutoff)?;

    if stmt.step()? {
        let count = u64::try_from(stmt.column_int64(0)).unwrap_or_default();
        let sum = if count > 0 {
            Some(stmt.column_double(1))
        } else {
            None
        };
        let avg = if count > 0 {
            Some(stmt.column_double(2))
        } else {
            None
        };
        let min = if count > 0 {
            Some(stmt.column_double(3))
        } else {
            None
        };
        let max = if count > 0 {
            Some(stmt.column_double(4))
        } else {
            None
        };
        Ok(WindowStats {
            count,
            sum,
            avg,
            min,
            max,
        })
    } else {
        Ok(WindowStats::default())
    }
}

fn insert_cep_event(
    tx: &mut NativeTransaction<'_>,
    rule_name: &str,
    partition_key: &str,
    now: i64,
    event_val: Option<f64>,
    payload: Option<&Value>,
) -> Result<()> {
    let mut stmt = tx.conn().prepare(
        "INSERT INTO _lilia_cep_window_events(rule_name, partition_key, event_timestamp_ms, event_value, event_payload) \
         VALUES(?1, ?2, ?3, ?4, ?5);",
    )?;
    stmt.bind_text_static(1, rule_name)?;
    stmt.bind_text_static(2, partition_key)?;
    stmt.bind_int64(3, now)?;
    match event_val {
        Some(v) => stmt.bind_double(4, v)?,
        None => stmt.bind_null(4)?,
    }
    match payload {
        Some(p) => stmt.bind_text_static(5, &p.to_string())?,
        None => stmt.bind_null(5)?,
    }
    stmt.step()?;
    stmt.reset()?;
    Ok(())
}

fn prune_cep_events(
    tx: &mut NativeTransaction<'_>,
    rule_name: &str,
    partition_key: &str,
    now: i64,
    duration_ms: u64,
) -> Result<()> {
    let cutoff = now.saturating_sub(i64::try_from(duration_ms).unwrap_or(i64::MAX));
    let mut stmt = tx.conn().prepare(
        "DELETE FROM _lilia_cep_window_events \
         WHERE rule_name = ?1 AND partition_key = ?2 AND event_timestamp_ms < ?3;",
    )?;
    stmt.bind_text_static(1, rule_name)?;
    stmt.bind_text_static(2, partition_key)?;
    stmt.bind_int64(3, cutoff)?;
    stmt.step()?;
    stmt.reset()?;
    Ok(())
}

fn evaluate_window_condition(cond: &WindowCondition, stats: WindowStats) -> bool {
    match cond {
        WindowCondition::CountGreaterThan(thresh) => stats.count > *thresh,
        WindowCondition::CountGreaterOrEqual(thresh) => stats.count >= *thresh,
        WindowCondition::SumGreaterThan { threshold, .. } => {
            stats.sum.is_some_and(|s| s > *threshold)
        }
        WindowCondition::AvgGreaterThan { threshold, .. } => {
            stats.avg.is_some_and(|a| a > *threshold)
        }
        WindowCondition::MaxGreaterThan { threshold, .. } => {
            stats.max.is_some_and(|m| m > *threshold)
        }
        WindowCondition::MinLessThan { threshold, .. } => stats.min.is_some_and(|m| m < *threshold),
    }
}

#[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
fn action_to_batch_operation(
    action: &Action,
    ctx: &RuleContext,
    tx: &mut NativeTransaction<'_>,
) -> Result<Option<BatchOperation>> {
    match action {
        Action::Insert {
            collection,
            document,
        } => {
            let target_coll = interpolate_string(collection, ctx);
            let mut final_doc = interpolate_value(document, ctx);
            let doc_id = if let Some(id_val) = final_doc.get("_id").or_else(|| final_doc.get("id"))
            {
                if let Some(s) = id_val.as_str() {
                    s.to_string()
                } else {
                    id_val.to_string()
                }
            } else {
                let generated = uuid::Uuid::new_v4().to_string();
                if let Value::Object(ref mut map) = final_doc {
                    map.insert("id".to_string(), Value::String(generated.clone()));
                }
                generated
            };

            Ok(Some(BatchOperation::JsonPut {
                space: target_coll,
                id: doc_id,
                value: final_doc,
                if_version: None,
            }))
        }
        Action::Update {
            collection,
            id,
            document,
        } => {
            let target_coll = interpolate_string(collection, ctx);
            let target_id = interpolate_string(id, ctx);
            let final_doc = interpolate_value(document, ctx);

            Ok(Some(BatchOperation::JsonPut {
                space: target_coll,
                id: target_id,
                value: final_doc,
                if_version: None,
            }))
        }
        Action::Delete { collection, id } => {
            let target_coll = interpolate_string(collection, ctx);
            let target_id = interpolate_string(id, ctx);

            Ok(Some(BatchOperation::JsonDelete {
                space: target_coll,
                id: target_id,
                if_version: None,
            }))
        }
        Action::KvSet {
            namespace,
            key,
            value,
        } => {
            let target_ns = interpolate_string(namespace, ctx);
            let target_key = interpolate_string(key, ctx);
            // Replace templates in value if valid UTF-8
            let target_val = if let Ok(s) = std::str::from_utf8(value) {
                interpolate_string(s, ctx).into_bytes()
            } else {
                value.clone()
            };

            Ok(Some(BatchOperation::KvSet {
                namespace: target_ns,
                key: target_key.into_bytes(),
                value: target_val,
                if_version: None,
                expires_at_ms: None,
            }))
        }
        Action::KvDelete { namespace, key } => {
            let target_ns = interpolate_string(namespace, ctx);
            let target_key = interpolate_string(key, ctx);

            Ok(Some(BatchOperation::KvDelete {
                namespace: target_ns,
                key: target_key.into_bytes(),
                if_version: None,
            }))
        }
        Action::ExecuteSql { sql, params } => {
            let interp_sql = interpolate_string(sql, ctx);
            let mut stmt = tx.conn().prepare(&interp_sql)?;
            for (idx, param) in params.iter().enumerate() {
                let col = i32::try_from(idx + 1).unwrap_or(i32::MAX);
                let p_interp = interpolate_value(param, ctx);
                match p_interp {
                    Value::Null => stmt.bind_null(col)?,
                    Value::Bool(b) => stmt.bind_int64(col, i64::from(b))?,
                    Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            stmt.bind_int64(col, i)?;
                        } else if let Some(f) = n.as_f64() {
                            stmt.bind_double(col, f)?;
                        }
                    }
                    Value::String(s) => stmt.bind_text_static(col, &s)?,
                    Value::Array(_) | Value::Object(_) => {
                        stmt.bind_text_static(col, &p_interp.to_string())?;
                    }
                }
            }
            stmt.step()?;
            stmt.reset()?;
            Ok(None)
        }
        Action::EmitEvent { .. } => {
            // Event emission is pushed to events broadcast channel after transaction commits
            Ok(None)
        }
    }
}

/// Deepest chain of rule actions triggering further rules before the write is refused as a cycle.
const MAX_RULE_CASCADE_DEPTH: usize = 8;

impl Database {
    /// Evaluates the reactive rules against `operations` inside `transaction` and applies the
    /// writes they trigger, level by level, so every action commits or rolls back with its cause.
    ///
    /// Both write paths call it: atomic batches and interactive [`crate::Transaction`]s.
    pub(crate) fn apply_rule_cascade(
        &self,
        transaction: &mut NativeTransaction<'_>,
        operations: &[BatchOperation],
        now: i64,
        cascades: &mut Vec<BatchOperation>,
    ) -> Result<()> {
        if self.rule_engine.is_empty() {
            return Ok(());
        }
        let mut pending = operations.to_vec();
        for _ in 0..MAX_RULE_CASCADE_DEPTH {
            if pending.is_empty() {
                return Ok(());
            }
            let level = std::mem::take(&mut pending);
            for operation in &level {
                for triggered in
                    self.rule_engine
                        .evaluate_in_transaction(transaction, operation, now)?
                {
                    cascades.push(triggered.clone());
                    crate::transaction::apply_collecting(
                        transaction,
                        &triggered,
                        self.codec.as_ref(),
                        &self.dict_registry,
                        &self.json_options,
                        self.hlc.as_ref(),
                        &self.cas,
                        self.shard_manager.as_deref(),
                        Some(self.integrity.as_ref()),
                        cascades,
                    )?;
                    pending.push(triggered);
                }
            }
        }
        if pending.is_empty() {
            Ok(())
        } else {
            Err(LiliaError::new(
                ErrorCode::Conflict,
                "maximum reactive rule cascade depth exceeded (possible cycle)",
                false,
            ))
        }
    }
}

/// Fluent public handle returned by [`crate::Database::rules()`].
#[derive(Debug, Clone)]
pub struct RulesManager<'a> {
    db: &'a Database,
}

impl<'a> RulesManager<'a> {
    #[must_use]
    pub(crate) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Register a declarative reactive rule or sliding window CEP rule.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn register(&self, name: &str, rule: impl Into<Rule>) -> Result<()> {
        let r = rule.into();
        let writer = self.db.lock_writer()?;
        self.db.rule_engine.register(&writer, name, &r)
    }

    /// Unregister a rule by name.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn unregister(&self, name: &str) -> Result<bool> {
        let writer = self.db.lock_writer()?;
        self.db.rule_engine.unregister(&writer, name)
    }

    /// Get a registered rule by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Rule> {
        self.db.rule_engine.get(name)
    }

    /// List all currently active registered rules.
    #[must_use]
    pub fn list(&self) -> Vec<(String, Rule)> {
        self.db.rule_engine.list()
    }

    /// Clear all rules.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn clear(&self) -> Result<usize> {
        let writer = self.db.lock_writer()?;
        self.db.rule_engine.clear(&writer)
    }
}
