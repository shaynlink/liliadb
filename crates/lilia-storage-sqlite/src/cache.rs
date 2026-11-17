use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use lilia_core::{AccessTarget, BitemporalEntry, GraphNode, JsonEntry, KvEntry};

const NUM_SHARDS: usize = 16;

fn get_shard_index<H: Hash>(key: &H) -> usize {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    (hasher.finish() as usize) % NUM_SHARDS
}

struct LruShard<K, V> {
    entries: HashMap<K, V>,
    order: VecDeque<K>,
    capacity: usize,
}

impl<K: Clone + Eq + Hash, V> LruShard<K, V> {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            capacity: capacity.max(1),
        }
    }

    fn get(&mut self, key: &K) -> Option<&V> {
        if self.entries.contains_key(key) {
            if let Some(pos) = self.order.iter().position(|k| k == key) {
                self.order.remove(pos);
            }
            self.order.push_back(key.clone());
            self.entries.get(key)
        } else {
            None
        }
    }

    fn put(&mut self, key: K, value: V) -> Option<(K, V)> {
        let mut evicted = None;
        if self.entries.contains_key(&key) {
            if let Some(pos) = self.order.iter().position(|k| *k == key) {
                self.order.remove(pos);
            }
        } else if self.entries.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                if let Some(val) = self.entries.remove(&oldest) {
                    evicted = Some((oldest, val));
                }
            }
        }
        self.order.push_back(key.clone());
        self.entries.insert(key, value);
        evicted
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        self.entries.remove(key)
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Sharded high-concurrency LRU cache.
struct ShardedLru<K, V> {
    shards: Vec<RwLock<LruShard<K, V>>>,
}

impl<K: Clone + Eq + Hash, V: Clone> ShardedLru<K, V> {
    fn new(total_capacity: usize) -> Self {
        let per_shard = (total_capacity / NUM_SHARDS).max(1);
        let mut shards = Vec::with_capacity(NUM_SHARDS);
        for _ in 0..NUM_SHARDS {
            shards.push(RwLock::new(LruShard::new(per_shard)));
        }
        Self { shards }
    }

    fn get(&self, key: &K) -> Option<V> {
        let idx = get_shard_index(key);
        let mut shard = self.shards[idx].write().ok()?;
        shard.get(key).cloned()
    }

    fn put(&self, key: K, value: V) -> Option<(K, V)> {
        let idx = get_shard_index(&key);
        let mut shard = self.shards[idx].write().ok()?;
        shard.put(key, value)
    }

    fn remove(&self, key: &K) -> Option<V> {
        let idx = get_shard_index(key);
        let mut shard = self.shards[idx].write().ok()?;
        shard.remove(key)
    }

    fn clear(&self) {
        for shard in &self.shards {
            if let Ok(mut s) = shard.write() {
                s.clear();
            }
        }
    }

    fn total_len(&self) -> usize {
        self.shards
            .iter()
            .map(|s| s.read().map_or(0, |guard| guard.len()))
            .sum()
    }
}

/// Raw payload stored in L2 cache before or after L1 eviction.
#[derive(Debug, Clone)]
pub struct L2Payload {
    pub bytes: Arc<[u8]>,
    pub version: u64,
}

/// Aggregate performance telemetry for L1/L2 cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CacheStats {
    pub l1_hits: u64,
    pub l2_hits: u64,
    pub misses: u64,
    pub l1_entries: usize,
    pub l2_entries: usize,
    pub prefetches_dispatched: u64,
    pub prefetches_completed: u64,
    pub hit_ratio: f64,
}

/// Unified multi-tier cache manager for L1 (deserialized objects) and L2 (raw payloads).
pub struct CacheManager {
    // L1: Deserialized entity caches
    l1_json: ShardedLru<(String, String), Arc<JsonEntry>>,
    l1_kv: ShardedLru<(String, Vec<u8>), Arc<KvEntry>>,
    l1_graph: ShardedLru<(String, String), Arc<GraphNode>>,
    l1_bitemporal: ShardedLru<(String, String, i64, i64), Arc<BitemporalEntry>>,

    // L2: Raw payload cache keyed by AccessTarget
    l2: ShardedLru<AccessTarget, L2Payload>,

    // Metrics counters
    l1_hits: AtomicU64,
    l2_hits: AtomicU64,
    misses: AtomicU64,
    prefetches_dispatched: AtomicU64,
    prefetches_completed: AtomicU64,
}

impl std::fmt::Debug for CacheManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheManager")
            .field("l1_json_len", &self.l1_json.total_len())
            .field("l1_kv_len", &self.l1_kv.total_len())
            .field("l1_graph_len", &self.l1_graph.total_len())
            .field("l1_bitemporal_len", &self.l1_bitemporal.total_len())
            .field("l2_len", &self.l2.total_len())
            .finish()
    }
}

