//! The write path: a batch is planned against the current state into engine writes and the
//! version expectations that make the plan valid, then committed atomically.
//!
//! Planning reads the records a write replaces when it needs them: to check `if_version`, to
//! check write policies, or to replace the secondary-index entries of a document. Every record
//! read this way gets an expectation on the version read, so the commit fails if another writer
//! changed it in between. Expectations from `if_version` and from a transaction's reads are
//! *strict*: their failure is the caller's [`Error::Conflict`]. The others are internal: their
//! failure only means the plan is stale, and the batch is planned again.

use std::collections::{BTreeSet, HashMap};

use bytes::Bytes;
use lilia_engine::{Db, WriteBatch};

use crate::batch::{Outcome, Write};
use crate::catalog::Catalog;
use crate::error::{invalid, Error, Result};
use crate::guard::{Access, Guard};
use crate::keys::{self, validate_name};
use crate::record::{decode_doc, decode_kv, encode_doc, encode_kv};

/// Model of a ledger delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    Kv,
    Json,
    GraphNode,
    GraphEdge,
    Fts,
    FtsDictionary,
    Vector,
    TimeSeries,
}

impl Model {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Kv => "kv",
            Self::Json => "json",
            Self::GraphNode => "graph_node",
            Self::GraphEdge => "graph_edge",
            Self::Fts => "fts",
            Self::FtsDictionary => "fts_dictionary",
            Self::Vector => "vector",
            Self::TimeSeries => "timeseries",
        }
    }
}

/// A record write of a plan, as the ledger attests it.
#[derive(Debug, Clone)]
pub struct Delta {
    pub model: Model,
    pub space: String,
    /// Document id, or the key-value key in hex.
    pub id: String,
    /// Index of the record's write among the plan's operations: its version is the batch's
    /// first sequence plus this.
    pub op: usize,
    /// BLAKE3 of the stored record; `None` for a delete.
    pub payload: Option<[u8; 32]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Expected {
    pub version: Option<u64>,
    pub strict: bool,
}

/// The engine writes of a batch and the conditions under which they are valid.
#[derive(Debug, Default)]
pub struct Plan {
    pub ops: Vec<(Bytes, Option<Bytes>)>,
    pub expected: HashMap<Bytes, Expected>,
    /// Per write of the batch: index of its record operation, and whether it deleted a live
    /// record.
    pub results: Vec<(Option<usize>, bool)>,
    pub deltas: Vec<Delta>,
    /// Term count changes of full-text writes, applied to the loaded vocabularies once the
    /// plan commits.
    pub vocabulary: crate::fts::Deltas,
    /// Vector changes, applied to the loaded HNSW indexes once the plan commits.
    pub vectors: Vec<crate::vectors::VectorDelta>,
}

impl Plan {
    /// The engine batch, with every expectation.
    pub fn batch(&self) -> WriteBatch {
        let mut batch = WriteBatch::new();
        for (key, expected) in &self.expected {
            batch.expect(key.clone(), expected.version);
        }
        self.append_ops(&mut batch);
        batch
    }

    pub fn append_ops(&self, batch: &mut WriteBatch) {
        for (key, value) in &self.ops {
            match value {
                Some(value) => batch.put(key.clone(), value.clone()),
                None => batch.delete(key.clone()),
            };
        }
    }

    /// Approximate size of the plan's writes, to bound commit groups.
    pub fn approximate_bytes(&self) -> usize {
        self.ops
            .iter()
            .map(|(key, value)| key.len() + value.as_ref().map_or(0, Bytes::len) + 16)
            .sum()
    }

    pub fn outcomes(&self, first: u64) -> Vec<Outcome> {
        self.results
            .iter()
            .map(|&(op, deleted)| Outcome {
                version: op.map(|i| first + i as u64),
                deleted,
            })
            .collect()
    }

