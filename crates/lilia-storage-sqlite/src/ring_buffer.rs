use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use crossbeam_queue::ArrayQueue;
use lilia_core::{
    BatchOperation, DatabaseEvent, DictionaryRegistry, ErrorCode, HnswIndex, HybridClock,
    LiliaError, MutationResult, OverflowPolicy, Result, RingBufferConfig, RingBufferStats,
    StorageTier, TimeSeriesPoint, ValueCodec,
};
#[cfg(not(target_arch = "wasm32"))]
use lilia_sqlite_native::{VTabValue, VirtualTableProvider};

use crate::engine::{JsonOptions, Resources};
use crate::lifecycle::Lifecycle;
use crate::mem_engine::ShardedMemTable;
#[cfg(not(target_arch = "wasm32"))]
use crate::mem_engine::SliceVTab;
use crate::storage::now_ms;
use crate::timeseries::append_chunk_in_tx;
use crate::transaction::apply;
use crate::vector::sync_hnsw_for_op_with_cache;

#[derive(Debug, Clone)]
pub(crate) struct OverlayCell<V> {
    pub pending_ops: u32,
    pub state: Option<(V, u64, Option<i64>)>,
}

#[derive(Debug, Clone)]
pub(crate) struct OverlayTsCell {
    pub pending_batches: u32,
    pub points: Vec<TimeSeriesPoint>,
}

/// Sequence-tracked in-memory overlay providing Read-Your-Own-Writes over buffered writes
/// before the background group-commit flusher persists them to SQLite.
#[derive(Debug)]
pub(crate) struct PendingOverlay {
    pub kv: ShardedMemTable<OverlayCell<Vec<u8>>>,
    pub json: ShardedMemTable<OverlayCell<rkyv::util::AlignedVec>>,
    pub ts: ShardedMemTable<OverlayTsCell>,
}

impl PendingOverlay {
    #[must_use]
    pub fn new() -> Self {
        Self {
            kv: ShardedMemTable::new(),
            json: ShardedMemTable::new(),
            ts: ShardedMemTable::new(),
        }
    }

    pub fn record_kv(&self, namespace: &str, key: &[u8], state: Option<(Vec<u8>, Option<i64>)>) {
        let shard_idx = ShardedMemTable::<OverlayCell<Vec<u8>>>::shard_index(namespace, key);
        let lookup = (namespace.to_owned(), key.to_vec());
        let seq = self.kv.next_seq();
        let mut shard = self.kv.shards_write_at(shard_idx);
        let _ = ShardedMemTable::mutate_locked(
            &mut shard,
            &self.kv,
            namespace,
            key,
            now_ms(),
            |existing| {
                let new_state = state.map(|(v, exp)| (v, 1, exp));
                let payload_bytes = new_state.as_ref().map_or(16, |(v, _, _)| v.len() + 16);
                if let Some(entry) = existing {
                    let prev_ver = entry.value.state.as_ref().map_or(1, |(_, ver, _)| ver + 1);
                    entry.value.pending_ops = entry.value.pending_ops.saturating_add(1);
                    entry.value.state = new_state.map(|(v, _, exp)| (v, prev_ver, exp));
                    entry.payload_bytes = namespace.len() + key.len() + payload_bytes;
                    entry.last_access_seq = seq;
                    Ok(None)
                } else {
                    Ok(Some(crate::mem_engine::MemEntry {
                        value: OverlayCell {
                            pending_ops: 1,
                            state: new_state,
                        },
                        version: 1,
                        expires_at_ms: None,
                        inserted_seq: seq,
                        last_access_seq: seq,
                        payload_bytes: namespace.len() + key.len() + payload_bytes,
                    }))
                }
            },
        );
        let _ = lookup;
    }

    pub fn release_kv(&self, namespace: &str, key: &[u8], count: u32) {
        if count == 0 {
            return;
        }
        let shard_idx = ShardedMemTable::<OverlayCell<Vec<u8>>>::shard_index(namespace, key);
        let lookup = (namespace.to_owned(), key.to_vec());
        let mut shard = self.kv.shards_write_at(shard_idx);
        let remove = if let Some(entry) = shard.get_mut(&lookup) {
            entry.value.pending_ops = entry.value.pending_ops.saturating_sub(count);
            entry.value.pending_ops == 0
        } else {
            false
        };
        drop(shard);
        if remove {
            self.kv.remove_exact(namespace, key);
        }
    }

    pub fn record_json(&self, space: &str, id: &str, state: Option<rkyv::util::AlignedVec>) {
        let key = id.as_bytes();
        let shard_idx =
            ShardedMemTable::<OverlayCell<rkyv::util::AlignedVec>>::shard_index(space, key);
        let seq = self.json.next_seq();
        let mut shard = self.json.shards_write_at(shard_idx);
        let _ = ShardedMemTable::mutate_locked(
            &mut shard,
            &self.json,
            space,
            key,
            now_ms(),
            |existing| {
                let payload_bytes = state.as_ref().map_or(16, |b| b.len() + 16);
                if let Some(entry) = existing {
                    let prev_ver = entry.value.state.as_ref().map_or(1, |(_, ver, _)| ver + 1);
                    entry.value.pending_ops = entry.value.pending_ops.saturating_add(1);
                    entry.value.state = state.map(|v| (v, prev_ver, None));
                    entry.payload_bytes = space.len() + key.len() + payload_bytes;
                    entry.last_access_seq = seq;
                    Ok(None)
                } else {
                    Ok(Some(crate::mem_engine::MemEntry {
                        value: OverlayCell {
                            pending_ops: 1,
                            state: state.map(|v| (v, 1, None)),
                        },
                        version: 1,
                        expires_at_ms: None,
                        inserted_seq: seq,
                        last_access_seq: seq,
                        payload_bytes: space.len() + key.len() + payload_bytes,
                    }))
                }
            },
        );
    }

