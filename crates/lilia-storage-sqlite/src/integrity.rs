//! Cross-model integrity contracts: JSON `CHECK` constraints and graph/KV → JSON references.
//!
//! Contracts live in the catalog (`_lilia_catalog.check_expr`, `_lilia_references`), are compiled
//! into an [`IntegrityRegistry`] at open, and are enforced by [`guard`] / [`CompiledCheck::enforce`]
//! inside the write transaction (single funnel: `transaction::apply`).
//!
//! The registry also carries the auto-embed configuration of JSON collections
//! (`_lilia_catalog.auto_embed_json`): it is the other per-collection contract resolved by the
//! write funnel, which queues the documents whose source text changed for background embedding.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use lilia_core::json::{BinaryDoc, CheckOutcome, Expr};
use lilia_core::{
    AutoEmbedConfig, BatchOperation, CollectionKind, CollectionOptions, ConstraintKind, ErrorCode,
    FlePolicy, KeyMapping, LiliaError, RefAction, Reference, ReferenceSource, Result, StorageTier,
};
use lilia_sqlite_native::{NativeConnection, NativeTransaction, StmtSlot};
use serde_json::json;

use crate::catalog::CollectionCatalog;
use crate::engine::Database;
use crate::sharding::ShardManager;
use crate::storage::{now_ms, validate_name};

/// Core tables whose rows are governed by integrity contracts; raw SQL may not write to them
/// while any contract is declared, since only the batch funnel enforces contracts.
pub(crate) const CONTRACT_TABLES: &[&str] = &["_lilia_json", "_lilia_kv", "_lilia_graph_nodes"];

/// Maximum number of offending ids reported when a definition is refused.
const MAX_REPORTED_OFFENDERS: usize = 10;

/// A parsed and validated `CHECK` expression bound to one JSON space.
#[derive(Debug)]
pub(crate) struct CompiledCheck {
    source: String,
    expr: Expr,
}

impl CompiledCheck {
    /// Parses `source` and rejects expressions that cannot be enforced reliably.
    ///
    /// A `CHECK` cannot reference a field covered by the collection's field-level encryption
    /// policy: documents are encrypted before they reach the write funnel.
    pub(crate) fn compile(source: &str, fle: Option<&FlePolicy>) -> Result<Self> {
        let expr = lilia_core::parse_expr(source)?;
        lilia_core::validate_check_expr(&expr)?;
        if let Some(policy) = fle {
            for path in lilia_core::referenced_paths(&expr) {
                if let Some(rule) = policy
                    .rules
                    .iter()
                    .find(|rule| lilia_core::paths_overlap(path, &rule.path))
                {
                    return Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!(
                            "CHECK cannot reference '{path}': it is covered by field encryption rule '{}'",
                            rule.path
                        ),
                        false,
                    ));
                }
            }
        }
        Ok(Self {
            source: source.to_owned(),
            expr,
        })
    }

    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    fn is_violated_by(&self, doc: &BinaryDoc) -> Result<bool> {
        Ok(lilia_core::eval_check(&self.expr, doc)? == CheckOutcome::False)
    }

    /// Rejects `doc` with a structured `CONSTRAINT_VIOLATION` when the expression evaluates to FALSE.
    pub(crate) fn enforce(&self, space: &str, id: &str, doc: &BinaryDoc) -> Result<()> {
        if self.is_violated_by(doc)? {
            return Err(LiliaError::constraint_violation(
                ConstraintKind::Check,
                &format!("check:{space}"),
                format!("document '{space}/{id}' violates CHECK ({})", self.source),
                json!({ "model": "json", "collection": space, "id": id, "expression": self.source }),
            ));
        }
        Ok(())
    }
}

/// Write-time contracts of one JSON space, resolved once per operation.
#[derive(Debug, Default)]
pub(crate) struct JsonContracts {
    pub(crate) check: Option<Arc<CompiledCheck>>,
    pub(crate) auto_embed: Option<Arc<AutoEmbedConfig>>,
}

