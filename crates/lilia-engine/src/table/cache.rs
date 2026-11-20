//! Cache of decoded blocks, bounded in bytes.
//!
//! CLOCK replacement: a hit only sets the block's reference bit under a shared lock, so
//! concurrent readers of a shard never wait on each other. Eviction (under the shard's exclusive
//! lock, on insert) walks the insertion ring: referenced blocks get a second chance, the others
//! go. Blocks that are read once, as by a scan, are evicted before blocks that are read again.

use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;

use super::block::Block;

const SHARDS: usize = 64;

/// `(table number, block offset)`.
type CacheKey = (u64, u64);

/// Multiply-fold hash for the two integers of a key: fast, and good enough for a hash table.
#[derive(Debug, Default)]
struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write_u64(u64::from(byte));
        }
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = (self.0.rotate_left(5) ^ value).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

#[derive(Debug)]
struct Slot {
    block: Arc<Block>,
    referenced: AtomicBool,
}

#[derive(Debug, Default)]
struct Shard {
    map: HashMap<CacheKey, Slot, BuildHasherDefault<KeyHasher>>,
    /// Keys in insertion order; may hold keys already removed by `evict_table`.
    ring: VecDeque<CacheKey>,
    bytes: usize,
}

/// Block cache shared by every table of a database.
#[derive(Debug)]
pub struct BlockCache {
    shards: Vec<RwLock<Shard>>,
    shard_capacity: usize,
}

impl BlockCache {
    pub fn new(capacity_bytes: usize) -> Self {
        Self { shards: (0..SHARDS).map(|_| RwLock::new(Shard::default())).collect(), shard_capacity: capacity_bytes / SHARDS }
    }

    /// Spreads the blocks of every table over all shards: both halves of the key go through a
    /// full avalanche, so neighbouring blocks of one table land in unrelated shards.
    fn shard(&self, key: CacheKey) -> &RwLock<Shard> {
        let mut z = key.0.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ key.1;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        &self.shards[usize::try_from(z % SHARDS as u64).unwrap_or_default()]
    }

    pub fn get(&self, table: u64, offset: u64) -> Option<Arc<Block>> {
        let shard = self.shard((table, offset)).read();
        let slot = shard.map.get(&(table, offset))?;
        if !slot.referenced.load(Ordering::Relaxed) {
            slot.referenced.store(true, Ordering::Relaxed);
        }
        Some(Arc::clone(&slot.block))
    }

    pub fn insert(&self, table: u64, offset: u64, block: Arc<Block>) {
        if self.shard_capacity == 0 {
            return;
        }
        let key = (table, offset);
        let mut shard = self.shard(key).write();
        let size = block.size();
        if let Some(old) = shard.map.insert(key, Slot { block, referenced: AtomicBool::new(false) }) {
            shard.bytes = shard.bytes - old.block.size() + size;
        } else {
            shard.bytes += size;
            shard.ring.push_back(key);
        }
        while shard.bytes > self.shard_capacity {
            let Some(candidate) = shard.ring.pop_front() else { break };
            let second_chance = match shard.map.get(&candidate) {
                Some(slot) => slot.referenced.swap(false, Ordering::Relaxed),
                None => continue,
            };
            if second_chance {
                shard.ring.push_back(candidate);
            } else if let Some(evicted) = shard.map.remove(&candidate) {
                shard.bytes -= evicted.block.size();
            }
        }
    }

    /// Drops every cached block of `table` (its file is being deleted).
    pub fn evict_table(&self, table: u64) {
        for shard in &self.shards {
            let mut shard = shard.write();
            let mut freed = 0;
            shard.map.retain(|key, slot| {
                let keep = key.0 != table;
                if !keep {
                    freed += slot.block.size();
                }
                keep
            });
            shard.bytes -= freed;
            shard.ring.retain(|key| key.0 != table);
        }
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;
    use crate::key::{encode, Kind};
    use crate::table::block::BlockBuilder;

    fn block() -> Arc<Block> {
        let mut builder = BlockBuilder::default();
        builder.add(&encode(b"k", 1, Kind::Put), &[7u8; 50]);
        Arc::new(Block::new(Bytes::from(builder.finish())).unwrap())
    }

    #[test]
    fn unreferenced_blocks_are_evicted_first_and_tables_can_be_dropped() {
        // One shard's capacity holds two blocks; pick three offsets of table 1 in one shard.
        let size = block().size();
        let cache = BlockCache::new(SHARDS * (2 * size + 1));
        let first = std::ptr::from_ref(cache.shard((1, 0)));
        let offsets: Vec<u64> = (0..).filter(|&o| std::ptr::eq(cache.shard((1, o)), first)).take(3).collect();
        let (a, b, c) = (offsets[0], offsets[1], offsets[2]);
        cache.insert(1, a, block());
        cache.insert(1, b, block());
        assert!(cache.get(1, a).is_some(), "block a is now referenced");
        cache.insert(1, c, block());
        assert!(cache.get(1, a).is_some(), "a referenced block gets a second chance");
        assert!(cache.get(1, b).is_none(), "the unreferenced block was evicted");
        assert!(cache.get(1, c).is_some());
        cache.evict_table(1);
        assert!(cache.get(1, a).is_none() && cache.get(1, c).is_none());
    }

    #[test]
    fn blocks_of_one_table_spread_over_all_shards() {
        let cache = BlockCache::new(1 << 20);
        let used: std::collections::HashSet<*const RwLock<Shard>> =
            (0..4_096u64).map(|i| std::ptr::from_ref(cache.shard((7, i * 16_384)))).collect();
        assert_eq!(used.len(), SHARDS);
    }
}