    /// The caller's error for a failed expectation on `key`; `None` if the expectation was an
    /// internal one, and the batch must be planned again.
    pub fn conflict(
        &self,
        key: &[u8],
        expected: Option<u64>,
        actual: Option<u64>,
    ) -> Option<Error> {
        self.expected
            .get(key)
            .filter(|e| e.strict)
            .map(|_| Error::Conflict {
                target: keys::describe(key),
                expected,
                actual,
            })
    }
}

/// State of a record touched by the batch being planned.
#[derive(Debug, Clone)]
struct Local {
    raw: Option<Bytes>,
    /// Version found by the planning read; unknown for a record only written blindly.
    version: Option<u64>,
    written: bool,
}

/// Plans one batch.
pub struct Planner<'a> {
    engine: &'a Db,
    catalog: &'a Catalog,
    access: &'a Access,
    now_ms: i64,
    local: HashMap<Bytes, Local>,
    plan: Plan,
}

impl<'a> Planner<'a> {
    pub fn new(engine: &'a Db, catalog: &'a Catalog, access: &'a Access, now_ms: i64) -> Self {
        Self {
            engine,
            catalog,
            access,
            now_ms,
            local: HashMap::new(),
            plan: Plan::default(),
        }
    }

    pub fn finish(self) -> Plan {
        self.plan
    }

    /// The record at `key` as the batch has left it so far, read from the engine (with an
    /// expectation on its version) the first time.
    pub(crate) fn current(&mut self, key: &Bytes) -> Result<Option<Bytes>> {
        if let Some(local) = self.local.get(key) {
            return Ok(local.raw.clone());
        }
        let found = self.engine.get_versioned(key)?;
        let version = found.as_ref().map(|v| v.version);
        let raw = found.map(|v| v.value);
        self.local.insert(
            key.clone(),
            Local {
                raw: raw.clone(),
                version,
                written: false,
            },
        );
        self.plan.expected.entry(key.clone()).or_insert(Expected {
            version,
            strict: false,
        });
        Ok(raw)
    }

    /// Checks `if_version` against the live version of the record at `key` (`None` when it is
    /// absent or expired), and makes the expectation on it strict.
    fn check_version(&mut self, key: &Bytes, if_version: Option<u64>, live: bool) -> Result<()> {
        let Some(wanted) = if_version else {
            return Ok(());
        };
        let Some(local) = self.local.get(key) else {
            return Err(invalid("if_version needs a planning read"));
        };
        if local.written {
            return Err(invalid(format!(
                "if_version on {}, already written earlier in the batch",
                keys::describe(key)
            )));
        }
        let actual = if live { local.version } else { None };
        let expected = (wanted != 0).then_some(wanted);
        if actual != expected {
            return Err(Error::Conflict {
                target: keys::describe(key),
                expected,
                actual,
            });
        }
        if let Some(entry) = self.plan.expected.get_mut(key) {
            entry.strict = true;
        }
        Ok(())
    }

    /// Records the outcome of one write of the batch: its record operation, and whether it
    /// deleted a live record.
    pub(crate) fn result(&mut self, op: Option<usize>, deleted: bool) {
        self.plan.results.push((op, deleted));
    }

    /// Live records with keys in `[start, end)` as the batch has left them so far. Read without
    /// expectations: only for writes whose result does not depend on concurrent changes to the
    /// range (a node deletion removes the edges it sees; an edge added concurrently is as if
    /// added after it).
    ///
    /// # Errors
    ///
    /// Engine errors.
    pub(crate) fn live_range(&mut self, start: &[u8], end: &[u8]) -> Result<Vec<(Bytes, Bytes)>> {
        let mut found: std::collections::BTreeMap<Bytes, Option<Bytes>> =
            std::collections::BTreeMap::new();
        for entry in self.engine.range(
            std::ops::Bound::Included(start),
            std::ops::Bound::Excluded(end),
        )? {
            let entry = entry?;
            found.insert(entry.key, Some(entry.value));
        }
        for (key, local) in &self.local {
            if key.as_ref() >= start && key.as_ref() < end {
                found.insert(key.clone(), local.raw.clone());
            }
        }
        Ok(found
            .into_iter()
            .filter_map(|(key, value)| value.map(|v| (key, v)))
            .collect())
    }

