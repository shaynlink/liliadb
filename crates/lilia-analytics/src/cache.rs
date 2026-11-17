//! Byte-bounded LRU cache of decoded column chunks.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard, PoisonError};

use datafusion::arrow::array::{Array, ArrayRef};

/// Identity of a decoded column chunk.
///
/// A segment id is never reused within a table and `table_uid` is unique per table
/// creation, so a cached chunk can never go stale: no invalidation is needed for
/// correctness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkKey {
    pub table_uid: i64,
    pub segment_id: i64,
    pub field: usize,
}

#[derive(Debug)]
struct Entry {
    array: ArrayRef,
    bytes: usize,
    tick: u64,
}

#[derive(Debug, Default)]
struct State {
    entries: HashMap<ChunkKey, Entry>,
    /// Recency index: the smallest tick is the least recently used chunk.
    recency: BTreeMap<u64, ChunkKey>,
    next_tick: u64,
    used_bytes: usize,
}

impl State {
    fn touch(&mut self) -> u64 {
        let tick = self.next_tick;
        self.next_tick += 1;
        tick
    }

    fn remove(&mut self, key: &ChunkKey) {
        if let Some(entry) = self.entries.remove(key) {
            self.recency.remove(&entry.tick);
            self.used_bytes -= entry.bytes;
        }
    }
}

/// Keeps recently scanned column chunks decoded in memory, within a byte budget.
#[derive(Debug)]
pub struct ChunkCache {
    capacity_bytes: usize,
    state: Mutex<State>,
}

impl ChunkCache {
    /// Creates a cache holding at most `capacity_bytes` of decoded arrays.
    /// A capacity of `0` disables caching.
    #[must_use]
    pub fn new(capacity_bytes: usize) -> Self {
        Self {
            capacity_bytes,
            state: Mutex::new(State::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // The state stays consistent across a panic: every mutation is a single step.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Returns the cached chunk and marks it as most recently used.
    #[must_use]
    pub fn get(&self, key: &ChunkKey) -> Option<ArrayRef> {
        let mut state = self.lock();
        let tick = state.touch();
        let entry = state.entries.get_mut(key)?;
        let previous = std::mem::replace(&mut entry.tick, tick);
        let array = ArrayRef::clone(&entry.array);
        state.recency.remove(&previous);
        state.recency.insert(tick, *key);
        Some(array)
    }

    /// Caches a chunk, evicting the least recently used ones to stay within budget.
    /// A chunk larger than the whole budget is not cached.
    pub fn insert(&self, key: ChunkKey, array: ArrayRef) {
        let bytes = array.get_array_memory_size();
        if bytes > self.capacity_bytes {
            return;
        }
        let mut state = self.lock();
        state.remove(&key);
        while state.used_bytes + bytes > self.capacity_bytes {
            let Some((_, oldest)) = state.recency.pop_first() else {
                break;
            };
            if let Some(evicted) = state.entries.remove(&oldest) {
                state.used_bytes -= evicted.bytes;
            }
        }
        let tick = state.touch();
        state.recency.insert(tick, key);
        state.entries.insert(key, Entry { array, bytes, tick });
        state.used_bytes += bytes;
    }

    /// Drops one chunk, typically because its segment was rewritten.
    pub fn remove(&self, key: &ChunkKey) {
        self.lock().remove(key);
    }

    /// Drops every chunk of a table, releasing its memory.
    pub fn remove_table(&self, table_uid: i64) {
        let mut state = self.lock();
        let keys: Vec<ChunkKey> = state
            .entries
            .keys()
            .filter(|key| key.table_uid == table_uid)
            .copied()
            .collect();
        for key in keys {
            state.remove(&key);
        }
    }

    /// Bytes of decoded arrays currently held.
    #[must_use]
    pub fn used_bytes(&self) -> usize {
        self.lock().used_bytes
    }

    /// Number of chunks currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    /// Whether the cache holds no chunk.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use datafusion::arrow::array::Int64Array;

    use super::*;

    fn chunk(rows: usize) -> ArrayRef {
        Arc::new(Int64Array::from(vec![7_i64; rows]))
    }

    fn key(table_uid: i64, segment_id: i64) -> ChunkKey {
        ChunkKey {
            table_uid,
            segment_id,
            field: 0,
        }
    }

    #[test]
    fn evicts_the_least_recently_used_chunk() {
        let size = chunk(1024).get_array_memory_size();
        let cache = ChunkCache::new(size * 2);
        cache.insert(key(1, 0), chunk(1024));
        cache.insert(key(1, 1), chunk(1024));
        // Touch segment 0 so segment 1 becomes the eviction candidate.
        assert!(cache.get(&key(1, 0)).is_some());
        cache.insert(key(1, 2), chunk(1024));

        assert!(cache.get(&key(1, 0)).is_some());
        assert!(cache.get(&key(1, 1)).is_none());
        assert!(cache.get(&key(1, 2)).is_some());
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.used_bytes(), size * 2);
    }

    #[test]
    fn oversized_chunks_and_zero_capacity_are_not_cached() {
        let size = chunk(1024).get_array_memory_size();
        let small = ChunkCache::new(size - 1);
        small.insert(key(1, 0), chunk(1024));
        assert!(small.is_empty());

        let disabled = ChunkCache::new(0);
        disabled.insert(key(1, 0), chunk(1));
        assert!(disabled.get(&key(1, 0)).is_none());
    }

    #[test]
    fn reinserting_a_key_replaces_it_without_leaking_bytes() {
        let cache = ChunkCache::new(usize::MAX);
        cache.insert(key(1, 0), chunk(1024));
        cache.insert(key(1, 0), chunk(2048));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.used_bytes(), chunk(2048).get_array_memory_size());
        assert_eq!(cache.get(&key(1, 0)).unwrap().len(), 2048);
    }

    #[test]
    fn remove_table_only_drops_that_table() {
        let cache = ChunkCache::new(usize::MAX);
        cache.insert(key(1, 0), chunk(8));
        cache.insert(key(1, 1), chunk(8));
        cache.insert(key(2, 0), chunk(8));
        cache.remove_table(1);
        assert_eq!(cache.len(), 1);
        assert!(cache.get(&key(2, 0)).is_some());
        assert_eq!(cache.used_bytes(), chunk(8).get_array_memory_size());
    }
}