    pub fn record_json_from_serde(&self, space: &str, id: &str, value: Option<&serde_json::Value>) {
        let state = value.and_then(|v| {
            let doc = lilia_core::json::JsonDocument::from_serde(v);
            doc.to_archived_bytes().ok()
        });
        self.record_json(space, id, state);
    }

    pub fn release_json(&self, space: &str, id: &str, count: u32) {
        if count == 0 {
            return;
        }
        let key = id.as_bytes();
        let shard_idx =
            ShardedMemTable::<OverlayCell<rkyv::util::AlignedVec>>::shard_index(space, key);
        let lookup = (space.to_owned(), key.to_vec());
        let mut shard = self.json.shards_write_at(shard_idx);
        let remove = if let Some(entry) = shard.get_mut(&lookup) {
            entry.value.pending_ops = entry.value.pending_ops.saturating_sub(count);
            entry.value.pending_ops == 0
        } else {
            false
        };
        drop(shard);
        if remove {
            self.json.remove_exact(space, key);
        }
    }

    pub fn record_ts(&self, series_id: &str, resolution_ms: i64, pts: &[TimeSeriesPoint]) {
        if pts.is_empty() {
            return;
        }
        let key = resolution_ms.to_be_bytes();
        let shard_idx = ShardedMemTable::<OverlayTsCell>::shard_index(series_id, &key);
        let seq = self.ts.next_seq();
        let mut shard = self.ts.shards_write_at(shard_idx);
        let _ = ShardedMemTable::mutate_locked(
            &mut shard,
            &self.ts,
            series_id,
            &key,
            now_ms(),
            |existing| {
                if let Some(entry) = existing {
                    entry.value.pending_batches = entry.value.pending_batches.saturating_add(1);
                    entry.value.points.extend_from_slice(pts);
                    entry.payload_bytes += std::mem::size_of_val(pts);
                    entry.last_access_seq = seq;
                    Ok(None)
                } else {
                    Ok(Some(crate::mem_engine::MemEntry {
                        value: OverlayTsCell {
                            pending_batches: 1,
                            points: pts.to_vec(),
                        },
                        version: 1,
                        expires_at_ms: None,
                        inserted_seq: seq,
                        last_access_seq: seq,
                        payload_bytes: series_id.len() + 8 + std::mem::size_of_val(pts),
                    }))
                }
            },
        );
    }

    pub fn release_ts(&self, series_id: &str, resolution_ms: i64, batches: u32) {
        if batches == 0 {
            return;
        }
        let key = resolution_ms.to_be_bytes();
        let shard_idx = ShardedMemTable::<OverlayTsCell>::shard_index(series_id, &key);
        let lookup = (series_id.to_owned(), key.to_vec());
        let mut shard = self.ts.shards_write_at(shard_idx);
        let remove = if let Some(entry) = shard.get_mut(&lookup) {
            entry.value.pending_batches = entry.value.pending_batches.saturating_sub(batches);
            entry.value.pending_batches == 0
        } else {
            false
        };
        drop(shard);
        if remove {
            self.ts.remove_exact(series_id, &key);
        }
    }

    #[must_use]
    pub fn total_entries(&self) -> usize {
        self.kv.len() + self.json.len() + self.ts.len()
    }
}

#[derive(Debug)]
struct SyncGroupWaiter {
    outcome: Mutex<Option<Result<Vec<MutationResult>>>>,
    cv: Condvar,
}

impl SyncGroupWaiter {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            outcome: Mutex::new(None),
            cv: Condvar::new(),
        })
    }

    fn complete(&self, res: Result<Vec<MutationResult>>) {
        let mut guard = self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(res);
        self.cv.notify_all();
    }

    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fn wait(&self, timeout: Option<Duration>) -> Result<Vec<MutationResult>> {
        let mut guard = self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let deadline = timeout.map(|d| Instant::now() + d);
        while guard.is_none() {
            match deadline {
                Some(dl) => {
                    let rem = dl.saturating_duration_since(Instant::now());
                    if rem.is_zero() {
                        return Err(LiliaError::new(
                            ErrorCode::Timeout,
                            "group commit sync waiter timed out",
                            true,
                        ));
                    }
                    let (g, _) = self
                        .cv
                        .wait_timeout(guard, rem)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    guard = g;
                }
                None => {
                    guard = self
                        .cv
                        .wait(guard)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
            }
        }
        guard.take().expect("checked guard.is_some()")
    }
}

enum WriteCommand {
    Op(BatchOperation),
    TsPoint {
        series_id: String,
        resolution_ms: i64,
        point: TimeSeriesPoint,
    },
    TsPoints {
        series_id: String,
        resolution_ms: i64,
        points: Vec<TimeSeriesPoint>,
    },
    GroupSync {
        ops: Vec<BatchOperation>,
        waiter: Arc<SyncGroupWaiter>,
    },
}

#[derive(Debug, Default)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
struct FlushSync {
    requested_gen: u64,
    completed_gen: u64,
    last_error: Option<String>,
}