    /// The first live key in `[start, end)` as the batch has left it so far. A committed record
    /// found this way is read as [`Planner::current`] reads it, but at the version the seek saw
    /// and with an expectation on that version: a writer that changes the record after the seek
    /// (in [`crate::timeseries`], the only one that can move the partition around it) fails the
    /// plan.
    ///
    /// # Errors
    ///
    /// Engine errors.
    pub(crate) fn first_live(&mut self, start: &[u8], end: &[u8]) -> Result<Option<Bytes>> {
        let mut committed = None;
        for entry in self.engine.range(
            std::ops::Bound::Included(start),
            std::ops::Bound::Excluded(end),
        )? {
            let entry = entry?;
            match self.local.get(&entry.key) {
                Some(Local { raw: None, .. }) => continue,
                Some(_) => {}
                None => {
                    self.local.insert(
                        entry.key.clone(),
                        Local {
                            raw: Some(entry.value),
                            version: Some(entry.version),
                            written: false,
                        },
                    );
                    self.plan
                        .expected
                        .entry(entry.key.clone())
                        .or_insert(Expected {
                            version: Some(entry.version),
                            strict: false,
                        });
                }
            }
            committed = Some(entry.key);
            break;
        }
        let own = self
            .local
            .iter()
            .filter(|(key, local)| {
                local.raw.is_some() && key.as_ref() >= start && key.as_ref() < end
            })
            .map(|(key, _)| key)
            .min()
            .cloned();
        Ok(match (committed, own) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        })
    }

    /// The schema of collection `name`, as the batch is planned under it.
    pub(crate) fn schema(&self, name: &str) -> Option<std::sync::Arc<crate::catalog::Schema>> {
        self.catalog.schema(name)
    }

    /// Records a vector change, for the HNSW index once the plan commits.
    pub(crate) fn vector_change(&mut self, delta: crate::vectors::VectorDelta) {
        self.plan.vectors.push(delta);
    }

    /// Records term count changes of a full-text write of `collection`.
    pub(crate) fn vocabulary(&mut self, collection: &str, deltas: Vec<(String, i64)>) {
        self.plan.vocabulary.extend(
            deltas
                .into_iter()
                .map(|(term, delta)| (collection.to_owned(), term, delta)),
        );
    }

    /// Records the write of `value` at `key`, which later reads of the batch see, and returns
    /// its operation index.
    pub(crate) fn op(&mut self, key: Bytes, value: Option<Bytes>) -> usize {
        let local = self.local.entry(key.clone()).or_insert(Local {
            raw: None,
            version: None,
            written: false,
        });
        local.raw.clone_from(&value);
        local.written = true;
        self.plan.ops.push((key, value));
        self.plan.ops.len() - 1
    }

    /// Records the write of the record at `key` and returns its operation index.
    pub(crate) fn write_record(
        &mut self,
        key: &Bytes,
        value: Option<Bytes>,
        delta: (Model, &str, String),
    ) -> usize {
        let payload = value.as_deref().map(|v| *blake3::hash(v).as_bytes());
        let op = self.op(key.clone(), value);
        let (model, space, id) = delta;
        self.plan.deltas.push(Delta {
            model,
            space: space.to_owned(),
            id,
            op,
            payload,
        });
        op
    }

    /// Plans one write of the batch.
    ///
    /// # Errors
    ///
    /// Invalid names, failed `if_version` conditions, policy denials, engine read errors.
    pub fn add(&mut self, write: &Write) -> Result<()> {
        match write {
            Write::KvPut {
                namespace,
                key,
                value,
                expires_at_ms,
                if_version,
            } => self.kv_put(namespace, key, value, *expires_at_ms, *if_version),
            Write::KvDelete {
                namespace,
                key,
                if_version,
            } => self.kv_delete(namespace, key, *if_version),
            Write::JsonPut {
                space,
                id,
                doc,
                if_version,
            } => self.json_put(space, id, doc, *if_version),
            Write::JsonDelete {
                space,
                id,
                if_version,
            } => self.json_delete(space, id, *if_version),
            Write::Graph(write) => self.graph(write),
            Write::Fts(write) => self.fts(write),
            Write::Vector(write) => self.vector(write),
            Write::TimeSeries(write) => self.series(write),
        }
    }

    fn kv_put(
        &mut self,
        namespace: &str,
        key: &[u8],
        value: &[u8],
        expires: Option<i64>,
        if_version: Option<u64>,
    ) -> Result<()> {
        validate_name("namespace", namespace)?;
        let record_key = keys::kv(namespace, key);
        let guard = Guard::new(self.access, self.catalog.schema(namespace));
        if if_version.is_some() || guard.checks_writes() {
            let old = self
                .current(&record_key)?
                .map(|raw| decode_kv(&raw))
                .transpose()?
                .filter(|r| !r.expired(self.now_ms));
            self.check_version(&record_key, if_version, old.is_some())?;
            guard.check_kv(
                namespace,
                key,
                old.as_ref().map(|r| r.value.as_ref()),
                Some(value),
            )?;
        }
        let record = encode_kv(value, expires);
        let op = self.write_record(
            &record_key,
            Some(record),
            (Model::Kv, namespace, hex::encode(key)),
        );
        self.plan.results.push((Some(op), false));
        Ok(())
    }

    fn kv_delete(&mut self, namespace: &str, key: &[u8], if_version: Option<u64>) -> Result<()> {
        validate_name("namespace", namespace)?;
        let record_key = keys::kv(namespace, key);
        let guard = Guard::new(self.access, self.catalog.schema(namespace));
        let Some(raw) = self.current(&record_key)? else {
            self.check_version(&record_key, if_version, false)?;
            self.plan.results.push((None, false));
            return Ok(());
        };
        let live = Some(decode_kv(&raw)?).filter(|r| !r.expired(self.now_ms));
        self.check_version(&record_key, if_version, live.is_some())?;
        if let Some(old) = &live {
            guard.check_kv(namespace, key, Some(&old.value), None)?;
        }
        let op = self.write_record(&record_key, None, (Model::Kv, namespace, hex::encode(key)));
        self.plan.results.push((Some(op), live.is_some()));
        Ok(())
    }

    /// Replaces the index entries of document `id` in `space` from `old` to `new`.
    fn reindex(
        &mut self,
        space: &str,
        id: &str,
        old: Option<&lilia_core::BinaryDoc>,
        new: Option<&lilia_core::BinaryDoc>,
    ) {
        let Some(schema) = self.catalog.schema(space) else {
            return;
        };
        for index in &schema.indexes {
            let before = old.map_or_default(|d| index.values(d));
            let after: BTreeSet<String> = new.map_or_default(|d| index.values(d));
            for value in before.difference(&after) {
                self.op(keys::index_entry(space, index.id, value, id), None);
            }
            // An index being built may not hold the old document's entries yet (the build
            // skips documents changed since its snapshot), so every new entry is written.
            let added: Vec<&String> = if index.ready {
                after.difference(&before).collect()
            } else {
                after.iter().collect()
            };
            for value in added {
                self.op(
                    keys::index_entry(space, index.id, value, id),
                    Some(Bytes::new()),
                );
            }
        }
    }

    fn json_put(
        &mut self,
        space: &str,
        id: &str,
        doc: &lilia_core::BinaryDoc,
        if_version: Option<u64>,
    ) -> Result<()> {
        validate_name("space", space)?;
        if id.is_empty() {
            return Err(invalid("document ids must not be empty"));
        }
        let record_key = keys::doc(space, id);
        let schema = self.catalog.schema(space);
        let indexed = schema.as_ref().is_some_and(|s| !s.indexes.is_empty());
        let guard = Guard::new(self.access, schema);
        if if_version.is_some() || guard.checks_writes() || indexed {
            let old = self
                .current(&record_key)?
                .map(|raw| decode_doc(&raw))
                .transpose()?;
            self.check_version(&record_key, if_version, old.is_some())?;
            guard.check_doc(space, old.as_ref(), Some(doc))?;
            self.reindex(space, id, old.as_ref(), Some(doc));
        }
        let op = self.write_record(
            &record_key,
            Some(encode_doc(doc)),
            (Model::Json, space, id.to_owned()),
        );
        self.plan.results.push((Some(op), false));
        Ok(())
    }

    fn json_delete(&mut self, space: &str, id: &str, if_version: Option<u64>) -> Result<()> {
        validate_name("space", space)?;
        let record_key = keys::doc(space, id);
        let guard = Guard::new(self.access, self.catalog.schema(space));
        let old = self
            .current(&record_key)?
            .map(|raw| decode_doc(&raw))
            .transpose()?;
        self.check_version(&record_key, if_version, old.is_some())?;
        let Some(old) = old else {
            self.plan.results.push((None, false));
            return Ok(());
        };
        guard.check_doc(space, Some(&old), None)?;
        self.reindex(space, id, Some(&old), None);
        let op = self.write_record(&record_key, None, (Model::Json, space, id.to_owned()));
        self.plan.results.push((Some(op), true));
        Ok(())
    }

    /// Deletes the key-value record at `key` of `namespace` if it is still expired; the batch's
    /// outcomes then count it.
    ///
    /// # Errors
    ///
    /// Engine read errors and records that do not decode.
    pub fn purge(&mut self, namespace: &str, key: &[u8]) -> Result<()> {
        let record_key = keys::kv(namespace, key);
        if let Some(raw) = self.current(&record_key)? {
            if decode_kv(&raw)?.expired(self.now_ms) {
                let op =
                    self.write_record(&record_key, None, (Model::Kv, namespace, hex::encode(key)));
                self.plan.results.push((Some(op), true));
            }
        }
        Ok(())
    }

    /// Adds a transaction's reads as strict expectations: the records it read must still have
    /// the versions it saw.
    ///
    /// # Errors
    ///
    /// [`Error::Conflict`] when planning already found another version.
    pub fn require(&mut self, reads: &HashMap<Bytes, Option<u64>>) -> Result<()> {
        for (key, &version) in reads {
            let entry = self.plan.expected.entry(key.clone()).or_insert(Expected {
                version,
                strict: true,
            });
            if entry.version != version {
                return Err(Error::Conflict {
                    target: keys::describe(key),
                    expected: version,
                    actual: entry.version,
                });
            }
            entry.strict = true;
        }
        Ok(())
    }
}