#[derive(Debug, Default)]
struct RegistryState {
    checks: HashMap<String, Arc<CompiledCheck>>,
    references: HashMap<String, Arc<Reference>>,
    auto_embeds: HashMap<String, Arc<AutoEmbedConfig>>,
}

/// In-memory view of every declared integrity contract and auto-embed configuration.
///
/// Mutated only while the writer lock is held, so a transaction always sees a stable set.
#[derive(Debug, Default)]
pub(crate) struct IntegrityRegistry {
    state: RwLock<RegistryState>,
    active: AtomicBool,
    embedding: AtomicBool,
}

impl IntegrityRegistry {
    /// Builds the registry from persisted collection specs and the `_lilia_references` table.
    pub(crate) fn load(conn: &NativeConnection, catalog: &CollectionCatalog) -> Result<Self> {
        let registry = Self::default();
        {
            let mut state = registry.write();
            for spec in catalog.list_specs() {
                if let (CollectionKind::Json, Some(source)) = (spec.kind, spec.check.as_deref()) {
                    let compiled = CompiledCheck::compile(source, spec.fle_policy.as_ref())?;
                    state.checks.insert(spec.name.clone(), Arc::new(compiled));
                }
                if let (CollectionKind::Json, Some(config)) = (spec.kind, spec.auto_embed) {
                    state.auto_embeds.insert(spec.name, Arc::new(config));
                }
            }
            for reference in load_references(conn)? {
                state
                    .references
                    .insert(reference.name.clone(), Arc::new(reference));
            }
        }
        registry.refresh_active();
        Ok(registry)
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, RegistryState> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, RegistryState> {
        self.state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn refresh_active(&self) {
        let state = self.read();
        let embedding = !state.auto_embeds.is_empty();
        self.embedding.store(embedding, Ordering::Release);
        self.active.store(
            embedding || !(state.checks.is_empty() && state.references.is_empty()),
            Ordering::Release,
        );
    }

    /// `true` when no contract is declared (the write funnel then skips all contract work).
    pub(crate) fn is_empty(&self) -> bool {
        !self.active.load(Ordering::Acquire)
    }

    /// `true` when at least one JSON collection is auto-embedded.
    pub(crate) fn has_auto_embed(&self) -> bool {
        self.embedding.load(Ordering::Acquire)
    }

    pub(crate) fn check_for(&self, space: &str) -> Option<Arc<CompiledCheck>> {
        self.read().checks.get(space).cloned()
    }

    pub(crate) fn auto_embed_for(&self, space: &str) -> Option<Arc<AutoEmbedConfig>> {
        self.read().auto_embeds.get(space).cloned()
    }

    pub(crate) fn json_contracts(&self, space: &str) -> JsonContracts {
        let state = self.read();
        JsonContracts {
            check: state.checks.get(space).cloned(),
            auto_embed: state.auto_embeds.get(space).cloned(),
        }
    }

    fn references_where(&self, predicate: impl Fn(&Reference) -> bool) -> Vec<Arc<Reference>> {
        let mut found: Vec<_> = self
            .read()
            .references
            .values()
            .filter(|reference| predicate(reference))
            .cloned()
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        found
    }

    fn refs_for_graph(&self, graph: &str) -> Vec<Arc<Reference>> {
        self.references_where(|r| matches!(&r.source, ReferenceSource::GraphNodes { graph: g } if g == graph))
    }

    fn refs_for_kv(&self, namespace: &str) -> Vec<Arc<Reference>> {
        self.references_where(|r| matches!(&r.source, ReferenceSource::Kv { namespace: n } if n == namespace))
    }

    fn refs_to_space(&self, space: &str) -> Vec<Arc<Reference>> {
        self.references_where(|r| r.target_space == space)
    }

    pub(crate) fn references(&self) -> Vec<Reference> {
        self.references_where(|_| true)
            .into_iter()
            .map(|reference| (*reference).clone())
            .collect()
    }

    pub(crate) fn checks(&self) -> Vec<(String, String)> {
        let mut checks: Vec<_> = self
            .read()
            .checks
            .iter()
            .map(|(space, check)| (space.clone(), check.source().to_owned()))
            .collect();
        checks.sort();
        checks
    }

    /// `true` when `(kind, name)` takes part in a contract and therefore must stay on `Disk`.
    ///
    /// An auto-embedded JSON collection pins both itself and the vector collection of the same
    /// name: the embedding worker only reads and writes the disk tables.
    pub(crate) fn is_constrained(&self, kind: CollectionKind, name: &str) -> bool {
        match kind {
            CollectionKind::Json => {
                self.check_for(name).is_some()
                    || self.auto_embed_for(name).is_some()
                    || !self.refs_to_space(name).is_empty()
            }
            CollectionKind::Vector => self.auto_embed_for(name).is_some(),
            CollectionKind::Graph => !self.refs_for_graph(name).is_empty(),
            CollectionKind::Kv => !self.refs_for_kv(name).is_empty(),
            _ => false,
        }
    }

    pub(crate) fn set_auto_embed(&self, space: &str, config: Option<AutoEmbedConfig>) {
        {
            let mut state = self.write();
            match config {
                Some(config) => state.auto_embeds.insert(space.to_owned(), Arc::new(config)),
                None => state.auto_embeds.remove(space),
            };
        }
        self.refresh_active();
    }

    fn set_check(&self, space: &str, check: Option<CompiledCheck>) {
        {
            let mut state = self.write();
            match check {
                Some(check) => state.checks.insert(space.to_owned(), Arc::new(check)),
                None => state.checks.remove(space),
            };
        }
        self.refresh_active();
    }

    fn insert_reference(&self, reference: Reference) {
        self.write()
            .references
            .insert(reference.name.clone(), Arc::new(reference));
        self.refresh_active();
    }

    fn remove_reference(&self, name: &str) -> bool {
        let removed = self.write().references.remove(name).is_some();
        self.refresh_active();
        removed
    }
}

fn reference_violation(
    reference: &Reference,
    message: String,
    id: &str,
    extra: &serde_json::Value,
) -> LiliaError {
    LiliaError::constraint_violation(
        ConstraintKind::Reference,
        &reference.name,
        message,
        json!({
            "model": reference.source.model(),
            "collection": reference.source.collection(),
            "id": id,
            "references": reference.target_space,
            "context": extra,
        }),
    )
}

fn restrict_violation(reference: &Reference, parent_id: &str, children: usize) -> LiliaError {
    LiliaError::constraint_violation(
        ConstraintKind::Restrict,
        &reference.name,
        format!(
            "cannot delete '{}/{parent_id}': {children} {} row(s) in '{}' still reference it",
            reference.target_space,
            reference.source.model(),
            reference.source.collection()
        ),
        json!({
            "model": "json",
            "collection": reference.target_space,
            "id": parent_id,
            "referenced_by": { "model": reference.source.model(), "collection": reference.source.collection() },
            "children": children,
        }),
    )
}

/// `true` when the JSON document `(space, id)` exists in any data schema.
fn json_exists(
    tx: &mut NativeTransaction<'_>,
    shards: Option<&ShardManager>,
    space: &str,
    id: &str,
) -> Result<bool> {
    // Without sharding the version slot reads `_lilia_json` directly; with sharding the locate
    // slot reads the union view spanning every shard.
    let slot = if shards.is_some() {
        StmtSlot::JsonLocate
    } else {
        StmtSlot::JsonVersion
    };
    let stmt = tx.slot(slot)?;
    stmt.reset()?;
    stmt.bind_text_static(1, space)?;
    stmt.bind_text_static(2, id)?;
    let found = stmt.step()?;
    stmt.reset()?;
    Ok(found)
}

fn graph_children(
    tx: &NativeTransaction<'_>,
    graph: &str,
    space: &str,
    doc_id: &str,
) -> Result<Vec<String>> {
    let mut stmt = tx.prepare(
        "SELECT id FROM _lilia_graph_nodes WHERE graph = ?1 AND doc_space = ?2 AND doc_id = ?3;",
    )?;
    stmt.bind_text_static(1, graph)?;
    stmt.bind_text_static(2, space)?;
    stmt.bind_text_static(3, doc_id)?;
    let mut ids = Vec::new();
    while stmt.step()? {
        ids.push(stmt.column_text(0).to_owned());
    }
    Ok(ids)
}

fn kv_children(
    tx: &NativeTransaction<'_>,
    namespace: &str,
    mapping: KeyMapping,
    parent: &str,
) -> Result<Vec<Vec<u8>>> {
    let (lower, upper) = mapping.child_range(parent);
    let mut stmt = tx.prepare(
        "SELECT key FROM _lilia_kv WHERE namespace = ?1 AND key >= ?2 AND key < ?3;",
    )?;
    stmt.bind_text_static(1, namespace)?;
    stmt.bind_blob_static(2, &lower)?;
    stmt.bind_blob_static(3, &upper)?;
    let mut keys = Vec::new();
    while stmt.step()? {
        keys.push(stmt.column_blob(0).to_vec());
    }
    Ok(keys)
}

/// Verifies `operation` against the declared references **before** it is applied and returns the
/// follow-up operations (cascades) that must be applied in the same transaction.
pub(crate) fn guard(
    tx: &mut NativeTransaction<'_>,
    operation: &BatchOperation,
    registry: &IntegrityRegistry,
    shards: Option<&ShardManager>,
) -> Result<Vec<BatchOperation>> {
    match operation {
        BatchOperation::KvSet { namespace, key, .. } => {
            for reference in registry.refs_for_kv(namespace) {
                let key_text = String::from_utf8_lossy(key).into_owned();
                let Some(parent) = reference.key.parent_id(key) else {
                    return Err(reference_violation(
                        &reference,
                        format!(
                            "key '{key_text}' of '{namespace}' does not designate a '{}' document id",
                            reference.target_space
                        ),
                        &key_text,
                        &json!({ "key_mapping": reference.key }),
                ));
                };
                if !json_exists(tx, shards, &reference.target_space, parent)? {
                    return Err(reference_violation(
                        &reference,
                        format!(
                            "key '{key_text}' of '{namespace}' references missing document '{}/{parent}'",
                            reference.target_space
                        ),
                        &key_text,
                        &json!({ "parent": parent }),
                ));
                }
            }
            Ok(Vec::new())
        }
        BatchOperation::GraphSetNode {
            graph,
            id,
            doc_space,
            doc_id,
            ..
        } => {
            let references = registry.refs_for_graph(graph);
            if references.is_empty() {
                return Ok(Vec::new());
            }
            let Some(space) = doc_space.as_deref() else {
                if let Some(required) = references.iter().find(|r| r.required) {
                    return Err(reference_violation(
                        required,
                        format!("node '{id}' of graph '{graph}' must link to a '{}' document", required.target_space),
                        id,
                        &json!({}),
                ));
                }
                return Ok(Vec::new());
            };
            let parent = doc_id.as_deref().unwrap_or(id);
            let Some(reference) = references.iter().find(|r| r.target_space == space) else {
                let declared: Vec<&str> = references.iter().map(|r| r.target_space.as_str()).collect();
                return Err(reference_violation(
                    &references[0],
                    format!("node '{id}' of graph '{graph}' links to undeclared space '{space}'"),
                    id,
                    &json!({ "declared_spaces": declared }),
                ));
            };
            if !json_exists(tx, shards, space, parent)? {
                return Err(reference_violation(
                    reference,
                    format!("node '{id}' of graph '{graph}' references missing document '{space}/{parent}'"),
                    id,
                    &json!({ "parent": parent }),
                ));
            }
            Ok(Vec::new())
        }
        BatchOperation::JsonDelete { space, id, .. } => {
            let references = registry.refs_to_space(space);
            if references.is_empty() || !json_exists(tx, shards, space, id)? {
                return Ok(Vec::new());
            }
            let mut follow_ups = Vec::new();
            for reference in references {
                follow_ups.extend(resolve_children(tx, &reference, id)?);
            }
            Ok(follow_ups)
        }
        _ => Ok(Vec::new()),
    }
}

/// Applies `reference.on_delete` to the children of the document `parent_id` that is being deleted.
fn resolve_children(
    tx: &mut NativeTransaction<'_>,
    reference: &Reference,
    parent_id: &str,
) -> Result<Vec<BatchOperation>> {
    match &reference.source {
        ReferenceSource::GraphNodes { graph } => {
            let children = graph_children(tx, graph, &reference.target_space, parent_id)?;
            if children.is_empty() {
                return Ok(Vec::new());
            }
            match reference.on_delete {
                RefAction::Restrict => Err(restrict_violation(reference, parent_id, children.len())),
                RefAction::Cascade => Ok(children
                    .into_iter()
                    .map(|id| BatchOperation::GraphDeleteNode {
                        graph: graph.clone(),
                        id,
                    })
                    .collect()),
                RefAction::SetNull => {
                    let mut stmt = tx.prepare(
                        "UPDATE _lilia_graph_nodes SET doc_space = NULL, doc_id = NULL \
                         WHERE graph = ?1 AND doc_space = ?2 AND doc_id = ?3;",
                    )?;
                    stmt.bind_text_static(1, graph)?;
                    stmt.bind_text_static(2, &reference.target_space)?;
                    stmt.bind_text_static(3, parent_id)?;
                    stmt.step()?;
                    Ok(Vec::new())
                }
            }
        }
        ReferenceSource::Kv { namespace } => {
            let keys = kv_children(tx, namespace, reference.key, parent_id)?;
            if keys.is_empty() {
                return Ok(Vec::new());
            }
            match reference.on_delete {
                RefAction::Cascade => Ok(keys
                    .into_iter()
                    .map(|key| BatchOperation::KvDelete {
                        namespace: namespace.clone(),
                        key,
                        if_version: None,
                    })
                    .collect()),
                RefAction::Restrict | RefAction::SetNull => {
                    Err(restrict_violation(reference, parent_id, keys.len()))
                }
            }
        }
    }
}

fn load_references(conn: &NativeConnection) -> Result<Vec<Reference>> {
    let mut stmt = conn.prepare(
        "SELECT name, source_kind, source_name, target_space, key_mapping_json, on_delete, required \
         FROM _lilia_references;",
    )?;
    let mut references = Vec::new();
    while stmt.step()? {
        let source_name = stmt.column_text(2).to_owned();
        let source = match stmt.column_text(1) {
            "graph" => ReferenceSource::GraphNodes { graph: source_name },
            "kv" => ReferenceSource::Kv {
                namespace: source_name,
            },
            other => {
                return Err(LiliaError::new(
                    ErrorCode::Corrupt,
                    format!("unknown reference source '{other}' in _lilia_references"),
                    false,
                ))
            }
        };
        let key: KeyMapping = serde_json::from_str(stmt.column_text(4)).map_err(|error| {
            LiliaError::new(ErrorCode::Corrupt, format!("invalid key mapping: {error}"), false)
        })?;
        let on_delete = RefAction::parse(stmt.column_text(5)).ok_or_else(|| {
            LiliaError::new(ErrorCode::Corrupt, "invalid reference action", false)
        })?;
        references.push(Reference {
            name: stmt.column_text(0).to_owned(),
            source,
            target_space: stmt.column_text(3).to_owned(),
            on_delete,
            key,
            required: stmt.column_int64(6) != 0,
        });
    }
    Ok(references)
}

fn persist_reference(conn: &NativeConnection, reference: &Reference) -> Result<()> {
    let key_json = serde_json::to_string(&reference.key)
        .map_err(|error| LiliaError::new(ErrorCode::Storage, error.to_string(), false))?;
    let mut stmt = conn.prepare(
        "INSERT INTO _lilia_references(name, source_kind, source_name, target_space, key_mapping_json, on_delete, required, created_at_ms) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8);",
    )?;
    stmt.bind_text_static(1, &reference.name)?;
    stmt.bind_text_static(2, reference.source.model())?;
    stmt.bind_text_static(3, reference.source.collection())?;
    stmt.bind_text_static(4, &reference.target_space)?;
    stmt.bind_text_static(5, &key_json)?;
    stmt.bind_text_static(6, reference.on_delete.as_str())?;
    stmt.bind_int64(7, i64::from(reference.required))?;
    stmt.bind_int64(8, now_ms())?;
    stmt.step()?;
    Ok(())
}

pub(crate) fn require_disk(db: &Database, kind: CollectionKind, name: &str) -> Result<()> {
    let tier = db.catalog.tier_for(kind, name);
    if tier == StorageTier::Disk {
        Ok(())
    } else {
        Err(LiliaError::new(
            ErrorCode::Unsupported,
            format!("integrity and auto-embed contracts require the Disk tier: {kind} '{name}' is on {tier}"),
            false,
        ))
    }
}

fn offenders_error(
    kind: ConstraintKind,
    constraint: &str,
    message: String,
    offenders: &[String],
) -> LiliaError {
    LiliaError::constraint_violation(
        kind,
        constraint,
        message,
        json!({ "existing_violations": offenders }),
    )
}

impl Database {
    /// Entry point for declaring cross-model references.
    #[must_use]
    pub fn integrity(&self) -> Integrity<'_> {
        Integrity { db: self }
    }

    /// Rejects moving a collection that takes part in an integrity contract off the `Disk` tier:
    /// `Memory` writes land after the SQLite commit and `Buffered` writes are asynchronous, so
    /// neither can honour a contract atomically.
    pub(crate) fn ensure_tier_allowed(
        &self,
        kind: CollectionKind,
        name: &str,
        tier: StorageTier,
    ) -> Result<()> {
        if tier != StorageTier::Disk && self.integrity.is_constrained(kind, name) {
            return Err(LiliaError::new(
                ErrorCode::Unsupported,
                format!("{kind} '{name}' takes part in an integrity or auto-embed contract and must stay on the Disk tier"),
                false,
            ));
        }
        Ok(())
    }

    /// Makes a collection spec consistent with the declared contracts before it is stored:
    /// the tier is validated, the `CHECK` expression can only change through
    /// [`JsonStore::set_check`](crate::JsonStore::set_check) and the auto-embed configuration
    /// through [`JsonStore::set_auto_embed`](crate::JsonStore::set_auto_embed).
    pub(crate) fn reconcile_spec(
        &self,
        mut spec: lilia_core::CollectionSpec,
    ) -> Result<lilia_core::CollectionSpec> {
        self.ensure_tier_allowed(spec.kind, &spec.name, spec.tier)?;
        if spec.kind == CollectionKind::Json {
            spec.check = match self.integrity.check_for(&spec.name) {
                Some(check) => {
                    CompiledCheck::compile(check.source(), spec.fle_policy.as_ref())?;
                    Some(check.source().to_owned())
                }
                None => None,
            };
            spec.auto_embed = match self.integrity.auto_embed_for(&spec.name) {
                Some(config) => {
                    crate::embed::ensure_source_readable(&config, spec.fle_policy.as_ref())?;
                    Some((*config).clone())
                }
                None => None,
            };
        }
        Ok(spec)
    }

    /// Declares a JSON collection with its options (`CHECK` expression, auto-embed).
    pub(crate) fn declare_json_collection(
        &self,
        space: &str,
        options: &CollectionOptions,
    ) -> Result<()> {
        validate_name("collection", space)?;
        if self
            .catalog
            .list_specs()
            .iter()
            .any(|spec| spec.kind == CollectionKind::Json && spec.name == space)
        {
            return Err(LiliaError::new(
                ErrorCode::Conflict,
                format!("JSON collection '{space}' already exists"),
                false,
            ));
        }
        if let Some(config) = &options.auto_embed {
            // Validated first so a refused auto-embed leaves no half-declared collection behind.
            self.validate_auto_embed(space, config)?;
        }
        self.apply_json_check(space, options.check.as_deref())?;
        if let Some(config) = &options.auto_embed {
            self.apply_json_auto_embed(space, Some(config))?;
        }
        Ok(())
    }

    /// Replaces (`Some`) or drops (`None`) the `CHECK` expression of a JSON collection.
    ///
    /// Existing documents are validated first: the change is refused if any of them would violate it.
    pub(crate) fn apply_json_check(&self, space: &str, check: Option<&str>) -> Result<()> {
        validate_name("collection", space)?;
        require_disk(self, CollectionKind::Json, space)?;
        let mut spec = self.catalog.spec_for(CollectionKind::Json, space);
        let compiled = check
            .map(|source| CompiledCheck::compile(source, spec.fle_policy.as_ref()))
            .transpose()?;
        spec.check = check.map(str::to_owned);

        let writer = self.lock_writer()?;
        if let Some(compiled) = &compiled {
            let offenders = self.check_offenders(&writer, space, compiled)?;
            if !offenders.is_empty() {
                return Err(offenders_error(
                    ConstraintKind::Check,
                    &format!("check:{space}"),
                    format!("existing documents of '{space}' violate the CHECK expression"),
                    &offenders,
                ));
            }
        }
        CollectionCatalog::persist_spec(&writer, &spec)?;
        self.catalog.set_spec(spec);
        self.integrity.set_check(space, compiled);
        Ok(())
    }

    fn check_offenders(
        &self,
        conn: &NativeConnection,
        space: &str,
        check: &CompiledCheck,
    ) -> Result<Vec<String>> {
        let mut offenders = Vec::new();
        for schema in self.data_schemas() {
            let table = crate::sharding::qualified(schema.as_deref(), "_lilia_json");
            let mut stmt = conn.prepare(&format!(
                "SELECT id, value, format, dict_id FROM {table} WHERE space = ?1;"
            ))?;
            stmt.bind_text_static(1, space)?;
            while stmt.step()? {
                let doc = crate::json::decode_stored_doc(
                    &stmt,
                    1,
                    stmt.column_int64(2),
                    stmt.column_int64(3),
                    space,
                    self.codec.as_ref(),
                    &self.dict_registry,
                    None,
                )?;
                if check.is_violated_by(&doc)? {
                    offenders.push(stmt.column_text(0).to_owned());
                    if offenders.len() >= MAX_REPORTED_OFFENDERS {
                        return Ok(offenders);
                    }
                }
            }
        }
        Ok(offenders)
    }

    fn json_exists_anywhere(&self, conn: &NativeConnection, space: &str, id: &str) -> Result<bool> {
        for schema in self.data_schemas() {
            let table = crate::sharding::qualified(schema.as_deref(), "_lilia_json");
            let mut stmt =
                conn.prepare(&format!("SELECT 1 FROM {table} WHERE space = ?1 AND id = ?2;"))?;
            stmt.bind_text_static(1, space)?;
            stmt.bind_text_static(2, id)?;
            if stmt.step()? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Lists children that would violate `reference` if it were declared now.
    fn reference_offenders(
        &self,
        conn: &NativeConnection,
        reference: &Reference,
    ) -> Result<Vec<String>> {
        let mut offenders = Vec::new();
        match &reference.source {
            ReferenceSource::GraphNodes { graph } => {
                let sibling_spaces: Vec<String> = self
                    .integrity
                    .refs_for_graph(graph)
                    .iter()
                    .map(|r| r.target_space.clone())
                    .chain(std::iter::once(reference.target_space.clone()))
                    .collect();
                let mut nodes = Vec::new();
                let mut stmt = conn.prepare(
                    "SELECT id, doc_space, doc_id FROM _lilia_graph_nodes WHERE graph = ?1;",
                )?;
                stmt.bind_text_static(1, graph)?;
                while stmt.step()? {
                    let space = (!stmt.column_is_null(1)).then(|| stmt.column_text(1).to_owned());
                    let doc_id = (!stmt.column_is_null(2)).then(|| stmt.column_text(2).to_owned());
                    nodes.push((stmt.column_text(0).to_owned(), space, doc_id));
                }
                drop(stmt);
                for (id, space, doc_id) in nodes {
                    let valid = match space {
                        None => !reference.required,
                        Some(space) => {
                            sibling_spaces.contains(&space)
                                && self.json_exists_anywhere(conn, &space, doc_id.as_deref().unwrap_or(&id))?
                        }
                    };
                    if !valid {
                        offenders.push(id);
                        if offenders.len() >= MAX_REPORTED_OFFENDERS {
                            break;
                        }
                    }
                }
            }
            ReferenceSource::Kv { namespace } => {
                let mut keys = Vec::new();
                for schema in self.data_schemas() {
                    let table = crate::sharding::qualified(schema.as_deref(), "_lilia_kv");
                    let mut stmt =
                        conn.prepare(&format!("SELECT key FROM {table} WHERE namespace = ?1;"))?;
                    stmt.bind_text_static(1, namespace)?;
                    while stmt.step()? {
                        keys.push(stmt.column_blob(0).to_vec());
                    }
                }
                for key in keys {
                    let valid = match reference.key.parent_id(&key) {
                        Some(parent) => {
                            self.json_exists_anywhere(conn, &reference.target_space, parent)?
                        }
                        None => false,
                    };
                    if !valid {
                        offenders.push(String::from_utf8_lossy(&key).into_owned());
                        if offenders.len() >= MAX_REPORTED_OFFENDERS {
                            break;
                        }
                    }
                }
            }
        }
        Ok(offenders)
    }
}

/// Declares and inspects cross-model references. Obtained from [`Database::integrity`].
#[derive(Debug, Clone, Copy)]
pub struct Integrity<'a> {
    db: &'a Database,
}

impl Integrity<'_> {
    /// Declares a foreign key from graph nodes or KV keys to JSON documents.
    ///
    /// Existing data is validated first; the declaration is refused if any child already
    /// violates it. All participating collections must live on the `Disk` tier.
    ///
    /// # Errors
    ///
    /// `InvalidInput` for an inconsistent declaration, `Unsupported` off the `Disk` tier,
    /// `Conflict` for a duplicate name and `CONSTRAINT_VIOLATION` when existing children violate it.
    pub fn add_reference(&self, reference: Reference) -> Result<()> {
        reference.validate()?;
        validate_name("reference", &reference.name)?;
        validate_name("target_space", &reference.target_space)?;
        validate_name("source", reference.source.collection())?;
        let db = self.db;
        let source_kind = match reference.source {
            ReferenceSource::GraphNodes { .. } => CollectionKind::Graph,
            ReferenceSource::Kv { .. } => CollectionKind::Kv,
        };
        require_disk(db, source_kind, reference.source.collection())?;
        require_disk(db, CollectionKind::Json, &reference.target_space)?;

        let writer = db.lock_writer()?;
        if db.integrity.references().iter().any(|r| r.name == reference.name) {
            return Err(LiliaError::new(
                ErrorCode::Conflict,
                format!("reference '{}' already exists", reference.name),
                false,
            ));
        }
        let offenders = db.reference_offenders(&writer, &reference)?;
        if !offenders.is_empty() {
            return Err(offenders_error(
                ConstraintKind::Reference,
                &reference.name,
                format!(
                    "existing {} rows of '{}' violate the reference to '{}'",
                    reference.source.model(),
                    reference.source.collection(),
                    reference.target_space
                ),
                &offenders,
            ));
        }
        persist_reference(&writer, &reference)?;
        db.integrity.insert_reference(reference);
        Ok(())
    }

    /// Removes a reference by name; returns `false` when it does not exist.
    ///
    /// # Errors
    ///
    /// Returns a storage error if the catalog row cannot be deleted.
    pub fn drop_reference(&self, name: &str) -> Result<bool> {
        let writer = self.db.lock_writer()?;
        let mut stmt = writer.prepare("DELETE FROM _lilia_references WHERE name = ?1;")?;
        stmt.bind_text_static(1, name)?;
        stmt.step()?;
        drop(stmt);
        Ok(self.db.integrity.remove_reference(name))
    }

    /// Declared references, sorted by name.
    #[must_use]
    pub fn references(&self) -> Vec<Reference> {
        self.db.integrity.references()
    }

    /// Declared `CHECK` expressions as `(collection, expression)` pairs, sorted by collection.
    #[must_use]
    pub fn checks(&self) -> Vec<(String, String)> {
        self.db.integrity.checks()
    }
}
