//! The database: opening, the commit loop, catalog administration, the ledger, and sessions.

use std::collections::{BTreeSet, HashMap};
use std::ops::Bound;
use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use lilia_core::{
    IndexOptions, JsonIndexInfo, LedgerCommit, LedgerVerificationReport, LedgerVerifyMode,
    MaskingRule, NamedPolicy, Policy, SecurityContext,
};
use lilia_engine::{Db, WriteBatch};
use lilia_io::{HardwareProfile, NativeVfs, Vfs};
use parking_lot::{Mutex, RwLock};

use crate::batch::{Outcome, Write};
use crate::catalog::{Catalog, CompiledMask, CompiledPolicy, Index, Schema};
use crate::error::{corruption, invalid, Error, Result};
use crate::fts::Vocabularies;
use crate::guard::Access;
use crate::keys::{self, validate_name};
use crate::ledger::{Attempt, CommitProof, Ledger, RecordProof};
use crate::record::{decode_doc, decode_kv, now_ms};
use crate::session::Session;
use crate::write::{Plan, Planner};

/// Version of this layer's on-disk layout ([`crate::keys`], [`crate::record`]).
const FORMAT_VERSION: u8 = 1;
/// Documents indexed, or index entries deleted, per engine batch while building or dropping an
/// index.
const MAINTENANCE_CHUNK: usize = 256;

/// A document read by an index build: its key, version, id and index values.
type IndexedDoc = (Bytes, u64, String, BTreeSet<String>);

/// Database configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub engine: lilia_engine::Options,
    /// Keep a ledger of every record write ([`crate::ledger`]). A database that keeps a ledger
    /// must always be opened with this set: a ledger cannot be turned off.
    pub ledger: bool,
    /// Verification of the ledger when the database opens; an invalid ledger fails the open.
    pub verify_ledger: LedgerVerifyMode,
}

impl Options {
    /// Engine buffers sized from `hardware`, no ledger.
    #[must_use]
    pub fn for_hardware(hardware: &HardwareProfile) -> Self {
        Self {
            engine: lilia_engine::Options::for_hardware(hardware),
            ledger: false,
            verify_ledger: LedgerVerifyMode::None,
        }
    }
}

impl Default for Options {
    fn default() -> Self {
        Self::for_hardware(&HardwareProfile::probe())
    }
}

/// An open database. Every method takes `&self`; share it across threads by reference or `Arc`.
///
/// Reads and writes go through a [`Session`], which carries the caller's identity: row-level
/// security policies and masking rules apply to every session but [`Database::system`].
#[derive(Debug)]
pub struct Database {
    engine: Db,
    /// Writers hold the read lock from planning to commit, so a catalog change (which takes the
    /// write lock) never lands between a plan and its commit.
    catalog: RwLock<Catalog>,
    ledger: Option<Ledger>,
    /// Serializes index builds and drops, which run outside the catalog lock.
    admin: Mutex<()>,
    /// Full-text vocabularies loaded so far.
    vocabularies: Vocabularies,
}

impl Database {
    /// Opens (creating if allowed) the database in `path` on the native file system.
    ///
    /// # Errors
    ///
    /// Engine errors, an unsupported layout, a ledger that cannot be opened or fails
    /// verification.
    pub fn open(path: impl AsRef<Path>, options: Options) -> Result<Self> {
        Self::open_with(Arc::new(NativeVfs), path, options)
    }

    /// Opens the database in `path` on `vfs`.
    ///
    /// # Errors
    ///
    /// As [`Database::open`].
    pub fn open_with(vfs: Arc<dyn Vfs>, path: impl AsRef<Path>, options: Options) -> Result<Self> {
        let max_group_bytes = options.engine.max_group_bytes;
        let engine = Db::open_with(vfs, path, options.engine)?;
        match engine.get(keys::FORMAT)? {
            None => {
                engine.put(Bytes::from_static(keys::FORMAT), vec![FORMAT_VERSION])?;
            }
            Some(format) if format.as_ref() == [FORMAT_VERSION] => {}
            Some(format) => {
                return Err(invalid(format!(
                    "unsupported data layout {format:?}; this build reads layout {FORMAT_VERSION}"
                )))
            }
        }
        let catalog = Catalog::load(&engine)?;
        let ledger = Ledger::open(&engine, options.ledger, max_group_bytes)?;
        let db = Self {
            engine,
            catalog: RwLock::new(catalog),
            ledger,
            admin: Mutex::new(()),
            vocabularies: Vocabularies::default(),
        };
        db.finish_maintenance()?;
        if db.ledger.is_some() && options.verify_ledger != LedgerVerifyMode::None {
            let report = Ledger::verify(&db.engine, options.verify_ledger)?;
            if !report.is_valid {
                return Err(corruption(format!(
                    "ledger verification failed: {}",
                    report.error.unwrap_or_default()
                )));
            }
        }
        Ok(db)
    }

