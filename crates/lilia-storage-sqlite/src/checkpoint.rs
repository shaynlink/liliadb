use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
#[cfg(not(target_arch = "wasm32"))]
use std::thread::{self, JoinHandle};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

pub use lilia_core::CheckpointStats;
#[cfg(not(target_arch = "wasm32"))]
use lilia_core::ErrorCode;
#[cfg(not(target_arch = "wasm32"))]
use lilia_core::LiliaError;
use lilia_core::Result;
#[cfg(not(target_arch = "wasm32"))]
use lilia_sqlite_native::NativeConnection;

use crate::engine::CheckpointPolicy;

/// WAL size, in bytes, past which writers checkpoint themselves between two transactions
/// (see `WalBackpressure`): four times the background threshold, and never below 4 MiB so that
/// writers only step in when the WAL is not being rewound.
pub(crate) fn writer_wal_limit(wal_bytes: u64) -> u64 {
    wal_bytes.saturating_mul(4).max(4 * 1024 * 1024)
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub(crate) struct Checkpointer {
    wake: Option<mpsc::SyncSender<()>>,
    worker: Option<JoinHandle<()>>,
    stats: Arc<Mutex<CheckpointStats>>,
}

#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
pub(crate) struct Checkpointer {
    stats: Arc<Mutex<CheckpointStats>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Checkpointer {
    pub(crate) fn start(
        path: &Path,
        policy: CheckpointPolicy,
        synchronous: &str,
        cas: Option<Arc<crate::cas::CasEngine>>,
        wear: Option<Arc<crate::wear::FlashWearManager>>,
        parity: Option<Arc<crate::parity::ParityManager>>,
    ) -> Result<Self> {
        let connection = NativeConnection::open_with_config(path, synchronous, 0, 0, -2048)?;
        connection.pragma_update("wal_autocheckpoint", "0")?;
        connection.pragma_update("temp_store", "MEMORY")?;
        // Initialize this connection's WAL view before inspecting checkpoint state.
        connection.exec("SELECT count(*) FROM sqlite_schema;")?;
        let page_size =
            u64::try_from(connection.pragma_query_int("PRAGMA page_size")?).unwrap_or(4096);
        let stats = Arc::new(Mutex::new(CheckpointStats::default()));
        let (wake, receive) = mpsc::sync_channel(1);
        let shared = Arc::clone(&stats);
        let worker = thread::Builder::new()
            .name("lilia-checkpoint".into())
            .spawn(move || {
                run(
                    &connection,
                    page_size,
                    policy,
                    &receive,
                    &shared,
                    cas.as_deref(),
                    wear,
                    parity,
                );
            })
            .map_err(|error| LiliaError::new(ErrorCode::Io, error.to_string(), false))?;
        let service = Self {
            wake: Some(wake),
            worker: Some(worker),
            stats,
        };
        // Also inspect WAL recovered at open, without waiting for a new mutation.
        service.notify();
        Ok(service)
    }

    pub(crate) fn notify(&self) {
        if let Some(wake) = &self.wake {
            // One pending notification is enough. Never block a committed writer.
            let _ = wake.try_send(());
        }
    }

    pub(crate) fn stats(&self) -> CheckpointStats {
        *self
            .stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(target_arch = "wasm32")]
impl Checkpointer {
    pub(crate) fn start(
        _path: &Path,
        _policy: CheckpointPolicy,
        _synchronous: &str,
        _cas: Option<Arc<crate::cas::CasEngine>>,
        _wear: Option<Arc<crate::wear::FlashWearManager>>,
        _parity: Option<Arc<crate::parity::ParityManager>>,
    ) -> Result<Self> {
        Ok(Self {
            stats: Arc::new(Mutex::new(CheckpointStats::default())),
        })
    }

    pub(crate) fn notify(&self) {}

    pub(crate) fn stats(&self) -> CheckpointStats {
        *self
            .stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Checkpointer {
    fn drop(&mut self) {
        // Disconnect wakes recv_timeout even for a very long configured interval.
        self.wake.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn run(
    connection: &NativeConnection,
    page_size: u64,
    policy: CheckpointPolicy,
    receive: &mpsc::Receiver<()>,
    stats: &Mutex<CheckpointStats>,
    cas: Option<&crate::cas::CasEngine>,
    wear: Option<Arc<crate::wear::FlashWearManager>>,
    parity: Option<Arc<crate::parity::ParityManager>>,
) {
    let mut tick = Instant::now();
    let mut defer_until_tick = false;
    loop {
        let current_interval_ms = wear.as_ref().map_or(policy.interval_ms, |w| {
            w.adaptive_interval(policy.interval_ms)
        });
        let interval = Duration::from_millis(current_interval_ms);

        if matches!(
            receive.recv_timeout(interval.saturating_sub(tick.elapsed())),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ) {
            return;
        }
        let elapsed = tick.elapsed() >= interval;
        if elapsed {
            tick = Instant::now();
        }
        if defer_until_tick && !elapsed {
            continue;
        }
        let result = maybe_checkpoint(connection, page_size, policy.wal_bytes, elapsed);
        let mut stats = stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match result {
            Ok(Some((busy, log, copied))) => {
                stats.attempts = stats.attempts.saturating_add(1);
                stats.log_frames = log;
                stats.checkpointed_frames = copied;
                stats.last_error = None;
                defer_until_tick = busy != 0 || (log >= 0 && copied < log);

                if copied > 0 {
                    let frame_bytes = page_size.saturating_add(24);
                    let copied_bytes = u64::try_from(copied)
                        .unwrap_or(0)
                        .saturating_mul(frame_bytes);
                    if let Some(w) = wear.as_ref() {
                        w.record_db_write(copied_bytes);
                        w.record_fsync();
                    }
                    if let Some(p) = parity.as_ref() {
                        let _ = p.sync_all();
                    }
                }

                if defer_until_tick {
                    stats.incomplete = stats.incomplete.saturating_add(1);
                } else if let Some(cas_engine) = cas {
                    if cas_engine.options().enabled && cas_engine.options().auto_purge_on_checkpoint
                    {
                        let _ = crate::cas::purge_orphan_blobs(connection, cas_engine);
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                stats.last_error = Some(error.code);
                defer_until_tick = true;
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn maybe_checkpoint(
    connection: &NativeConnection,
    page_size: u64,
    limit: u64,
    elapsed: bool,
) -> Result<Option<(i64, i64, i64)>> {
    let (_, log, copied) = state(connection, "PRAGMA wal_checkpoint(NOOP)")?;
    let outstanding = u64::try_from(log.saturating_sub(copied)).unwrap_or(0);
    // WAL frames contain a page plus a 24-byte header. Allocated file length may
    // stay large after checkpoint/reuse and must not trigger repeated checkpoints.
    let frame_bytes = page_size.saturating_add(24);
    let outstanding_bytes = outstanding.saturating_mul(frame_bytes);
    let wal_bytes = u64::try_from(log).unwrap_or(0).saturating_mul(frame_bytes);
    // Past the writer limit, the writer that sent this notification checkpoints itself: competing
    // for the checkpoint lock would only make its attempt fail. Interval ticks always run.
    if !elapsed && (outstanding_bytes < limit || wal_bytes >= writer_wal_limit(limit)) {
        return Ok(None);
    }
    state(connection, "PRAGMA wal_checkpoint(PASSIVE)").map(Some)
}

#[cfg(not(target_arch = "wasm32"))]
fn state(connection: &NativeConnection, pragma: &str) -> Result<(i64, i64, i64)> {
    let mut stmt = connection.prepare(pragma)?;
    if stmt.step()? {
        Ok((
            stmt.column_int64(0),
            stmt.column_int64(1),
            stmt.column_int64(2),
        ))
    } else {
        Ok((0, 0, 0))
    }
}

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod tests;
