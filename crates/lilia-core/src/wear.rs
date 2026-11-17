//! Flash Wear Leveling Telemetry and Adaptive WAL Fsync Control.
//!
//! Tracks physical byte write volume to NAND flash memory and dynamically
//! adapts WAL fsync frequency to preserve hardware lifespan on mobile devices.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// Flash wear tier categorization based on write rate and cumulative endurance budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FlashWearTier {
    /// Normal operation: write volume is well within daily hardware limits.
    #[default]
    Nominal,
    /// Elevated write volume: fsync frequency is moderately extended to coalesce writes.
    Elevated,
    /// High write volume: aggressive coalescing and longer intervals to protect flash cells.
    Severe,
    /// Critical wear pressure: maximum interval stretching to avoid device flash exhaustion.
    Critical,
}

/// Dynamic wear leveling and adaptive WAL fsync policy.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WearLevelingPolicy {
    /// Whether dynamic wear leveling and adaptive fsync is active.
    pub enabled: bool,
    /// Baseline fsync/checkpoint interval when write volume is nominal (e.g. 3,000 ms).
    pub nominal_fsync_interval_ms: u64,
    /// Maximum allowable fsync/checkpoint interval during heavy wear pressure (e.g. 15,000 ms).
    pub max_fsync_interval_ms: u64,
    /// Target daily physical write budget in bytes (default 5 GiB/day).
    pub target_daily_budget_bytes: u64,
}

impl Default for WearLevelingPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            nominal_fsync_interval_ms: 3_000,
            max_fsync_interval_ms: 15_000,
            target_daily_budget_bytes: 5 * 1024 * 1024 * 1024, // 5 GiB
        }
    }
}

impl WearLevelingPolicy {
    /// Computes the dynamically adapted fsync interval in milliseconds.
    #[must_use]
    pub fn adaptive_interval(&self, nominal_requested_ms: u64, current_rate_bps: u64) -> u64 {
        if !self.enabled {
            return nominal_requested_ms;
        }

        let base_interval = nominal_requested_ms.max(self.nominal_fsync_interval_ms);
        let max_interval = self.max_fsync_interval_ms.max(base_interval);

        // Target write budget rate per second: daily budget / 86400 seconds
        let budget_rate_bps = (self.target_daily_budget_bytes / 86_400).max(1);

        // Wear factor: ratio of current write rate to budget rate, clamped to [0.0, 1.0]
        let wear_factor = if current_rate_bps == 0 {
            0.0
        } else {
            ((current_rate_bps as f64) / (budget_rate_bps as f64)).min(1.0)
        };

        let span = (max_interval - base_interval) as f64;
        let addition = span * wear_factor;
        base_interval.saturating_add(addition.round() as u64)
    }

    /// Determines the wear tier based on recent write rate and daily budget.
    #[must_use]
    pub fn classify_tier(
        &self,
        current_rate_bps: u64,
        cumulative_daily_bytes: u64,
    ) -> FlashWearTier {
        let budget_rate_bps = (self.target_daily_budget_bytes / 86_400).max(1);
        let rate_ratio = (current_rate_bps as f64) / (budget_rate_bps as f64);
        let daily_ratio = (cumulative_daily_bytes as f64) / (self.target_daily_budget_bytes as f64);

        if daily_ratio > 1.5 || rate_ratio > 3.0 {
            FlashWearTier::Critical
        } else if daily_ratio > 1.0 || rate_ratio > 1.5 {
            FlashWearTier::Severe
        } else if daily_ratio > 0.5 || rate_ratio > 0.8 {
            FlashWearTier::Elevated
        } else {
            FlashWearTier::Nominal
        }
    }
}

