//! Flash Wear Leveling manager and persistence for SQLite storage engine.

use std::sync::Arc;

use lilia_core::wear::{FlashWearTelemetry, FlashWearTracker, WearLevelingPolicy};
use lilia_core::{ErrorCode, LiliaError, Result};
use lilia_sqlite_native::NativeConnection;

use crate::storage::now_ms;

/// Coordinates flash write accounting and dynamic fsync adaptation for the database.
#[derive(Debug, Clone)]
pub struct FlashWearManager {
    tracker: Arc<FlashWearTracker>,
    policy: WearLevelingPolicy,
}

impl FlashWearManager {
    /// Creates a new flash wear manager with the specified wear leveling policy.
    #[must_use]
    pub fn new(policy: WearLevelingPolicy) -> Self {
        Self {
            tracker: Arc::new(FlashWearTracker::new()),
            policy,
        }
    }

    #[must_use]
    pub fn tracker(&self) -> &Arc<FlashWearTracker> {
        &self.tracker
    }

    #[must_use]
    pub const fn policy(&self) -> &WearLevelingPolicy {
        &self.policy
    }

    /// Records physical bytes written to the main database file (e.g. during checkpoints).
    pub fn record_db_write(&self, bytes: u64) {
        self.tracker.record_db_write(bytes, now_ms());
    }

    /// Records physical bytes written to the WAL file.
    pub fn record_wal_write(&self, bytes: u64) {
        self.tracker.record_wal_write(bytes, now_ms());
    }

    /// Records physical bytes written to the Reed-Solomon parity file.
    pub fn record_parity_write(&self, bytes: u64) {
        self.tracker.record_parity_write(bytes, now_ms());
    }

    /// Records physical bytes written to CAS blobs.
    pub fn record_cas_write(&self, bytes: u64) {
        self.tracker.record_cas_write(bytes, now_ms());
    }

    /// Records a disk fsync / barrier operation.
    pub fn record_fsync(&self) {
        self.tracker.record_fsync();
    }

    /// Returns the dynamically adapted fsync interval in milliseconds.
    #[must_use]
    pub fn adaptive_interval(&self, nominal_requested_ms: u64) -> u64 {
        let snap = self.tracker.snapshot(now_ms(), &self.policy);
        self.policy
            .adaptive_interval(nominal_requested_ms, snap.current_write_rate_bps)
    }

    /// Returns the current telemetry report.
    #[must_use]
    pub fn report(&self) -> FlashWearTelemetry {
        self.tracker.snapshot(now_ms(), &self.policy)
    }

    /// Loads persisted write counters from `_lilia_metadata` if available.
    pub fn load_from_metadata(&self, conn: &NativeConnection) {
        let Ok(mut stmt) =
            conn.prepare("SELECT value FROM _lilia_metadata WHERE key = 'flash_wear_telemetry';")
        else {
            return;
        };

        if let Ok(true) = stmt.step() {
            let json_str = stmt.column_text(0);
            if let Ok(snap) = serde_json::from_str::<FlashWearTelemetry>(json_str) {
                let now = now_ms();
                self.tracker.record_db_write(snap.bytes_written_db, now);
                self.tracker.record_wal_write(snap.bytes_written_wal, now);
                self.tracker
                    .record_parity_write(snap.bytes_written_parity, now);
                self.tracker.record_cas_write(snap.bytes_written_cas, now);
                for _ in 0..snap.total_fsync_count {
                    self.tracker.record_fsync();
                }
            }
        }
    }

    /// Persists current flash wear telemetry to `_lilia_metadata`.
    ///
    /// # Errors
    /// Returns error if SQL execution fails.
    pub fn persist_to_metadata(&self, conn: &NativeConnection) -> Result<()> {
        let snap = self.report();
        let json_str = serde_json::to_string(&snap).map_err(|e| {
            LiliaError::new(
                ErrorCode::Storage,
                format!("serialize wear telemetry error: {e}"),
                false,
            )
        })?;

        let mut stmt = conn.prepare(
            "INSERT INTO _lilia_metadata(key, value) VALUES('flash_wear_telemetry', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = ?1;",
        )?;
        stmt.bind_text_static(1, &json_str)?;
        stmt.step()?;
        Ok(())
    }
}