/// Lock-free circular write buffer and micro-batch group-commit coordinator.
pub(crate) struct WriteRingBuffer {
    queue: ArrayQueue<WriteCommand>,
    pub(crate) config: RingBufferConfig,
    pub(crate) overlay: Arc<PendingOverlay>,
    resources: Weak<Resources>,
    lifecycle: Weak<Lifecycle>,
    codec: Arc<dyn ValueCodec>,
    dict_registry: Arc<DictionaryRegistry>,
    json_options: JsonOptions,
    hlc: Arc<HybridClock>,
    hnsw_cache: Arc<std::sync::RwLock<HashMap<String, Arc<std::sync::RwLock<HnswIndex>>>>>,
    events: Arc<tokio::sync::broadcast::Sender<DatabaseEvent>>,
    cas: Arc<crate::cas::CasEngine>,
    closing: AtomicBool,
    enqueued_total: AtomicU64,
    committed_total: AtomicU64,
    batches_committed: AtomicU64,
    coalesced_ts_points: AtomicU64,
    coalesced_kv_ops: AtomicU64,
    dropped_oldest_total: AtomicU64,
    rejected_total: AtomicU64,
    flush_errors_total: AtomicU64,
    last_commit_latency_us: AtomicU64,
    wake_pair: (Mutex<bool>, Condvar),
    space_pair: (Mutex<()>, Condvar),
    flush_pair: (Mutex<FlushSync>, Condvar),
}

impl std::fmt::Debug for WriteRingBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteRingBuffer")
            .field("capacity", &self.config.capacity)
            .field("queued_len", &self.queue.len())
            .finish_non_exhaustive()
    }
}

/// Manages lifecycle and background worker shutdown for [`WriteRingBuffer`].
#[derive(Debug)]
pub(crate) struct RingBufferEngine {
    pub(crate) core: Arc<WriteRingBuffer>,
    #[cfg(not(target_arch = "wasm32"))]
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl RingBufferEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        config: &RingBufferConfig,
        resources: &Arc<Resources>,
        lifecycle: &Arc<Lifecycle>,
        codec: Arc<dyn ValueCodec>,
        dict_registry: Arc<DictionaryRegistry>,
        json_options: JsonOptions,
        hlc: Arc<HybridClock>,
        hnsw_cache: Arc<std::sync::RwLock<HashMap<String, Arc<std::sync::RwLock<HnswIndex>>>>>,
        events: Arc<tokio::sync::broadcast::Sender<DatabaseEvent>>,
        cas: Arc<crate::cas::CasEngine>,
    ) -> Result<Self> {
        let config = config.normalized();
        let queue = ArrayQueue::new(config.capacity);
        let core = Arc::new(WriteRingBuffer {
            queue,
            config,
            overlay: Arc::new(PendingOverlay::new()),
            resources: Arc::downgrade(resources),
            lifecycle: Arc::downgrade(lifecycle),
            codec,
            dict_registry,
            json_options,
            hlc,
            hnsw_cache,
            events,
            cas,
            closing: AtomicBool::new(false),
            enqueued_total: AtomicU64::new(0),
            committed_total: AtomicU64::new(0),
            batches_committed: AtomicU64::new(0),
            coalesced_ts_points: AtomicU64::new(0),
            coalesced_kv_ops: AtomicU64::new(0),
            dropped_oldest_total: AtomicU64::new(0),
            rejected_total: AtomicU64::new(0),
            flush_errors_total: AtomicU64::new(0),
            last_commit_latency_us: AtomicU64::new(0),
            wake_pair: (Mutex::new(false), Condvar::new()),
            space_pair: (Mutex::new(()), Condvar::new()),
            flush_pair: (Mutex::new(FlushSync::default()), Condvar::new()),
        });

        #[cfg(not(target_arch = "wasm32"))]
        let worker = {
            let worker_core = Arc::clone(&core);
            let handle = std::thread::Builder::new()
                .name("lilia-ringbuffer-flusher".to_owned())
                .spawn(move || group_commit_worker_loop(&worker_core))
                .map_err(|e| {
                    LiliaError::new(
                        ErrorCode::Storage,
                        format!("failed to spawn ring buffer flusher thread: {e}"),
                        false,
                    )
                })?;
            Mutex::new(Some(handle))
        };

        Ok(Self {
            core,
            #[cfg(not(target_arch = "wasm32"))]
            worker,
        })
    }

    pub fn shutdown_and_drain(&self) {
        self.core.closing.store(true, Ordering::Release);
        self.core.wake_flusher();
        self.core.space_pair.1.notify_all();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut worker_guard = self
                .worker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(handle) = worker_guard.take() {
                let _ = handle.join();
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = drain_all_sync(&self.core);
        }
    }
}

impl Drop for RingBufferEngine {
    fn drop(&mut self) {
        self.shutdown_and_drain();
    }
}

impl WriteRingBuffer {
    #[inline]
    fn overlay_enabled(&self) -> bool {
        self.config.enable_read_overlay
            && !matches!(self.config.overflow_policy, OverflowPolicy::DropOldest)
    }

    #[inline]
    fn wake_flusher(&self) {
        let (lock, cv) = &self.wake_pair;
        let mut signaled = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *signaled = true;
        cv.notify_one();
    }

