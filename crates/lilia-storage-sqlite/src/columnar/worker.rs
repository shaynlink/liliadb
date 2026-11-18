//! Background compaction of columnar tables.

use std::sync::mpsc;
use std::sync::Weak;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use lilia_core::{ErrorCode, LiliaError, Result};

use super::compaction;
use super::core::ColumnarCore;

/// Thread that periodically folds large deltas back into segments.
///
/// It only holds a weak reference to the store, so it can never keep a closed database
/// alive. Dropping the worker wakes the thread and joins it.
#[derive(Debug)]
pub(crate) struct CompactionWorker {
    stop: Option<mpsc::SyncSender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl CompactionWorker {
    /// Starts the worker, or returns `None` when periodic compaction is disabled
    /// (`interval` is zero).
    pub(crate) fn start(core: Weak<ColumnarCore>, interval: Duration) -> Result<Option<Self>> {
        if interval.is_zero() {
            return Ok(None);
        }
        let (stop, stopped) = mpsc::sync_channel::<()>(1);
        let thread = thread::Builder::new()
            .name("lilia-columnar".into())
            .spawn(move || loop {
                // Anything other than a timeout means the worker was dropped.
                if !matches!(
                    stopped.recv_timeout(interval),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    return;
                }
                let Some(core) = core.upgrade() else {
                    return;
                };
                if let Err(error) = compaction::compact_due(&core) {
                    if error.code == ErrorCode::Closed {
                        return;
                    }
                    // Any other failure is retried on the next tick; scans stay correct
                    // meanwhile because the delta is overlaid on read.
                }
            })
            .map_err(|error| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to spawn columnar compaction worker: {error}"),
                    false,
                )
            })?;
        Ok(Some(Self {
            stop: Some(stop),
            thread: Some(thread),
        }))
    }
}

impl Drop for CompactionWorker {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(thread) = self.thread.take() {
            // The worker thread can itself release the last reference to the store;
            // joining its own handle would deadlock.
            if thread.thread().id() != thread::current().id() {
                let _ = thread.join();
            }
        }
    }
}
