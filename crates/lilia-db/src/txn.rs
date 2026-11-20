//! Optimistic transactions.
//!
//! A transaction reads a snapshot of the database taken when it starts, with its own writes on
//! top, and records the version of every record it reads (an absent record as absent). Its
//! writes are buffered and committed as one batch, which requires every record read to still
//! have the version seen: if another writer changed one of them, the commit fails with
//! [`crate::Error::Conflict`] and nothing is written.
//!
//! This makes transactions serializable for the records they read, by key or through a scan or
//! a find. A record inserted by another writer into a range the transaction scanned (a phantom)
//! is not detected.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::iter::Peekable;
use std::ops::Bound;

use bytes::Bytes;
use lilia_core::{BinaryDoc, JsonEntry, KvEntry};
use lilia_engine::{Db, Snapshot};
use serde_json::{Map, Value};

use crate::batch::{Outcome, Write};
use crate::db::Database;
use crate::error::{Error, Result};
use crate::guard::{Access, Guard};
use crate::keys::{self, validate_name};
use crate::read::{self, Item, Items, Scan, Source};
use crate::record::{encode_doc, encode_kv, now_ms};

/// A transaction; see the [module documentation](self). Dropping it without committing
/// discards its writes.
#[derive(Debug)]
pub struct Transaction<'db> {
    db: &'db Database,
    access: Access,
    snapshot: Snapshot,
    reads: RefCell<HashMap<Bytes, Option<u64>>>,
    writes: Vec<Write>,
    /// Records as the transaction's writes leave them; `None` for a delete.
    overlay: BTreeMap<Bytes, Option<Bytes>>,
}

/// The transaction's view: its snapshot under its own writes.
struct View<'t> {
    engine: &'t Db,
    snapshot: &'t Snapshot,
    reads: &'t RefCell<HashMap<Bytes, Option<u64>>>,
    overlay: &'t BTreeMap<Bytes, Option<Bytes>>,
}

fn pending(key: &Bytes, value: &Bytes) -> Item {
    Item {
        key: key.clone(),
        value: value.clone(),
        version: 0,
        pending: true,
    }
}

impl Source for View<'_> {
    fn get(&self, key: &Bytes) -> Result<Option<Item>> {
        if let Some(own) = self.overlay.get(key) {
            return Ok(own.as_ref().map(|value| pending(key, value)));
        }
        let found = self.engine.get_at(self.snapshot, key)?;
        self.reads
            .borrow_mut()
            .entry(key.clone())
            .or_insert(found.as_ref().map(|v| v.version));
        Ok(found.map(|v| Item {
            key: key.clone(),
            value: v.value,
            version: v.version,
            pending: false,
        }))
    }

    fn range(&self, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<Items<'_>> {
        let committed = self
            .engine
            .range_at(self.snapshot, start, end)?
            .map(|entry| {
                let entry = entry?;
                Ok(Item {
                    key: entry.key,
                    value: entry.value,
                    version: entry.version,
                    pending: false,
                })
            });
        let own = self.overlay.range::<[u8], _>((start, end));
        Ok(Box::new(Merge {
            committed: committed.peekable(),
            own: own.peekable(),
        }))
    }

    fn observe(&self, item: &Item) {
        if !item.pending {
            self.reads
                .borrow_mut()
                .entry(item.key.clone())
                .or_insert(Some(item.version));
        }
    }

    fn pending(&self, start: &[u8], end: &[u8]) -> Vec<Item> {
        self.overlay
            .range::<[u8], _>((Bound::Included(start), Bound::Excluded(end)))
            .filter_map(|(key, value)| value.as_ref().map(|value| pending(key, value)))
            .collect()
    }

    fn is_pending(&self, key: &[u8]) -> bool {
        self.overlay.contains_key(key)
    }
}

/// Committed records merged with the transaction's own writes, in key order; an own write
/// replaces (or, as a delete, hides) the committed record of its key.
struct Merge<C: Iterator<Item = Result<Item>>, O: Iterator> {
    committed: Peekable<C>,
    own: Peekable<O>,
}

impl<'o, C, O> Iterator for Merge<C, O>
where
    C: Iterator<Item = Result<Item>>,
    O: Iterator<Item = (&'o Bytes, &'o Option<Bytes>)>,
{
    type Item = Result<Item>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let committed_key = match self.committed.peek() {
                Some(Ok(item)) => Some(item.key.clone()),
                Some(Err(_)) => return self.committed.next(),
                None => None,
            };
            let own_first = match (&committed_key, self.own.peek()) {
                (_, None) => return self.committed.next(),
                (None, Some(_)) => true,
                (Some(committed), Some((own, _))) => *own <= committed,
            };
            if !own_first {
                return self.committed.next();
            }
            let (key, value) = self.own.next()?;
            if committed_key.as_ref() == Some(key) {
                self.committed.next();
            }
            if let Some(value) = value {
                return Some(Ok(pending(key, value)));
            }
        }
    }
}

impl<'db> Transaction<'db> {
    pub(crate) fn new(db: &'db Database, access: Access) -> Self {
        let snapshot = db.engine().snapshot();
        Self {
            db,
            access,
            snapshot,
            reads: RefCell::new(HashMap::new()),
            writes: Vec::new(),
            overlay: BTreeMap::new(),
        }
    }