    fn record_overlay_for_op(&self, op: &BatchOperation) -> Option<OverlayTag> {
        if !self.overlay_enabled() {
            return None;
        }
        match op {
            BatchOperation::KvSet {
                namespace,
                key,
                value,
                if_version: None,
                expires_at_ms,
            } => {
                if let Ok(encoded) = self.codec.encode(value) {
                    self.overlay.record_kv(
                        namespace,
                        key,
                        Some((encoded.into_owned(), *expires_at_ms)),
                    );
                    return Some(OverlayTag::Kv {
                        namespace: namespace.clone(),
                        key: key.clone(),
                    });
                }
                None
            }
            BatchOperation::KvDelete {
                namespace,
                key,
                if_version: None,
            } => {
                self.overlay.record_kv(namespace, key, None);
                Some(OverlayTag::Kv {
                    namespace: namespace.clone(),
                    key: key.clone(),
                })
            }
            BatchOperation::JsonPut {
                space,
                id,
                value,
                if_version: None,
            } => {
                self.overlay.record_json_from_serde(space, id, Some(value));
                Some(OverlayTag::Json {
                    space: space.clone(),
                    id: id.clone(),
                })
            }
            BatchOperation::JsonDelete {
                space,
                id,
                if_version: None,
            } => {
                self.overlay.record_json(space, id, None);
                Some(OverlayTag::Json {
                    space: space.clone(),
                    id: id.clone(),
                })
            }
            _ => None,
        }
    }

    fn rollback_overlay_tag(&self, tag: Option<OverlayTag>) {
        match tag {
            Some(OverlayTag::Kv { namespace, key }) => self.overlay.release_kv(&namespace, &key, 1),
            Some(OverlayTag::Json { space, id }) => self.overlay.release_json(&space, &id, 1),
            None => {}
        }
    }

    fn push_command(&self, mut cmd: WriteCommand, force_wake: bool) -> Result<()> {
        if self.closing.load(Ordering::Acquire) {
            return Err(LiliaError::new(
                ErrorCode::Closed,
                "database ring buffer is closing",
                false,
            ));
        }

        match self.config.overflow_policy {
            OverflowPolicy::DropOldest => {
                let evicted = self.queue.force_push(cmd);
                if evicted.is_some() {
                    self.dropped_oldest_total.fetch_add(1, Ordering::Relaxed);
                }
            }
            OverflowPolicy::Reject => {
                if self.queue.push(cmd).is_err() {
                    self.rejected_total.fetch_add(1, Ordering::Relaxed);
                    self.wake_flusher();
                    return Err(LiliaError::new(
                        ErrorCode::Busy,
                        "write ring buffer is full",
                        true,
                    ));
                }
            }
            OverflowPolicy::Block { timeout_ms } => {
                let deadline = timeout_ms.map(|ms| Instant::now() + Duration::from_millis(ms));
                loop {
                    if self.closing.load(Ordering::Acquire) {
                        return Err(LiliaError::new(
                            ErrorCode::Closed,
                            "database ring buffer is closing",
                            false,
                        ));
                    }
                    match self.queue.push(cmd) {
                        Ok(()) => break,
                        Err(returned) => {
                            cmd = returned;
                            self.wake_flusher();
                            let (lock, cv) = &self.space_pair;
                            let guard = lock
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            // Re-check after locking to avoid lost wakeups.
                            if !self.queue.is_full() {
                                drop(guard);
                                continue;
                            }
                            if let Some(dl) = deadline {
                                let rem = dl.saturating_duration_since(Instant::now());
                                if rem.is_zero() {
                                    self.rejected_total.fetch_add(1, Ordering::Relaxed);
                                    return Err(LiliaError::new(
                                        ErrorCode::Timeout,
                                        "timed out waiting for ring buffer slot",
                                        true,
                                    ));
                                }
                                let _ = cv
                                    .wait_timeout(guard, rem.min(Duration::from_millis(10)))
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                            } else {
                                let _ = cv
                                    .wait_timeout(guard, Duration::from_millis(10))
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                            }
                        }
                    }
                }
            }
        }

        self.enqueued_total.fetch_add(1, Ordering::Relaxed);
        if force_wake || self.queue.len() >= self.config.watermark_trigger {
            self.wake_flusher();
        }
        Ok(())
    }

    pub fn push_op(&self, op: BatchOperation) -> Result<()> {
        let tag = self.record_overlay_for_op(&op);
        if let Err(err) = self.push_command(WriteCommand::Op(op), false) {
            self.rollback_overlay_tag(tag);
            return Err(err);
        }
        Ok(())
    }

    pub fn push_ts_point(
        &self,
        series_id: &str,
        resolution_ms: i64,
        point: TimeSeriesPoint,
    ) -> Result<()> {
        let has_overlay = self.overlay_enabled();
        if has_overlay {
            self.overlay
                .record_ts(series_id, resolution_ms, std::slice::from_ref(&point));
        }
        let cmd = WriteCommand::TsPoint {
            series_id: series_id.to_owned(),
            resolution_ms,
            point,
        };
        if let Err(err) = self.push_command(cmd, false) {
            if has_overlay {
                self.overlay.release_ts(series_id, resolution_ms, 1);
            }
            return Err(err);
        }
        Ok(())
    }

    pub fn push_ts_points(
        &self,
        series_id: &str,
        resolution_ms: i64,
        points: Vec<TimeSeriesPoint>,
    ) -> Result<()> {
        if points.is_empty() {
            return Ok(());
        }
        let has_overlay = self.overlay_enabled();
        if has_overlay {
            self.overlay.record_ts(series_id, resolution_ms, &points);
        }
        let cmd = WriteCommand::TsPoints {
            series_id: series_id.to_owned(),
            resolution_ms,
            points,
        };
        if let Err(err) = self.push_command(cmd, false) {
            if has_overlay {
                self.overlay.release_ts(series_id, resolution_ms, 1);
            }
            return Err(err);
        }
        Ok(())
    }

