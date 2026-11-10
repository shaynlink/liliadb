#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Weak;
#[cfg(not(target_arch = "wasm32"))]
use std::thread::{self, JoinHandle};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

use lilia_core::Result;
use serde::{Deserialize, Serialize};

#[cfg(not(target_arch = "wasm32"))]
use crate::engine::Resources;
#[cfg(not(target_arch = "wasm32"))]
use crate::lifecycle::Lifecycle;

/// Configuration options for time-series background downsampling and retention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSeriesOptions {
    pub auto_retention: bool,
    pub retention_interval_ms: u64,
}

impl Default for TimeSeriesOptions {
    fn default() -> Self {
        Self {
            auto_retention: true,
            retention_interval_ms: 60_000,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub(crate) struct TimeSeriesWorker {
    wake: Option<mpsc::SyncSender<()>>,
    worker: Option<JoinHandle<()>>,
}

#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
pub(crate) struct TimeSeriesWorker;

#[cfg(not(target_arch = "wasm32"))]
impl TimeSeriesWorker {
    pub(crate) fn start(
        weak_resources: Weak<Resources>,
        weak_lifecycle: Weak<Lifecycle>,
        options: TimeSeriesOptions,
    ) -> Result<Self> {
        if !options.auto_retention || options.retention_interval_ms == 0 {
            return Ok(Self {
                wake: None,
                worker: None,
            });
        }

        let (wake, receive) = mpsc::sync_channel(1);
        let interval = Duration::from_millis(options.retention_interval_ms);

        let worker = thread::Builder::new()
            .name("lilia-ts-retention".into())
            .spawn(move || loop {
                match receive.recv_timeout(interval) {
                    Ok(()) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }

                if let (Some(resources), Some(lifecycle)) =
                    (weak_resources.upgrade(), weak_lifecycle.upgrade())
                {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as i64;
                    let _ = crate::timeseries::run_retention_pass(&resources, &lifecycle, now_ms);
                } else {
                    return;
                }
            })
            .map_err(|e| {
                lilia_core::LiliaError::new(
                    lilia_core::ErrorCode::Io,
                    format!("failed to spawn time-series retention worker: {e}"),
                    false,
                )
            })?;

        Ok(Self {
            wake: Some(wake),
            worker: Some(worker),
        })
    }

    #[allow(dead_code)]
    pub(crate) fn notify(&self) {
        if let Some(wake) = &self.wake {
            let _ = wake.try_send(());
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl TimeSeriesWorker {
    pub(crate) fn start(_options: TimeSeriesOptions) -> Result<Self> {
        Ok(Self)
    }

    #[allow(dead_code)]
    pub(crate) fn notify(&self) {}
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for TimeSeriesWorker {
    fn drop(&mut self) {
        self.wake.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
