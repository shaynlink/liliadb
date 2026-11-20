use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
#[cfg(not(target_arch = "wasm32"))]
use std::thread::{self, JoinHandle};
use std::time::Instant;

use lilia_core::{
    AccessTarget, BitemporalEntry, GraphNode, JsonEntry, KvEntry, LiliaError, MarkovConfig,
    MarkovModel, MarkovStats, Result, ValueCodec,
};
#[cfg(not(target_arch = "wasm32"))]
use lilia_sqlite_native::NativeConnection;

use crate::cache::CacheManager;
use crate::json::{decode_json_value, decode_stored_doc};
use crate::storage::{now_ms, parse_version};

/// Options configuring the Markov predictive cache and prefetching engine.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CacheOptions {
    /// Whether L1/L2 caching is active.
    pub enabled: bool,
    /// Capacity of L1 cache in number of decoded entity objects.
    pub l1_capacity: usize,
    /// Capacity of L2 cache in number of raw/compact payloads.
    pub l2_capacity: usize,
    /// Whether background Markov prefetching is enabled.
    pub prefetch_enabled: bool,
    /// Maximum horizon depth when predicting future queries (e.g. 1 or 2).
    pub prefetch_max_depth: usize,
    /// Markov probability micro-model configuration.
    pub markov_config: MarkovConfig,
    /// Whether to persist the Markov model in SQLite across database sessions.
    pub persist_markov: bool,
}

impl Default for CacheOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            l1_capacity: 512,
            l2_capacity: 2048,
            prefetch_enabled: true,
            prefetch_max_depth: 2,
            markov_config: MarkovConfig::default(),
            persist_markov: true,
        }
    }
}

impl CacheOptions {
    #[must_use]
    pub fn mobile() -> Self {
        Self {
            enabled: true,
            l1_capacity: 256,
            l2_capacity: 1024,
            prefetch_enabled: true,
            prefetch_max_depth: 2,
            markov_config: MarkovConfig {
                max_states: 512,
                max_transitions_per_state: 8,
                confidence_threshold: 0.50,
                min_observations: 2,
                decay_threshold: 500,
                session_window_ms: 2000,
            },
            persist_markov: true,
        }
    }

    #[must_use]
    pub fn server() -> Self {
        Self {
            enabled: true,
            l1_capacity: 4096,
            l2_capacity: 16384,
            prefetch_enabled: true,
            prefetch_max_depth: 2,
            markov_config: MarkovConfig {
                max_states: 2048,
                max_transitions_per_state: 16,
                confidence_threshold: 0.40,
                min_observations: 2,
                decay_threshold: 1000,
                session_window_ms: 3000,
            },
            persist_markov: true,
        }
    }

    #[must_use]
    pub fn constrained() -> Self {
        Self {
            enabled: true,
            l1_capacity: 64,
            l2_capacity: 256,
            prefetch_enabled: true,
            prefetch_max_depth: 1,
            markov_config: MarkovConfig {
                max_states: 128,
                max_transitions_per_state: 4,
                confidence_threshold: 0.60,
                min_observations: 2,
                decay_threshold: 250,
                session_window_ms: 1500,
            },
            persist_markov: false,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
struct PrefetchWorkerHandle {
    sender: mpsc::SyncSender<Vec<AccessTarget>>,
    #[allow(dead_code)]
    worker: Option<JoinHandle<()>>,
}

#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
struct PrefetchWorkerHandle;

/// Predictive prefetching engine backed by a Markov Chain micro-model.
pub struct PrefetchEngine {
    markov: Arc<RwLock<MarkovModel>>,
    cache: Arc<CacheManager>,
    last_access: Mutex<Option<(AccessTarget, Instant)>>,
    options: CacheOptions,
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<PrefetchWorkerHandle>,
    running: Arc<AtomicBool>,
}

impl std::fmt::Debug for PrefetchEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrefetchEngine")
            .field("options", &self.options)
            .finish()
    }
}

impl PrefetchEngine {
    pub fn start(
        db_path: &Path,
        options: CacheOptions,
        cache: Arc<CacheManager>,
        codec: Arc<dyn ValueCodec>,
        restored_markov: Option<MarkovModel>,
    ) -> Result<Arc<Self>> {
        let markov =
            Arc::new(RwLock::new(restored_markov.unwrap_or_else(|| {
                MarkovModel::new(options.markov_config.clone())
            })));
        let running = Arc::new(AtomicBool::new(true));

        #[cfg(not(target_arch = "wasm32"))]
        let worker_handle = if options.prefetch_enabled {
            let (tx, rx) = mpsc::sync_channel::<Vec<AccessTarget>>(128);
            let path_buf = db_path.to_path_buf();
            let cache_clone = Arc::clone(&cache);
            let is_running = Arc::clone(&running);
            let codec_clone = Arc::clone(&codec);

            let handle = thread::Builder::new()
                .name("lilia-prefetch".into())
                .spawn(move || {
                    run_prefetch_loop(&path_buf, rx, cache_clone, codec_clone, is_running);
                })
                .map_err(|e| LiliaError::new(lilia_core::ErrorCode::Io, e.to_string(), false))?;

            Some(PrefetchWorkerHandle {
                sender: tx,
                worker: Some(handle),
            })
        } else {
            None
        };

        Ok(Arc::new(Self {
            markov,
            cache,
            last_access: Mutex::new(None),
            options,
            #[cfg(not(target_arch = "wasm32"))]
            worker: worker_handle,
            running,
        }))
    }

