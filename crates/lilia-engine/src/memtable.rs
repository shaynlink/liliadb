//! In-memory sorted buffer of recent writes: a lock-free skip list keyed by internal key.

use std::ops::Bound;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use crossbeam_skiplist::SkipMap;

use crate::batch::Op;
use crate::error::Result;
use crate::iter::InternalIterator;
use crate::key::{self, InternalKey, Kind};
use crate::table::filter;

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
    /// Bloom filter over the user keys, so a lookup of a key that is not here (the common case
    /// for older keys) skips the skip-list traversal. Bits are only ever set.
    bloom: Vec<AtomicU64>,
    /// Number of the journal file holding this memtable's writes.
    pub journal: u64,
}

/// Overhead charged per entry for skip-list nodes, on top of key and value bytes.
const ENTRY_OVERHEAD: usize = 64;
const BLOOM_PROBES: u32 = 6;
const BLOOM_BLOCK_WORDS: usize = 8;
/// Entries read ahead by a memtable iterator per skip-list search.
const ITER_BATCH: usize = 64;

impl Memtable {
    /// A memtable sized for about `capacity_bytes`: the Bloom filter gets one bit per 8 bytes,
    /// which is at least 10 bits per entry for entries of 80 bytes or more.
    pub fn new(journal: u64, capacity_bytes: usize) -> Self {
        let words = (capacity_bytes / 8 / 64).max(64).next_multiple_of(BLOOM_BLOCK_WORDS);
        Self { map: SkipMap::new(), bytes: AtomicUsize::new(0), bloom: (0..words).map(|_| AtomicU64::new(0)).collect(), journal }
    }

    /// Blocked Bloom filter: every probe of a key falls in one 64-byte block (8 words), so a
    /// lookup touches a single cache line. Returns the block's first word and one mask per word.
    fn bloom_block(&self, user: &[u8]) -> (usize, [u64; BLOOM_BLOCK_WORDS]) {
        let h = filter::hash(user);
        let blocks = (self.bloom.len() / BLOOM_BLOCK_WORDS) as u64;
        let base = usize::try_from((h >> 32) % blocks).unwrap_or_default() * BLOOM_BLOCK_WORDS;
        let mut masks = [0u64; BLOOM_BLOCK_WORDS];
        let mut bits = h.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        for _ in 0..BLOOM_PROBES {
            // 9 bits pick one of the block's 512 bits.
            let bit = usize::try_from(bits & 511).unwrap_or_default();
            masks[bit / 64] |= 1u64 << (bit % 64);
            bits = bits.rotate_right(9);
        }
        (base, masks)
    }

    fn may_contain(&self, user: &[u8]) -> bool {
        let (base, masks) = self.bloom_block(user);
        masks.iter().enumerate().all(|(i, &mask)| self.bloom[base + i].load(Ordering::Relaxed) & mask == mask)
    }

    /// Inserts one write. The commit leader is the only inserter; readers never block.
    pub fn insert(&self, sequence: u64, op: &Op) {
        let (kind, value) = match op {
            Op::Put { value, .. } => (Kind::Put, value.clone()),
            Op::Delete { .. } => (Kind::Delete, Bytes::new()),
        };
        // Only the commit leader (or replay) inserts, so a plain load and store per touched
        // word is enough; readers load the words concurrently.
        let (base, masks) = self.bloom_block(op.key());
        for (i, &mask) in masks.iter().enumerate().filter(|(_, &m)| m != 0) {
            let word = &self.bloom[base + i];
            word.store(word.load(Ordering::Relaxed) | mask, Ordering::Relaxed);
        }
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

    /// The newest version of `user` visible at the sequence of `lookup` (built by
    /// [`key::lookup`] for `user`).
    pub fn get(&self, user: &[u8], lookup: &InternalKey) -> Option<Found> {
        // Bits are set before the entry is inserted, and readers only see entries whose sequence
        // was published after the insert, so a visible entry always has its bits set.
        if !self.may_contain(user) {
            return None;
        }
        let entry = self.map.lower_bound(Bound::Included(lookup))?;
        let found = &entry.key().0;
        (key::user(found) == user).then(|| Found {
            // Keys are encoded by `insert`, so the kind byte is always valid.
            kind: key::kind(found).unwrap_or(Kind::Delete),
            sequence: key::sequence(found),
            value: entry.value().clone(),
        })
    }

    pub fn iter(self: &Arc<Self>) -> MemtableIter {
        MemtableIter { table: Arc::clone(self), batch: Vec::with_capacity(ITER_BATCH), at: 0 }
    }
}

/// Iterator over a memtable; keeps the memtable alive while it exists. It reads entries ahead in
/// batches: one skip-list search per batch instead of one per entry. Entries share their bytes
/// with the memtable.
pub struct MemtableIter {
    table: Arc<Memtable>,
    batch: Vec<(Bytes, Bytes)>,
    at: usize,
}

impl MemtableIter {
    fn fill(&mut self, from: Bound<&InternalKey>) {
        let batch: Vec<(Bytes, Bytes)> = self
            .table
            .map
            .range((from, Bound::Unbounded))
            .take(ITER_BATCH)
            .map(|e| (e.key().0.clone(), e.value().clone()))
            .collect();
        self.batch = batch;
        self.at = 0;
    }

    fn current(&self) -> Option<&(Bytes, Bytes)> {
        self.batch.get(self.at)
    }
}

impl InternalIterator for MemtableIter {
    fn valid(&self) -> bool {
        self.at < self.batch.len()
    }

    fn seek_to_first(&mut self) -> Result<()> {
        self.fill(Bound::Unbounded);
        Ok(())
    }

    fn seek(&mut self, target: &[u8]) -> Result<()> {
        self.fill(Bound::Included(&InternalKey(Bytes::copy_from_slice(target))));
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        if !self.valid() {
            return Ok(());
        }
        self.at += 1;
        if self.at == self.batch.len() && self.batch.len() == ITER_BATCH {
            let last = InternalKey(self.batch[ITER_BATCH - 1].0.clone());
            self.fill(Bound::Excluded(&last));
        }
        Ok(())
    }

    fn key(&self) -> &[u8] {
        self.current().map_or(&[], |(k, _)| k)
    }

    fn value(&self) -> &[u8] {
        self.current().map_or(&[], |(_, v)| v)
    }

    fn value_bytes(&self) -> Bytes {
        self.current().map_or_else(Bytes::new, |(_, v)| v.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups_respect_sequence_visibility_and_tombstones() {
        let table = Memtable::new(1, 1 << 20);
        table.insert(1, &Op::Put { key: "k".into(), value: "v1".into() });
        table.insert(2, &Op::Delete { key: "k".into() });
        table.insert(3, &Op::Put { key: "k".into(), value: "v3".into() });
        table.insert(4, &Op::Put { key: "k2".into(), value: "x".into() });
        let at = |user: &[u8], sequence| table.get(user, &InternalKey(Bytes::from(key::lookup(user, sequence))));
        assert_eq!(at(b"k", 1).unwrap().value, "v1");
        assert_eq!(at(b"k", 2).unwrap().kind, Kind::Delete);
        assert_eq!(at(b"k", 10).unwrap(), Found { kind: Kind::Put, sequence: 3, value: "v3".into() });
        assert!(at(b"k", 0).is_none());
        assert!(at(b"j", 10).is_none());
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
