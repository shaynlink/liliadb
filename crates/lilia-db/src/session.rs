//! Sessions: reads and writes on behalf of one caller.

use lilia_core::{JsonEntry, KvEntry};
use serde_json::{Map, Value};

use crate::batch::{Batch, Outcome, Write};
use crate::context::{sealed, Context, Reader};
use crate::db::Database;
use crate::error::{corruption, Error, Result};
use crate::fts::Fts;
use crate::graph::Graph;
use crate::guard::{Access, Guard};
use crate::keys::validate_name;
use crate::read::{self, Latest, Scan};
use crate::record::now_ms;
use crate::txn::Transaction;
use crate::vectors::Vectors;

/// Reads and writes on behalf of one caller. Cheap to create and clone.
#[derive(Debug, Clone)]
pub struct Session<'db> {
    db: &'db Database,
    access: Access,
}

impl<'db> Session<'db> {
    pub(crate) fn new(db: &'db Database, access: Access) -> Self {
        Self { db, access }
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
            &Latest::now(self.db.engine()),
            &self.guard(namespace),
            namespace,
            key,
            now_ms(),
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
            &Latest::pinned(self.db.engine()),
            &self.guard(namespace),
            namespace,
            scan,
            now_ms(),
        )
    }

    /// Document `id` of `space`, if a read policy admits it, with its masked fields masked.
    ///
    /// # Errors
    ///
    /// Invalid space, engine errors.
    pub fn json_get(&self, space: &str, id: &str) -> Result<Option<JsonEntry>> {
        validate_name("space", space)?;
        read::doc_get(
            &Latest::now(self.db.engine()),
            &self.guard(space),
            space,
            id,
        )
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
            &Latest::pinned(self.db.engine()),
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
        self.db.execute(&self.access, batch.writes(), None)
    }

    /// Commits one write.
    ///
    /// # Errors
    ///
    /// As [`Session::commit`].
    pub fn write(&self, write: &Write) -> Result<Outcome> {
        let mut outcomes = self
            .db
            .execute(&self.access, std::slice::from_ref(write), None)?;
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

    /// Starts an optimistic transaction over a snapshot of the database as of now.
    #[must_use]
    pub fn transaction(&self) -> Transaction<'db> {
        Transaction::new(self.db, self.access.clone())
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
        if pinned {
            Reader::Latest(Latest::pinned(self.db.engine()))
        } else {
            Reader::Latest(Latest::now(self.db.engine()))
        }
    }

    fn submit(&self, writes: Vec<Write>) -> Result<Vec<Outcome>> {
        self.db.execute(&self.access, &writes, None)
    }
}
