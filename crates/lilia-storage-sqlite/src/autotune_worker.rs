#[cfg(not(target_arch = "wasm32"))]
use std::sync::{mpsc, Arc, Weak};
#[cfg(not(target_arch = "wasm32"))]
use std::thread::{self, JoinHandle};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use lilia_core::ValueCodec;
#[cfg(not(target_arch = "wasm32"))]
use lilia_core::{ErrorCode, LiliaError, Result};

#[cfg(not(target_arch = "wasm32"))]
use crate::autotune::{AutotuneEngine, IndexEnv};
#[cfg(not(target_arch = "wasm32"))]
use crate::catalog::CollectionCatalog;
#[cfg(not(target_arch = "wasm32"))]
use crate::engine::Resources;
#[cfg(not(target_arch = "wasm32"))]
use crate::lifecycle::Lifecycle;

/// Background thread that runs one auto-tuning cycle per interval.
///
/// It only holds weak references to the database resources, so it can never keep a closed
/// database alive. Dropping the worker wakes the thread and joins it.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub(crate) struct AutotuneWorker {
    wake: Option<mpsc::SyncSender<()>>,
    worker: Option<JoinHandle<()>>,
}

#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
pub(crate) struct AutotuneWorker;

#[cfg(not(target_arch = "wasm32"))]
impl AutotuneWorker {
    pub(crate) fn start(
        engine: Arc<AutotuneEngine>,
        resources: Weak<Resources>,
        lifecycle: Weak<Lifecycle>,
        catalog: Arc<CollectionCatalog>,
        codec: Arc<dyn ValueCodec>,
        dict_registry: Arc<lilia_core::DictionaryRegistry>,
        interval: Duration,
    ) -> Result<Self> {
        let (wake, receive) = mpsc::sync_channel(1);

        let worker = thread::Builder::new()
            .name("lilia-autotune".into())
            .spawn(move || loop {
                match receive.recv_timeout(interval) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
                let (Some(resources), Some(lifecycle)) = (resources.upgrade(), lifecycle.upgrade())
                else {
                    return;
                };
                let env = IndexEnv {
                    resources: &resources,
                    lifecycle: &lifecycle,
                    catalog: &catalog,
                    codec: codec.as_ref(),
                    dict_registry: &dict_registry,
                };
                // A failed cycle (busy, closing) is retried on the next tick.
                let _ = engine.run_cycle(&env, crate::storage::now_ms());
            })
            .map_err(|error| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to spawn autotune worker: {error}"),
                    false,
                )
            })?;

        Ok(Self {
            wake: Some(wake),
            worker: Some(worker),
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for AutotuneWorker {
    fn drop(&mut self) {
        self.wake.take();
        if let Some(worker) = self.worker.take() {
            // The worker thread can itself release the last reference to the database
            // resources; joining its own handle would deadlock.
            if worker.thread().id() != thread::current().id() {
                let _ = worker.join();
            }
        }
    }
}
