use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock, RwLockWriteGuard};

use lilia_core::crdt::{CvRdt, LwwClock, LwwJsonDocument, OrSet, PNCounter};
use lilia_core::{
    BatchOperation, CollectionKind, ErrorCode, EvictionPolicy, GraphEdge, GraphNode, Hlc,
    HnswConfig, HnswIndex, HybridClock, LiliaError, MemoryTierStats, MutationResult, Result,
    StorageTier, TimeSeriesPoint, ValueCodec, VectorElementType,
};
#[cfg(not(target_arch = "wasm32"))]
use lilia_sqlite_native::{
    ConstraintOp, IndexConstraint, IndexPlan, VTabValue, VirtualCursor, VirtualTableProvider,
};

use crate::catalog::CollectionCatalog;
use crate::storage::now_ms;

const NUM_SHARDS: usize = 64;

#[derive(Debug, Clone)]
pub(crate) struct MemEntry<V> {
    pub value: V,
    pub version: u64,
    pub expires_at_ms: Option<i64>,
    pub inserted_seq: u64,
    pub last_access_seq: u64,
    pub payload_bytes: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct MemSnapshot<V> {
    pub value: V,
    pub version: u64,
    pub expires_at_ms: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct ShardBucket<V> {
    map: HashMap<(String, Vec<u8>), MemEntry<V>>,
}

impl<V> Default for ShardBucket<V> {
    fn default() -> Self {
        Self {
            map: HashMap::new(),
        }
    }
}

impl<V> std::ops::Deref for ShardBucket<V> {
    type Target = HashMap<(String, Vec<u8>), MemEntry<V>>;

    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

impl<V> std::ops::DerefMut for ShardBucket<V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.map
    }
}

/// 64-way sharded concurrent in-memory table with native TTL expiration, CAS versioning,
/// ordered shard locking for mixed transactions, and optional LRU / drop-oldest eviction.
#[derive(Debug)]
pub(crate) struct ShardedMemTable<V> {
    shards: [RwLock<ShardBucket<V>>; NUM_SHARDS],
    space_counts: RwLock<HashMap<String, Arc<AtomicUsize>>>,
    seq_counter: AtomicU64,
    total_entries: AtomicUsize,
    estimated_bytes: AtomicUsize,
    ttl_evictions: AtomicU64,
    lru_evictions: AtomicU64,
}

impl<V> Default for ShardedMemTable<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V> ShardedMemTable<V> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            shards: std::array::from_fn(|_| RwLock::new(ShardBucket::default())),
            space_counts: RwLock::new(HashMap::new()),
            seq_counter: AtomicU64::new(1),
            total_entries: AtomicUsize::new(0),
            estimated_bytes: AtomicUsize::new(0),
            ttl_evictions: AtomicU64::new(0),
            lru_evictions: AtomicU64::new(0),
        }
    }

    #[inline]
    pub fn shard_index(space: &str, key: &[u8]) -> usize {
        let mut h = DefaultHasher::new();
        space.hash(&mut h);
        key.hash(&mut h);
        // The low bits of the hash pick the shard: the mask keeps the value below `NUM_SHARDS`.
        usize::try_from(h.finish() & (NUM_SHARDS as u64 - 1)).unwrap_or_default()
    }

    fn space_counter(&self, space: &str) -> Arc<AtomicUsize> {
        {
            let read = self
                .space_counts
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(counter) = read.get(space) {
                return Arc::clone(counter);
            }
        }
        let mut write = self
            .space_counts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(
            write
                .entry(space.to_owned())
                .or_insert_with(|| Arc::new(AtomicUsize::new(0))),
        )
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.total_entries.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        self.estimated_bytes.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn ttl_evictions(&self) -> u64 {
        self.ttl_evictions.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn lru_evictions(&self) -> u64 {
        self.lru_evictions.load(Ordering::Relaxed)
    }

    pub fn next_seq(&self) -> u64 {
        self.seq_counter.fetch_add(1, Ordering::Relaxed)
    }

    pub fn clear_space(&self, space: &str) -> usize {
        let mut removed = 0usize;
        for shard in &self.shards {
            let mut guard = shard
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut to_remove = Vec::new();
            for (ns, key) in guard.map.keys() {
                if ns == space {
                    to_remove.push(key.clone());
                }
            }
            for k in to_remove {
                if let Some(old) = guard.map.remove(&(space.to_owned(), k)) {
                    self.total_entries.fetch_sub(1, Ordering::Relaxed);
                    self.estimated_bytes
                        .fetch_sub(old.payload_bytes, Ordering::Relaxed);
                    removed += 1;
                }
            }
        }
        if let Some(counter) = self
            .space_counts
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(space)
            .cloned()
        {
            counter.store(0, Ordering::Relaxed);
        }
        removed
    }

    #[inline]
    pub fn shards_write_at(&self, idx: usize) -> RwLockWriteGuard<'_, ShardBucket<V>> {
        self.shards[idx]
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn lock_shards_for_keys<'a>(
        &'a self,
        keys: &[(&str, &[u8])],
    ) -> Vec<(usize, RwLockWriteGuard<'a, ShardBucket<V>>)> {
        let mut indices: Vec<usize> = keys
            .iter()
            .map(|(space, key)| Self::shard_index(space, key))
            .collect();
        indices.sort_unstable();
        indices.dedup();
        indices
            .into_iter()
            .map(|idx| {
                let guard = self.shards[idx]
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                (idx, guard)
            })
            .collect()
    }
}

impl<V: Clone> ShardedMemTable<V> {
    pub fn get(
        &self,
        space: &str,
        key: &[u8],
        now: i64,
        track_lru: bool,
    ) -> Option<MemSnapshot<V>> {
        let shard_idx = Self::shard_index(space, key);
        let lookup_key = (space.to_owned(), key.to_vec());
        let expired = {
            let guard = self.shards[shard_idx]
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            {
                let entry = guard.map.get(&lookup_key)?;
                if entry.expires_at_ms.is_some_and(|exp| exp <= now) {
                    true
                } else if !track_lru {
                    return Some(MemSnapshot {
                        value: entry.value.clone(),
                        version: entry.version,
                        expires_at_ms: entry.expires_at_ms,
                    });
                } else {
                    false
                }
            }
        };

        // Expired cleanup or LRU access timestamp update.
        let mut guard = self.shards[shard_idx]
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if expired {
            if let Some(old) = guard.map.remove(&lookup_key) {
                self.total_entries.fetch_sub(1, Ordering::Relaxed);
                self.estimated_bytes
                    .fetch_sub(old.payload_bytes, Ordering::Relaxed);
                self.ttl_evictions.fetch_add(1, Ordering::Relaxed);
                self.space_counter(space).fetch_sub(1, Ordering::Relaxed);
            }
            return None;
        }
        let seq = self.next_seq();
        if let Some(entry) = guard.map.get_mut(&lookup_key) {
            if entry.expires_at_ms.is_some_and(|exp| exp <= now) {
                let old = guard.map.remove(&lookup_key).expect("checked");
                self.total_entries.fetch_sub(1, Ordering::Relaxed);
                self.estimated_bytes
                    .fetch_sub(old.payload_bytes, Ordering::Relaxed);
                self.ttl_evictions.fetch_add(1, Ordering::Relaxed);
                self.space_counter(space).fetch_sub(1, Ordering::Relaxed);
                return None;
            }
            entry.last_access_seq = seq;
            return Some(MemSnapshot {
                value: entry.value.clone(),
                version: entry.version,
                expires_at_ms: entry.expires_at_ms,
            });
        }
        None
    }

