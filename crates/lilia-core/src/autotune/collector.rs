use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::fingerprint::QueryFingerprint;
use super::stats::{QueryObservation, QueryStats, SqlObservation, SqlQueryStats};

const SHARDS: usize = 16;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug)]
struct Tracked<T> {
    value: T,
    dirty: bool,
}

/// Lookups served by one index since the collector started (plus any seeded history).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexLookups {
    pub lookups: u64,
    pub last_used_ms: i64,
}

type IndexKey = (String, String);

/// Thread-safe, bounded in-memory workload statistics.
///
/// Query shapes are spread over 16 independently locked shards so that concurrent readers do not
/// contend on one mutex. Entries carry a dirty flag so the storage engine can persist only what
/// changed. When a shard is full, the least recently seen shape is evicted.
#[derive(Debug)]
pub struct AutotuneCollector {
    shard_capacity: usize,
    sql_capacity: usize,
    slow_threshold_us: u64,
    queries: Vec<Mutex<HashMap<QueryFingerprint, Tracked<QueryStats>>>>,
    lookups: Mutex<HashMap<IndexKey, Tracked<IndexLookups>>>,
    sql: Mutex<HashMap<String, SqlQueryStats>>,
}

impl AutotuneCollector {
    #[must_use]
    pub fn new(max_tracked_queries: usize, slow_threshold_us: u64) -> Self {
        let capacity = max_tracked_queries.max(1);
        Self {
            shard_capacity: capacity.div_ceil(SHARDS),
            sql_capacity: capacity,
            slow_threshold_us,
            queries: (0..SHARDS).map(|_| Mutex::new(HashMap::new())).collect(),
            lookups: Mutex::new(HashMap::new()),
            sql: Mutex::new(HashMap::new()),
        }
    }

    fn shard(
        &self,
        fingerprint: &QueryFingerprint,
    ) -> &Mutex<HashMap<QueryFingerprint, Tracked<QueryStats>>> {
        let mut hasher = DefaultHasher::new();
        fingerprint.hash(&mut hasher);
        let index = usize::try_from(hasher.finish() % SHARDS as u64).unwrap_or(0);
        &self.queries[index]
    }

    /// Records one `find` execution.
    pub fn record_query(&self, fingerprint: QueryFingerprint, observation: &QueryObservation) {
        let mut shard = lock(self.shard(&fingerprint));
        if !shard.contains_key(&fingerprint) && shard.len() >= self.shard_capacity {
            evict_oldest(&mut shard, |tracked| tracked.value.last_seen_ms);
        }
        let tracked = shard.entry(fingerprint).or_insert_with(|| Tracked {
            value: QueryStats::default(),
            dirty: false,
        });
        tracked.value.record(observation, self.slow_threshold_us);
        tracked.dirty = true;
    }

    /// Restores persisted statistics for a query shape.
    pub fn seed_query(&self, fingerprint: QueryFingerprint, stats: QueryStats) {
        let mut shard = lock(self.shard(&fingerprint));
        if shard.len() < self.shard_capacity || shard.contains_key(&fingerprint) {
            shard.insert(
                fingerprint,
                Tracked {
                    value: stats,
                    dirty: false,
                },
            );
        }
    }

    /// Forgets the scan evidence of a shape, so that dropping its index later does not
    /// immediately trigger a rebuild from stale observations.
    pub fn clear_scan_evidence(&self, fingerprint: &QueryFingerprint) {
        if let Some(tracked) = lock(self.shard(fingerprint)).get_mut(fingerprint) {
            tracked.value.clear_scan_evidence();
            tracked.dirty = true;
        }
    }

    /// Records one lookup served by `index` of `space`.
    pub fn record_index_lookup(&self, space: &str, index: &str, at_ms: i64) {
        let mut lookups = lock(&self.lookups);
        let tracked = lookups
            .entry((space.to_owned(), index.to_owned()))
            .or_insert_with(|| Tracked {
                value: IndexLookups::default(),
                dirty: false,
            });
        tracked.value.lookups = tracked.value.lookups.saturating_add(1);
        tracked.value.last_used_ms = tracked.value.last_used_ms.max(at_ms);
        tracked.dirty = true;
    }

    /// Restores persisted lookup counters for an index.
    pub fn seed_index_lookups(&self, space: &str, index: &str, value: IndexLookups) {
        lock(&self.lookups).insert(
            (space.to_owned(), index.to_owned()),
            Tracked {
                value,
                dirty: false,
            },
        );
    }

    /// Drops the lookup counters of an index that no longer exists.
    pub fn forget_index(&self, space: &str, index: &str) {
        lock(&self.lookups).remove(&(space.to_owned(), index.to_owned()));
    }

    /// Records one SQL statement execution; `sql` must already be normalized.
    pub fn record_sql(&self, sql: String, observation: &SqlObservation) {
        let mut statements = lock(&self.sql);
        if !statements.contains_key(&sql) && statements.len() >= self.sql_capacity {
            evict_oldest(&mut statements, |stats| stats.last_seen_ms);
        }
        statements
            .entry(sql)
            .or_default()
            .record(observation, self.slow_threshold_us);
    }

    #[must_use]
    pub fn query_snapshot(&self) -> Vec<(QueryFingerprint, QueryStats)> {
        let mut all = Vec::new();
        for shard in &self.queries {
            all.extend(
                lock(shard)
                    .iter()
                    .map(|(fingerprint, tracked)| (fingerprint.clone(), tracked.value.clone())),
            );
        }
        all.sort_by(|a, b| a.0.cmp(&b.0));
        all
    }

    #[must_use]
    pub fn index_lookup_snapshot(&self) -> HashMap<(String, String), IndexLookups> {
        lock(&self.lookups)
            .iter()
            .map(|(key, tracked)| (key.clone(), tracked.value))
            .collect()
    }

