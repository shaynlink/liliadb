//! In-memory sorted buffer of recent writes: a lock-free skip list keyed by internal key.

use std::ops::Bound;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use crossbeam_skiplist::SkipMap;

use crate::batch::Op;
use crate::error::Result;
use crate::iter::InternalIterator;
use crate::key::{self, InternalKey, Kind};

/// The newest version of a key found by a point lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub kind: Kind,
    pub sequence: u64,
    pub value: Bytes,
}

#[derive(Debug)]
pub struct Memtable {
    map: SkipMap<InternalKey, Bytes>,
    bytes: AtomicUsize,
    /// Number of the journal file holding this memtable's writes.
    pub journal: u64,
}

/// Overhead charged per entry for skip-list nodes, on top of key and value bytes.
const ENTRY_OVERHEAD: usize = 64;

impl Memtable {
    pub fn new(journal: u64) -> Self {
        Self { map: SkipMap::new(), bytes: AtomicUsize::new(0), journal }
    }

    /// Inserts one write. The commit leader is the only inserter; readers never block.
    pub fn insert(&self, sequence: u64, op: &Op) {
        let (kind, value) = match op {
            Op::Put { value, .. } => (Kind::Put, value.clone()),
            Op::Delete { .. } => (Kind::Delete, Bytes::new()),
        };
        let encoded = Bytes::from(key::encode(op.key(), sequence, kind));
        self.bytes.fetch_add(encoded.len() + value.len() + ENTRY_OVERHEAD, Ordering::Relaxed);
        self.map.insert(InternalKey(encoded), value);
    }

    /// Approximate memory used, to decide when to switch to a new memtable.
    pub fn approximate_bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The newest version of `user` with a sequence number at most `sequence`.
    pub fn get(&self, user: &[u8], sequence: u64) -> Option<Found> {
        let target = InternalKey(Bytes::from(key::lookup(user, sequence)));
        let entry = self.map.lower_bound(Bound::Included(&target))?;
        let found = &entry.key().0;
        (key::user(found) == user).then(|| Found {
            // Keys are encoded by `insert`, so the kind byte is always valid.
            kind: key::kind(found).unwrap_or(Kind::Delete),
            sequence: key::sequence(found),
            value: entry.value().clone(),
        })
    }

    pub fn iter(self: &Arc<Self>) -> MemtableIter {
        MemtableIter { table: Arc::clone(self), current: None }
    }
}

/// Iterator over a memtable; keeps the memtable alive while it exists.
pub struct MemtableIter {
    table: Arc<Memtable>,
    current: Option<(Bytes, Bytes)>,
}

impl MemtableIter {
    fn set(&mut self, bound: Bound<&InternalKey>) {
        self.current = self.table.map.lower_bound(bound).map(|e| (e.key().0.clone(), e.value().clone()));
    }
}

impl InternalIterator for MemtableIter {
    fn valid(&self) -> bool {
        self.current.is_some()
    }

    fn seek_to_first(&mut self) -> Result<()> {
        self.current = self.table.map.front().map(|e| (e.key().0.clone(), e.value().clone()));
        Ok(())
    }

    fn seek(&mut self, target: &[u8]) -> Result<()> {
        self.set(Bound::Included(&InternalKey(Bytes::copy_from_slice(target))));
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        if let Some((key, _)) = self.current.take() {
            self.set(Bound::Excluded(&InternalKey(key)));
        }
        Ok(())
    }

    fn key(&self) -> &[u8] {
        self.current.as_ref().map_or(&[], |(k, _)| k)
    }

    fn value(&self) -> &[u8] {
        self.current.as_ref().map_or(&[], |(_, v)| v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups_respect_sequence_visibility_and_tombstones() {
        let table = Memtable::new(1);
        table.insert(1, &Op::Put { key: "k".into(), value: "v1".into() });
        table.insert(2, &Op::Delete { key: "k".into() });
        table.insert(3, &Op::Put { key: "k".into(), value: "v3".into() });
        table.insert(4, &Op::Put { key: "k2".into(), value: "x".into() });
        assert_eq!(table.get(b"k", 1).unwrap().value, "v1");
        assert_eq!(table.get(b"k", 2).unwrap().kind, Kind::Delete);
        assert_eq!(table.get(b"k", 10).unwrap(), Found { kind: Kind::Put, sequence: 3, value: "v3".into() });
        assert!(table.get(b"k", 0).is_none());
        assert!(table.get(b"j", 10).is_none());
        assert!(table.approximate_bytes() > 0);

        let table = Arc::new(table);
        let mut iter = table.iter();
        iter.seek_to_first().unwrap();
        let mut seen = Vec::new();
        while iter.valid() {
            seen.push((key::user(iter.key()).to_vec(), key::sequence(iter.key())));
            iter.next().unwrap();
        }
        assert_eq!(seen, [(b"k".to_vec(), 3), (b"k".to_vec(), 2), (b"k".to_vec(), 1), (b"k2".to_vec(), 4)]);
    }
}