    pub fn version(&self, space: &str, key: &[u8], now: i64) -> Option<u64> {
        let shard_idx = Self::shard_index(space, key);
        let lookup_key = (space.to_owned(), key.to_vec());
        let guard = self.shards[shard_idx]
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.map.get(&lookup_key).and_then(|e| {
            if e.expires_at_ms.is_some_and(|exp| exp <= now) {
                None
            } else {
                Some(e.version)
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn put(
        &self,
        space: &str,
        key: &[u8],
        value: V,
        payload_bytes: usize,
        if_version: Option<u64>,
        expires_at_ms: Option<i64>,
        eviction: EvictionPolicy,
        now: i64,
    ) -> Result<u64> {
        let shard_idx = Self::shard_index(space, key);
        let lookup_key = (space.to_owned(), key.to_vec());
        let seq = self.next_seq();
        let new_bytes = space.len() + key.len() + payload_bytes;

        let new_version = {
            let mut guard = self.shards[shard_idx]
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let active_ver = match guard.map.get(&lookup_key) {
                Some(e) if e.expires_at_ms.is_none_or(|exp| exp > now) => Some(e.version),
                _ => None,
            };
            assert_mem_version(active_ver, if_version)?;
            let next_ver = active_ver.unwrap_or(0) + 1;
            let entry = MemEntry {
                value,
                version: next_ver,
                expires_at_ms,
                inserted_seq: seq,
                last_access_seq: seq,
                payload_bytes: new_bytes,
            };
            if let Some(old) = guard.map.insert(lookup_key, entry) {
                self.estimated_bytes
                    .fetch_sub(old.payload_bytes, Ordering::Relaxed);
                self.estimated_bytes.fetch_add(new_bytes, Ordering::Relaxed);
                if old.expires_at_ms.is_some_and(|exp| exp <= now) {
                    self.ttl_evictions.fetch_add(1, Ordering::Relaxed);
                }
            } else {
                self.total_entries.fetch_add(1, Ordering::Relaxed);
                self.estimated_bytes.fetch_add(new_bytes, Ordering::Relaxed);
                self.space_counter(space).fetch_add(1, Ordering::Relaxed);
            }
            next_ver
        };

        self.enforce_eviction(space, eviction, now);
        Ok(new_version)
    }

    pub fn delete(
        &self,
        space: &str,
        key: &[u8],
        if_version: Option<u64>,
        now: i64,
    ) -> Result<bool> {
        let shard_idx = Self::shard_index(space, key);
        let lookup_key = (space.to_owned(), key.to_vec());
        let mut guard = self.shards[shard_idx]
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let active_ver = match guard.map.get(&lookup_key) {
            Some(e) if e.expires_at_ms.is_none_or(|exp| exp > now) => Some(e.version),
            _ => None,
        };
        assert_mem_version(active_ver, if_version)?;
        if let Some(old) = guard.map.remove(&lookup_key) {
            self.total_entries.fetch_sub(1, Ordering::Relaxed);
            self.estimated_bytes
                .fetch_sub(old.payload_bytes, Ordering::Relaxed);
            self.space_counter(space).fetch_sub(1, Ordering::Relaxed);
            Ok(old.expires_at_ms.is_none_or(|exp| exp > now))
        } else {
            Ok(false)
        }
    }

    pub fn mutate_locked<F>(
        shard: &mut ShardBucket<V>,
        table: &Self,
        space: &str,
        key: &[u8],
        now: i64,
        f: F,
    ) -> Result<()>
    where
        F: FnOnce(Option<&mut MemEntry<V>>) -> Result<Option<MemEntry<V>>>,
    {
        let lookup_key = (space.to_owned(), key.to_vec());
        if let Some(existing) = shard.map.get_mut(&lookup_key) {
            let old_bytes = existing.payload_bytes;
            if let Some(replacement) = f(Some(existing))? {
                table
                    .estimated_bytes
                    .fetch_sub(old_bytes, Ordering::Relaxed);
                table
                    .estimated_bytes
                    .fetch_add(replacement.payload_bytes, Ordering::Relaxed);
                shard.map.insert(lookup_key, replacement);
            } else {
                if let Some(updated) = shard.map.get(&lookup_key) {
                    let new_bytes = updated.payload_bytes;
                    if new_bytes >= old_bytes {
                        table
                            .estimated_bytes
                            .fetch_add(new_bytes - old_bytes, Ordering::Relaxed);
                    } else {
                        table
                            .estimated_bytes
                            .fetch_sub(old_bytes - new_bytes, Ordering::Relaxed);
                    }
                }
                if updated_expired(shard.map.get(&lookup_key), now) {
                    if let Some(old) = shard.map.remove(&lookup_key) {
                        table.total_entries.fetch_sub(1, Ordering::Relaxed);
                        table
                            .estimated_bytes
                            .fetch_sub(old.payload_bytes, Ordering::Relaxed);
                        table.space_counter(space).fetch_sub(1, Ordering::Relaxed);
                    }
                }
            }
        } else if let Some(new_entry) = f(None)? {
            table.total_entries.fetch_add(1, Ordering::Relaxed);
            table
                .estimated_bytes
                .fetch_add(new_entry.payload_bytes, Ordering::Relaxed);
            table.space_counter(space).fetch_add(1, Ordering::Relaxed);
            shard.map.insert(lookup_key, new_entry);
        }
        Ok(())
    }

    pub fn remove_exact(&self, space: &str, key: &[u8]) -> bool {
        let shard_idx = Self::shard_index(space, key);
        let lookup_key = (space.to_owned(), key.to_vec());
        let mut guard = self.shards[shard_idx]
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(old) = guard.map.remove(&lookup_key) {
            self.total_entries.fetch_sub(1, Ordering::Relaxed);
            self.estimated_bytes
                .fetch_sub(old.payload_bytes, Ordering::Relaxed);
            self.space_counter(space).fetch_sub(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    pub fn scan(
        &self,
        space: Option<&str>,
        after: Option<&[u8]>,
        limit: usize,
        now: i64,
    ) -> Vec<(String, Vec<u8>, MemSnapshot<V>)> {
        let mut items = Vec::new();
        for shard in &self.shards {
            let guard = shard
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for ((ns, key), entry) in &guard.map {
                if let Some(target_ns) = space {
                    if ns != target_ns {
                        continue;
                    }
                }
                if let Some(after_key) = after {
                    if key.as_slice() <= after_key {
                        continue;
                    }
                }
                if entry.expires_at_ms.is_some_and(|exp| exp <= now) {
                    continue;
                }
                items.push((
                    ns.clone(),
                    key.clone(),
                    MemSnapshot {
                        value: entry.value.clone(),
                        version: entry.version,
                        expires_at_ms: entry.expires_at_ms,
                    },
                ));
            }
        }
        items.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        if items.len() > limit {
            items.truncate(limit);
        }
        items
    }

    fn enforce_eviction(&self, space: &str, eviction: EvictionPolicy, now: i64) {
        let (max_entries, use_lru) = match eviction {
            EvictionPolicy::None => return,
            EvictionPolicy::Lru { max_entries } => (max_entries, true),
            EvictionPolicy::DropOldest { max_entries } => (max_entries, false),
        };
        let counter = self.space_counter(space);
        let current = counter.load(Ordering::Relaxed);
        if current <= max_entries {
            return;
        }

        // First sweep expired keys and gather candidate victim timestamps.
        let mut candidates: Vec<(usize, Vec<u8>, u64)> = Vec::new();
        for (idx, shard) in self.shards.iter().enumerate() {
            let mut guard = shard
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut expired_keys = Vec::new();
            for ((ns, k), entry) in &guard.map {
                if ns != space {
                    continue;
                }
                if entry.expires_at_ms.is_some_and(|exp| exp <= now) {
                    expired_keys.push(k.clone());
                } else {
                    let rank = if use_lru {
                        entry.last_access_seq
                    } else {
                        entry.inserted_seq
                    };
                    candidates.push((idx, k.clone(), rank));
                }
            }
            for k in expired_keys {
                if let Some(old) = guard.map.remove(&(space.to_owned(), k)) {
                    self.total_entries.fetch_sub(1, Ordering::Relaxed);
                    self.estimated_bytes
                        .fetch_sub(old.payload_bytes, Ordering::Relaxed);
                    self.ttl_evictions.fetch_add(1, Ordering::Relaxed);
                    counter.fetch_sub(1, Ordering::Relaxed);
                }
            }
        }

        if candidates.len() <= max_entries {
            return;
        }
        let excess = candidates.len() - max_entries;
        candidates.sort_by_key(|c| c.2);
        for (idx, k, _) in candidates.into_iter().take(excess) {
            let mut guard = self.shards[idx]
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(old) = guard.map.remove(&(space.to_owned(), k)) {
                self.total_entries.fetch_sub(1, Ordering::Relaxed);
                self.estimated_bytes
                    .fetch_sub(old.payload_bytes, Ordering::Relaxed);
                self.lru_evictions.fetch_add(1, Ordering::Relaxed);
                counter.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
}

fn updated_expired<V>(entry: Option<&MemEntry<V>>, now: i64) -> bool {
    entry
        .and_then(|e| e.expires_at_ms)
        .is_some_and(|exp| exp <= now)
}

pub(crate) fn assert_mem_version(current: Option<u64>, expected: Option<u64>) -> Result<()> {
    match expected {
        None => Ok(()),
        Some(0) => {
            if let Some(ver) = current {
                return Err(LiliaError::new(
                    ErrorCode::Conflict,
                    format!("expected absent record, found version {ver}"),
                    false,
                ));
            }
            Ok(())
        }
        Some(exp) => match current {
            Some(cur) if cur == exp => Ok(()),
            Some(cur) => Err(LiliaError::new(
                ErrorCode::Conflict,
                format!("version mismatch: expected {exp}, found {cur}"),
                false,
            )),
            None => Err(LiliaError::new(
                ErrorCode::Conflict,
                format!("version mismatch: expected {exp}, found absent"),
                false,
            )),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemIndexInfo {
    pub expression: String,
    pub filter: Option<String>,
}

/// Pure-RAM sharded storage engine supporting zero-disk KV, JSON, `TimeSeries`, Graph, Vectors, and CRDTs.
#[derive(Debug)]
pub struct MemEngine {
    pub(crate) kv: ShardedMemTable<Vec<u8>>,
    pub(crate) json: ShardedMemTable<serde_json::Value>,
    pub(crate) json_indexes: RwLock<HashMap<(String, String), MemIndexInfo>>,
    pub(crate) timeseries: ShardedMemTable<Vec<TimeSeriesPoint>>,
    pub(crate) graph_nodes: ShardedMemTable<GraphNode>,
    pub(crate) graph_edges: ShardedMemTable<GraphEdge>,
    pub(crate) vectors: ShardedMemTable<(u32, VectorElementType, Vec<u8>)>,
    pub(crate) hnsw_meta: RwLock<HashMap<String, HnswConfig>>,
    pub(crate) hnsw_indices: RwLock<HashMap<String, Arc<RwLock<HnswIndex>>>>,
    pub(crate) crdt_counters: ShardedMemTable<PNCounter>,
    pub(crate) crdt_orsets: ShardedMemTable<OrSet<String>>,
    pub(crate) crdt_json: ShardedMemTable<(LwwJsonDocument, serde_json::Value, Option<Hlc>)>,
    pub(crate) catalog: Arc<CollectionCatalog>,
}

impl MemEngine {
    #[must_use]
    pub fn new(catalog: Arc<CollectionCatalog>) -> Self {
        Self {
            kv: ShardedMemTable::new(),
            json: ShardedMemTable::new(),
            json_indexes: RwLock::new(HashMap::new()),
            timeseries: ShardedMemTable::new(),
            graph_nodes: ShardedMemTable::new(),
            graph_edges: ShardedMemTable::new(),
            vectors: ShardedMemTable::new(),
            hnsw_meta: RwLock::new(HashMap::new()),
            hnsw_indices: RwLock::new(HashMap::new()),
            crdt_counters: ShardedMemTable::new(),
            crdt_orsets: ShardedMemTable::new(),
            crdt_json: ShardedMemTable::new(),
            catalog,
        }
    }

    #[must_use]
    pub fn stats(&self) -> MemoryTierStats {
        let now = now_ms();
        let ts_series = self.timeseries.scan(None, None, usize::MAX, now);
        let ts_points: usize = ts_series.iter().map(|(_, _, s)| s.value.len()).sum();
        let estimated_bytes = self.kv.estimated_bytes()
            + self.json.estimated_bytes()
            + self.timeseries.estimated_bytes()
            + self.graph_nodes.estimated_bytes()
            + self.graph_edges.estimated_bytes()
            + self.vectors.estimated_bytes()
            + self.crdt_counters.estimated_bytes()
            + self.crdt_orsets.estimated_bytes()
            + self.crdt_json.estimated_bytes();

        let ttl_evictions =
            self.kv.ttl_evictions() + self.json.ttl_evictions() + self.timeseries.ttl_evictions();
        let lru_evictions =
            self.kv.lru_evictions() + self.json.lru_evictions() + self.timeseries.lru_evictions();

        MemoryTierStats {
            kv_entries: self.kv.len(),
            json_entries: self.json.len(),
            timeseries_points: ts_points,
            graph_nodes: self.graph_nodes.len(),
            graph_edges: self.graph_edges.len(),
            vector_entries: self.vectors.len(),
            crdt_entries: self.crdt_counters.len() + self.crdt_orsets.len() + self.crdt_json.len(),
            estimated_bytes,
            ttl_evictions,
            lru_evictions,
        }
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn kv_put_direct(
        &self,
        namespace: &str,
        key: &[u8],
        stored_value: Vec<u8>,
        if_version: Option<u64>,
        explicit_expires_at_ms: Option<i64>,
    ) -> Result<u64> {
        let now = now_ms();
        let spec = self.catalog.spec_for(CollectionKind::Kv, namespace);
        let expires = explicit_expires_at_ms.or_else(|| {
            spec.ttl_ms
                .map(|ttl| now.saturating_add(i64::try_from(ttl).unwrap_or(i64::MAX)))
        });
        let bytes = stored_value.len();
        self.kv.put(
            namespace,
            key,
            stored_value,
            bytes,
            if_version,
            expires,
            spec.eviction,
            now,
        )
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn kv_delete_direct(
        &self,
        namespace: &str,
        key: &[u8],
        if_version: Option<u64>,
    ) -> Result<bool> {
        self.kv.delete(namespace, key, if_version, now_ms())
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn json_put_direct(
        &self,
        space: &str,
        id: &str,
        value: serde_json::Value,
        if_version: Option<u64>,
    ) -> Result<u64> {
        let now = now_ms();
        let spec = self.catalog.spec_for(CollectionKind::Json, space);
        let expires = spec
            .ttl_ms
            .map(|ttl| now.saturating_add(i64::try_from(ttl).unwrap_or(i64::MAX)));
        let bytes = serde_json::to_vec(&value).map_or(64, |v| v.len());
        self.json.put(
            space,
            id.as_bytes(),
            value,
            bytes,
            if_version,
            expires,
            spec.eviction,
            now,
        )
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn ts_append_direct(
        &self,
        series_id: &str,
        resolution_ms: i64,
        points: &[TimeSeriesPoint],
    ) -> Result<()> {
        if points.is_empty() {
            return Ok(());
        }
        let now = now_ms();
        let spec = self.catalog.spec_for(CollectionKind::TimeSeries, series_id);
        let key = resolution_ms.to_be_bytes();
        let shard_idx = ShardedMemTable::<Vec<TimeSeriesPoint>>::shard_index(series_id, &key);
        let lookup = (series_id.to_owned(), key.to_vec());
        let mut guard = self.timeseries.shards[shard_idx]
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let existing_len = guard.map.get(&lookup).map_or(0, |e| e.value.len());
        let mut merged = if let Some(old) = guard.map.get_mut(&lookup) {
            std::mem::take(&mut old.value)
        } else {
            Vec::with_capacity(points.len())
        };
        merged.extend_from_slice(points);
        merged.sort_by_key(|p| p.timestamp);
        if let Some(ttl_ms) = spec.ttl_ms {
            let cutoff = now.saturating_sub(i64::try_from(ttl_ms).unwrap_or(i64::MAX));
            merged.retain(|p| p.timestamp >= cutoff);
        }
        if let EvictionPolicy::DropOldest { max_entries } | EvictionPolicy::Lru { max_entries } =
            spec.eviction
        {
            if merged.len() > max_entries {
                let drop_count = merged.len() - max_entries;
                merged.drain(..drop_count);
                self.timeseries
                    .lru_evictions
                    .fetch_add(drop_count as u64, Ordering::Relaxed);
            }
        }
        let payload_bytes = merged.len() * std::mem::size_of::<TimeSeriesPoint>();
        let seq = self.timeseries.next_seq();
        let next_ver = guard.map.get(&lookup).map_or(1, |e| e.version + 1);
        let old_bytes = guard.map.get(&lookup).map_or(0, |e| e.payload_bytes);
        guard.map.insert(
            lookup,
            MemEntry {
                value: merged,
                version: next_ver,
                expires_at_ms: None,
                inserted_seq: seq,
                last_access_seq: seq,
                payload_bytes: series_id.len() + key.len() + payload_bytes,
            },
        );
        if existing_len == 0 {
            self.timeseries
                .total_entries
                .fetch_add(1, Ordering::Relaxed);
        }
        let new_bytes = series_id.len() + key.len() + payload_bytes;
        if new_bytes >= old_bytes {
            self.timeseries
                .estimated_bytes
                .fetch_add(new_bytes - old_bytes, Ordering::Relaxed);
        } else {
            self.timeseries
                .estimated_bytes
                .fetch_sub(old_bytes - new_bytes, Ordering::Relaxed);
        }
        Ok(())
    }

    pub fn ts_query_direct(
        &self,
        series_id: &str,
        resolution_ms: i64,
        start_time: i64,
        end_time: i64,
    ) -> Vec<TimeSeriesPoint> {
        let key = resolution_ms.to_be_bytes();
        let Some(snap) = self.timeseries.get(series_id, &key, now_ms(), false) else {
            return Vec::new();
        };
        snap.value
            .into_iter()
            .filter(|p| p.timestamp >= start_time && p.timestamp <= end_time)
            .collect()
    }

    pub fn crdt_counter_incr(
        &self,
        space: &str,
        id: &str,
        actor_id: &str,
        amount: u64,
        is_increment: bool,
    ) -> i64 {
        let now = now_ms();
        let key = id.as_bytes();
        let shard_idx = ShardedMemTable::<PNCounter>::shard_index(space, key);
        let lookup = (space.to_owned(), key.to_vec());
        let mut guard = self.crdt_counters.shards[shard_idx]
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let seq = self.crdt_counters.next_seq();
        let entry = guard.map.entry(lookup).or_insert_with(|| {
            self.crdt_counters
                .total_entries
                .fetch_add(1, Ordering::Relaxed);
            MemEntry {
                value: PNCounter::new(),
                version: 0,
                expires_at_ms: None,
                inserted_seq: seq,
                last_access_seq: seq,
                payload_bytes: space.len() + id.len() + 64,
            }
        });
        if is_increment {
            entry.value.increment(actor_id, amount);
        } else {
            entry.value.decrement(actor_id, amount);
        }
        entry.version += 1;
        entry.last_access_seq = seq;
        let _ = now;
        entry.value.value()
    }

    pub fn crdt_orset_mutate(
        &self,
        space: &str,
        id: &str,
        actor_id: Option<&str>,
        element: &str,
        add: bool,
        ts_ms: i64,
    ) -> bool {
        let key = id.as_bytes();
        let shard_idx = ShardedMemTable::<OrSet<String>>::shard_index(space, key);
        let lookup = (space.to_owned(), key.to_vec());
        let mut guard = self.crdt_orsets.shards[shard_idx]
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let seq = self.crdt_orsets.next_seq();
        let entry = guard.map.entry(lookup).or_insert_with(|| {
            self.crdt_orsets
                .total_entries
                .fetch_add(1, Ordering::Relaxed);
            MemEntry {
                value: OrSet::new(),
                version: 0,
                expires_at_ms: None,
                inserted_seq: seq,
                last_access_seq: seq,
                payload_bytes: space.len() + id.len() + 64,
            }
        });
        entry.version += 1;
        entry.last_access_seq = seq;
        if add {
            let actor = actor_id.unwrap_or("local");
            let tag = format!("{actor}:{ts_ms}:{seq}");
            entry
                .value
                .entries
                .entry(element.to_owned())
                .or_default()
                .insert(tag);
            true
        } else {
            let mut removed = false;
            if let Some(tags) = entry.value.entries.get(element).cloned() {
                for tag in tags {
                    entry.value.tombstones.insert(tag, ts_ms);
                    removed = true;
                }
            }
            removed
        }
    }

    pub fn crdt_orset_elements(&self, space: &str, id: &str) -> BTreeSet<String> {
        let Some(snap) = self.crdt_orsets.get(space, id.as_bytes(), now_ms(), false) else {
            return BTreeSet::new();
        };
        snap.value
            .entries
            .into_iter()
            .filter_map(|(elem, tags)| {
                if tags.iter().any(|t| !snap.value.tombstones.contains_key(t)) {
                    Some(elem)
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn crdt_json_set_field(
        &self,
        space: &str,
        id: &str,
        actor_id: &str,
        path: &str,
        value: serde_json::Value,
        ts_ms: i64,
    ) -> u64 {
        self.crdt_json_mutate(space, id, |doc| {
            let clock = LwwClock::at(ts_ms, actor_id);
            doc.set_field(path, value, clock);
        })
    }

    pub fn crdt_json_delete_field(
        &self,
        space: &str,
        id: &str,
        actor_id: &str,
        path: &str,
        ts_ms: i64,
    ) -> u64 {
        self.crdt_json_mutate(space, id, |doc| {
            let clock = LwwClock::at(ts_ms, actor_id);
            doc.delete_field(path, &clock);
        })
    }

    pub fn crdt_json_merge(&self, space: &str, id: &str, remote: &LwwJsonDocument) -> u64 {
        self.crdt_json_mutate(space, id, |doc| {
            CvRdt::merge(doc, remote);
        })
    }

    pub fn crdt_counter_merge(&self, space: &str, id: &str, remote: &PNCounter) -> i64 {
        let key = id.as_bytes();
        let shard_idx = ShardedMemTable::<PNCounter>::shard_index(space, key);
        let lookup = (space.to_owned(), key.to_vec());
        let mut guard = self.crdt_counters.shards_write_at(shard_idx);
        let seq = self.crdt_counters.next_seq();
        let is_new = !guard.contains_key(&lookup);
        let entry = guard.entry(lookup).or_insert_with(|| MemEntry {
            value: PNCounter::new(),
            version: 0,
            expires_at_ms: None,
            inserted_seq: seq,
            last_access_seq: seq,
            payload_bytes: space.len() + id.len() + 32,
        });
        CvRdt::merge(&mut entry.value, remote);
        entry.version += 1;
        entry.last_access_seq = seq;
        if is_new {
            self.crdt_counters
                .total_entries
                .fetch_add(1, Ordering::Relaxed);
        }
        entry.value.value()
    }

    pub fn crdt_orset_prune(&self, space: &str, id: &str, cutoff_ms: i64) -> usize {
        let key = id.as_bytes();
        let shard_idx = ShardedMemTable::<OrSet<String>>::shard_index(space, key);
        let lookup = (space.to_owned(), key.to_vec());
        let mut guard = self.crdt_orsets.shards_write_at(shard_idx);
        let Some(entry) = guard.get_mut(&lookup) else {
            return 0;
        };
        let before = entry.value.tombstones.len();
        entry.value.prune(cutoff_ms);
        let removed = before.saturating_sub(entry.value.tombstones.len());
        if removed > 0 {
            entry.version += 1;
            entry.last_access_seq = self.crdt_orsets.next_seq();
        }
        removed
    }

    pub fn crdt_orset_merge(&self, space: &str, id: &str, remote: &OrSet<String>) {
        let key = id.as_bytes();
        let shard_idx = ShardedMemTable::<OrSet<String>>::shard_index(space, key);
        let lookup = (space.to_owned(), key.to_vec());
        let mut guard = self.crdt_orsets.shards_write_at(shard_idx);
        let seq = self.crdt_orsets.next_seq();
        let is_new = !guard.contains_key(&lookup);
        let entry = guard.entry(lookup).or_insert_with(|| MemEntry {
            value: OrSet::new(),
            version: 0,
            expires_at_ms: None,
            inserted_seq: seq,
            last_access_seq: seq,
            payload_bytes: space.len() + id.len() + 64,
        });
        CvRdt::merge(&mut entry.value, remote);
        entry.version += 1;
        entry.last_access_seq = seq;
        if is_new {
            self.crdt_orsets
                .total_entries
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn crdt_json_mutate<F>(&self, space: &str, id: &str, f: F) -> u64
    where
        F: FnOnce(&mut LwwJsonDocument),
    {
        let key = id.as_bytes();
        let shard_idx =
            ShardedMemTable::<(LwwJsonDocument, serde_json::Value, Option<Hlc>)>::shard_index(
                space, key,
            );
        let lookup = (space.to_owned(), key.to_vec());
        let mut guard = self.crdt_json.shards_write_at(shard_idx);
        let seq = self.crdt_json.next_seq();
        let mut new_entry = !guard.contains_key(&lookup);
        let entry = guard.entry(lookup).or_insert_with(|| MemEntry {
            value: (LwwJsonDocument::new(), serde_json::json!({}), None),
            version: 0,
            expires_at_ms: None,
            inserted_seq: seq,
            last_access_seq: seq,
            payload_bytes: space.len() + id.len() + 32,
        });
        f(&mut entry.value.0);
        entry.value.1 = entry.value.0.to_value();
        entry.version += 1;
        entry.last_access_seq = seq;
        if new_entry {
            new_entry = false;
            self.crdt_json.total_entries.fetch_add(1, Ordering::Relaxed);
        }
        let _ = new_entry;
        entry.version
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn graph_set_node(&self, node: GraphNode) -> Result<u64> {
        let now = now_ms();
        let spec = self.catalog.spec_for(CollectionKind::Graph, &node.graph);
        let expires = spec
            .ttl_ms
            .map(|ttl| now.saturating_add(i64::try_from(ttl).unwrap_or(i64::MAX)));
        let id = node.id.clone();
        let graph = node.graph.clone();
        self.graph_nodes.put(
            &graph,
            id.as_bytes(),
            node,
            id.len() + 64,
            None,
            expires,
            spec.eviction,
            now,
        )
    }

    pub fn graph_delete_node(&self, graph: &str, id: &str) -> bool {
        let now = now_ms();
        // Remove all edges touching `id` inside `graph`.
        let edge_keys: Vec<Vec<u8>> = self
            .graph_edges
            .scan(Some(graph), None, usize::MAX, now)
            .into_iter()
            .filter_map(|(_, k, snap)| {
                if snap.value.source == id || snap.value.target == id {
                    Some(k)
                } else {
                    None
                }
            })
            .collect();
        for ek in edge_keys {
            self.graph_edges.remove_exact(graph, &ek);
        }
        self.graph_nodes.remove_exact(graph, id.as_bytes())
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn graph_add_edge(&self, edge: GraphEdge) -> Result<u64> {
        let now = now_ms();
        let spec = self.catalog.spec_for(CollectionKind::Graph, &edge.graph);
        let expires = spec
            .ttl_ms
            .map(|ttl| now.saturating_add(i64::try_from(ttl).unwrap_or(i64::MAX)));
        let ek = encode_edge_key(&edge.source, &edge.target, &edge.kind);
        let graph = edge.graph.clone();
        self.graph_edges.put(
            &graph,
            &ek,
            edge,
            ek.len() + 32,
            None,
            expires,
            spec.eviction,
            now,
        )
    }

    pub fn graph_remove_edge(&self, graph: &str, source: &str, target: &str, kind: &str) -> bool {
        let ek = encode_edge_key(source, target, kind);
        self.graph_edges.remove_exact(graph, &ek)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn vector_set(
        &self,
        collection: &str,
        id: &str,
        dim: u32,
        element_type: VectorElementType,
        vector: &[u8],
    ) -> Result<()> {
        let now = now_ms();
        let spec = self.catalog.spec_for(CollectionKind::Vector, collection);
        let expires = spec
            .ttl_ms
            .map(|ttl| now.saturating_add(i64::try_from(ttl).unwrap_or(i64::MAX)));
        self.vectors.put(
            collection,
            id.as_bytes(),
            (dim, element_type, vector.to_vec()),
            vector.len() + 16,
            None,
            expires,
            spec.eviction,
            now,
        )?;
        let indices = self
            .hnsw_indices
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index_arc) = indices.get(collection) {
            let mut idx = index_arc
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let index_vec = crate::vector::prepare_vector_for_index(
                vector,
                element_type,
                idx.config().element_type,
            );
            let _ = idx.insert(id, &index_vec);
        }
        Ok(())
    }

    pub fn vector_delete(&self, collection: &str, id: &str) -> bool {
        let deleted = self.vectors.remove_exact(collection, id.as_bytes());
        let indices = self
            .hnsw_indices
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index_arc) = indices.get(collection) {
            let mut idx = index_arc
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            idx.delete(id);
        }
        deleted
    }

    /// Validates CAS preconditions for a slice of memory KV/JSON operations without mutating state.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn preflight_ops(&self, ops: &[&BatchOperation], now: i64) -> Result<()> {
        for op in ops {
            match op {
                BatchOperation::KvSet {
                    namespace,
                    key,
                    if_version: Some(exp),
                    ..
                }
                | BatchOperation::KvDelete {
                    namespace,
                    key,
                    if_version: Some(exp),
                } => {
                    let cur = self.kv.version(namespace, key, now);
                    assert_mem_version(cur, Some(*exp))?;
                }
                BatchOperation::JsonPut {
                    space,
                    id,
                    if_version: Some(exp),
                    ..
                }
                | BatchOperation::JsonDelete {
                    space,
                    id,
                    if_version: Some(exp),
                } => {
                    let cur = self.json.version(space, id.as_bytes(), now);
                    assert_mem_version(cur, Some(*exp))?;
                }
                BatchOperation::JsonPutRaw {
                    space,
                    id,
                    data,
                    if_version,
                } => {
                    let mut buf = data.clone();
                    let _ = lilia_core::json::parse_json_bytes_to_binary(&mut buf)?;
                    if let Some(exp) = if_version {
                        let cur = self.json.version(space, id.as_bytes(), now);
                        assert_mem_version(cur, Some(*exp))?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Applies one non-FTS operation against pure-RAM structures and returns its [`MutationResult`].
    ///
    /// # Errors
    /// Returns `InvalidInput` or `Storage` errors detected here, and propagates errors from the
    /// operations it performs.
    #[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
    pub fn apply_op(
        &self,
        op: &BatchOperation,
        codec: &dyn ValueCodec,
        hlc_clock: &HybridClock,
    ) -> Result<MutationResult> {
        let now = now_ms();
        match op {
            BatchOperation::KvSet {
                namespace,
                key,
                value,
                if_version,
                expires_at_ms,
            } => {
                let encoded = codec.encode(value)?.into_owned();
                let ver =
                    self.kv_put_direct(namespace, key, encoded, *if_version, *expires_at_ms)?;
                Ok(mem_result(Some(ver), false))
            }
            BatchOperation::KvDelete {
                namespace,
                key,
                if_version,
            } => {
                let deleted = self.kv_delete_direct(namespace, key, *if_version)?;
                Ok(mem_result(None, deleted))
            }
            BatchOperation::JsonPut {
                space,
                id,
                value,
                if_version,
            } => {
                let ver = self.json_put_direct(space, id, value.clone(), *if_version)?;
                Ok(mem_result(Some(ver), false))
            }
            BatchOperation::JsonPutRaw {
                space,
                id,
                data,
                if_version,
            } => {
                let mut buf = data.clone();
                let bin_doc = lilia_core::json::parse_json_bytes_to_binary(&mut buf)?;
                let ver = self.json_put_direct(space, id, bin_doc.to_json_value(), *if_version)?;
                Ok(mem_result(Some(ver), false))
            }
            BatchOperation::JsonDelete {
                space,
                id,
                if_version,
            } => {
                let deleted = self.json.delete(space, id.as_bytes(), *if_version, now)?;
                Ok(mem_result(None, deleted))
            }
            BatchOperation::JsonPutHlc {
                space,
                id,
                value,
                hlc,
                ..
            } => {
                let final_hlc = hlc.clone().unwrap_or_else(|| hlc_clock.now());
                let ver = self.json_put_direct(space, id, value.clone(), None)?;
                Ok(MutationResult {
                    version: Some(ver),
                    deleted: false,
                    hlc: Some(final_hlc),
                    conflict_detected: Some(false),
                    tier: StorageTier::Memory,
                    buffered: false,
                })
            }
            BatchOperation::GraphSetNode {
                graph,
                id,
                kind,
                metadata,
                doc_space,
                doc_id,
            } => {
                self.graph_set_node(GraphNode {
                    graph: graph.clone(),
                    id: id.clone(),
                    kind: kind.clone(),
                    metadata: metadata.clone(),
                    doc_space: doc_space.clone(),
                    doc_id: doc_id.clone(),
                })?;
                Ok(mem_result(None, false))
            }
            BatchOperation::GraphDeleteNode { graph, id } => {
                let deleted = self.graph_delete_node(graph, id);
                Ok(mem_result(None, deleted))
            }
            BatchOperation::GraphAddEdge {
                graph,
                source,
                target,
                kind,
                weight,
            } => {
                self.graph_add_edge(GraphEdge {
                    graph: graph.clone(),
                    source: source.clone(),
                    target: target.clone(),
                    kind: kind.clone(),
                    weight: *weight,
                })?;
                Ok(mem_result(None, false))
            }
            BatchOperation::GraphRemoveEdge {
                graph,
                source,
                target,
                kind,
            } => {
                let deleted = self.graph_remove_edge(graph, source, target, kind);
                Ok(mem_result(None, deleted))
            }
            BatchOperation::VectorSet {
                collection,
                id,
                dim,
                element_type,
                vector,
            } => {
                self.vector_set(collection, id, *dim, *element_type, vector)?;
                Ok(mem_result(None, false))
            }
            BatchOperation::VectorDelete { collection, id } => {
                let deleted = self.vector_delete(collection, id);
                Ok(mem_result(None, deleted))
            }
            BatchOperation::CrdtPnCounterIncrement {
                space,
                id,
                actor_id,
                amount,
            } => {
                self.crdt_counter_incr(space, id, actor_id, *amount, true);
                Ok(mem_result(None, false))
            }
            BatchOperation::CrdtPnCounterDecrement {
                space,
                id,
                actor_id,
                amount,
            } => {
                self.crdt_counter_incr(space, id, actor_id, *amount, false);
                Ok(mem_result(None, false))
            }
            BatchOperation::CrdtOrSetAdd {
                space,
                id,
                actor_id,
                element,
            } => {
                self.crdt_orset_mutate(space, id, Some(actor_id), element, true, now);
                Ok(mem_result(None, false))
            }
            BatchOperation::CrdtOrSetRemove {
                space,
                id,
                element,
                timestamp_ms,
            } => {
                let deleted = self.crdt_orset_mutate(
                    space,
                    id,
                    None,
                    element,
                    false,
                    timestamp_ms.unwrap_or(now),
                );
                Ok(mem_result(None, deleted))
            }
            BatchOperation::CrdtJsonSetField {
                space,
                id,
                actor_id,
                path,
                value,
                timestamp_ms,
            } => {
                let ver = self.crdt_json_set_field(
                    space,
                    id,
                    actor_id,
                    path,
                    value.clone(),
                    timestamp_ms.unwrap_or(now),
                );
                Ok(mem_result(Some(ver), false))
            }
            BatchOperation::CrdtJsonDeleteField {
                space,
                id,
                actor_id,
                path,
                timestamp_ms,
            } => {
                let ver = self.crdt_json_delete_field(
                    space,
                    id,
                    actor_id,
                    path,
                    timestamp_ms.unwrap_or(now),
                );
                Ok(mem_result(Some(ver), false))
            }
            BatchOperation::CrdtJsonMerge { space, id, state } => {
                let remote: LwwJsonDocument = serde_json::from_slice(state)
                    .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
                let ver = self.crdt_json_merge(space, id, &remote);
                Ok(mem_result(Some(ver), false))
            }
            BatchOperation::FtsIndex { .. } | BatchOperation::FtsDelete { .. } => {
                Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "memory FTS operations route to _lilia_mem SQLite engine",
                    false,
                ))
            }
            BatchOperation::BitemporalPut { .. } | BatchOperation::BitemporalDelete { .. } => {
                Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "bitemporal operations route to disk SQLite engine",
                    false,
                ))
            }
        }
    }

    pub(crate) fn lock_kv_json_shards<'a>(&'a self, ops: &[&BatchOperation]) -> MemShardGuards<'a> {
        let mut kv_keys: Vec<(&str, &[u8])> = Vec::new();
        let mut json_keys: Vec<(&str, &[u8])> = Vec::new();
        for op in ops {
            match op {
                BatchOperation::KvSet { namespace, key, .. }
                | BatchOperation::KvDelete { namespace, key, .. } => {
                    kv_keys.push((namespace.as_str(), key.as_slice()));
                }
                BatchOperation::JsonPut { space, id, .. }
                | BatchOperation::JsonPutRaw { space, id, .. }
                | BatchOperation::JsonDelete { space, id, .. }
                | BatchOperation::JsonPutHlc { space, id, .. } => {
                    json_keys.push((space.as_str(), id.as_bytes()));
                }
                _ => {}
            }
        }
        MemShardGuards {
            kv: self.kv.lock_shards_for_keys(&kv_keys),
            json: self.json.lock_shards_for_keys(&json_keys),
            evict_kv: Vec::new(),
            evict_json: Vec::new(),
        }
    }

    pub(crate) fn preflight_with_guards(
        ops: &[&BatchOperation],
        guards: &MemShardGuards<'_>,
        now: i64,
    ) -> Result<()> {
        for op in ops {
            match op {
                BatchOperation::KvSet {
                    namespace,
                    key,
                    if_version: Some(exp),
                    ..
                }
                | BatchOperation::KvDelete {
                    namespace,
                    key,
                    if_version: Some(exp),
                } => {
                    let s_idx = ShardedMemTable::<Vec<u8>>::shard_index(namespace, key);
                    let bucket = guards.kv_bucket(s_idx);
                    let active_ver = bucket
                        .get(&(namespace.to_owned(), key.clone()))
                        .filter(|e| e.expires_at_ms.is_none_or(|exp_ms| exp_ms > now))
                        .map(|e| e.version);
                    assert_mem_version(active_ver, Some(*exp))?;
                }
                BatchOperation::JsonPut {
                    space,
                    id,
                    if_version: Some(exp),
                    ..
                }
                | BatchOperation::JsonDelete {
                    space,
                    id,
                    if_version: Some(exp),
                } => {
                    let key_bytes = id.as_bytes();
                    let s_idx = ShardedMemTable::<serde_json::Value>::shard_index(space, key_bytes);
                    let bucket = guards.json_bucket(s_idx);
                    let active_ver = bucket
                        .get(&(space.to_owned(), key_bytes.to_vec()))
                        .filter(|e| e.expires_at_ms.is_none_or(|exp_ms| exp_ms > now))
                        .map(|e| e.version);
                    assert_mem_version(active_ver, Some(*exp))?;
                }
                BatchOperation::JsonPutRaw {
                    space,
                    id,
                    data,
                    if_version,
                } => {
                    let mut buf = data.clone();
                    let _ = lilia_core::json::parse_json_bytes_to_binary(&mut buf)?;
                    if let Some(exp) = if_version {
                        let key_bytes = id.as_bytes();
                        let s_idx =
                            ShardedMemTable::<serde_json::Value>::shard_index(space, key_bytes);
                        let bucket = guards.json_bucket(s_idx);
                        let active_ver = bucket
                            .get(&(space.to_owned(), key_bytes.to_vec()))
                            .filter(|e| e.expires_at_ms.is_none_or(|exp_ms| exp_ms > now))
                            .map(|e| e.version);
                        assert_mem_version(active_ver, Some(*exp))?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
    pub(crate) fn apply_op_with_guards(
        &self,
        op: &BatchOperation,
        guards: &mut MemShardGuards<'_>,
        codec: &dyn ValueCodec,
        hlc_clock: &HybridClock,
        now: i64,
    ) -> Result<MutationResult> {
        match op {
            BatchOperation::KvSet {
                namespace,
                key,
                value,
                if_version,
                expires_at_ms,
            } => {
                let encoded = codec.encode(value)?.into_owned();
                let spec = self.catalog.spec_for(CollectionKind::Kv, namespace);
                let exp = expires_at_ms.or_else(|| {
                    spec.ttl_ms
                        .map(|t| now.saturating_add(i64::try_from(t).unwrap_or(i64::MAX)))
                });
                let s_idx = ShardedMemTable::<Vec<u8>>::shard_index(namespace, key);
                let bucket = guards.kv_bucket_mut(s_idx);
                let seq = self.kv.next_seq();
                let payload_len = encoded.len();
                let new_bytes = namespace.len() + key.len() + payload_len;
                let lookup = (namespace.clone(), key.clone());
                let active_ver = bucket
                    .get(&lookup)
                    .filter(|e| e.expires_at_ms.is_none_or(|x| x > now))
                    .map(|e| e.version);
                assert_mem_version(active_ver, *if_version)?;
                let next_ver = active_ver.unwrap_or(0) + 1;
                let old = bucket.insert(
                    lookup,
                    MemEntry {
                        value: encoded,
                        version: next_ver,
                        expires_at_ms: exp,
                        inserted_seq: seq,
                        last_access_seq: seq,
                        payload_bytes: new_bytes,
                    },
                );
                if let Some(o) = old {
                    self.kv
                        .estimated_bytes
                        .fetch_sub(o.payload_bytes, Ordering::Relaxed);
                    self.kv
                        .estimated_bytes
                        .fetch_add(new_bytes, Ordering::Relaxed);
                } else {
                    self.kv.total_entries.fetch_add(1, Ordering::Relaxed);
                    self.kv
                        .estimated_bytes
                        .fetch_add(new_bytes, Ordering::Relaxed);
                    self.kv
                        .space_counter(namespace)
                        .fetch_add(1, Ordering::Relaxed);
                }
                if !matches!(spec.eviction, EvictionPolicy::None) {
                    guards.evict_kv.push((namespace.clone(), spec.eviction));
                }
                Ok(mem_result(Some(next_ver), false))
            }
            BatchOperation::KvDelete {
                namespace,
                key,
                if_version,
            } => {
                let s_idx = ShardedMemTable::<Vec<u8>>::shard_index(namespace, key);
                let bucket = guards.kv_bucket_mut(s_idx);
                let lookup = (namespace.clone(), key.clone());
                let active_ver = bucket
                    .get(&lookup)
                    .filter(|e| e.expires_at_ms.is_none_or(|x| x > now))
                    .map(|e| e.version);
                assert_mem_version(active_ver, *if_version)?;
                let deleted = if let Some(old) = bucket.remove(&lookup) {
                    self.kv.total_entries.fetch_sub(1, Ordering::Relaxed);
                    self.kv
                        .estimated_bytes
                        .fetch_sub(old.payload_bytes, Ordering::Relaxed);
                    self.kv
                        .space_counter(namespace)
                        .fetch_sub(1, Ordering::Relaxed);
                    old.expires_at_ms.is_none_or(|x| x > now)
                } else {
                    false
                };
                Ok(mem_result(None, deleted))
            }
            BatchOperation::JsonPut {
                space,
                id,
                value,
                if_version,
            } => self.put_json_in_guards(space, id, value.clone(), *if_version, None, guards, now),
            BatchOperation::JsonPutRaw {
                space,
                id,
                data,
                if_version,
            } => {
                let mut buf = data.clone();
                let bin_doc = lilia_core::json::parse_json_bytes_to_binary(&mut buf)?;
                self.put_json_in_guards(
                    space,
                    id,
                    bin_doc.to_json_value(),
                    *if_version,
                    None,
                    guards,
                    now,
                )
            }
            BatchOperation::JsonPutHlc {
                space,
                id,
                value,
                hlc,
                ..
            } => {
                let final_hlc = hlc.clone().unwrap_or_else(|| hlc_clock.now());
                self.put_json_in_guards(
                    space,
                    id,
                    value.clone(),
                    None,
                    Some(final_hlc),
                    guards,
                    now,
                )
            }
            BatchOperation::JsonDelete {
                space,
                id,
                if_version,
            } => {
                let key = id.as_bytes();
                let s_idx = ShardedMemTable::<serde_json::Value>::shard_index(space, key);
                let bucket = guards.json_bucket_mut(s_idx);
                let lookup = (space.clone(), key.to_vec());
                let active_ver = bucket
                    .get(&lookup)
                    .filter(|e| e.expires_at_ms.is_none_or(|x| x > now))
                    .map(|e| e.version);
                assert_mem_version(active_ver, *if_version)?;
                let deleted = if let Some(old) = bucket.remove(&lookup) {
                    self.json.total_entries.fetch_sub(1, Ordering::Relaxed);
                    self.json
                        .estimated_bytes
                        .fetch_sub(old.payload_bytes, Ordering::Relaxed);
                    self.json
                        .space_counter(space)
                        .fetch_sub(1, Ordering::Relaxed);
                    old.expires_at_ms.is_none_or(|x| x > now)
                } else {
                    false
                };
                Ok(mem_result(None, deleted))
            }
            _ => self.apply_op(op, codec, hlc_clock),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn put_json_in_guards(
        &self,
        space: &str,
        id: &str,
        value: serde_json::Value,
        if_version: Option<u64>,
        hlc: Option<Hlc>,
        guards: &mut MemShardGuards<'_>,
        now: i64,
    ) -> Result<MutationResult> {
        let key = id.as_bytes();
        let spec = self.catalog.spec_for(CollectionKind::Json, space);
        let exp = spec
            .ttl_ms
            .map(|t| now.saturating_add(i64::try_from(t).unwrap_or(i64::MAX)));
        let s_idx = ShardedMemTable::<serde_json::Value>::shard_index(space, key);
        let bucket = guards.json_bucket_mut(s_idx);
        let seq = self.json.next_seq();
        let payload_len = serde_json::to_vec(&value).map_or(64, |v| v.len());
        let new_bytes = space.len() + key.len() + payload_len;
        let lookup = (space.to_owned(), key.to_vec());
        let active_ver = bucket
            .get(&lookup)
            .filter(|e| e.expires_at_ms.is_none_or(|x| x > now))
            .map(|e| e.version);
        assert_mem_version(active_ver, if_version)?;
        let next_ver = active_ver.unwrap_or(0) + 1;
        let old = bucket.insert(
            lookup,
            MemEntry {
                value,
                version: next_ver,
                expires_at_ms: exp,
                inserted_seq: seq,
                last_access_seq: seq,
                payload_bytes: new_bytes,
            },
        );
        if let Some(o) = old {
            self.json
                .estimated_bytes
                .fetch_sub(o.payload_bytes, Ordering::Relaxed);
            self.json
                .estimated_bytes
                .fetch_add(new_bytes, Ordering::Relaxed);
        } else {
            self.json.total_entries.fetch_add(1, Ordering::Relaxed);
            self.json
                .estimated_bytes
                .fetch_add(new_bytes, Ordering::Relaxed);
            self.json
                .space_counter(space)
                .fetch_add(1, Ordering::Relaxed);
        }
        if !matches!(spec.eviction, EvictionPolicy::None) {
            guards.evict_json.push((space.to_owned(), spec.eviction));
        }
        Ok(MutationResult {
            version: Some(next_ver),
            deleted: false,
            conflict_detected: hlc.as_ref().map(|_| false),
            hlc,
            tier: StorageTier::Memory,
            buffered: false,
        })
    }

    pub(crate) fn finish_guards(&self, guards: MemShardGuards<'_>, now: i64) {
        let MemShardGuards {
            kv,
            json,
            evict_kv,
            evict_json,
        } = guards;
        drop((kv, json));
        for (ns, ev) in evict_kv {
            self.kv.enforce_eviction(&ns, ev, now);
        }
        for (sp, ev) in evict_json {
            self.json.enforce_eviction(&sp, ev, now);
        }
    }
}

pub(crate) struct MemShardGuards<'a> {
    kv: Vec<(usize, RwLockWriteGuard<'a, ShardBucket<Vec<u8>>>)>,
    json: Vec<(usize, RwLockWriteGuard<'a, ShardBucket<serde_json::Value>>)>,
    evict_kv: Vec<(String, EvictionPolicy)>,
    evict_json: Vec<(String, EvictionPolicy)>,
}

impl MemShardGuards<'_> {
    fn kv_bucket(&self, idx: usize) -> &ShardBucket<Vec<u8>> {
        &self.kv[self
            .kv
            .binary_search_by_key(&idx, |(i, _)| *i)
            .expect("locked kv shard")]
        .1
    }

    fn kv_bucket_mut(&mut self, idx: usize) -> &mut ShardBucket<Vec<u8>> {
        let pos = self
            .kv
            .binary_search_by_key(&idx, |(i, _)| *i)
            .expect("locked kv shard");
        &mut self.kv[pos].1
    }

    fn json_bucket(&self, idx: usize) -> &ShardBucket<serde_json::Value> {
        &self.json[self
            .json
            .binary_search_by_key(&idx, |(i, _)| *i)
            .expect("locked json shard")]
        .1
    }

    fn json_bucket_mut(&mut self, idx: usize) -> &mut ShardBucket<serde_json::Value> {
        let pos = self
            .json
            .binary_search_by_key(&idx, |(i, _)| *i)
            .expect("locked json shard");
        &mut self.json[pos].1
    }
}

#[inline]
pub(crate) fn mem_result(version: Option<u64>, deleted: bool) -> MutationResult {
    MutationResult {
        version,
        deleted,
        hlc: None,
        conflict_detected: None,
        tier: StorageTier::Memory,
        buffered: false,
    }
}

pub(crate) fn encode_edge_key(source: &str, target: &str, kind: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(source.len() + target.len() + kind.len() + 2);
    out.extend_from_slice(source.as_bytes());
    out.push(0);
    out.extend_from_slice(target.as_bytes());
    out.push(0);
    out.extend_from_slice(kind.as_bytes());
    out
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use vtab_tables::*;

#[cfg(not(target_arch = "wasm32"))]
mod vtab_tables {
    use super::{
        now_ms, Arc, CollectionCatalog, ConstraintOp, IndexConstraint, IndexPlan, MemEngine,
        Result, VTabValue, ValueCodec, VirtualCursor, VirtualTableProvider,
    };
    use lilia_core::{compress_timeseries, CollectionSpec};

    type RowFetcher<R> = Arc<dyn Fn(Option<&str>, Option<&[u8]>) -> Vec<R> + Send + Sync>;

    /// Generic `VirtualTableProvider` backed by an in-memory closure and column-0/column-1 filter pushdown.
    pub(crate) struct SliceVTab<R: Send + Sync + 'static> {
        ddl: &'static str,
        fetcher: RowFetcher<R>,
        col_mapper: fn(&R, i32) -> VTabValue,
        has_key_pushdown: bool,
    }

    impl<R: Send + Sync + 'static> SliceVTab<R> {
        #[allow(clippy::new_ret_no_self)]
        pub fn new(
            ddl: &'static str,
            has_key_pushdown: bool,
            fetcher: impl Fn(Option<&str>, Option<&[u8]>) -> Vec<R> + Send + Sync + 'static,
            col_mapper: fn(&R, i32) -> VTabValue,
        ) -> Arc<dyn VirtualTableProvider> {
            Arc::new(Self {
                ddl,
                fetcher: Arc::new(fetcher),
                col_mapper,
                has_key_pushdown,
            })
        }
    }

    struct SliceCursor<R: Send + Sync + 'static> {
        fetcher: RowFetcher<R>,
        col_mapper: fn(&R, i32) -> VTabValue,
        rows: Vec<R>,
        pos: usize,
    }

    impl<R: Send + Sync + 'static> VirtualCursor for SliceCursor<R> {
        fn filter(
            &mut self,
            idx_num: i32,
            _idx_str: Option<&str>,
            args: &[VTabValue],
        ) -> Result<()> {
            let col0 = if (idx_num & 1) != 0 {
                match args.first() {
                    Some(VTabValue::Text(s)) => Some(s.as_str()),
                    _ => None,
                }
            } else {
                None
            };
            let col1_owned = if (idx_num & 2) != 0 {
                let arg_idx = usize::from((idx_num & 1) != 0);
                match args.get(arg_idx) {
                    Some(VTabValue::Blob(b)) => Some(b.clone()),
                    Some(VTabValue::Text(s)) => Some(s.as_bytes().to_vec()),
                    _ => None,
                }
            } else {
                None
            };
            self.rows = (self.fetcher)(col0, col1_owned.as_deref());
            self.pos = 0;
            Ok(())
        }

        fn next(&mut self) -> Result<()> {
            self.pos += 1;
            Ok(())
        }

        fn eof(&self) -> bool {
            self.pos >= self.rows.len()
        }

        fn column(&self, col: i32) -> Result<VTabValue> {
            Ok((self.col_mapper)(&self.rows[self.pos], col))
        }

        fn rowid(&self) -> i64 {
            i64::try_from(self.pos).unwrap_or(0)
        }
    }

    impl<R: Send + Sync + 'static> VirtualTableProvider for SliceVTab<R> {
        fn schema_ddl(&self) -> &str {
            self.ddl
        }

        fn best_index(&self, constraints: &[IndexConstraint]) -> IndexPlan {
            let mut idx_num = 0;
            let mut usage = Vec::new();
            let mut next_arg = 1;
            let mut col0_idx = None;
            let mut col1_idx = None;

            for (i, c) in constraints.iter().enumerate() {
                if c.usable && c.op == ConstraintOp::Eq {
                    if c.column == 0 && col0_idx.is_none() {
                        col0_idx = Some(i);
                    } else if self.has_key_pushdown && c.column == 1 && col1_idx.is_none() {
                        col1_idx = Some(i);
                    }
                }
            }

            if let Some(i) = col0_idx {
                idx_num |= 1;
                usage.push((i, next_arg, false));
                next_arg += 1;
            }
            if let Some(i) = col1_idx {
                idx_num |= 2;
                usage.push((i, next_arg, false));
            }

            let (estimated_cost, estimated_rows, unique_scan) = match idx_num {
                3 => (1.0, 1, true),
                1 => (20.0, 50, false),
                _ => (1000.0, 1000, false),
            };

            IndexPlan {
                idx_num,
                idx_str: None,
                constraint_usage: usage,
                estimated_cost,
                estimated_rows,
                unique_scan,
            }
        }

        fn open_cursor(&self) -> Result<Box<dyn VirtualCursor>> {
            Ok(Box::new(SliceCursor {
                fetcher: Arc::clone(&self.fetcher),
                col_mapper: self.col_mapper,
                rows: Vec::new(),
                pos: 0,
            }))
        }
    }

    pub(crate) fn build_mem_kv_vtab(
        mem: &Arc<MemEngine>,
        codec: &Arc<dyn ValueCodec>,
    ) -> Arc<dyn VirtualTableProvider> {
        let mem = Arc::clone(mem);
        let codec = Arc::clone(codec);
        SliceVTab::new(
        "CREATE TABLE x(namespace TEXT, key BLOB, value BLOB, version INTEGER, expires_at_ms INTEGER)",
        true,
        move |ns, key| {
            let now = now_ms();
            if let (Some(n), Some(k)) = (ns, key) {
                return mem
                    .kv
                    .get(n, k, now, false)
                    .into_iter()
                    .map(|snap| {
                        let decoded = codec.decode(&snap.value).unwrap_or(snap.value);
                        (n.to_owned(), k.to_vec(), decoded, snap.version, snap.expires_at_ms)
                    })
                    .collect();
            }
            mem.kv
                .scan(ns, None, usize::MAX, now)
                .into_iter()
                .map(|(n, k, snap)| {
                    let decoded = codec.decode(&snap.value).unwrap_or(snap.value);
                    (n, k, decoded, snap.version, snap.expires_at_ms)
                })
                .collect()
        },
        |r, col| match col {
            0 => VTabValue::Text(r.0.clone()),
            1 => VTabValue::Blob(r.1.clone()),
            2 => VTabValue::Blob(r.2.clone()),
            3 => VTabValue::Int(i64::try_from(r.3).unwrap_or(i64::MAX)),
            4 => r.4.map_or(VTabValue::Null, VTabValue::Int),
            _ => VTabValue::Null,
        },
    )
    }

    pub(crate) fn build_mem_json_vtab(mem: &Arc<MemEngine>) -> Arc<dyn VirtualTableProvider> {
        let mem = Arc::clone(mem);
        SliceVTab::new(
            "CREATE TABLE x(space TEXT, id TEXT, value TEXT, version INTEGER)",
            true,
            move |space, id| {
                let now = now_ms();
                if let (Some(s), Some(i)) = (space, id) {
                    return mem
                        .json
                        .get(s, i, now, false)
                        .into_iter()
                        .map(|snap| {
                            (
                                s.to_owned(),
                                String::from_utf8_lossy(i).into_owned(),
                                serde_json::to_string(&snap.value)
                                    .unwrap_or_else(|_| "null".into()),
                                snap.version,
                            )
                        })
                        .collect();
                }
                mem.json
                    .scan(space, None, usize::MAX, now)
                    .into_iter()
                    .map(|(s, i, snap)| {
                        (
                            s,
                            String::from_utf8_lossy(&i).into_owned(),
                            serde_json::to_string(&snap.value).unwrap_or_else(|_| "null".into()),
                            snap.version,
                        )
                    })
                    .collect()
            },
            |r, col| match col {
                0 => VTabValue::Text(r.0.clone()),
                1 => VTabValue::Text(r.1.clone()),
                2 => VTabValue::Text(r.2.clone()),
                3 => VTabValue::Int(i64::try_from(r.3).unwrap_or(i64::MAX)),
                _ => VTabValue::Null,
            },
        )
    }

    #[derive(Clone)]
    pub(crate) struct TsVTabRow {
        series_id: String,
        resolution_ms: i64,
        start_time: i64,
        end_time: i64,
        point_count: i64,
        min_val: f64,
        max_val: f64,
        sum_val: f64,
        avg_val: f64,
        compressed_bytes: i64,
    }

    pub(crate) fn build_mem_ts_vtab(mem: &Arc<MemEngine>) -> Arc<dyn VirtualTableProvider> {
        let mem = Arc::clone(mem);
        SliceVTab::new(
        "CREATE TABLE x(series_id TEXT, resolution_ms INTEGER, start_time INTEGER, end_time INTEGER, point_count INTEGER, min_val REAL, max_val REAL, sum_val REAL, avg_val REAL, compressed_bytes INTEGER)",
        false,
        move |series_id, _| {
            let items = mem.timeseries.scan(series_id, None, usize::MAX, now_ms());
            let mut rows = Vec::new();
            for (sid, key, snap) in items {
                if snap.value.is_empty() || key.len() != 8 {
                    continue;
                }
                let res_ms = i64::from_be_bytes(key.try_into().unwrap_or([0; 8]));
                let pts = &snap.value;
                let start_time = pts[0].timestamp;
                let end_time = pts.last().map_or(start_time, |p| p.timestamp);
                let mut min_val = pts[0].value;
                let mut max_val = pts[0].value;
                let mut sum_val = 0.0;
                for p in pts {
                    if p.value < min_val {
                        min_val = p.value;
                    }
                    if p.value > max_val {
                        max_val = p.value;
                    }
                    sum_val += p.value;
                }
                #[allow(clippy::cast_precision_loss)]
                let avg_val = sum_val / (pts.len() as f64);
                let compressed_bytes = compress_timeseries(pts)
                    .map_or(0, |c| i64::try_from(c.len()).unwrap_or(0));
                rows.push(TsVTabRow {
                    series_id: sid,
                    resolution_ms: res_ms,
                    start_time,
                    end_time,
                    point_count: i64::try_from(pts.len()).unwrap_or(0),
                    min_val,
                    max_val,
                    sum_val,
                    avg_val,
                    compressed_bytes,
                });
            }
            rows
        },
        |r, col| match col {
            0 => VTabValue::Text(r.series_id.clone()),
            1 => VTabValue::Int(r.resolution_ms),
            2 => VTabValue::Int(r.start_time),
            3 => VTabValue::Int(r.end_time),
            4 => VTabValue::Int(r.point_count),
            5 => VTabValue::Float(r.min_val),
            6 => VTabValue::Float(r.max_val),
            7 => VTabValue::Float(r.sum_val),
            8 => VTabValue::Float(r.avg_val),
            9 => VTabValue::Int(r.compressed_bytes),
            _ => VTabValue::Null,
        },
    )
    }

    pub(crate) fn build_mem_graph_nodes_vtab(
        mem: &Arc<MemEngine>,
    ) -> Arc<dyn VirtualTableProvider> {
        let mem = Arc::clone(mem);
        SliceVTab::new(
        "CREATE TABLE x(graph TEXT, id TEXT, kind TEXT, metadata TEXT, doc_space TEXT, doc_id TEXT)",
        true,
        move |graph, id| {
            let now = now_ms();
            if let (Some(g), Some(i)) = (graph, id) {
                return mem
                    .graph_nodes
                    .get(g, i, now, false)
                    .into_iter()
                    .map(|s| s.value)
                    .collect();
            }
            mem.graph_nodes
                .scan(graph, None, usize::MAX, now)
                .into_iter()
                .map(|(_, _, s)| s.value)
                .collect()
        },
        |n, col| match col {
            0 => VTabValue::Text(n.graph.clone()),
            1 => VTabValue::Text(n.id.clone()),
            2 => VTabValue::Text(n.kind.clone()),
            3 => VTabValue::Text(serde_json::to_string(&n.metadata).unwrap_or_else(|_| "{}".into())),
            4 => n.doc_space.clone().map_or(VTabValue::Null, VTabValue::Text),
            5 => n.doc_id.clone().map_or(VTabValue::Null, VTabValue::Text),
            _ => VTabValue::Null,
        },
    )
    }

    pub(crate) fn build_mem_graph_edges_vtab(
        mem: &Arc<MemEngine>,
    ) -> Arc<dyn VirtualTableProvider> {
        let mem = Arc::clone(mem);
        SliceVTab::new(
            "CREATE TABLE x(graph TEXT, source TEXT, target TEXT, kind TEXT, weight REAL)",
            false,
            move |graph, _| {
                mem.graph_edges
                    .scan(graph, None, usize::MAX, now_ms())
                    .into_iter()
                    .map(|(_, _, s)| s.value)
                    .collect()
            },
            |e, col| match col {
                0 => VTabValue::Text(e.graph.clone()),
                1 => VTabValue::Text(e.source.clone()),
                2 => VTabValue::Text(e.target.clone()),
                3 => VTabValue::Text(e.kind.clone()),
                4 => VTabValue::Float(e.weight),
                _ => VTabValue::Null,
            },
        )
    }

    pub(crate) fn build_mem_vectors_vtab(mem: &Arc<MemEngine>) -> Arc<dyn VirtualTableProvider> {
        let mem = Arc::clone(mem);
        SliceVTab::new(
        "CREATE TABLE x(collection TEXT, id TEXT, dim INTEGER, element_type INTEGER, vector BLOB)",
        true,
        move |coll, id| {
            let now = now_ms();
            if let (Some(c), Some(i)) = (coll, id) {
                return mem
                    .vectors
                    .get(c, i, now, false)
                    .into_iter()
                    .map(|s| {
                        (
                            c.to_owned(),
                            String::from_utf8_lossy(i).into_owned(),
                            s.value.0,
                            s.value.1,
                            s.value.2,
                        )
                    })
                    .collect();
            }
            mem.vectors
                .scan(coll, None, usize::MAX, now)
                .into_iter()
                .map(|(c, i, s)| {
                    (
                        c,
                        String::from_utf8_lossy(&i).into_owned(),
                        s.value.0,
                        s.value.1,
                        s.value.2,
                    )
                })
                .collect()
        },
        |r, col| match col {
            0 => VTabValue::Text(r.0.clone()),
            1 => VTabValue::Text(r.1.clone()),
            2 => VTabValue::Int(i64::from(r.2)),
            3 => VTabValue::Int(i64::from(r.3 as u8)),
            4 => VTabValue::Blob(r.4.clone()),
            _ => VTabValue::Null,
        },
    )
    }

    pub(crate) fn build_mem_vector_indexes_vtab(
        mem: &Arc<MemEngine>,
    ) -> Arc<dyn VirtualTableProvider> {
        let mem = Arc::clone(mem);
        SliceVTab::new(
        "CREATE TABLE x(collection TEXT, entry_id TEXT, max_level INTEGER, m INTEGER, ef_construction INTEGER, ef_search INTEGER, metric TEXT, element_type INTEGER, dim INTEGER)",
        false,
        move |coll, _| {
            let guard = mem
                .hnsw_meta
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut rows = Vec::new();
            for (c, cfg) in &*guard {
                if coll.is_some_and(|target| target != c) {
                    continue;
                }
                rows.push((c.clone(), cfg.clone()));
            }
            rows.sort_by(|a, b| a.0.cmp(&b.0));
            rows
        },
        |(c, cfg), col| match col {
            0 => VTabValue::Text(c.clone()),
            3 => VTabValue::Int(i64::try_from(cfg.m).unwrap_or(0)),
            4 => VTabValue::Int(i64::try_from(cfg.ef_construction).unwrap_or(0)),
            5 => VTabValue::Int(i64::try_from(cfg.ef_search).unwrap_or(0)),
            6 => VTabValue::Text(cfg.metric.as_str().to_owned()),
            7 => VTabValue::Int(i64::from(cfg.element_type as u8)),
            8 => VTabValue::Int(i64::from(cfg.dim)),
            _ => VTabValue::Null,
        },
    )
    }

    pub(crate) fn build_mem_crdt_counters_vtab(
        mem: &Arc<MemEngine>,
    ) -> Arc<dyn VirtualTableProvider> {
        let mem = Arc::clone(mem);
        SliceVTab::new(
            "CREATE TABLE x(space TEXT, id TEXT, value INTEGER)",
            true,
            move |space, id| {
                mem.crdt_counters
                    .scan(space, None, usize::MAX, now_ms())
                    .into_iter()
                    .filter(|(_, k, _)| id.is_none_or(|target| k.as_slice() == target))
                    .map(|(s, i, snap)| {
                        (
                            s,
                            String::from_utf8_lossy(&i).into_owned(),
                            snap.value.value(),
                        )
                    })
                    .collect()
            },
            |r, col| match col {
                0 => VTabValue::Text(r.0.clone()),
                1 => VTabValue::Text(r.1.clone()),
                2 => VTabValue::Int(r.2),
                _ => VTabValue::Null,
            },
        )
    }

    pub(crate) fn build_mem_crdt_orsets_vtab(
        mem: &Arc<MemEngine>,
    ) -> Arc<dyn VirtualTableProvider> {
        let mem = Arc::clone(mem);
        SliceVTab::new(
            "CREATE TABLE x(space TEXT, id TEXT, element TEXT)",
            true,
            move |space, id| {
                let mut out = Vec::new();
                for (s, i_bytes, snap) in mem.crdt_orsets.scan(space, None, usize::MAX, now_ms()) {
                    if id.is_some_and(|target| i_bytes.as_slice() != target) {
                        continue;
                    }
                    let id_str = String::from_utf8_lossy(&i_bytes).into_owned();
                    for (elem, tags) in snap.value.entries {
                        if tags.iter().any(|t| !snap.value.tombstones.contains_key(t)) {
                            out.push((s.clone(), id_str.clone(), elem));
                        }
                    }
                }
                out
            },
            |r, col| match col {
                0 => VTabValue::Text(r.0.clone()),
                1 => VTabValue::Text(r.1.clone()),
                2 => VTabValue::Text(r.2.clone()),
                _ => VTabValue::Null,
            },
        )
    }

    pub(crate) fn build_mem_crdt_json_vtab(mem: &Arc<MemEngine>) -> Arc<dyn VirtualTableProvider> {
        let mem = Arc::clone(mem);
        SliceVTab::new(
            "CREATE TABLE x(space TEXT, id TEXT, value TEXT, version INTEGER)",
            true,
            move |space, id| {
                mem.crdt_json
                    .scan(space, None, usize::MAX, now_ms())
                    .into_iter()
                    .filter(|(_, k, _)| id.is_none_or(|target| k.as_slice() == target))
                    .map(|(s, i, snap)| {
                        (
                            s,
                            String::from_utf8_lossy(&i).into_owned(),
                            serde_json::to_string(&snap.value.1).unwrap_or_else(|_| "{}".into()),
                            snap.version,
                        )
                    })
                    .collect()
            },
            |r, col| match col {
                0 => VTabValue::Text(r.0.clone()),
                1 => VTabValue::Text(r.1.clone()),
                2 => VTabValue::Text(r.2.clone()),
                3 => VTabValue::Int(i64::try_from(r.3).unwrap_or(i64::MAX)),
                _ => VTabValue::Null,
            },
        )
    }

    pub(crate) fn build_catalog_vtab(
        catalog: &Arc<CollectionCatalog>,
    ) -> Arc<dyn VirtualTableProvider> {
        let catalog = Arc::clone(catalog);
        SliceVTab::new(
            "CREATE TABLE x(kind TEXT, name TEXT, tier TEXT, ttl_ms INTEGER, eviction TEXT)",
            true,
            move |kind_filter, name_filter| {
                catalog
                    .list_specs()
                    .into_iter()
                    .filter(|s| {
                        kind_filter.is_none_or(|k| s.kind.as_str() == k)
                            && name_filter.is_none_or(|n| s.name.as_bytes() == n)
                    })
                    .collect::<Vec<CollectionSpec>>()
            },
            |s, col| match col {
                0 => VTabValue::Text(s.kind.as_str().to_owned()),
                1 => VTabValue::Text(s.name.clone()),
                2 => VTabValue::Text(s.tier.as_str().to_owned()),
                3 => s.ttl_ms.map_or(VTabValue::Null, |t| {
                    VTabValue::Int(i64::try_from(t).unwrap_or(i64::MAX))
                }),
                4 => VTabValue::Text(serde_json::to_string(&s.eviction).unwrap_or_default()),
                _ => VTabValue::Null,
            },
        )
    }
}
