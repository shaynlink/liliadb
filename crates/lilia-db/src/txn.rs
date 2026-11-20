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

use std::cell::{Ref, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::iter::Peekable;
use std::ops::Bound;

use bytes::Bytes;
use lilia_core::{BinaryDoc, JsonEntry, KvEntry};
use lilia_engine::{Db, Snapshot};
use serde_json::{Map, Value};

use crate::batch::{Outcome, Write};
use crate::context::{sealed, Context, Reader};
use crate::db::Database;
use crate::error::{Error, Result};
use crate::fts::Fts;
use crate::graph::Graph;
use crate::guard::{Access, Guard};
use crate::keys::validate_name;
use crate::read::{self, Item, Items, Scan, Source};
use crate::record::now_ms;
use crate::write::overlay;

/// A transaction; see the [module documentation](self). Dropping it without committing
/// discards its writes.
#[derive(Debug)]
pub struct Transaction<'db> {
    db: &'db Database,
    access: Access,
    snapshot: Snapshot,
    reads: RefCell<HashMap<Bytes, Option<u64>>>,
    writes: RefCell<Vec<Write>>,
    /// Records as the transaction's writes leave them; `None` for a delete.
    overlay: RefCell<BTreeMap<Bytes, Option<Bytes>>>,
}

/// The transaction's view: its snapshot under its own writes.
pub struct View<'t> {
    engine: &'t Db,
    snapshot: &'t Snapshot,
    reads: &'t RefCell<HashMap<Bytes, Option<u64>>>,
    overlay: Ref<'t, BTreeMap<Bytes, Option<Bytes>>>,
}

impl std::fmt::Debug for View<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("View")
            .field("sequence", &self.snapshot.sequence())
            .field("pending", &self.overlay.len())
            .finish_non_exhaustive()
    }
}

fn pending(key: &Bytes, value: &Bytes) -> Item {
    Item {
        key: key.clone(),
        value: value.clone(),
        version: 0,
        pending: true,
    }
}

impl View<'_> {
    /// The record at `key` as the transaction sees it, without recording a read: for writes
    /// that only report what they replace.
    pub(crate) fn peek(&self, key: &Bytes) -> Result<Option<Item>> {
        if let Some(own) = self.overlay.get(key) {
            return Ok(own.as_ref().map(|value| pending(key, value)));
        }
        let found = self.engine.get_at(self.snapshot, key)?;
        Ok(found.map(|v| Item {
            key: key.clone(),
            value: v.value,
            version: v.version,
            pending: false,
        }))
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
            writes: RefCell::new(Vec::new()),
            overlay: RefCell::new(BTreeMap::new()),
        }
    }

    fn view(&self) -> View<'_> {
        View {
            engine: self.db.engine(),
            snapshot: &self.snapshot,
            reads: &self.reads,
            overlay: self.overlay.borrow(),
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

    /// Buffers `write`; it is planned, checked and applied by [`Transaction::commit`]. The
    /// outcome says what the write does within the transaction (no version yet).
    ///
    /// # Errors
    ///
    /// Invalid names; engine errors of the reads a write needs (a node deletion reads the
    /// node's edges).
    pub fn write(&self, write: Write) -> Result<Outcome> {
        let (changes, deleted) = overlay::changes(&write, &self.view())?;
        self.overlay.borrow_mut().extend(changes);
        self.writes.borrow_mut().push(write);
        Ok(Outcome {
            version: None,
            deleted,
        })
    }

    /// # Errors
    ///
    /// Invalid namespace.
    pub fn kv_put(&self, namespace: &str, key: &[u8], value: &[u8]) -> Result<()> {
        self.write(Write::KvPut {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
            value: value.to_vec(),
            expires_at_ms: None,
            if_version: None,
        })
        .map(drop)
    }

    /// # Errors
    ///
    /// Invalid namespace.
    pub fn kv_delete(&self, namespace: &str, key: &[u8]) -> Result<()> {
        self.write(Write::KvDelete {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
            if_version: None,
        })
        .map(drop)
    }

    /// # Errors
    ///
    /// Invalid space; [`Error::Document`] for a value that cannot be stored.
    pub fn json_put(&self, space: &str, id: &str, value: &Value) -> Result<()> {
        let doc = BinaryDoc::from_json_value(value).map_err(|e| Error::Document(e.to_string()))?;
        self.write(Write::JsonPut {
            space: space.to_owned(),
            id: id.to_owned(),
            doc,
            if_version: None,
        })
        .map(drop)
    }

    /// # Errors
    ///
    /// Invalid space.
    pub fn json_delete(&self, space: &str, id: &str) -> Result<()> {
        self.write(Write::JsonDelete {
            space: space.to_owned(),
            id: id.to_owned(),
            if_version: None,
        })
        .map(drop)
    }

    /// Graph `name`; its writes are buffered until [`Transaction::commit`] and its reads see them.
    #[must_use]
    pub fn graph(&self, name: &str) -> Graph<'_, Self> {
        Graph::new(self, name)
    }

    /// Full-text collection `name`; its writes are buffered until [`Transaction::commit`] and
    /// its searches see them.
    #[must_use]
    pub fn fts(&self, name: &str) -> Fts<'_, Self> {
        Fts::new(self, name)
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
        self.db
            .execute(&self.access, &self.writes.into_inner(), Some(&reads))
    }
}

impl Context for Transaction<'_> {}

impl sealed::Inner for Transaction<'_> {
    fn database(&self) -> &Database {
        self.db
    }

    fn access(&self) -> &Access {
        &self.access
    }

    fn reader(&self, _pinned: bool) -> Reader<'_> {
        Reader::View(self.view())
    }

    fn submit(&self, writes: Vec<Write>) -> Result<Vec<Outcome>> {
        writes.into_iter().map(|write| self.write(write)).collect()
    }
}