/// How a transaction's buffered writes change what it reads.
pub mod overlay {
    use bytes::Bytes;

    use crate::batch::Write;
    use crate::error::Result;
    use crate::keys::{self, validate_name};
    use crate::record::{decode_kv, encode_doc, encode_kv, now_ms};
    use crate::txn::View;

    /// Records written by one write, and an update of the overlay.
    pub type Changes = Vec<(Bytes, Option<Bytes>)>;

    /// The records `write` changes in `view`, and whether it deletes a live record.
    ///
    /// # Errors
    ///
    /// Invalid names; engine errors of the reads a write needs.
    pub fn changes(write: &Write, view: &View<'_>) -> Result<(Changes, bool)> {
        match write {
            Write::KvPut {
                namespace,
                key,
                value,
                expires_at_ms,
                ..
            } => {
                validate_name("namespace", namespace)?;
                Ok((
                    vec![(
                        keys::kv(namespace, key),
                        Some(encode_kv(value, *expires_at_ms)),
                    )],
                    false,
                ))
            }
            Write::KvDelete { namespace, key, .. } => {
                validate_name("namespace", namespace)?;
                let record_key = keys::kv(namespace, key);
                let live = match view.peek(&record_key)? {
                    Some(item) => !decode_kv(&item.value)?.expired(now_ms()),
                    None => false,
                };
                Ok((vec![(record_key, None)], live))
            }
            Write::JsonPut { space, id, doc, .. } => {
                validate_name("space", space)?;
                Ok((vec![(keys::doc(space, id), Some(encode_doc(doc)))], false))
            }
            Write::JsonDelete { space, id, .. } => {
                validate_name("space", space)?;
                let record_key = keys::doc(space, id);
                let live = view.peek(&record_key)?.is_some();
                Ok((vec![(record_key, None)], live))
            }
            Write::Graph(write) => crate::graph::overlay(write, view),
            Write::Fts(write) => crate::fts::overlay(write, view),
            Write::Vector(write) => crate::vectors::overlay(write, view),
            Write::TimeSeries(write) => crate::timeseries::overlay(write, view),
        }
    }
}
