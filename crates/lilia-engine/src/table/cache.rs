//! Cache of decoded blocks, bounded in bytes, sharded to keep lock contention low.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use parking_lot::Mutex;

use super::block::Block;

const SHARDS: usize = 16;

/// `(table number, block offset)`.
type CacheKey = (u64, u64);

#[derive(Debug, Default)]
struct Shard {
    entries: HashMap<CacheKey, (Arc<Block>, u64)>,
    /// Least recently used first: tick → key.
    order: BTreeMap<u64, CacheKey>,
    tick: u64,
    bytes: usize,
}

/// Least-recently-used block cache shared by every table of a database.
#[derive(Debug)]
pub struct BlockCache {
    shards: Vec<Mutex<Shard>>,
    shard_capacity: usize,
}

impl BlockCache {
    pub fn new(capacity_bytes: usize) -> Self {
        Self { shards: (0..SHARDS).map(|_| Mutex::new(Shard::default())).collect(), shard_capacity: capacity_bytes / SHARDS }
    }

    fn shard(&self, key: CacheKey) -> &Mutex<Shard> {
        let mix = (key.0.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (key.1 >> 12)).rotate_left(29);
        &self.shards[usize::try_from(mix % SHARDS as u64).unwrap_or_default()]
    }

    pub fn get(&self, table: u64, offset: u64) -> Option<Arc<Block>> {
        let key = (table, offset);
        let mut shard = self.shard(key).lock();
        shard.tick += 1;
        let tick = shard.tick;
        let (block, old) = shard.entries.get_mut(&key).map(|(b, t)| (Arc::clone(b), std::mem::replace(t, tick)))?;
        shard.order.remove(&old);
        shard.order.insert(tick, key);
        Some(block)
    }

    pub fn insert(&self, table: u64, offset: u64, block: Arc<Block>) {
        if self.shard_capacity == 0 {
            return;
        }
        let key = (table, offset);
        let mut shard = self.shard(key).lock();
        shard.tick += 1;
        let tick = shard.tick;
        let size = block.size();
        if let Some((_, old)) = shard.entries.insert(key, (block, tick)) {
            shard.order.remove(&old);
        } else {
            shard.bytes += size;
        }
        shard.order.insert(tick, key);
        while shard.bytes > self.shard_capacity {
            let Some((_, victim)) = shard.order.pop_first() else { break };
            if let Some((evicted, _)) = shard.entries.remove(&victim) {
                shard.bytes -= evicted.size();
            }
        }
    }

    /// Drops every cached block of `table` (its file is being deleted).
    pub fn evict_table(&self, table: u64) {
        for shard in &self.shards {
            let mut shard = shard.lock();
            let victims: Vec<(CacheKey, u64)> =
                shard.entries.iter().filter(|(k, _)| k.0 == table).map(|(k, (_, t))| (*k, *t)).collect();
            for (key, tick) in victims {
                if let Some((block, _)) = shard.entries.remove(&key) {
                    shard.bytes -= block.size();
                }
                shard.order.remove(&tick);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;
    use crate::key::{encode, Kind};
    use crate::table::block::BlockBuilder;

    #[test]
    fn evicts_least_recently_used_blocks_beyond_capacity() {
        let cache = BlockCache::new(SHARDS * 100);
        let block = || {
            let mut builder = BlockBuilder::default();
            builder.add(&encode(b"k", 1, Kind::Put), &[7u8; 50]);
            Arc::new(Block::new(Bytes::from(builder.finish())).unwrap())
        };
        // Same shard: same table, offsets that differ only below bit 12.
        cache.insert(1, 0, block());
        cache.insert(1, 1, block());
        assert!(cache.get(1, 0).is_none(), "the oldest block was evicted");
        assert!(cache.get(1, 1).is_some());
        cache.evict_table(1);
        assert!(cache.get(1, 1).is_none());
    }
}
