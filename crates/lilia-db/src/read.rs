//! The read path, shared by sessions and transactions.
//!
//! Every read goes through a [`Source`] (the latest state, or a transaction's snapshot with its
//! uncommitted writes on top) and returns only what the caller's [`Guard`] admits. These
//! functions are the only ones that turn stored records into entries.

use std::ops::Bound;
use std::sync::Arc;

use bytes::Bytes;
use lilia_core::{JsonEntry, KvEntry};
use lilia_engine::{Db, Snapshot};
use serde_json::{Map, Value};

use crate::catalog::Schema;
use crate::error::{corruption, Result};
use crate::find::Query;
use crate::guard::Guard;
use crate::keys;
use crate::record::{decode_doc, decode_kv};

/// Which records of a collection a scan returns: those whose key (or id) starts with `prefix`
/// and comes after `after`, in key order, at most `limit` of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    pub prefix: Vec<u8>,
    pub after: Option<Vec<u8>>,
    pub limit: Option<usize>,
}

impl Scan {
    /// Every record of the collection.
    #[must_use]
    pub fn all() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    #[must_use]
    pub fn prefix(mut self, prefix: impl Into<Vec<u8>>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Resumes after the key (or id) `after`, the last one a previous page returned.
    #[must_use]
    pub fn after(mut self, after: impl Into<Vec<u8>>) -> Self {
        self.after = Some(after.into());
        self
    }

    fn full(&self, count: usize) -> bool {
        self.limit.is_some_and(|limit| count >= limit)
    }

    /// Engine bounds of the scan over the collection whose keys start with `base`.
    fn bounds(&self, base: &[u8]) -> (Vec<u8>, bool, Vec<u8>) {
        let mut first = base.to_vec();
        first.extend_from_slice(&self.prefix);
        // `base` ends with the 0x00 separator, so its end always exists.
        let end = keys::prefix_end(&first).unwrap_or_default();
        match &self.after {
            Some(after) => {
                let mut resume = base.to_vec();
                resume.extend_from_slice(after);
                if resume < first {
                    (first, true, end)
                } else {
                    (resume, false, end)
                }
            }
            None => (first, true, end),
        }
    }
}

/// One stored record as a source returns it.
#[derive(Debug, Clone)]
pub struct Item {
    pub key: Bytes,
    pub value: Bytes,
    /// Engine version; 0 for a write of the reading transaction that is not committed yet.
    pub version: u64,
    pub pending: bool,
}

pub type Items<'s> = Box<dyn Iterator<Item = Result<Item>> + 's>;

/// Where reads find records.
pub trait Source {
    fn get(&self, key: &Bytes) -> Result<Option<Item>>;
    fn range(&self, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<Items<'_>>;
    /// Records that a committed record was read, for sources that validate their reads.
    fn observe(&self, item: &Item);
    /// Uncommitted records with keys in `[start, end)`, in key order.
    fn pending(&self, start: &[u8], end: &[u8]) -> Vec<Item>;
    fn is_pending(&self, key: &[u8]) -> bool;
}

/// The latest committed state, optionally pinned to a snapshot so a multi-step read (an index
/// lookup and its documents, a long scan) sees one state.
#[derive(Debug)]
pub struct Latest<'a> {
    engine: &'a Db,
    snapshot: Option<Arc<Snapshot>>,
}

impl<'a> Latest<'a> {
    /// Reads the newest version of each record: for single-record reads.
    pub fn now(engine: &'a Db) -> Self {
        Self {
            engine,
            snapshot: None,
        }
    }

    /// Reads one fixed state.
    pub fn pinned(engine: &'a Db) -> Self {
        Self {
            engine,
            snapshot: Some(Arc::new(engine.snapshot())),
        }
    }

    /// Reads the state of `snapshot`.
    pub fn at(engine: &'a Db, snapshot: &Arc<Snapshot>) -> Self {
        Self {
            engine,
            snapshot: Some(Arc::clone(snapshot)),
        }
    }
}

impl Source for Latest<'_> {
    fn get(&self, key: &Bytes) -> Result<Option<Item>> {
        let found = match &self.snapshot {
            Some(snapshot) => self.engine.get_at(snapshot, key)?,
            None => self.engine.get_versioned(key)?,
        };
        Ok(found.map(|v| Item {
            key: key.clone(),
            value: v.value,
            version: v.version,
            pending: false,
        }))
    }