/// Real-time snapshot of flash wear telemetry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlashWearTelemetry {
    /// Total physical bytes written across all storage channels.
    pub total_physical_bytes: u64,
    /// Physical bytes written to the main database file.
    pub bytes_written_db: u64,
    /// Physical bytes written to the WAL file.
    pub bytes_written_wal: u64,
    /// Physical bytes written to the Reed-Solomon parity sidecar file.
    pub bytes_written_parity: u64,
    /// Physical bytes written to CAS blob storage.
    pub bytes_written_cas: u64,
    /// Total fsync / fcntl F_FULLFSYNC calls executed.
    pub total_fsync_count: u64,
    /// Recent physical write rate in bytes per second.
    pub current_write_rate_bps: u64,
    /// Current dynamically adapted WAL fsync interval in milliseconds.
    pub adaptive_fsync_interval_ms: u64,
    /// Current wear classification.
    pub wear_tier: FlashWearTier,
    /// Percentage of the daily write budget consumed.
    pub daily_budget_used_percent: f64,
}

/// Thread-safe atomic tracker for physical flash writes.
#[derive(Debug)]
pub struct FlashWearTracker {
    bytes_written_db: AtomicU64,
    bytes_written_wal: AtomicU64,
    bytes_written_parity: AtomicU64,
    bytes_written_cas: AtomicU64,
    total_fsync_count: AtomicU64,
    window_start_ms: AtomicI64,
    window_bytes_written: AtomicU64,
    last_computed_rate_bps: AtomicU64,
}

impl Default for FlashWearTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl FlashWearTracker {
    /// Creates a new flash wear tracker starting at the current time.
    #[must_use]
    pub fn new() -> Self {
        Self {
            bytes_written_db: AtomicU64::new(0),
            bytes_written_wal: AtomicU64::new(0),
            bytes_written_parity: AtomicU64::new(0),
            bytes_written_cas: AtomicU64::new(0),
            total_fsync_count: AtomicU64::new(0),
            window_start_ms: AtomicI64::new(0),
            window_bytes_written: AtomicU64::new(0),
            last_computed_rate_bps: AtomicU64::new(0),
        }
    }

    /// Records physical bytes written to the SQLite main database file.
    pub fn record_db_write(&self, bytes: u64, now_ms: i64) {
        self.bytes_written_db.fetch_add(bytes, Ordering::Relaxed);
        self.update_window_rate(bytes, now_ms);
    }

    /// Records physical bytes written to the WAL file.
    pub fn record_wal_write(&self, bytes: u64, now_ms: i64) {
        self.bytes_written_wal.fetch_add(bytes, Ordering::Relaxed);
        self.update_window_rate(bytes, now_ms);
    }

    /// Records physical bytes written to the Reed-Solomon parity file.
    pub fn record_parity_write(&self, bytes: u64, now_ms: i64) {
        self.bytes_written_parity
            .fetch_add(bytes, Ordering::Relaxed);
        self.update_window_rate(bytes, now_ms);
    }

    /// Records physical bytes written to CAS blobs.
    pub fn record_cas_write(&self, bytes: u64, now_ms: i64) {
        self.bytes_written_cas.fetch_add(bytes, Ordering::Relaxed);
        self.update_window_rate(bytes, now_ms);
    }

    /// Records an fsync / flush barrier execution.
    pub fn record_fsync(&self) {
        self.total_fsync_count.fetch_add(1, Ordering::Relaxed);
    }

    fn update_window_rate(&self, bytes: u64, now_ms: i64) {
        let win_start = self.window_start_ms.load(Ordering::Relaxed);
        if win_start == 0 {
            self.window_start_ms.store(now_ms, Ordering::Relaxed);
            self.window_bytes_written.store(bytes, Ordering::Relaxed);
            return;
        }

        let elapsed_ms = now_ms.saturating_sub(win_start);
        if elapsed_ms >= 5_000 {
            // Window duration expired (5s sliding window): compute rate
            let win_bytes = self.window_bytes_written.swap(bytes, Ordering::Relaxed);
            let rate = if elapsed_ms > 0 {
                (win_bytes.saturating_mul(1_000)) / (elapsed_ms as u64)
            } else {
                0
            };
            self.last_computed_rate_bps.store(rate, Ordering::Relaxed);
            self.window_start_ms.store(now_ms, Ordering::Relaxed);
        } else {
            self.window_bytes_written
                .fetch_add(bytes, Ordering::Relaxed);
        }
    }