    #[must_use]
    pub fn sql_snapshot(&self) -> Vec<(String, SqlQueryStats)> {
        let mut all: Vec<_> = lock(&self.sql)
            .iter()
            .map(|(sql, stats)| (sql.clone(), stats.clone()))
            .collect();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        all
    }

    /// Returns the query statistics changed since the previous drain and marks them clean.
    #[must_use]
    pub fn drain_dirty_queries(&self) -> Vec<(QueryFingerprint, QueryStats)> {
        let mut dirty = Vec::new();
        for shard in &self.queries {
            for (fingerprint, tracked) in lock(shard).iter_mut() {
                if tracked.dirty {
                    tracked.dirty = false;
                    dirty.push((fingerprint.clone(), tracked.value.clone()));
                }
            }
        }
        dirty
    }

    /// Returns the index lookup counters changed since the previous drain and marks them clean.
    #[must_use]
    pub fn drain_dirty_index_lookups(&self) -> Vec<(String, String, IndexLookups)> {
        lock(&self.lookups)
            .iter_mut()
            .filter(|(_, tracked)| tracked.dirty)
            .map(|((space, index), tracked)| {
                tracked.dirty = false;
                (space.clone(), index.clone(), tracked.value)
            })
            .collect()
    }

    /// Marks every entry dirty again, e.g. after a failed persistence attempt.
    pub fn mark_all_dirty(&self) {
        for shard in &self.queries {
            for tracked in lock(shard).values_mut() {
                tracked.dirty = true;
            }
        }
        for tracked in lock(&self.lookups).values_mut() {
            tracked.dirty = true;
        }
    }

    /// Discards every recorded statistic.
    pub fn reset(&self) {
        for shard in &self.queries {
            lock(shard).clear();
        }
        lock(&self.lookups).clear();
        lock(&self.sql).clear();
    }
}

fn evict_oldest<K: Clone + Eq + Hash, V>(map: &mut HashMap<K, V>, last_seen: impl Fn(&V) -> i64) {
    let oldest = map
        .iter()
        .min_by_key(|(_, value)| last_seen(value))
        .map(|(key, _)| key.clone());
    if let Some(key) = oldest {
        map.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(rows: u64, at_ms: i64) -> QueryObservation {
        QueryObservation {
            used_index: false,
            rows_examined: rows,
            rows_returned: 1,
            duration_us: 100,
            at_ms,
        }
    }

    fn fingerprint(field: &str) -> QueryFingerprint {
        QueryFingerprint::new("orders", [field])
    }

    #[test]
    fn records_and_drains_only_dirty_entries() {
        let collector = AutotuneCollector::new(64, 5_000);
        collector.record_query(fingerprint("status"), &scan(10, 1));
        collector.record_query(fingerprint("status"), &scan(10, 2));
        collector.record_query(fingerprint("store_id"), &scan(10, 3));

        let drained = collector.drain_dirty_queries();
        assert_eq!(drained.len(), 2);
        assert_eq!(collector.drain_dirty_queries(), []);

        collector.record_query(fingerprint("status"), &scan(10, 4));
        let drained = collector.drain_dirty_queries();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].1.executions, 3);

        collector.mark_all_dirty();
        assert_eq!(collector.drain_dirty_queries().len(), 2);
    }

    #[test]
    fn capacity_is_bounded_by_evicting_the_oldest_shape() {
        let collector = AutotuneCollector::new(SHARDS, 5_000);
        for i in 0..500_i64 {
            collector.record_query(fingerprint(&format!("field_{i}")), &scan(1, i));
        }
        assert!(collector.query_snapshot().len() <= SHARDS);
    }

    #[test]
    fn index_lookups_track_last_use_and_can_be_forgotten() {
        let collector = AutotuneCollector::new(16, 5_000);
        collector.record_index_lookup("orders", "idx_a", 10);
        collector.record_index_lookup("orders", "idx_a", 5);
        let snapshot = collector.index_lookup_snapshot();
        let usage = snapshot[&("orders".to_owned(), "idx_a".to_owned())];
        assert_eq!((usage.lookups, usage.last_used_ms), (2, 10));
        assert_eq!(collector.drain_dirty_index_lookups().len(), 1);
        collector.forget_index("orders", "idx_a");
        assert!(collector.index_lookup_snapshot().is_empty());
    }

    #[test]
    fn scan_evidence_can_be_cleared_and_seeded_stats_are_clean() {
        let collector = AutotuneCollector::new(16, 5_000);
        collector.seed_query(
            fingerprint("status"),
            QueryStats {
                executions: 9,
                scans: 9,
                scan_rows_examined: 900,
                ..QueryStats::default()
            },
        );
        assert_eq!(collector.drain_dirty_queries(), []);
        collector.clear_scan_evidence(&fingerprint("status"));
        let snapshot = collector.query_snapshot();
        assert_eq!((snapshot[0].1.executions, snapshot[0].1.scans), (9, 0));
        assert_eq!(collector.drain_dirty_queries().len(), 1);
    }

    #[test]
    fn sql_statistics_are_bounded_and_resettable() {
        let collector = AutotuneCollector::new(2, 5_000);
        for i in 0..10_i64 {
            collector.record_sql(
                format!("SELECT {i}"),
                &SqlObservation {
                    duration_us: 1,
                    full_scan_steps: 0,
                    at_ms: i,
                },
            );
        }
        assert_eq!(collector.sql_snapshot().len(), 2);
        collector.reset();
        assert_eq!(collector.sql_snapshot(), []);
        assert_eq!(collector.query_snapshot(), []);
    }
}