    /// Records an access to `target`, updates the Markov chain, and schedules prefetching.
    pub fn on_access(&self, target: AccessTarget) {
        if !self.options.enabled {
            return;
        }

        let now = Instant::now();
        let now_epoch_ms = now_ms();
        let session_window = self.options.markov_config.session_window_ms;

        // 1. Transition observation
        let mut last_guard = self
            .last_access
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((prev_target, prev_time)) = last_guard.take() {
            let elapsed_ms = now.duration_since(prev_time).as_millis() as u64;
            if elapsed_ms <= session_window {
                if let Ok(mut markov) = self.markov.write() {
                    let ts_u64 = u64::try_from(now_epoch_ms).unwrap_or(0);
                    markov.record_transition(prev_target, target.clone(), ts_u64);
                }
            }
        }
        *last_guard = Some((target.clone(), now));
        drop(last_guard);

        // 2. Predictive prefetching
        if !self.options.prefetch_enabled {
            return;
        }

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(ref worker) = self.worker {
            let candidates = if let Ok(markov) = self.markov.read() {
                markov.predict(&target, self.options.prefetch_max_depth)
            } else {
                Vec::new()
            };

            if candidates.is_empty() {
                return;
            }

            // Filter out items already in L1/L2
            let mut to_prefetch = Vec::new();
            for (cand, _) in candidates {
                let in_cache = match &cand {
                    AccessTarget::Json { space, id } => self.cache.get_json(space, id).is_some(),
                    AccessTarget::Kv { namespace, key } => {
                        self.cache.get_kv(namespace, key).is_some()
                    }
                    AccessTarget::GraphNode { graph, id } => {
                        self.cache.get_graph_node(graph, id).is_some()
                    }
                    AccessTarget::Bitemporal { space, id } => self
                        .cache
                        .get_bitemporal(space, id, now_epoch_ms, now_epoch_ms)
                        .is_some(),
                };
                if !in_cache {
                    to_prefetch.push(cand);
                }
            }

            if !to_prefetch.is_empty() {
                if let Ok(mut markov) = self.markov.write() {
                    markov.mark_predictions_generated(to_prefetch.len());
                }
                self.cache.mark_prefetch_dispatched(to_prefetch.len());
                // Non-blocking try_send: drops if queue is full
                let _ = worker.sender.try_send(to_prefetch);
            }
        }
    }

    /// Returns telemetry snapshot of the Markov probability model.
    #[must_use]
    pub fn markov_stats(&self) -> MarkovStats {
        self.markov
            .read()
            .map_or_else(|_| MarkovStats::default(), |m| m.stats())
    }

    /// Exports the current Markov model clone (for persistence).
    #[must_use]
    pub fn export_markov_model(&self) -> Option<MarkovModel> {
        self.markov.read().ok().map(|m| m.clone())
    }