    /// Returns a comprehensive snapshot of flash wear telemetry.
    #[must_use]
    pub fn snapshot(&self, now_ms: i64, policy: &WearLevelingPolicy) -> FlashWearTelemetry {
        let db = self.bytes_written_db.load(Ordering::Relaxed);
        let wal = self.bytes_written_wal.load(Ordering::Relaxed);
        let parity = self.bytes_written_parity.load(Ordering::Relaxed);
        let cas = self.bytes_written_cas.load(Ordering::Relaxed);
        let fsyncs = self.total_fsync_count.load(Ordering::Relaxed);
        let total = db
            .saturating_add(wal)
            .saturating_add(parity)
            .saturating_add(cas);

        // Check if rate window needs updating
        let win_start = self.window_start_ms.load(Ordering::Relaxed);
        let elapsed_ms = if win_start > 0 {
            now_ms.saturating_sub(win_start)
        } else {
            0
        };

        let current_rate = if elapsed_ms >= 5_000 {
            let win_bytes = self.window_bytes_written.load(Ordering::Relaxed);
            (win_bytes.saturating_mul(1_000)) / (elapsed_ms as u64)
        } else {
            self.last_computed_rate_bps.load(Ordering::Relaxed)
        };

        let adaptive_interval =
            policy.adaptive_interval(policy.nominal_fsync_interval_ms, current_rate);
        let wear_tier = policy.classify_tier(current_rate, total);
        let daily_budget_percent = if policy.target_daily_budget_bytes > 0 {
            ((total as f64) / (policy.target_daily_budget_bytes as f64)) * 100.0
        } else {
            0.0
        };

        FlashWearTelemetry {
            total_physical_bytes: total,
            bytes_written_db: db,
            bytes_written_wal: wal,
            bytes_written_parity: parity,
            bytes_written_cas: cas,
            total_fsync_count: fsyncs,
            current_write_rate_bps: current_rate,
            adaptive_fsync_interval_ms: adaptive_interval,
            wear_tier,
            daily_budget_used_percent: daily_budget_percent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wear_policy_adaptive_interval() {
        let policy = WearLevelingPolicy {
            enabled: true,
            nominal_fsync_interval_ms: 3_000,
            max_fsync_interval_ms: 15_000,
            target_daily_budget_bytes: 86_400 * 1_000, // 1000 bytes/sec budget
        };

        // Zero rate -> nominal interval
        assert_eq!(policy.adaptive_interval(3_000, 0), 3_000);

        // Half budget rate -> halfway
        let mid = policy.adaptive_interval(3_000, 500);
        assert!((8_500..=9_500).contains(&mid), "mid={mid}");

        // Full budget rate or higher -> capped at max
        let max = policy.adaptive_interval(3_000, 2_000);
        assert_eq!(max, 15_000);
    }

    #[test]
    fn test_tracker_accumulates_physical_writes() {
        let tracker = FlashWearTracker::new();
        let policy = WearLevelingPolicy::default();

        tracker.record_db_write(4096, 1000);
        tracker.record_wal_write(8192, 1050);
        tracker.record_parity_write(4096, 1100);
        tracker.record_cas_write(1024, 1150);
        tracker.record_fsync();
        tracker.record_fsync();

        let snap = tracker.snapshot(2000, &policy);
        assert_eq!(snap.total_physical_bytes, 4096 + 8192 + 4096 + 1024);
        assert_eq!(snap.bytes_written_db, 4096);
        assert_eq!(snap.bytes_written_wal, 8192);
        assert_eq!(snap.bytes_written_parity, 4096);
        assert_eq!(snap.bytes_written_cas, 1024);
        assert_eq!(snap.total_fsync_count, 2);
    }
}