    fn range(&self, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<Items<'_>> {
        let iter = match &self.snapshot {
            Some(snapshot) => self.engine.range_at(snapshot, start, end)?,
            None => self.engine.range(start, end)?,
        };
        Ok(Box::new(iter.map(|entry| {
            let entry = entry?;
            Ok(Item {
                key: entry.key,
                value: entry.value,
                version: entry.version,
                pending: false,
            })
        })))
    }

    fn observe(&self, _item: &Item) {}

    fn pending(&self, _start: &[u8], _end: &[u8]) -> Vec<Item> {
        Vec::new()
    }

    fn is_pending(&self, _key: &[u8]) -> bool {
        false
    }
}

fn kv_entry(namespace: &str, key: &[u8], item: &Item, now_ms: i64) -> Result<Option<KvEntry>> {
    let record = decode_kv(&item.value)?;
    if record.expired(now_ms) {
        return Ok(None);
    }
    Ok(Some(KvEntry {
        namespace: namespace.to_owned(),
        key: key.to_vec(),
        value: record.value.to_vec(),
        version: item.version,
        expires_at_ms: record.expires_at_ms,
    }))
}

fn doc_entry(space: &str, id: &[u8], item: &Item) -> Result<JsonEntry> {
    let id = std::str::from_utf8(id)
        .map_err(|_| corruption(format!("document id in {space} is not UTF-8")))?;
    Ok(JsonEntry::new(
        space,
        id,
        decode_doc(&item.value)?,
        item.version,
    ))
}

pub fn kv_get(
    source: &impl Source,
    guard: &Guard,
    namespace: &str,
    key: &[u8],
    now_ms: i64,
) -> Result<Option<KvEntry>> {
    let Some(item) = source.get(&keys::kv(namespace, key))? else {
        return Ok(None);
    };
    Ok(kv_entry(namespace, key, &item, now_ms)?.and_then(|entry| guard.kv(entry)))
}

/// The records of the collection whose keys start with `base` that `scan` selects (before its
/// limit), in key order.
///
/// # Errors
///
/// Engine errors opening the range.
pub fn scan_range<'s>(source: &'s impl Source, base: &[u8], scan: &Scan) -> Result<Items<'s>> {
    let (start, inclusive, end) = scan.bounds(base);
    let start = if inclusive {
        Bound::Included(start.as_slice())
    } else {
        Bound::Excluded(start.as_slice())
    };
    source.range(start, Bound::Excluded(&end))
}

pub fn kv_scan(
    source: &impl Source,
    guard: &Guard,
    namespace: &str,
    scan: &Scan,
    now_ms: i64,
) -> Result<Vec<KvEntry>> {
    let base = keys::collection(keys::KV, namespace);
    let mut out = Vec::new();
    if scan.full(0) {
        return Ok(out);
    }
    for item in scan_range(source, &base, scan)? {
        let item = item?;
        source.observe(&item);
        let key = keys::suffix(&item.key, &base);
        if let Some(entry) =
            kv_entry(namespace, key, &item, now_ms)?.and_then(|entry| guard.kv(entry))
        {
            out.push(entry);
            if scan.full(out.len()) {
                break;
            }
        }
    }
    Ok(out)
}

pub fn doc_get(
    source: &impl Source,
    guard: &Guard,
    space: &str,
    id: &str,
) -> Result<Option<JsonEntry>> {
    let Some(item) = source.get(&keys::doc(space, id))? else {
        return Ok(None);
    };
    guard.doc(doc_entry(space, id.as_bytes(), &item)?)
}