    fn view(&self) -> View<'_> {
        View {
            engine: self.db.engine(),
            snapshot: &self.snapshot,
            reads: &self.reads,
            overlay: &self.overlay,
        }
    }

    fn guard(&self, name: &str) -> Guard<'_> {
        Guard::new(&self.access, self.db.schema(name))
    }

    /// See [`crate::Session::kv_get`]; a value written by this transaction has version 0.
    ///
    /// # Errors
    ///
    /// Invalid namespace, engine errors.
    pub fn kv_get(&self, namespace: &str, key: &[u8]) -> Result<Option<KvEntry>> {
        validate_name("namespace", namespace)?;
        read::kv_get(
            &self.view(),
            &self.guard(namespace),
            namespace,
            key,
            now_ms(),
        )
    }

    /// See [`crate::Session::kv_scan`].
    ///
    /// # Errors
    ///
    /// As [`Transaction::kv_get`].
    pub fn kv_scan(&self, namespace: &str, scan: &Scan) -> Result<Vec<KvEntry>> {
        validate_name("namespace", namespace)?;
        read::kv_scan(
            &self.view(),
            &self.guard(namespace),
            namespace,
            scan,
            now_ms(),
        )
    }

    /// See [`crate::Session::json_get`].
    ///
    /// # Errors
    ///
    /// Invalid space, engine errors.
    pub fn json_get(&self, space: &str, id: &str) -> Result<Option<JsonEntry>> {
        validate_name("space", space)?;
        read::doc_get(&self.view(), &self.guard(space), space, id)
    }

    /// See [`crate::Session::json_scan`].
    ///
    /// # Errors
    ///
    /// As [`Transaction::json_get`].
    pub fn json_scan(&self, space: &str, scan: &Scan) -> Result<Vec<JsonEntry>> {
        self.json_find(space, &Map::new(), scan)
    }

    /// See [`crate::Session::json_find`]; the transaction's own writes are matched too.
    ///
    /// # Errors
    ///
    /// As [`Transaction::json_get`].
    pub fn json_find(
        &self,
        space: &str,
        query: &Map<String, Value>,
        scan: &Scan,
    ) -> Result<Vec<JsonEntry>> {
        validate_name("space", space)?;
        let schema = self.db.schema(space);
        let guard = Guard::new(&self.access, schema.clone());
        read::doc_find(&self.view(), &guard, schema.as_deref(), space, query, scan)
    }

    /// Buffers `write`; it is planned, checked and applied by [`Transaction::commit`].
    ///
    /// # Errors
    ///
    /// Invalid names.
    pub fn write(&mut self, write: Write) -> Result<()> {
        let (key, record) = match &write {
            Write::KvPut {
                namespace,
                key,
                value,
                expires_at_ms,
                ..
            } => {
                validate_name("namespace", namespace)?;
                (
                    keys::kv(namespace, key),
                    Some(encode_kv(value, *expires_at_ms)),
                )
            }
            Write::KvDelete { namespace, key, .. } => {
                validate_name("namespace", namespace)?;
                (keys::kv(namespace, key), None)
            }
            Write::JsonPut { space, id, doc, .. } => {
                validate_name("space", space)?;
                (keys::doc(space, id), Some(encode_doc(doc)))
            }
            Write::JsonDelete { space, id, .. } => {
                validate_name("space", space)?;
                (keys::doc(space, id), None)
            }
        };
        self.overlay.insert(key, record);
        self.writes.push(write);
        Ok(())
    }

    /// # Errors
    ///
    /// Invalid namespace.
    pub fn kv_put(&mut self, namespace: &str, key: &[u8], value: &[u8]) -> Result<()> {
        self.write(Write::KvPut {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
            value: value.to_vec(),
            expires_at_ms: None,
            if_version: None,
        })
    }

    /// # Errors
    ///
    /// Invalid namespace.
    pub fn kv_delete(&mut self, namespace: &str, key: &[u8]) -> Result<()> {
        self.write(Write::KvDelete {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
            if_version: None,
        })
    }

    /// # Errors
    ///
    /// Invalid space; [`Error::Document`] for a value that cannot be stored.
    pub fn json_put(&mut self, space: &str, id: &str, value: &Value) -> Result<()> {
        let doc = BinaryDoc::from_json_value(value).map_err(|e| Error::Document(e.to_string()))?;
        self.write(Write::JsonPut {
            space: space.to_owned(),
            id: id.to_owned(),
            doc,
            if_version: None,
        })
    }

    /// # Errors
    ///
    /// Invalid space.
    pub fn json_delete(&mut self, space: &str, id: &str) -> Result<()> {
        self.write(Write::JsonDelete {
            space: space.to_owned(),
            id: id.to_owned(),
            if_version: None,
        })
    }

    /// Commits the transaction's writes if every record it read is unchanged; returns one
    /// outcome per write.
    ///
    /// # Errors
    ///
    /// [`Error::Conflict`] when a record read (or an `if_version`) no longer holds;
    /// [`Error::Denied`] for a write a policy rejects; engine errors. Nothing is written on error.
    pub fn commit(self) -> Result<Vec<Outcome>> {
        let reads = self.reads.into_inner();
        self.db.execute(&self.access, &self.writes, Some(&reads))
    }
}