    /// Completes the index builds and drops an earlier process left unfinished.
    fn finish_maintenance(&self) -> Result<()> {
        let pending: Vec<(String, Vec<u64>, Vec<u64>)> = self
            .catalog
            .read()
            .schemas()
            .map(|(name, schema)| {
                (
                    name.clone(),
                    schema
                        .indexes
                        .iter()
                        .filter(|i| !i.ready)
                        .map(|i| i.id)
                        .collect::<Vec<_>>(),
                    schema.dropping.clone(),
                )
            })
            .filter(|(_, building, dropping)| !building.is_empty() || !dropping.is_empty())
            .collect();
        for (space, building, dropping) in pending {
            for id in building {
                self.build_index(&space, id)?;
            }
            for id in dropping {
                self.clear_index(&space, id)?;
            }
        }
        Ok(())
    }

    /// A session whose reads and writes are subject to every policy and masking rule.
    #[must_use]
    pub fn session(&self, context: SecurityContext) -> Session<'_> {
        Session::new(self, Access::Caller(context))
    }

    /// A session of trusted code: no policy or masking rule applies.
    #[must_use]
    pub fn system(&self) -> Session<'_> {
        Session::new(self, Access::System)
    }

    pub(crate) fn vocabularies(&self) -> &Vocabularies {
        &self.vocabularies
    }

    pub(crate) fn engine(&self) -> &Db {
        &self.engine
    }

    pub(crate) fn schema(&self, name: &str) -> Option<Arc<Schema>> {
        self.catalog.read().schema(name)
    }

    /// Plans with `plan` and commits, planning again while the plan goes stale.
    fn run(&self, plan: impl Fn(&Catalog) -> Result<Plan>) -> Result<Vec<Outcome>> {
        loop {
            let catalog = self.catalog.read();
            let mut plan = plan(&catalog)?;
            let vocabulary = std::mem::take(&mut plan.vocabulary);
            let attempt = match &self.ledger {
                Some(ledger) => ledger.commit(&self.engine, plan)?,
                None => self.commit_plain(&plan)?,
            };
            drop(catalog);
            if let Attempt::Committed { first, outcomes } = attempt {
                self.vocabularies.apply(&vocabulary, first);
                return Ok(outcomes);
            }
        }
    }

    fn commit_plain(&self, plan: &Plan) -> Result<Attempt> {
        match self.engine.write(plan.batch()) {
            Ok(first) => Ok(Attempt::Committed {
                first,
                outcomes: plan.outcomes(first),
            }),
            Err(lilia_engine::Error::Conflict {
                key,
                expected,
                actual,
            }) => plan
                .conflict(&key, expected, actual)
                .map_or(Ok(Attempt::Retry), Err),
            Err(error) => Err(error.into()),
        }
    }

    /// Commits `writes` for `access`; `reads` are a transaction's reads, which must still hold.
    pub(crate) fn execute(
        &self,
        access: &Access,
        writes: &[Write],
        reads: Option<&HashMap<Bytes, Option<u64>>>,
    ) -> Result<Vec<Outcome>> {
        self.run(|catalog| {
            let mut planner = Planner::new(&self.engine, catalog, access, now_ms());
            for write in writes {
                planner.add(write)?;
            }
            if let Some(reads) = reads {
                planner.require(reads)?;
            }
            Ok(planner.finish())
        })
    }

    fn store_schema(&self, catalog: &mut Catalog, name: &str, schema: Schema) -> Result<()> {
        if schema.is_empty() {
            self.engine.delete(keys::schema(name))?;
        } else {
            self.engine.put(keys::schema(name), schema.encode()?)?;
        }
        catalog.set(name, schema);
        Ok(())
    }

    // ---- secondary indexes ----

    /// Creates a secondary index of `space` over `expression` (a path such as `$.email`, or an
    /// expression such as `lower($.email)`), optionally partial ([`IndexOptions::filter`]).
    /// Returns once the existing documents are indexed; writes keep going meanwhile.
    ///
    /// # Errors
    ///
    /// Invalid names, an index of that name already on the space, an expression that does not
    /// parse, engine errors.
    pub fn create_index(
        &self,
        space: &str,
        name: &str,
        expression: &str,
        options: &IndexOptions,
    ) -> Result<()> {
        validate_name("space", space)?;
        validate_name("index name", name)?;
        let _admin = self.admin.lock();
        let id = {
            let mut catalog = self.catalog.write();
            let mut schema = catalog.schema_or_default(space);
            if schema.index(name).is_some() {
                return Err(invalid(format!("index {name} already exists on {space}")));
            }
            let id = catalog.next_index_id;
            schema
                .indexes
                .push(Index::new(id, name, expression, options.filter.as_deref())?);
            let mut batch = WriteBatch::new();
            batch.put(keys::schema(space), schema.encode()?).put(
                Bytes::from_static(keys::NEXT_INDEX),
                (id + 1).to_le_bytes().to_vec(),
            );
            self.engine.write(batch)?;
            catalog.next_index_id = id + 1;
            catalog.set(space, schema);
            id
        };
        self.build_index(space, id)
    }

    /// Indexes the documents of `space` present when the build starts, then marks index `id`
    /// ready. Writers have maintained the index since its definition was installed; each entry
    /// written here is conditional on its document being unchanged, so a document a writer
    /// changed meanwhile keeps the writer's entries only.
    fn build_index(&self, space: &str, id: u64) -> Result<()> {
        let index = self
            .schema(space)
            .and_then(|s| s.indexes.iter().find(|i| i.id == id).cloned())
            .ok_or_else(|| corruption(format!("index {id} of {space} is missing")))?;
        let snapshot = self.engine.snapshot();
        let base = keys::collection(keys::DOC, space);
        let end = keys::prefix_end(&base).unwrap_or_default();
        let mut chunk = Vec::with_capacity(MAINTENANCE_CHUNK);
        for entry in
            self.engine
                .range_at(&snapshot, Bound::Included(&base), Bound::Excluded(&end))?
        {
            let entry = entry?;
            let doc_id = String::from_utf8(keys::suffix(&entry.key, &base).to_vec())
                .map_err(|_| corruption(format!("document id in {space} is not UTF-8")))?;
            let values = index.values(&decode_doc(&entry.value)?);
            chunk.push((entry.key, entry.version, doc_id, values));
            if chunk.len() == MAINTENANCE_CHUNK {
                self.index_chunk(space, id, &chunk)?;
                chunk.clear();
            }
        }
        self.index_chunk(space, id, &chunk)?;
        drop(snapshot);

        let mut catalog = self.catalog.write();
        let mut schema = catalog.schema_or_default(space);
        if let Some(index) = schema.indexes.iter_mut().find(|i| i.id == id) {
            index.ready = true;
        }
        self.store_schema(&mut catalog, space, schema)
    }

    fn index_chunk(&self, space: &str, id: u64, chunk: &[IndexedDoc]) -> Result<()> {
        let batch_of = |docs: &[IndexedDoc]| {
            let mut batch = WriteBatch::new();
            for (key, version, doc_id, values) in docs {
                batch.expect(key.clone(), Some(*version));
                for value in values {
                    batch.put(keys::index_entry(space, id, value, doc_id), Bytes::new());
                }
            }
            batch
        };
        if chunk.is_empty() {
            return Ok(());
        }
        match self.engine.write(batch_of(chunk)) {
            Ok(_) => Ok(()),
            // Some documents changed since the snapshot: index the others one by one.
            Err(lilia_engine::Error::Conflict { .. }) => {
                for doc in chunk {
                    match self.engine.write(batch_of(std::slice::from_ref(doc))) {
                        Ok(_) | Err(lilia_engine::Error::Conflict { .. }) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Drops index `name` of `space`; `false` if there is none.
    ///
    /// # Errors
    ///
    /// Engine errors.
    pub fn drop_index(&self, space: &str, name: &str) -> Result<bool> {
        validate_name("space", space)?;
        let _admin = self.admin.lock();
        let id = {
            let mut catalog = self.catalog.write();
            let mut schema = catalog.schema_or_default(space);
            let Some(position) = schema.indexes.iter().position(|i| i.name == name) else {
                return Ok(false);
            };
            let id = schema.indexes.remove(position).id;
            schema.dropping.push(id);
            self.store_schema(&mut catalog, space, schema)?;
            id
        };
        self.clear_index(space, id)?;
        Ok(true)
    }

    /// Deletes every entry of dropped index `id`, then forgets it. Nothing writes entries of a
    /// dropped index, so the deletes need no conditions.
    fn clear_index(&self, space: &str, id: u64) -> Result<()> {
        let prefix = keys::index(space, id);
        let end = keys::prefix_end(&prefix).unwrap_or_default();
        loop {
            let mut batch = WriteBatch::new();
            for entry in self
                .engine
                .range(Bound::Included(&prefix), Bound::Excluded(&end))?
                .take(MAINTENANCE_CHUNK * 4)
            {
                batch.delete(entry?.key);
            }
            if batch.is_empty() {
                break;
            }
            self.engine.write(batch)?;
        }
        let mut catalog = self.catalog.write();
        let mut schema = catalog.schema_or_default(space);
        schema.dropping.retain(|&d| d != id);
        self.store_schema(&mut catalog, space, schema)
    }

    /// The indexes of `space`, including those still being built.
    #[must_use]
    pub fn indexes(&self, space: &str) -> Vec<JsonIndexInfo> {
        self.schema(space)
            .map_or_default(|s| s.indexes.iter().map(|i| i.info(space)).collect())
    }

    // ---- row-level security and masking ----

    /// Creates the row-level security policy `name` on the collection `policy.table`.
    ///
    /// # Errors
    ///
    /// Invalid names, a policy of that name already, a condition that does not parse, engine
    /// errors.
    pub fn create_policy(&self, name: &str, policy: Policy) -> Result<()> {
        validate_name("policy name", name)?;
        validate_name("policy table", &policy.table)?;
        let created_at_ms = u64::try_from(now_ms()).unwrap_or_default();
        let compiled = CompiledPolicy::new(NamedPolicy {
            name: name.to_owned(),
            policy,
            created_at_ms,
        })?;
        let mut catalog = self.catalog.write();
        if catalog
            .schemas()
            .any(|(_, s)| s.policies.iter().any(|p| p.named.name == name))
        {
            return Err(invalid(format!("policy {name} already exists")));
        }
        let table = compiled.named.policy.table.clone();
        let mut schema = catalog.schema_or_default(&table);
        schema.policies.push(compiled);
        self.store_schema(&mut catalog, &table, schema)
    }

    /// Drops policy `name`; `false` if there is none.
    ///
    /// # Errors
    ///
    /// Engine errors.
    pub fn drop_policy(&self, name: &str) -> Result<bool> {
        let mut catalog = self.catalog.write();
        let Some(table) = catalog
            .schemas()
            .find(|(_, s)| s.policies.iter().any(|p| p.named.name == name))
            .map(|(t, _)| t.clone())
        else {
            return Ok(false);
        };
        let mut schema = catalog.schema_or_default(&table);
        schema.policies.retain(|p| p.named.name != name);
        self.store_schema(&mut catalog, &table, schema)?;
        Ok(true)
    }

    /// Every policy, by name.
    #[must_use]
    pub fn policies(&self) -> Vec<NamedPolicy> {
        let catalog = self.catalog.read();
        let mut policies: Vec<NamedPolicy> = catalog
            .schemas()
            .flat_map(|(_, s)| s.policies.iter().map(|p| p.named.clone()))
            .collect();
        policies.sort_by(|a, b| a.name.cmp(&b.name));
        policies
    }

    /// Creates a masking rule, replacing the rule of the same table and path.
    ///
    /// # Errors
    ///
    /// Invalid table name, an exemption condition that does not parse, engine errors.
    pub fn create_masking_rule(&self, rule: MaskingRule) -> Result<()> {
        validate_name("masking table", &rule.table)?;
        let compiled = CompiledMask::new(rule)?;
        let mut catalog = self.catalog.write();
        let table = compiled.rule.table.clone();
        let mut schema = catalog.schema_or_default(&table);
        schema
            .masking
            .retain(|m| !m.rule.matches_path(&compiled.rule.path));
        schema.masking.push(compiled);
        self.store_schema(&mut catalog, &table, schema)
    }

    /// Drops the masking rule of `table` on `path`; `false` if there is none.
    ///
    /// # Errors
    ///
    /// Engine errors.
    pub fn drop_masking_rule(&self, table: &str, path: &str) -> Result<bool> {
        let mut catalog = self.catalog.write();
        let mut schema = catalog.schema_or_default(table);
        let before = schema.masking.len();
        schema.masking.retain(|m| !m.rule.matches_path(path));
        if schema.masking.len() == before {
            return Ok(false);
        }
        self.store_schema(&mut catalog, table, schema)?;
        Ok(true)
    }

    /// Every masking rule, by table and path.
    #[must_use]
    pub fn masking_rules(&self) -> Vec<MaskingRule> {
        let catalog = self.catalog.read();
        let mut rules: Vec<MaskingRule> = catalog
            .schemas()
            .flat_map(|(_, s)| s.masking.iter().map(|m| m.rule.clone()))
            .collect();
        rules.sort_by(|a, b| (&a.table, &a.path).cmp(&(&b.table, &b.path)));
        rules
    }

    // ---- ledger ----

    fn ledger(&self) -> Result<&Ledger> {
        self.ledger.as_ref().ok_or(Error::LedgerDisabled)
    }

    /// The ledger's current root, which attests every commit so far.
    ///
    /// # Errors
    ///
    /// [`Error::LedgerDisabled`]; engine errors.
    pub fn ledger_root(&self) -> Result<[u8; 32]> {
        self.ledger()?;
        Ok(Ledger::state(&self.engine)?.0)
    }

    /// Number of ledger commits.
    ///
    /// # Errors
    ///
    /// [`Error::LedgerDisabled`]; engine errors.
    pub fn ledger_len(&self) -> Result<u64> {
        self.ledger()?;
        Ok(Ledger::state(&self.engine)?.1)
    }

    /// The header of ledger commit `sequence` (1-based).
    ///
    /// # Errors
    ///
    /// [`Error::LedgerDisabled`]; [`Error::NotFound`] past the last commit.
    pub fn ledger_commit(&self, sequence: u64) -> Result<LedgerCommit> {
        self.ledger()?;
        Ledger::commit_header(&self.engine, sequence)
    }

    /// Proof that commit `sequence` (1-based) is in the ledger.
    ///
    /// # Errors
    ///
    /// As [`Database::ledger_commit`].
    pub fn prove_commit(&self, sequence: u64) -> Result<CommitProof> {
        self.ledger()?;
        Ledger::prove_commit(&self.engine, sequence)
    }

    /// Proof that the current version of key `key` of `namespace` was written by a ledger commit.
    ///
    /// # Errors
    ///
    /// [`Error::LedgerDisabled`]; [`Error::NotFound`] for an absent record or one older than
    /// the ledger.
    pub fn prove_kv(&self, namespace: &str, key: &[u8]) -> Result<RecordProof> {
        self.ledger()?;
        validate_name("namespace", namespace)?;
        Ledger::prove_record(&self.engine, &keys::kv(namespace, key))
    }

    /// Proof that the current version of document `id` of `space` was written by a ledger commit.
    ///
    /// # Errors
    ///
    /// As [`Database::prove_kv`].
    pub fn prove_doc(&self, space: &str, id: &str) -> Result<RecordProof> {
        self.ledger()?;
        validate_name("space", space)?;
        Ledger::prove_record(&self.engine, &keys::doc(space, id))
    }

    /// Proof that the current version of node `id` of graph `graph` was written by a ledger
    /// commit.
    ///
    /// # Errors
    ///
    /// As [`Database::prove_kv`].
    pub fn prove_node(&self, graph: &str, id: &str) -> Result<RecordProof> {
        self.ledger()?;
        validate_name("graph", graph)?;
        Ledger::prove_record(&self.engine, &crate::graph::node_key(graph, id))
    }

    /// Proof that the current version of the edge `source -kind-> target` of graph `graph` was
    /// written by a ledger commit.
    ///
    /// # Errors
    ///
    /// As [`Database::prove_kv`].
    pub fn prove_edge(
        &self,
        graph: &str,
        source: &str,
        target: &str,
        kind: &str,
    ) -> Result<RecordProof> {
        self.ledger()?;
        validate_name("graph", graph)?;
        let key = crate::graph::edge_key(keys::EDGE_OUT, graph, source, kind, target);
        Ledger::prove_record(&self.engine, &key)
    }

    /// Proof that the current version of document `doc_id` of full-text collection
    /// `collection` was written by a ledger commit.
    ///
    /// # Errors
    ///
    /// As [`Database::prove_kv`].
    pub fn prove_fts_doc(&self, collection: &str, doc_id: &str) -> Result<RecordProof> {
        self.ledger()?;
        validate_name("collection", collection)?;
        Ledger::prove_record(&self.engine, &crate::fts::doc_key(collection, doc_id))
    }

    /// Verifies the ledger; see [`Ledger::verify`].
    ///
    /// # Errors
    ///
    /// [`Error::LedgerDisabled`]; engine errors. Integrity failures are in the report.
    pub fn verify_ledger(&self, mode: LedgerVerifyMode) -> Result<LedgerVerificationReport> {
        self.ledger()?;
        Ledger::verify(&self.engine, mode)
    }

    // ---- maintenance ----

    /// Deletes every expired key-value record (reads already skip them) and returns how many.
    /// With a ledger, each deletion is a ledger delta.
    ///
    /// # Errors
    ///
    /// Engine errors.
    pub fn purge_expired(&self) -> Result<u64> {
        let now = now_ms();
        let mut purged = 0u64;
        let mut chunk: Vec<(String, Vec<u8>)> = Vec::with_capacity(MAINTENANCE_CHUNK);
        let snapshot = self.engine.snapshot();
        let tags = [keys::KV, keys::KV + 1];
        for entry in self.engine.range_at(
            &snapshot,
            Bound::Included(&tags[..1]),
            Bound::Excluded(&tags[1..]),
        )? {
            let entry = entry?;
            if !decode_kv(&entry.value)?.expired(now) {
                continue;
            }
            let rest = &entry.key[1..];
            let split = rest
                .iter()
                .position(|&b| b == 0)
                .ok_or_else(|| corruption("key-value record key without a namespace"))?;
            let namespace = String::from_utf8(rest[..split].to_vec())
                .map_err(|_| corruption("namespace is not UTF-8"))?;
            chunk.push((namespace, rest[split + 1..].to_vec()));
            if chunk.len() == MAINTENANCE_CHUNK {
                purged += self.purge_chunk(&chunk)?;
                chunk.clear();
            }
        }
        if !chunk.is_empty() {
            purged += self.purge_chunk(&chunk)?;
        }
        Ok(purged)
    }

    fn purge_chunk(&self, chunk: &[(String, Vec<u8>)]) -> Result<u64> {
        let outcomes = self.run(|catalog| {
            let mut planner = Planner::new(&self.engine, catalog, &Access::System, now_ms());
            for (namespace, key) in chunk {
                planner.purge(namespace, key)?;
            }
            Ok(planner.finish())
        })?;
        Ok(outcomes.len() as u64)
    }

    /// Writes the memtable to a sorted run.
    ///
    /// # Errors
    ///
    /// Engine errors.
    pub fn flush(&self) -> Result<()> {
        Ok(self.engine.flush()?)
    }

    /// Closes the database, making every acknowledged write durable.
    ///
    /// # Errors
    ///
    /// The engine's failure, if writes had been disabled, or the final sync's.
    pub fn close(self) -> Result<()> {
        Ok(self.engine.close()?)
    }
}