    pub fn push_group_sync(
        &self,
        ops: Vec<BatchOperation>,
        timeout: Option<Duration>,
    ) -> Result<Vec<MutationResult>> {
        if ops.is_empty() {
            return Ok(Vec::new());
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = timeout;
            self.push_command(
                WriteCommand::GroupSync {
                    ops: ops.clone(),
                    waiter: SyncGroupWaiter::new(),
                },
                true,
            )?;
            // Inline fallback on wasm32 commits directly.
            let mut results = Vec::new();
            drain_one_pass(self, true, &mut Some(&mut results))?;
            Ok(results)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let waiter = SyncGroupWaiter::new();
            let mut tags = Vec::new();
            for op in &ops {
                tags.push(self.record_overlay_for_op(op));
            }
            if let Err(err) = self.push_command(
                WriteCommand::GroupSync {
                    ops,
                    waiter: Arc::clone(&waiter),
                },
                true,
            ) {
                for t in tags {
                    self.rollback_overlay_tag(t);
                }
                return Err(err);
            }
            waiter.wait(timeout)
        }
    }

    pub fn flush(&self) -> Result<RingBufferStats> {
        #[cfg(target_arch = "wasm32")]
        {
            drain_all_sync(self)?;
            Ok(self.stats())
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let (lock, cv) = &self.flush_pair;
            let mut sync = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            sync.requested_gen = sync.requested_gen.saturating_add(1);
            let target = sync.requested_gen;

            self.wake_flusher();

            while sync.completed_gen < target {
                if self.closing.load(Ordering::Acquire) && self.queue.is_empty() {
                    break;
                }
                sync = cv
                    .wait(sync)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            let err_opt = sync.last_error.clone();
            drop(sync);

            if let Some(err_msg) = err_opt {
                return Err(LiliaError::new(ErrorCode::Storage, err_msg, true));
            }
            Ok(self.stats())
        }
    }

    #[must_use]
    pub fn stats(&self) -> RingBufferStats {
        let last_error = self
            .flush_pair
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last_error
            .clone();
        RingBufferStats {
            capacity: self.config.capacity,
            queued_len: self.queue.len(),
            enqueued_total: self.enqueued_total.load(Ordering::Relaxed),
            committed_total: self.committed_total.load(Ordering::Relaxed),
            batches_committed: self.batches_committed.load(Ordering::Relaxed),
            coalesced_ts_points: self.coalesced_ts_points.load(Ordering::Relaxed),
            coalesced_kv_ops: self.coalesced_kv_ops.load(Ordering::Relaxed),
            dropped_oldest_total: self.dropped_oldest_total.load(Ordering::Relaxed),
            rejected_total: self.rejected_total.load(Ordering::Relaxed),
            flush_errors_total: self.flush_errors_total.load(Ordering::Relaxed),
            overlay_entries: self.overlay.total_entries(),
            last_error,
            last_commit_latency_us: self.last_commit_latency_us.load(Ordering::Relaxed),
        }
    }
}

enum OverlayTag {
    Kv { namespace: String, key: Vec<u8> },
    Json { space: String, id: String },
}

/// Cloneable high-speed producer handle into `LiliaDB`'s lock-free [`WriteRingBuffer`].
#[derive(Debug, Clone)]
pub struct RingBufferHandle {
    pub(crate) core: Arc<WriteRingBuffer>,
}

impl RingBufferHandle {
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn push_kv(&self, namespace: &str, key: &[u8], value: &[u8]) -> Result<()> {
        self.core.push_op(BatchOperation::KvSet {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
            value: value.to_vec(),
            if_version: None,
            expires_at_ms: None,
        })
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn push_kv_with_ttl(
        &self,
        namespace: &str,
        key: &[u8],
        value: &[u8],
        expires_at_ms: Option<i64>,
    ) -> Result<()> {
        self.core.push_op(BatchOperation::KvSet {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
            value: value.to_vec(),
            if_version: None,
            expires_at_ms,
        })
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn push_kv_delete(&self, namespace: &str, key: &[u8]) -> Result<()> {
        self.core.push_op(BatchOperation::KvDelete {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
            if_version: None,
        })
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn push_json(&self, space: &str, id: &str, value: serde_json::Value) -> Result<()> {
        self.core.push_op(BatchOperation::JsonPut {
            space: space.to_owned(),
            id: id.to_owned(),
            value,
            if_version: None,
        })
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn push_ts_point(
        &self,
        series_id: &str,
        resolution_ms: i64,
        point: TimeSeriesPoint,
    ) -> Result<()> {
        self.core.push_ts_point(series_id, resolution_ms, point)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn push_ts_points(
        &self,
        series_id: &str,
        resolution_ms: i64,
        points: Vec<TimeSeriesPoint>,
    ) -> Result<()> {
        self.core.push_ts_points(series_id, resolution_ms, points)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn push_op(&self, op: BatchOperation) -> Result<()> {
        self.core.push_op(op)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn commit_sync(
        &self,
        ops: Vec<BatchOperation>,
        timeout: Option<Duration>,
    ) -> Result<Vec<MutationResult>> {
        self.core.push_group_sync(ops, timeout)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn flush(&self) -> Result<RingBufferStats> {
        self.core.flush()
    }

    #[must_use]
    pub fn stats(&self) -> RingBufferStats {
        self.core.stats()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.core.queue.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.core.queue.is_empty()
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn group_commit_worker_loop(core: &WriteRingBuffer) {
    let flush_interval = Duration::from_millis(core.config.flush_interval_ms);
    loop {
        let closing = core.closing.load(Ordering::Acquire);
        if core.queue.is_empty() {
            // Ensure any pending flush barrier sees the empty queue cleanly.
            advance_flush_if_empty(core);
            if closing {
                break;
            }
            let (lock, cv) = &core.wake_pair;
            let mut signaled = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while !*signaled && core.queue.is_empty() && !core.closing.load(Ordering::Acquire) {
                let (g, timeout_res) = cv
                    .wait_timeout(signaled, flush_interval)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                signaled = g;
                if timeout_res.timed_out() {
                    break;
                }
            }
            *signaled = false;
            continue;
        }

        let target_gen = core
            .flush_pair
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .requested_gen;
        let drain_all = closing
            || target_gen
                > core
                    .flush_pair
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .completed_gen;

        let res = drain_pass(core, drain_all);
        let err_opt = res.err().map(|e| e.to_string());
        let (lock, cv) = &core.flush_pair;
        let mut sync = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sync.last_error = err_opt;
        let latest_requested = sync.requested_gen;
        if latest_requested > sync.completed_gen {
            sync.completed_gen = latest_requested;
        }
        cv.notify_all();
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn advance_flush_if_empty(core: &WriteRingBuffer) {
    let (lock, cv) = &core.flush_pair;
    let mut sync = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if core.queue.is_empty() && sync.completed_gen < sync.requested_gen {
        sync.completed_gen = sync.requested_gen;
        cv.notify_all();
    }
}

#[cfg(target_arch = "wasm32")]
fn drain_all_sync(core: &WriteRingBuffer) -> Result<()> {
    drain_pass(core, true)?;
    Ok(())
}

fn drain_pass(core: &WriteRingBuffer, drain_all: bool) -> Result<usize> {
    let mut total_drained = 0usize;
    let snapshot_target = core.queue.len();
    loop {
        let mut unused = None;
        let batch_n = drain_one_pass(core, drain_all, &mut unused)?;
        total_drained += batch_n;
        if batch_n == 0 || !drain_all || core.queue.is_empty() || total_drained >= snapshot_target {
            break;
        }
    }
    Ok(total_drained)
}

#[allow(clippy::too_many_lines)]
fn drain_one_pass(
    core: &WriteRingBuffer,
    _prefer_full: bool,
    sync_capture: &mut Option<&mut Vec<MutationResult>>,
) -> Result<usize> {
    let limit = core.config.max_batch_size;
    let mut popped = Vec::with_capacity(limit.min(core.queue.len() + 8));
    for _ in 0..limit {
        let Some(cmd) = core.queue.pop() else {
            break;
        };
        popped.push(cmd);
    }
    if popped.is_empty() {
        return Ok(0);
    }
    // Wake blocked producers immediately after freeing queue capacity.
    core.space_pair.1.notify_all();

    let raw_count = popped.len();
    let overlay_on = core.overlay_enabled();

    let mut ts_groups: HashMap<(String, i64), Vec<TimeSeriesPoint>> = HashMap::new();
    let mut ts_overlay_counts: HashMap<(String, i64), u32> = HashMap::new();
    let mut kv_overlay_counts: HashMap<(String, Vec<u8>), u32> = HashMap::new();
    let mut json_overlay_counts: HashMap<(String, String), u32> = HashMap::new();

    // Standalone async ops (eligible for optional KV coalescing) vs GroupSync ops (preserve exact indices).
    let mut async_ops: Vec<BatchOperation> = Vec::new();
    let mut sync_waiters: Vec<(Arc<SyncGroupWaiter>, Vec<BatchOperation>)> = Vec::new();
    let mut coalesced_ts_count = 0u64;

    for cmd in popped {
        match cmd {
            WriteCommand::Op(op) => {
                if overlay_on {
                    record_op_release_count(&op, &mut kv_overlay_counts, &mut json_overlay_counts);
                }
                async_ops.push(op);
            }
            WriteCommand::TsPoint {
                series_id,
                resolution_ms,
                point,
            } => {
                if overlay_on {
                    *ts_overlay_counts
                        .entry((series_id.clone(), resolution_ms))
                        .or_insert(0) += 1;
                }
                coalesced_ts_count += 1;
                ts_groups
                    .entry((series_id, resolution_ms))
                    .or_default()
                    .push(point);
            }
            WriteCommand::TsPoints {
                series_id,
                resolution_ms,
                points,
            } => {
                if overlay_on {
                    *ts_overlay_counts
                        .entry((series_id.clone(), resolution_ms))
                        .or_insert(0) += 1;
                }
                coalesced_ts_count += points.len() as u64;
                ts_groups
                    .entry((series_id, resolution_ms))
                    .or_default()
                    .extend(points);
            }
            WriteCommand::GroupSync { ops, waiter } => {
                if overlay_on {
                    for op in &ops {
                        record_op_release_count(
                            op,
                            &mut kv_overlay_counts,
                            &mut json_overlay_counts,
                        );
                    }
                }
                sync_waiters.push((waiter, ops));
            }
        }
    }

    let mut coalesced_kv = 0u64;
    if core.config.coalesce_kv_writes && async_ops.len() > 1 {
        let mut last_idx_by_key: HashMap<(String, Vec<u8>), usize> = HashMap::new();
        for (i, op) in async_ops.iter().enumerate() {
            match op {
                BatchOperation::KvSet {
                    namespace,
                    key,
                    if_version: None,
                    ..
                }
                | BatchOperation::KvDelete {
                    namespace,
                    key,
                    if_version: None,
                } => {
                    last_idx_by_key.insert((namespace.clone(), key.clone()), i);
                }
                _ => {}
            }
        }
        let mut filtered = Vec::with_capacity(async_ops.len());
        for (i, op) in async_ops.into_iter().enumerate() {
            let keep = match &op {
                BatchOperation::KvSet {
                    namespace,
                    key,
                    if_version: None,
                    ..
                }
                | BatchOperation::KvDelete {
                    namespace,
                    key,
                    if_version: None,
                } => {
                    last_idx_by_key
                        .get(&(namespace.clone(), key.clone()))
                        .copied()
                        == Some(i)
                }
                _ => true,
            };
            if keep {
                filtered.push(op);
            } else {
                coalesced_kv += 1;
            }
        }
        async_ops = filtered;
    }

    let start = Instant::now();
    let commit_outcome: Result<Vec<Vec<MutationResult>>> = (|| {
        let res_arc = core.resources.upgrade().ok_or_else(|| {
            LiliaError::new(ErrorCode::Closed, "database resources dropped", false)
        })?;
        let lc_arc = core.lifecycle.upgrade().ok_or_else(|| {
            LiliaError::new(ErrorCode::Closed, "database lifecycle dropped", false)
        })?;
        let mut writer =
            res_arc.lock_writer_for_drain(&lc_arc, core.closing.load(Ordering::Acquire))?;
        let mut tx = writer.transaction()?;

        if core.config.coalesce_timeseries {
            for ((series_id, resolution_ms), mut pts) in ts_groups {
                if pts.windows(2).any(|w| w[0].timestamp > w[1].timestamp) {
                    pts.sort_by_key(|p| p.timestamp);
                }
                append_chunk_in_tx(&tx, &series_id, resolution_ms, &pts, true, res_arc.shards())?;
            }
        } else {
            for ((series_id, resolution_ms), pts) in ts_groups {
                for p in &pts {
                    append_chunk_in_tx(
                        &tx,
                        &series_id,
                        resolution_ms,
                        std::slice::from_ref(p),
                        false,
                        res_arc.shards(),
                    )?;
                }
            }
        }

        let mut apply_one = |op: &BatchOperation| -> Result<MutationResult> {
            let mut res = apply(
                &mut tx,
                op,
                core.codec.as_ref(),
                &core.dict_registry,
                &core.json_options,
                core.hlc.as_ref(),
                &core.cas,
                res_arc.shards(),
                None,
            )?;
            sync_hnsw_for_op_with_cache(&mut tx, &core.hnsw_cache, core.codec.as_ref(), op)?;
            res.tier = StorageTier::Buffered;
            res.buffered = false;
            Ok(res)
        };

        let mut async_results = Vec::with_capacity(async_ops.len());
        for op in &async_ops {
            async_results.push(apply_one(op)?);
        }

        let mut waiter_results = Vec::with_capacity(sync_waiters.len());
        for (_, ops) in &sync_waiters {
            let mut res_vec = Vec::with_capacity(ops.len());
            for op in ops {
                res_vec.push(apply_one(op)?);
            }
            waiter_results.push(res_vec);
        }

        let mut ledger_entries = crate::ledger::operation_entries(&async_ops, &async_results);
        for ((_, ops), results) in sync_waiters.iter().zip(&waiter_results) {
            ledger_entries.extend(crate::ledger::operation_entries(ops, results));
        }
        res_arc
            .ledger
            .record_commit(tx.conn(), ledger_entries, crate::storage::now_ms())?;
        tx.commit()?;
        Ok(waiter_results)
    })();

    // Always release overlay pending counts so reads never get stuck after an error/commit.
    if overlay_on {
        for ((ns, key), count) in kv_overlay_counts {
            core.overlay.release_kv(&ns, &key, count);
        }
        for ((space, id), count) in json_overlay_counts {
            core.overlay.release_json(&space, &id, count);
        }
        for ((sid, res_ms), count) in ts_overlay_counts {
            core.overlay.release_ts(&sid, res_ms, count);
        }
    }

    match commit_outcome {
        Ok(waiter_results) => {
            let latency_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
            core.committed_total
                .fetch_add(raw_count as u64, Ordering::Relaxed);
            core.batches_committed.fetch_add(1, Ordering::Relaxed);
            if coalesced_ts_count > 1 {
                core.coalesced_ts_points
                    .fetch_add(coalesced_ts_count, Ordering::Relaxed);
            }
            if coalesced_kv > 0 {
                core.coalesced_kv_ops
                    .fetch_add(coalesced_kv, Ordering::Relaxed);
            }
            core.last_commit_latency_us
                .store(latency_us, Ordering::Relaxed);

            for ((waiter, _), res) in sync_waiters.into_iter().zip(waiter_results) {
                if let Some(cap) = sync_capture.as_deref_mut() {
                    cap.extend(res.iter().cloned());
                }
                waiter.complete(Ok(res));
            }
            if let Some(res_arc) = core.resources.upgrade() {
                res_arc.notify_checkpointer();
            }
            if core.events.receiver_count() > 0 {
                let _ = core.events.send(DatabaseEvent::RingBufferFlushed {
                    ops_committed: raw_count as u64,
                    coalesced_ts_points: coalesced_ts_count,
                    latency_us,
                });
            }
            Ok(raw_count)
        }
        Err(err) => {
            core.flush_errors_total.fetch_add(1, Ordering::Relaxed);
            for (waiter, _) in sync_waiters {
                waiter.complete(Err(LiliaError::new(
                    err.code,
                    err.message.clone(),
                    err.retryable,
                )));
            }
            Err(err)
        }
    }
}

fn record_op_release_count(
    op: &BatchOperation,
    kv_counts: &mut HashMap<(String, Vec<u8>), u32>,
    json_counts: &mut HashMap<(String, String), u32>,
) {
    match op {
        BatchOperation::KvSet {
            namespace,
            key,
            if_version: None,
            ..
        }
        | BatchOperation::KvDelete {
            namespace,
            key,
            if_version: None,
        } => {
            *kv_counts
                .entry((namespace.clone(), key.clone()))
                .or_insert(0) += 1;
        }
        BatchOperation::JsonPut {
            space,
            id,
            if_version: None,
            ..
        }
        | BatchOperation::JsonDelete {
            space,
            id,
            if_version: None,
        } => {
            *json_counts.entry((space.clone(), id.clone())).or_insert(0) += 1;
        }
        _ => {}
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use vtab_buffered::*;

#[cfg(not(target_arch = "wasm32"))]
mod vtab_buffered {
    use super::{
        now_ms, Arc, PendingOverlay, SliceVTab, VTabValue, ValueCodec, VirtualTableProvider,
    };

    pub(crate) fn build_buffered_kv_vtab(
        overlay: &Arc<PendingOverlay>,
        codec: &Arc<dyn ValueCodec>,
    ) -> Arc<dyn VirtualTableProvider> {
        let overlay = Arc::clone(overlay);
        let codec = Arc::clone(codec);
        SliceVTab::new(
        "CREATE TABLE x(namespace TEXT, key BLOB, value BLOB, version INTEGER, expires_at_ms INTEGER, deleted INTEGER)",
        true,
        move |ns, key| {
            let now = now_ms();
            if let (Some(n), Some(k)) = (ns, key) {
                return overlay
                    .kv
                    .get(n, k, now, false)
                    .into_iter()
                    .map(|snap| match snap.value.state {
                        Some((v, ver, exp)) => {
                            let decoded = codec.decode(&v).unwrap_or(v);
                            (n.to_owned(), k.to_vec(), decoded, ver, exp, 0i64)
                        }
                        None => (n.to_owned(), k.to_vec(), Vec::new(), 0, None, 1i64),
                    })
                    .collect();
            }
            overlay
                .kv
                .scan(ns, None, usize::MAX, now)
                .into_iter()
                .map(|(n, k, snap)| match snap.value.state {
                    Some((v, ver, exp)) => {
                        let decoded = codec.decode(&v).unwrap_or(v);
                        (n, k, decoded, ver, exp, 0i64)
                    }
                    None => (n, k, Vec::new(), 0, None, 1i64),
                })
                .collect()
        },
        |r, col| match col {
            0 => VTabValue::Text(r.0.clone()),
            1 => VTabValue::Blob(r.1.clone()),
            2 => VTabValue::Blob(r.2.clone()),
            3 => VTabValue::Int(i64::try_from(r.3).unwrap_or(i64::MAX)),
            4 => r.4.map_or(VTabValue::Null, VTabValue::Int),
            5 => VTabValue::Int(r.5),
            _ => VTabValue::Null,
        },
    )
    }

    pub(crate) fn build_buffered_json_vtab(
        overlay: &Arc<PendingOverlay>,
    ) -> Arc<dyn VirtualTableProvider> {
        let overlay = Arc::clone(overlay);
        SliceVTab::new(
            "CREATE TABLE x(space TEXT, id TEXT, value TEXT, version INTEGER, deleted INTEGER)",
            true,
            move |space, id| {
                let now = now_ms();
                if let (Some(s), Some(i)) = (space, id) {
                    return overlay
                        .json
                        .get(s, i, now, false)
                        .into_iter()
                        .map(|snap| {
                            let id_str = String::from_utf8_lossy(i).into_owned();
                            match snap.value.state {
                                Some((ref aligned, ver, _)) => {
                                    let archived = unsafe {
                                        lilia_core::json::access_archived_doc_unchecked(aligned)
                                    };
                                    let json_val = archived.to_serde();
                                    (
                                        s.to_owned(),
                                        id_str,
                                        serde_json::to_string(&json_val)
                                            .unwrap_or_else(|_| "null".into()),
                                        ver,
                                        0i64,
                                    )
                                }
                                None => (s.to_owned(), id_str, "null".into(), 0, 1i64),
                            }
                        })
                        .collect();
                }
                overlay
                    .json
                    .scan(space, None, usize::MAX, now)
                    .into_iter()
                    .map(|(s, i, snap)| {
                        let id_str = String::from_utf8_lossy(&i).into_owned();
                        match snap.value.state {
                            Some((ref aligned, ver, _)) => {
                                let archived = unsafe {
                                    lilia_core::json::access_archived_doc_unchecked(aligned)
                                };
                                let json_val = archived.to_serde();
                                (
                                    s,
                                    id_str,
                                    serde_json::to_string(&json_val)
                                        .unwrap_or_else(|_| "null".into()),
                                    ver,
                                    0i64,
                                )
                            }
                            None => (s, id_str, "null".into(), 0, 1i64),
                        }
                    })
                    .collect()
            },
            |r, col| match col {
                0 => VTabValue::Text(r.0.clone()),
                1 => VTabValue::Text(r.1.clone()),
                2 => VTabValue::Text(r.2.clone()),
                3 => VTabValue::Int(i64::try_from(r.3).unwrap_or(i64::MAX)),
                4 => VTabValue::Int(r.4),
                _ => VTabValue::Null,
            },
        )
    }
}