    /// Stops the prefetch worker.
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn run_prefetch_loop(
    path: &Path,
    rx: mpsc::Receiver<Vec<AccessTarget>>,
    cache: Arc<CacheManager>,
    codec: Arc<dyn ValueCodec>,
    running: Arc<AtomicBool>,
) {
    // Open dedicated query-only read connection for prefetching
    let conn = match NativeConnection::open_with_config(path, "NORMAL", 0, 1000, -2048) {
        Ok(c) => c,
        Err(_) => return,
    };
    let _ = conn.pragma_update("query_only", 1);

    while running.load(Ordering::Relaxed) {
        match rx.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(batch) => {
                let mut completed = 0;
                for target in batch {
                    if !running.load(Ordering::Relaxed) {
                        return;
                    }
                    if prefetch_single_target(&conn, &target, &cache, codec.as_ref()).is_ok() {
                        completed += 1;
                    }
                }
                if completed > 0 {
                    cache.mark_prefetch_completed(completed);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn prefetch_single_target(
    conn: &NativeConnection,
    target: &AccessTarget,
    cache: &CacheManager,
    codec: &dyn ValueCodec,
) -> Result<()> {
    match target {
        AccessTarget::Json { space, id } => {
            if cache.get_json(space, id).is_some() {
                return Ok(());
            }
            let mut stmt = conn.prepare("SELECT value, format, dict_id, version FROM _lilia_json WHERE space = ?1 AND id = ?2;")?;
            stmt.bind_text_static(1, space)?;
            stmt.bind_text_static(2, id)?;
            if stmt.step()? {
                let format = stmt.column_int64(1);
                let dict_id = stmt.column_int64(2);
                let version = parse_version(stmt.column_int64(3))?;
                let dict_reg = lilia_core::DictionaryRegistry::default();
                let doc =
                    decode_stored_doc(&stmt, 0, format, dict_id, space, codec, &dict_reg, None)?;
                let entry = Arc::new(JsonEntry::new(space.clone(), id.clone(), doc, version));
                cache.put_json(space, id, entry);
            }
        }
        AccessTarget::Kv { namespace, key } => {
            if cache.get_kv(namespace, key).is_some() {
                return Ok(());
            }
            let mut stmt = conn.prepare(
                "SELECT value, version, expires_at_ms FROM _lilia_kv WHERE namespace = ?1 AND key = ?2 AND (expires_at_ms IS NULL OR expires_at_ms > ?3);",
            )?;
            stmt.bind_text_static(1, namespace)?;
            stmt.bind_blob_static(2, key)?;
            stmt.bind_int64(3, now_ms())?;
            if stmt.step()? {
                let raw_val = stmt.column_blob(0);
                let version = parse_version(stmt.column_int64(1))?;
                let expires_at_ms = if stmt.column_is_null(2) {
                    None
                } else {
                    Some(stmt.column_int64(2))
                };
                let value = codec.decode(raw_val)?;
                let entry = Arc::new(KvEntry {
                    namespace: namespace.clone(),
                    key: key.clone(),
                    value,
                    version,
                    expires_at_ms,
                });
                cache.put_kv(namespace, key, entry);
            }
        }
        AccessTarget::GraphNode { graph, id } => {
            if cache.get_graph_node(graph, id).is_some() {
                return Ok(());
            }
            let mut stmt = conn.prepare(
                "SELECT kind, metadata, doc_space, doc_id FROM _lilia_graph_nodes WHERE graph = ?1 AND id = ?2;",
            )?;
            stmt.bind_text_static(1, graph)?;
            stmt.bind_text_static(2, id)?;
            if stmt.step()? {
                let kind = stmt.column_text(0).to_string();
                let meta_str = stmt.column_text(1);
                let metadata = serde_json::from_str(meta_str).map_err(|e| {
                    LiliaError::new(lilia_core::ErrorCode::Storage, e.to_string(), false)
                })?;
                let doc_space = if stmt.column_is_null(2) {
                    None
                } else {
                    Some(stmt.column_text(2).to_string())
                };
                let doc_id = if stmt.column_is_null(3) {
                    None
                } else {
                    Some(stmt.column_text(3).to_string())
                };
                let entry = Arc::new(GraphNode {
                    graph: graph.clone(),
                    id: id.clone(),
                    kind,
                    metadata,
                    doc_space,
                    doc_id,
                });
                cache.put_graph_node(graph, id, entry);
            }
        }
        AccessTarget::Bitemporal { space, id } => {
            let now = now_ms();
            if cache.get_bitemporal(space, id, now, now).is_some() {
                return Ok(());
            }
            let mut stmt = conn.prepare(
                "SELECT vt_from, vt_to, tt_from, tt_to, value, version \
                 FROM _lilia_bitemporal \
                 WHERE space = ?1 AND id = ?2 \
                   AND tt_from <= ?3 AND ?3 < tt_to \
                   AND vt_from <= ?4 AND ?4 < vt_to \
                 LIMIT 1;",
            )?;
            stmt.bind_text_static(1, space)?;
            stmt.bind_text_static(2, id)?;
            stmt.bind_int64(3, now)?;
            stmt.bind_int64(4, now)?;
            if stmt.step()? {
                let v_from = stmt.column_int64(0);
                let v_to = stmt.column_int64(1);
                let t_from = stmt.column_int64(2);
                let t_to = stmt.column_int64(3);
                let enc = stmt.column_text(4);
                let ver = parse_version(stmt.column_int64(5))?;
                let val = decode_json_value(enc, codec)?;
                let entry = Arc::new(BitemporalEntry {
                    space: space.clone(),
                    id: id.clone(),
                    vt_from: v_from,
                    vt_to: v_to,
                    tt_from: t_from,
                    tt_to: t_to,
                    value: val,
                    version: ver,
                });
                cache.put_bitemporal(space, id, now, now, entry);
            }
        }
    }
    Ok(())
}
