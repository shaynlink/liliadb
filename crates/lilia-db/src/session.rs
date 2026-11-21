//! Sessions: reads and writes on behalf of one caller.

use std::sync::Arc;

use lilia_core::{JsonEntry, KvEntry};
use lilia_engine::Snapshot;
use serde_json::{Map, Value};

use crate::batch::{Batch, Outcome, Write};
use crate::bitemporal::Bitemporal;
use crate::context::{sealed, Context, Reader};
use crate::crdt::Crdt;
use crate::db::Database;
use crate::error::{corruption, Error, Result};
use crate::fts::Fts;
use crate::geo::Geo;
use crate::graph::Graph;
use crate::guard::{Access, Guard};
use crate::history::{self, JsonRevision, KvRevision};
use crate::keys::validate_name;
use crate::read::{self, Latest, Scan};
use crate::record::now_ms;
use crate::timeseries::TimeSeries;
use crate::txn::Transaction;
use crate::vectors::Vectors;

/// Reads and writes on behalf of one caller. Cheap to create and clone.
///
/// A session in the past ([`Session::at_version`], [`Session::at_time`]) reads the database as
/// it was then, through every model, and refuses writes with [`Error::ReadOnly`].
#[derive(Debug, Clone)]
pub struct Session<'db> {
    db: &'db Database,
    access: Access,
    past: Option<Past>,
}

/// The state a session in the past reads.
#[derive(Debug, Clone)]
struct Past {
    snapshot: Arc<Snapshot>,
    /// The time key-value expiry is judged at.
    time_ms: i64,
}

impl<'db> Session<'db> {
    pub(crate) fn new(db: &'db Database, access: Access) -> Self {
        Self {
            db,
            access,
            past: None,
        }
    }

    /// This session's caller, reading the database as it was after the commit of `version`.
    /// Key-value expiry is judged at the current time.
    ///
    /// # Errors
    ///
    /// [`Error::HistoryUnavailable`] for a version older than [`Database::oldest_version`];
    /// [`Error::InvalidArgument`] for one not written yet.
    pub fn at_version(&self, version: u64) -> Result<Self> {
        self.past_at(version, now_ms())
    }

    /// This session's caller, reading the database as it was at `time_ms` (milliseconds since
    /// the epoch); see [`crate::history`] for the precision. Key-value expiry is judged at
    /// `time_ms`.
    ///
    /// # Errors
    ///
    /// As [`Database::version_at`].
    pub fn at_time(&self, time_ms: i64) -> Result<Self> {
        let version = self.db.version_at(time_ms)?;
        self.past_at(version, time_ms)
    }

    fn past_at(&self, version: u64, time_ms: i64) -> Result<Self> {
        let snapshot = self
            .db
            .engine()
            .snapshot_at(version)
            .map_err(history::unavailable)?;
        Ok(Self {
            db: self.db,
            access: self.access.clone(),
            past: Some(Past {
                snapshot: Arc::new(snapshot),
                time_ms,
            }),
        })
    }

    /// The version a session in the past reads at; `None` for the latest state.
    #[must_use]
    pub fn version(&self) -> Option<u64> {
        self.past.as_ref().map(|p| p.snapshot.sequence())
    }

    /// The versions of document `id` of `space` kept in the history window (up to this
    /// session's version), oldest first, as this caller may read them: a version a read policy
    /// rejects is left out, and masked fields are masked.
    ///
    /// # Errors
    ///
    /// Invalid space, records that do not decode, engine errors.
    pub fn json_history(&self, space: &str, id: &str) -> Result<Vec<JsonRevision>> {
        validate_name("space", space)?;
        history::json_revisions(
            self.db.engine(),
            &self.guard(space),
            (space, id),
            self.upto(),
        )
    }

    /// The versions of `key` of `namespace`, as [`Session::json_history`].
    ///
    /// # Errors
    ///
    /// As [`Session::json_history`].
    pub fn kv_history(&self, namespace: &str, key: &[u8]) -> Result<Vec<KvRevision>> {
        validate_name("namespace", namespace)?;
        history::kv_revisions(
            self.db.engine(),
            &self.guard(namespace),
            (namespace, key),
            self.upto(),
        )
    }