impl CacheManager {
    #[must_use]
    pub fn new(l1_capacity: usize, l2_capacity: usize) -> Self {
        Self {
            l1_json: ShardedLru::new(l1_capacity),
            l1_kv: ShardedLru::new(l1_capacity),
            l1_graph: ShardedLru::new(l1_capacity),
            l1_bitemporal: ShardedLru::new(l1_capacity),
            l2: ShardedLru::new(l2_capacity),
            l1_hits: AtomicU64::new(0),
            l2_hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            prefetches_dispatched: AtomicU64::new(0),
            prefetches_completed: AtomicU64::new(0),
        }
    }

    // --- JSON operations ---

    #[must_use]
    pub fn get_json(&self, space: &str, id: &str) -> Option<Arc<JsonEntry>> {
        let key = (space.to_string(), id.to_string());
        if let Some(entry) = self.l1_json.get(&key) {
            self.l1_hits.fetch_add(1, Ordering::Relaxed);
            return Some(entry);
        }

        // Check L2
        let target = AccessTarget::json(space, id);
        if self.l2.get(&target).is_some() {
            self.l2_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        None
    }

    pub fn put_json(&self, space: &str, id: &str, entry: Arc<JsonEntry>) {
        let key = (space.to_string(), id.to_string());
        self.l1_json.put(key, entry);
    }

    pub fn invalidate_json(&self, space: &str, id: &str) {
        let key = (space.to_string(), id.to_string());
        self.l1_json.remove(&key);
        self.l2.remove(&AccessTarget::json(space, id));
    }

    // --- KV operations ---

    #[must_use]
    pub fn get_kv(&self, namespace: &str, key_bytes: &[u8]) -> Option<Arc<KvEntry>> {
        let key = (namespace.to_string(), key_bytes.to_vec());
        if let Some(entry) = self.l1_kv.get(&key) {
            self.l1_hits.fetch_add(1, Ordering::Relaxed);
            return Some(entry);
        }

        let target = AccessTarget::kv(namespace, key_bytes);
        if self.l2.get(&target).is_some() {
            self.l2_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        None
    }

    pub fn put_kv(&self, namespace: &str, key_bytes: &[u8], entry: Arc<KvEntry>) {
        let key = (namespace.to_string(), key_bytes.to_vec());
        self.l1_kv.put(key, entry);
    }

    pub fn invalidate_kv(&self, namespace: &str, key_bytes: &[u8]) {
        let key = (namespace.to_string(), key_bytes.to_vec());
        self.l1_kv.remove(&key);
        self.l2.remove(&AccessTarget::kv(namespace, key_bytes));
    }

    // --- Graph operations ---

    #[must_use]
    pub fn get_graph_node(&self, graph: &str, id: &str) -> Option<Arc<GraphNode>> {
        let key = (graph.to_string(), id.to_string());
        if let Some(entry) = self.l1_graph.get(&key) {
            self.l1_hits.fetch_add(1, Ordering::Relaxed);
            return Some(entry);
        }

        let target = AccessTarget::graph_node(graph, id);
        if self.l2.get(&target).is_some() {
            self.l2_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        None
    }

    pub fn put_graph_node(&self, graph: &str, id: &str, entry: Arc<GraphNode>) {
        let key = (graph.to_string(), id.to_string());
        self.l1_graph.put(key, entry);
    }

    pub fn invalidate_graph_node(&self, graph: &str, id: &str) {
        let key = (graph.to_string(), id.to_string());
        self.l1_graph.remove(&key);
        self.l2.remove(&AccessTarget::graph_node(graph, id));
    }

    // --- Bitemporal operations ---

    #[must_use]
    pub fn get_bitemporal(&self, space: &str, id: &str, tt: i64, vt: i64) -> Option<Arc<BitemporalEntry>> {
        let key = (space.to_string(), id.to_string(), tt, vt);
        if let Some(entry) = self.l1_bitemporal.get(&key) {
            self.l1_hits.fetch_add(1, Ordering::Relaxed);
            return Some(entry);
        }

        let target = AccessTarget::bitemporal(space, id);
        if self.l2.get(&target).is_some() {
            self.l2_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        None
    }

    pub fn put_bitemporal(&self, space: &str, id: &str, tt: i64, vt: i64, entry: Arc<BitemporalEntry>) {
        let key = (space.to_string(), id.to_string(), tt, vt);
        self.l1_bitemporal.put(key, entry);
    }

    pub fn invalidate_bitemporal(&self, space: &str, id: &str) {
        for shard in &self.l1_bitemporal.shards {
            if let Ok(mut s) = shard.write() {
                let to_remove: Vec<(String, String, i64, i64)> = s
                    .entries
                    .keys()
                    .filter(|(sp, i, _, _)| sp == space && i == id)
                    .cloned()
                    .collect();
                for k in to_remove {
                    s.remove(&k);
                }
            }
        }
        self.l2.remove(&AccessTarget::bitemporal(space, id));
    }

    // --- L2 generic operations ---

    pub fn put_l2(&self, target: AccessTarget, payload: L2Payload) {
        self.l2.put(target, payload);
    }

    #[must_use]
    pub fn get_l2(&self, target: &AccessTarget) -> Option<L2Payload> {
        self.l2.get(target)
    }

    // --- Prefetch metrics ---

    pub fn mark_prefetch_dispatched(&self, count: usize) {
        self.prefetches_dispatched
            .fetch_add(count as u64, Ordering::Relaxed);
    }

    pub fn mark_prefetch_completed(&self, count: usize) {
        self.prefetches_completed
            .fetch_add(count as u64, Ordering::Relaxed);
    }

    // --- Invalidation & Cleanup ---

    /// Invalidates all entries across all cache levels and resets counters.
    pub fn clear(&self) {
        self.l1_json.clear();
        self.l1_kv.clear();
        self.l1_graph.clear();
        self.l1_bitemporal.clear();
        self.l2.clear();
    }

    /// Returns aggregate telemetry snapshot.
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        let l1_hits = self.l1_hits.load(Ordering::Relaxed);
        let l2_hits = self.l2_hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let total = l1_hits + l2_hits + misses;

        let hit_ratio = if total > 0 {
            (l1_hits + l2_hits) as f64 / total as f64
        } else {
            0.0
        };

        let l1_entries = self.l1_json.total_len()
            + self.l1_kv.total_len()
            + self.l1_graph.total_len()
            + self.l1_bitemporal.total_len();
        let l2_entries = self.l2.total_len();

        CacheStats {
            l1_hits,
            l2_hits,
            misses,
            l1_entries,
            l2_entries,
            prefetches_dispatched: self.prefetches_dispatched.load(Ordering::Relaxed),
            prefetches_completed: self.prefetches_completed.load(Ordering::Relaxed),
            hit_ratio,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_cache_l1_json_roundtrip_and_invalidation() {
        let cache = CacheManager::new(64, 128);

        let entry = Arc::new(JsonEntry {
            space: "users".into(),
            id: "ada".into(),
            value: json!({"name": "Ada Lovelace"}),
            version: 1,
        });

        assert!(cache.get_json("users", "ada").is_none());
        cache.put_json("users", "ada", Arc::clone(&entry));

        let hit = cache.get_json("users", "ada");
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().value["name"], "Ada Lovelace");

        let stats = cache.stats();
        assert_eq!(stats.l1_hits, 1);
        assert_eq!(stats.misses, 1);
        assert!((stats.hit_ratio - 0.5).abs() < 1e-6);

        // Invalidate
        cache.invalidate_json("users", "ada");
        assert!(cache.get_json("users", "ada").is_none());
    }

    #[test]
    fn test_cache_all_models() {
        let cache = CacheManager::new(64, 128);

        // KV
        let kv = Arc::new(KvEntry {
            namespace: "auth".into(),
            key: b"token".to_vec(),
            value: b"xyz".to_vec(),
            version: 1,
            expires_at_ms: None,
        });
        cache.put_kv("auth", b"token", kv);
        assert_eq!(cache.get_kv("auth", b"token").unwrap().value, b"xyz");

        // Graph
        let node = Arc::new(GraphNode {
            graph: "friends".into(),
            id: "user_1".into(),
            kind: "person".into(),
            metadata: json!({"age": 30}),
            doc_space: None,
            doc_id: None,
        });
        cache.put_graph_node("friends", "user_1", node);
        assert_eq!(
            cache.get_graph_node("friends", "user_1").unwrap().kind,
            "person"
        );

        // Bitemporal
        let bitemp = Arc::new(BitemporalEntry {
            space: "deals".into(),
            id: "d1".into(),
            vt_from: 100,
            vt_to: 200,
            tt_from: 100,
            tt_to: 200,
            value: json!({"amount": 500}),
            version: 1,
        });
        cache.put_bitemporal("deals", "d1", 150, 150, bitemp);
        assert_eq!(
            cache.get_bitemporal("deals", "d1", 150, 150).unwrap().value["amount"],
            500
        );

        let stats = cache.stats();
        assert_eq!(stats.l1_entries, 3);
    }
}