/// Documents of `space` matching `query` (every document for an empty query), served by a
/// secondary index when one is complete for the query.
pub fn doc_find(
    source: &impl Source,
    guard: &Guard,
    schema: Option<&Schema>,
    space: &str,
    query: &Map<String, Value>,
    scan: &Scan,
) -> Result<Vec<JsonEntry>> {
    let query = Query::new(query, schema);
    let mut out = Vec::new();
    if scan.full(0) {
        return Ok(out);
    }
    let base = keys::collection(keys::DOC, space);
    let accept = |item: &Item, out: &mut Vec<JsonEntry>| -> Result<bool> {
        let entry = doc_entry(space, keys::suffix(&item.key, &base), item)?;
        if let Some(entry) = guard.doc(entry)? {
            if query.matches(&entry.doc) {
                out.push(entry);
            }
        }
        Ok(scan.full(out.len()))
    };

    let Some(probe) = schema
        .filter(|_| !query.is_empty())
        .and_then(|s| query.probe(s))
    else {
        let (start, inclusive, end) = scan.bounds(&base);
        let start = if inclusive {
            Bound::Included(start.as_slice())
        } else {
            Bound::Excluded(start.as_slice())
        };
        for item in source.range(start, Bound::Excluded(&end))? {
            let item = item?;
            source.observe(&item);
            if accept(&item, &mut out)? {
                break;
            }
        }
        return Ok(out);
    };

    // Index entries of the probed value are ordered by document id; a transaction's own
    // writes are not indexed yet, so its pending documents of the space are merged in by id.
    let value_base = keys::index_value(space, probe.index, &probe.value);
    let (start, inclusive, end) = scan.bounds(&value_base);
    let (doc_start, _, doc_end) = scan.bounds(&base);
    let mut pending = source
        .pending(&doc_start, &doc_end)
        .into_iter()
        .filter(|item| match &scan.after {
            Some(after) => keys::suffix(&item.key, &base) > after.as_slice(),
            None => true,
        });
    let mut next_pending = pending.next();
    let bound = if inclusive {
        Bound::Included(start.as_slice())
    } else {
        Bound::Excluded(start.as_slice())
    };
    for entry in source.range(bound, Bound::Excluded(&end))? {
        let entry = entry?;
        let id = keys::suffix(&entry.key, &value_base);
        let mut doc_key = base.clone();
        doc_key.extend_from_slice(id);
        while let Some(item) = next_pending.take() {
            if item.key.as_ref() >= doc_key.as_slice() {
                next_pending = Some(item);
                break;
            }
            if accept(&item, &mut out)? {
                return Ok(out);
            }
            next_pending = pending.next();
        }
        if source.is_pending(&doc_key) {
            continue;
        }
        if let Some(item) = source.get(&Bytes::from(doc_key))? {
            if accept(&item, &mut out)? {
                return Ok(out);
            }
        }
    }
    while let Some(item) = next_pending {
        if accept(&item, &mut out)? {
            break;
        }
        next_pending = pending.next();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use serde_json::json;

    use super::*;
    use crate::guard::Access;
    use crate::Database;

    /// Counts the records a find reads.
    struct Counting<'a> {
        inner: Latest<'a>,
        gets: Cell<usize>,
        ranged: Cell<usize>,
    }

    impl Source for Counting<'_> {
        fn get(&self, key: &Bytes) -> Result<Option<Item>> {
            self.gets.set(self.gets.get() + 1);
            self.inner.get(key)
        }

        fn range(&self, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<Items<'_>> {
            let items = self.inner.range(start, end)?;
            Ok(Box::new(
                items.inspect(|_| self.ranged.set(self.ranged.get() + 1)),
            ))
        }

        fn observe(&self, _item: &Item) {}

        fn pending(&self, _start: &[u8], _end: &[u8]) -> Vec<Item> {
            Vec::new()
        }

        fn is_pending(&self, _key: &[u8]) -> bool {
            false
        }
    }

    #[test]
    fn finds_read_only_the_documents_the_index_lists() {
        let dir = tempfile::tempdir().unwrap();
        let options = crate::Options::for_hardware(&lilia_io::HardwareProfile::minimal());
        let db = Database::open(dir.path(), options).unwrap();
        let s = db.system();
        for i in 0..500 {
            s.json_put("docs", &format!("d{i:03}"), &json!({"bucket": i % 50}))
                .unwrap();
        }
        db.create_index(
            "docs",
            "by_bucket",
            "$.bucket",
            &lilia_core::IndexOptions::default(),
        )
        .unwrap();
        let schema = db.schema("docs");
        let source = Counting {
            inner: Latest::pinned(db.engine()),
            gets: Cell::new(0),
            ranged: Cell::new(0),
        };
        let access = Access::System;
        let guard = Guard::new(&access, schema.clone());
        let query = json!({"bucket": 7}).as_object().unwrap().clone();
        let found = doc_find(
            &source,
            &guard,
            schema.as_deref(),
            "docs",
            &query,
            &Scan::all(),
        )
        .unwrap();
        assert_eq!(found.len(), 10);
        assert_eq!(
            (source.gets.get(), source.ranged.get()),
            (10, 10),
            "ten index entries, ten documents"
        );

        let unindexed = json!({"missing": 1}).as_object().unwrap().clone();
        let source = Counting {
            inner: Latest::pinned(db.engine()),
            gets: Cell::new(0),
            ranged: Cell::new(0),
        };
        assert!(doc_find(
            &source,
            &guard,
            schema.as_deref(),
            "docs",
            &unindexed,
            &Scan::all()
        )
        .unwrap()
        .is_empty());
        assert_eq!(
            (source.gets.get(), source.ranged.get()),
            (0, 500),
            "without an index, a scan"
        );
    }
}