    /// The JSON patch (RFC 6902) from document `id` of `space` at version `from` to the
    /// document at version `to`, as this caller reads them; an absent document is `null`.
    ///
    /// # Errors
    ///
    /// As [`Session::at_version`] and [`Session::json_get`].
    pub fn json_diff(&self, space: &str, id: &str, from: u64, to: u64) -> Result<Value> {
        let value = |version| -> Result<Value> {
            Ok(self
                .at_version(version)?
                .json_get(space, id)?
                .map_or(Value::Null, |doc| doc.value()))
        };
        Ok(lilia_core::json_patch::diff(&value(from)?, &value(to)?))
    }

    fn upto(&self) -> u64 {
        self.version().unwrap_or(u64::MAX)
    }

    /// The source of a read: this session's past state, or the latest one (`pinned` for a
    /// multi-step read).
    fn latest(&self, pinned: bool) -> Latest<'db> {
        match &self.past {
            Some(past) => Latest::at(self.db.engine(), &past.snapshot),
            None if pinned => Latest::pinned(self.db.engine()),
            None => Latest::now(self.db.engine()),
        }
    }

    /// This session reading one fixed state: its own if it is in the past, now otherwise.
    #[cfg(feature = "sql")]
    pub(crate) fn pinned(&self) -> Self {
        Self {
            db: self.db,
            access: self.access.clone(),
            past: Some(self.past.clone().unwrap_or_else(|| Past {
                snapshot: Arc::new(self.db.engine().snapshot()),
                time_ms: now_ms(),
            })),
        }
    }

    /// The time key-value expiry is judged at.
    fn now(&self) -> i64 {
        self.past.as_ref().map_or_else(now_ms, |p| p.time_ms)
    }

    fn execute(&self, writes: &[Write]) -> Result<Vec<Outcome>> {
        if self.past.is_some() {
            return Err(Error::ReadOnly);
        }
        self.db.execute(&self.access, writes, None)
    }

    #[must_use]
    pub fn access(&self) -> &Access {
        &self.access
    }

    fn guard(&self, name: &str) -> Guard<'_> {
        Guard::new(&self.access, self.db.schema(name))
    }

    /// The live value of `key` in `namespace`, if a read policy admits it.
    ///
    /// # Errors
    ///
    /// Invalid namespace, engine errors.
    pub fn kv_get(&self, namespace: &str, key: &[u8]) -> Result<Option<KvEntry>> {
        validate_name("namespace", namespace)?;
        read::kv_get(
            &self.latest(false),
            &self.guard(namespace),
            namespace,
            key,
            self.now(),
        )
    }

    /// Live entries of `namespace` in key order.
    ///
    /// # Errors
    ///
    /// As [`Session::kv_get`].
    pub fn kv_scan(&self, namespace: &str, scan: &Scan) -> Result<Vec<KvEntry>> {
        validate_name("namespace", namespace)?;
        read::kv_scan(
            &self.latest(true),
            &self.guard(namespace),
            namespace,
            scan,
            self.now(),
        )
    }

    /// Document `id` of `space`, if a read policy admits it, with its masked fields masked.
    ///
    /// # Errors
    ///
    /// Invalid space, engine errors.
    pub fn json_get(&self, space: &str, id: &str) -> Result<Option<JsonEntry>> {
        validate_name("space", space)?;
        read::doc_get(&self.latest(false), &self.guard(space), space, id)
    }

    /// Documents of `space` in id order.
    ///
    /// # Errors
    ///
    /// As [`Session::json_get`].
    pub fn json_scan(&self, space: &str, scan: &Scan) -> Result<Vec<JsonEntry>> {
        self.json_find(space, &Map::new(), scan)
    }

    /// Documents of `space` matching `query` (see [`crate::find`]) in id order.
    ///
    /// # Errors
    ///
    /// As [`Session::json_get`].
    pub fn json_find(
        &self,
        space: &str,
        query: &Map<String, Value>,
        scan: &Scan,
    ) -> Result<Vec<JsonEntry>> {
        validate_name("space", space)?;
        let schema = self.db.schema(space);
        let guard = Guard::new(&self.access, schema.clone());
        read::doc_find(
            &self.latest(true),
            &guard,
            schema.as_deref(),
            space,
            query,
            scan,
        )
    }

    /// Commits `batch` atomically.
    ///
    /// # Errors
    ///
    /// [`Error::Conflict`] for a failed `if_version`, [`Error::Denied`] for a write a policy
    /// rejects, invalid names, engine errors. Nothing is written on error.
    pub fn commit(&self, batch: &Batch) -> Result<Vec<Outcome>> {
        self.execute(batch.writes())
    }

    /// Commits one write.
    ///
    /// # Errors
    ///
    /// As [`Session::commit`].
    pub fn write(&self, write: &Write) -> Result<Outcome> {
        let mut outcomes = self.execute(std::slice::from_ref(write))?;
        outcomes
            .pop()
            .ok_or_else(|| corruption("a write produced no outcome"))
    }

    /// Writes `value` at `key` of `namespace`; returns its version.
    ///
    /// # Errors
    ///
    /// As [`Session::commit`].
    pub fn kv_put(&self, namespace: &str, key: &[u8], value: &[u8]) -> Result<u64> {
        let outcome = self.write(&Write::KvPut {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
            value: value.to_vec(),
            expires_at_ms: None,
            if_version: None,
        })?;
        outcome
            .version
            .ok_or_else(|| corruption("a put produced no version"))
    }

    /// Deletes `key` of `namespace`; `true` if a live record was deleted.
    ///
    /// # Errors
    ///
    /// As [`Session::commit`].
    pub fn kv_delete(&self, namespace: &str, key: &[u8]) -> Result<bool> {
        Ok(self
            .write(&Write::KvDelete {
                namespace: namespace.to_owned(),
                key: key.to_vec(),
                if_version: None,
            })?
            .deleted)
    }

    /// Writes document `id` of `space`; returns its version.
    ///
    /// # Errors
    ///
    /// [`Error::Document`] for a value that cannot be stored; as [`Session::commit`].
    pub fn json_put(&self, space: &str, id: &str, value: &Value) -> Result<u64> {
        let doc = lilia_core::BinaryDoc::from_json_value(value)
            .map_err(|e| Error::Document(e.to_string()))?;
        let outcome = self.write(&Write::JsonPut {
            space: space.to_owned(),
            id: id.to_owned(),
            doc,
            if_version: None,
        })?;
        outcome
            .version
            .ok_or_else(|| corruption("a put produced no version"))
    }

    /// Deletes document `id` of `space`; `true` if it existed.
    ///
    /// # Errors
    ///
    /// As [`Session::commit`].
    pub fn json_delete(&self, space: &str, id: &str) -> Result<bool> {
        Ok(self
            .write(&Write::JsonDelete {
                space: space.to_owned(),
                id: id.to_owned(),
                if_version: None,
            })?
            .deleted)
    }

    /// Graph `name`; its writes commit at once.
    #[must_use]
    pub fn graph(&self, name: &str) -> Graph<'_, Self> {
        Graph::new(self, name)
    }

    /// Full-text collection `name`; its writes commit at once.
    #[must_use]
    pub fn fts(&self, name: &str) -> Fts<'_, Self> {
        Fts::new(self, name)
    }

    /// Vector collection `name`; its writes commit at once.
    #[must_use]
    pub fn vectors(&self, name: &str) -> Vectors<'_, Self> {
        Vectors::new(self, name)
    }

    /// Bitemporal space `name`; its writes commit at once.
    #[must_use]
    pub fn bitemporal(&self, name: &str) -> Bitemporal<'_, Self> {
        Bitemporal::new(self, name)
    }

    /// CRDT space `name`; its writes commit at once.
    #[must_use]
    pub fn crdt(&self, name: &str) -> Crdt<'_, Self> {
        Crdt::new(self, name)
    }

    /// Geo collection `name`; its writes commit at once.
    #[must_use]
    pub fn geo(&self, name: &str) -> Geo<'_, Self> {
        Geo::new(self, name)
    }

    /// Time series `name`; its writes commit at once.
    #[must_use]
    pub fn series(&self, name: &str) -> TimeSeries<'_, Self> {
        TimeSeries::new(self, name)
    }

    /// Starts an optimistic transaction over a snapshot of the database as of now, or, for a
    /// session in the past, as of its version: its commit then fails if a record it read has
    /// changed since.
    #[must_use]
    pub fn transaction(&self) -> Transaction<'db> {
        let snapshot = self.past.as_ref().map_or_else(
            || Arc::new(self.db.engine().snapshot()),
            |past| Arc::clone(&past.snapshot),
        );
        Transaction::new(self.db, self.access.clone(), snapshot)
    }
}

impl Context for Session<'_> {}

impl sealed::Inner for Session<'_> {
    fn database(&self) -> &Database {
        self.db
    }

    fn access(&self) -> &Access {
        &self.access
    }

    fn reader(&self, pinned: bool) -> Reader<'_> {
        Reader::Latest(self.latest(pinned))
    }

    fn submit(&self, writes: Vec<Write>) -> Result<Vec<Outcome>> {
        self.execute(&writes)
    }
}
