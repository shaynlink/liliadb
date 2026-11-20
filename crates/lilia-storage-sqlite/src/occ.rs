use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lilia_core::{
    BatchOperation, ErrorCode, JsonEntry, KvEntry, LiliaError, MutationResult, Result,
};
use parking_lot::{Condvar, Mutex, RwLock};

use crate::engine::Database;

/// Primary resource key tracked for optimistic concurrency control (OCC).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OccKey {
    Json {
        space: String,
        id: String,
    },
    Kv {
        namespace: String,
        key: Vec<u8>,
    },
    GraphNode {
        graph: String,
        id: String,
    },
    GraphEdge {
        graph: String,
        source: String,
        target: String,
        kind: String,
    },
    Fts {
        collection: String,
        doc_id: String,
    },
    Vector {
        collection: String,
        id: String,
    },
    Crdt {
        space: String,
        id: String,
    },
}

impl OccKey {
    /// Extract the primary resource key modified by a [`BatchOperation`].
    #[must_use]
    pub fn from_op(op: &BatchOperation) -> Option<Self> {
        match op {
            BatchOperation::JsonPut { space, id, .. }
            | BatchOperation::JsonPutRaw { space, id, .. }
            | BatchOperation::JsonDelete { space, id, .. }
            | BatchOperation::JsonPutHlc { space, id, .. }
            | BatchOperation::BitemporalPut { space, id, .. }
            | BatchOperation::BitemporalDelete { space, id, .. } => Some(Self::Json {
                space: space.clone(),
                id: id.clone(),
            }),
            BatchOperation::KvSet { namespace, key, .. }
            | BatchOperation::KvDelete { namespace, key, .. } => Some(Self::Kv {
                namespace: namespace.clone(),
                key: key.clone(),
            }),
            BatchOperation::GraphSetNode { graph, id, .. }
            | BatchOperation::GraphDeleteNode { graph, id } => Some(Self::GraphNode {
                graph: graph.clone(),
                id: id.clone(),
            }),
            BatchOperation::GraphAddEdge {
                graph,
                source,
                target,
                kind,
                ..
            }
            | BatchOperation::GraphRemoveEdge {
                graph,
                source,
                target,
                kind,
            } => Some(Self::GraphEdge {
                graph: graph.clone(),
                source: source.clone(),
                target: target.clone(),
                kind: kind.clone(),
            }),
            BatchOperation::FtsIndex {
                collection, doc_id, ..
            }
            | BatchOperation::FtsDelete {
                collection, doc_id, ..
            } => Some(Self::Fts {
                collection: collection.clone(),
                doc_id: doc_id.clone(),
            }),
            BatchOperation::VectorSet { collection, id, .. }
            | BatchOperation::VectorDelete { collection, id } => Some(Self::Vector {
                collection: collection.clone(),
                id: id.clone(),
            }),
            BatchOperation::CrdtPnCounterIncrement { space, id, .. }
            | BatchOperation::CrdtPnCounterDecrement { space, id, .. }
            | BatchOperation::CrdtOrSetAdd { space, id, .. }
            | BatchOperation::CrdtOrSetRemove { space, id, .. }
            | BatchOperation::CrdtJsonSetField { space, id, .. }
            | BatchOperation::CrdtJsonDeleteField { space, id, .. }
            | BatchOperation::CrdtJsonMerge { space, id, .. } => Some(Self::Crdt {
                space: space.clone(),
                id: id.clone(),
            }),
        }
    }
}

/// Representation of a staged value in the transaction workspace for Read-Your-Own-Writes.
#[derive(Debug, Clone)]
pub(crate) enum WorkspaceValue<T> {
    Put(T),
    Deleted,
}

/// Speculative savepoint frame in RAM for [`OccTransaction`].
#[derive(Debug, Clone)]
pub(crate) struct OccSavepointFrame {
    pub name: String,
    pub ops_len: usize,
    pub staged_json: HashMap<(String, String), WorkspaceValue<JsonEntry>>,
    pub staged_kv: HashMap<(String, Vec<u8>), WorkspaceValue<KvEntry>>,
    pub staged_edges: HashMap<(String, String, String, String), WorkspaceValue<f64>>,
    pub read_set: HashMap<OccKey, u64>,
    pub write_set: HashSet<OccKey>,
}

/// In-memory transactional workspace buffering mutations and tracking the Read Set / Write Set.
#[derive(Debug, Default, Clone)]
pub(crate) struct OccWorkspace {
    pub staged_ops: Vec<BatchOperation>,
    pub staged_json: HashMap<(String, String), WorkspaceValue<JsonEntry>>,
    pub staged_kv: HashMap<(String, Vec<u8>), WorkspaceValue<KvEntry>>,
    pub staged_edges: HashMap<(String, String, String, String), WorkspaceValue<f64>>,
    pub read_set: HashMap<OccKey, u64>,
    pub write_set: HashSet<OccKey>,
    pub savepoint_stack: Vec<OccSavepointFrame>,
}

impl OccWorkspace {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_op(&mut self, op: BatchOperation) {
        if let Some(key) = OccKey::from_op(&op) {
            self.write_set.insert(key);
        }
        self.staged_ops.push(op);
    }
}

/// Configuration options for the in-memory OCC engine.
#[derive(Debug, Clone)]
pub struct OccOptions {
    /// Maximum number of validated transactions to merge into a single SQLite write block.
    pub max_batch_size: usize,
    /// Timeout in milliseconds when waiting for the group commit to complete on disk.
    pub commit_timeout_ms: u64,
    /// Whether to check write-write conflicts between concurrent transactions.
    pub conflict_check_writes: bool,
}

impl Default for OccOptions {
    fn default() -> Self {
        Self {
            max_batch_size: 100,
            commit_timeout_ms: 10_000,
            conflict_check_writes: true,
        }
    }
}

/// Cumulative telemetry metrics for the in-memory OCC engine.
#[derive(Debug, Default)]
pub struct OccStats {
    pub transactions_started: AtomicU64,
    pub transactions_committed: AtomicU64,
    pub conflicts_detected: AtomicU64,
    pub group_commits_executed: AtomicU64,
    pub transactions_merged: AtomicU64,
}

impl OccStats {
    #[must_use]
    pub fn snapshot(&self) -> OccStatsSnapshot {
        OccStatsSnapshot {
            transactions_started: self.transactions_started.load(Ordering::Relaxed),
            transactions_committed: self.transactions_committed.load(Ordering::Relaxed),
            conflicts_detected: self.conflicts_detected.load(Ordering::Relaxed),
            group_commits_executed: self.group_commits_executed.load(Ordering::Relaxed),
            transactions_merged: self.transactions_merged.load(Ordering::Relaxed),
        }
    }
}

/// Point-in-time snapshot of [`OccStats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OccStatsSnapshot {
    pub transactions_started: u64,
    pub transactions_committed: u64,
    pub conflicts_detected: u64,
    pub group_commits_executed: u64,
    pub transactions_merged: u64,
}

/// Sharded version registry storing the highest committed version for each [`OccKey`] in RAM.
#[derive(Debug)]
pub(crate) struct ShardedVersionRegistry {
    shards: Vec<RwLock<HashMap<OccKey, u64>>>,
    shard_count: usize,
}

impl ShardedVersionRegistry {
    pub fn new(shard_count: usize) -> Self {
        let count = shard_count.next_power_of_two().max(16);
        let mut shards = Vec::with_capacity(count);
        for _ in 0..count {
            shards.push(RwLock::new(HashMap::new()));
        }
        Self {
            shards,
            shard_count: count,
        }
    }

    fn shard_index(&self, key: &OccKey) -> usize {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        (hasher.finish() as usize) & (self.shard_count - 1)
    }

    pub fn get(&self, key: &OccKey) -> Option<u64> {
        let idx = self.shard_index(key);
        self.shards[idx].read().get(key).copied()
    }

    pub fn insert(&self, key: OccKey, version: u64) {
        let idx = self.shard_index(&key);
        self.shards[idx].write().insert(key, version);
    }
}

/// Internal item submitted to the group commit coordinator.
#[derive(Debug)]
struct CommitItem {
    ops: Vec<BatchOperation>,
    version: u64,
    waiter: Arc<GroupCommitWaiter>,
}

/// Synchronization waiter completed by the Group Commit Leader.
struct GroupCommitWaiter {
    outcome: Mutex<Option<Result<Vec<MutationResult>>>>,
    cv: Condvar,
}

impl std::fmt::Debug for GroupCommitWaiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupCommitWaiter").finish_non_exhaustive()
    }
}

impl GroupCommitWaiter {
    fn new() -> Self {
        Self {
            outcome: Mutex::new(None),
            cv: Condvar::new(),
        }
    }

    fn complete(&self, res: Result<Vec<MutationResult>>) {
        let mut guard = self.outcome.lock();
        *guard = Some(res);
        self.cv.notify_all();
    }

    fn wait(&self, timeout: Duration) -> Result<Vec<MutationResult>> {
        let mut guard = self.outcome.lock();
        if guard.is_some() {
            return guard.take().expect("checked guard.is_some()");
        }
        let deadline = Instant::now() + timeout;
        while guard.is_none() {
            let now = Instant::now();
            if now >= deadline {
                return Err(LiliaError::new(
                    ErrorCode::Timeout,
                    "OCC group commit waiter timed out",
                    true,
                ));
            }
            let rem = deadline - now;
            let timeout_res = self.cv.wait_for(&mut guard, rem);
            if timeout_res.timed_out() && guard.is_none() {
                return Err(LiliaError::new(
                    ErrorCode::Timeout,
                    "OCC group commit waiter timed out",
                    true,
                ));
            }
        }
        guard.take().expect("checked guard.is_some()")
    }
}

/// Ephemeral in-memory overlay buffering validated modifications pending persistence in SQLite.
#[derive(Debug, Default)]
pub(crate) struct OccOverlay {
    json: RwLock<HashMap<(String, String), (u64, Option<JsonEntry>)>>,
    kv: RwLock<HashMap<(String, Vec<u8>), (u64, Option<KvEntry>)>>,
}

impl OccOverlay {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_json(&self, space: &str, id: &str) -> Option<(u64, Option<JsonEntry>)> {
        self.json
            .read()
            .get(&(space.to_owned(), id.to_owned()))
            .cloned()
    }

    pub fn get_kv(&self, namespace: &str, key: &[u8]) -> Option<(u64, Option<KvEntry>)> {
        self.kv
            .read()
            .get(&(namespace.to_owned(), key.to_vec()))
            .cloned()
    }

    pub fn record_batch(&self, ops: &[BatchOperation], version: u64) {
        let mut json_guard = self.json.write();
        let mut kv_guard = self.kv.write();
        for op in ops {
            match op {
                BatchOperation::JsonPut {
                    space, id, value, ..
                } => {
                    if let Ok(doc) = lilia_core::BinaryDoc::from_json_value(value) {
                        let entry = JsonEntry::new(space, id, doc, version);
                        json_guard.insert((space.clone(), id.clone()), (version, Some(entry)));
                    }
                }
                BatchOperation::JsonDelete { space, id, .. } => {
                    json_guard.insert((space.clone(), id.clone()), (version, None));
                }
                BatchOperation::KvSet {
                    namespace,
                    key,
                    value,
                    ..
                } => {
                    let entry = KvEntry {
                        namespace: namespace.clone(),
                        key: key.clone(),
                        value: value.clone(),
                        version,
                        expires_at_ms: None,
                    };
                    kv_guard.insert((namespace.clone(), key.clone()), (version, Some(entry)));
                }
                BatchOperation::KvDelete { namespace, key, .. } => {
                    kv_guard.insert((namespace.clone(), key.clone()), (version, None));
                }
                _ => {}
            }
        }
    }

    pub fn release_batch(&self, ops: &[BatchOperation], version: u64) {
        let mut json_guard = self.json.write();
        let mut kv_guard = self.kv.write();
        for op in ops {
            match op {
                BatchOperation::JsonPut { space, id, .. }
                | BatchOperation::JsonDelete { space, id, .. } => {
                    let lookup = (space.clone(), id.clone());
                    if let Some((ver, _)) = json_guard.get(&lookup) {
                        if *ver <= version {
                            json_guard.remove(&lookup);
                        }
                    }
                }
                BatchOperation::KvSet { namespace, key, .. }
                | BatchOperation::KvDelete { namespace, key, .. } => {
                    let lookup = (namespace.clone(), key.clone());
                    if let Some((ver, _)) = kv_guard.get(&lookup) {
                        if *ver <= version {
                            kv_guard.remove(&lookup);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// Group commit coordinator that implements cooperative Leader-Follower micro-batching.
#[derive(Debug)]
pub(crate) struct OccGroupCommitCoordinator {
    pending: Mutex<Vec<CommitItem>>,
    leader_active: AtomicBool,
    options: OccOptions,
    overlay: Arc<OccOverlay>,
}

impl OccGroupCommitCoordinator {
    pub fn new(options: OccOptions, overlay: Arc<OccOverlay>) -> Self {
        Self {
            pending: Mutex::new(Vec::new()),
            leader_active: AtomicBool::new(false),
            options,
            overlay,
        }
    }

    pub fn submit(
        &self,
        db: &Database,
        ops: Vec<BatchOperation>,
        version: u64,
        stats: &OccStats,
    ) -> Result<Vec<MutationResult>> {
        let waiter = Arc::new(GroupCommitWaiter::new());
        let item = CommitItem {
            ops,
            version,
            waiter: Arc::clone(&waiter),
        };

        let mut queue = self.pending.lock();
        queue.push(item);

        if self.leader_active.load(Ordering::Acquire) {
            drop(queue);
            // Leader is already running: wait as a follower
            return waiter.wait(Duration::from_millis(self.options.commit_timeout_ms));
        }

        // Claim leadership
        self.leader_active.store(true, Ordering::Release);
        drop(queue);

        // Run leader loop to process pending batches
        self.run_leader_loop(db, stats);

        waiter.wait(Duration::from_millis(self.options.commit_timeout_ms))
    }

    fn run_leader_loop(&self, db: &Database, stats: &OccStats) {
        loop {
            let batch: Vec<CommitItem> = {
                let mut queue = self.pending.lock();
                if queue.is_empty() {
                    self.leader_active.store(false, Ordering::Release);
                    break;
                }
                let limit = self.options.max_batch_size;
                let count = queue.len().min(limit);
                queue.drain(0..count).collect()
            };

            if batch.is_empty() {
                self.leader_active.store(false, Ordering::Release);
                break;
            }

            let batch_len = batch.len();
            stats.group_commits_executed.fetch_add(1, Ordering::Relaxed);
            stats
                .transactions_merged
                .fetch_add(batch_len as u64, Ordering::Relaxed);

            // Execute the entire merged batch inside a SINGLE SQLite write transaction!
            let outcome: Result<Vec<Vec<MutationResult>>> = (|| {
                let mut writer = db.lock_writer()?;
                let mut tx = writer.transaction()?;

                let mut batch_results = Vec::with_capacity(batch.len());
                for item in &batch {
                    let mut item_results = Vec::with_capacity(item.ops.len());
                    for op in &item.ops {
                        let res = crate::transaction::apply(
                            &mut tx,
                            op,
                            db.codec.as_ref(),
                            &db.dict_registry,
                            &db.json_options,
                            db.hlc.as_ref(),
                            &db.cas,
                            db.shard_manager.as_deref(),
                            Some(db.integrity.as_ref()),
                        )?;
                        match op {
                            BatchOperation::VectorSet {
                                collection,
                                id,
                                dim,
                                element_type,
                                vector,
                            } => {
                                db.sync_hnsw_insert(
                                    collection,
                                    id,
                                    *dim,
                                    *element_type,
                                    vector,
                                    &mut tx,
                                )?;
                            }
                            BatchOperation::VectorDelete { collection, id } => {
                                db.sync_hnsw_delete(collection, id, &mut tx)?;
                            }
                            _ => {}
                        }
                        item_results.push(res);
                    }
                    batch_results.push(item_results);
                }

                // Record commit in Merkle ledger
                let mut ledger_entries = Vec::new();
                for (item, res) in batch.iter().zip(&batch_results) {
                    ledger_entries.extend(crate::ledger::operation_entries(&item.ops, res));
                }
                db.ledger
                    .record_commit(tx.conn(), ledger_entries, crate::storage::now_ms())?;

                tx.commit()?;
                Ok(batch_results)
            })();

            match outcome {
                Ok(batch_results) => {
                    let mut all_ops = Vec::new();
                    for item in &batch {
                        all_ops.extend(item.ops.iter().cloned());
                    }
                    db.invalidate_cache_for_operations(&all_ops);
                    for item in &batch {
                        self.overlay.release_batch(&item.ops, item.version);
                    }
                    for (item, res) in batch.into_iter().zip(batch_results) {
                        item.waiter.complete(Ok(res));
                    }
                }
                Err(err) => {
                    let err_msg = err.to_string();
                    let err_code = err.code;
                    let retryable = err.retryable;
                    for item in batch {
                        item.waiter.complete(Err(LiliaError::new(
                            err_code,
                            err_msg.clone(),
                            retryable,
                        )));
                    }
                }
            }
        }
    }
}

/// Central manager orchestrating lock-free optimistic concurrency control.
#[derive(Debug)]
pub struct OccManager {
    global_version: AtomicU64,
    version_registry: ShardedVersionRegistry,
    validation_mutex: Mutex<()>,
    coordinator: OccGroupCommitCoordinator,
    pub(crate) overlay: Arc<OccOverlay>,
    pub(crate) options: OccOptions,
    pub(crate) stats: OccStats,
}

impl OccManager {
    #[must_use]
    pub fn new(options: OccOptions) -> Self {
        let overlay = Arc::new(OccOverlay::new());
        let coordinator = OccGroupCommitCoordinator::new(options.clone(), Arc::clone(&overlay));
        Self {
            global_version: AtomicU64::new(1),
            version_registry: ShardedVersionRegistry::new(64),
            validation_mutex: Mutex::new(()),
            coordinator,
            overlay,
            options,
            stats: OccStats::default(),
        }
    }

    #[inline]
    #[must_use]
    pub fn current_version(&self) -> u64 {
        self.global_version.load(Ordering::Acquire)
    }

    #[inline]
    #[must_use]
    pub fn key_version(&self, key: &OccKey) -> u64 {
        self.version_registry.get(key).unwrap_or(0)
    }

    /// Fast RAM validation of an [`OccWorkspace`] against concurrent commits.
    ///
    /// # Errors
    /// Returns `LiliaError` with `ErrorCode::Conflict` and `retryable: true` if any read key
    /// or written key was modified by another transaction since `start_version`.
    pub(crate) fn validate_and_record(
        &self,
        workspace: &OccWorkspace,
        start_version: u64,
    ) -> Result<u64> {
        let _guard = self.validation_mutex.lock();

        // 1. Validate read set: has any key read been modified after this transaction started?
        for (key, observed_ver) in &workspace.read_set {
            let current_ver = self.version_registry.get(key).unwrap_or(0);
            if current_ver > start_version || current_ver > *observed_ver {
                self.stats
                    .conflicts_detected
                    .fetch_add(1, Ordering::Relaxed);
                return Err(LiliaError::new(
                    ErrorCode::Conflict,
                    format!("OCC conflict: key {key:?} was modified concurrently by another transaction"),
                    true,
                ));
            }
        }

        // 2. Validate write set (write-write conflict check)
        if self.options.conflict_check_writes {
            for key in &workspace.write_set {
                let current_ver = self.version_registry.get(key).unwrap_or(0);
                if current_ver > start_version {
                    self.stats
                        .conflicts_detected
                        .fetch_add(1, Ordering::Relaxed);
                    return Err(LiliaError::new(
                        ErrorCode::Conflict,
                        format!("OCC conflict: write-write conflict on key {key:?} modified concurrently"),
                        true,
                    ));
                }
            }
        }

        // 3. Validation passed! Allocate next commit version and register all keys in write_set.
        let next_version = self.global_version.fetch_add(1, Ordering::SeqCst) + 1;
        for key in &workspace.write_set {
            self.version_registry.insert(key.clone(), next_version);
        }
        self.overlay
            .record_batch(&workspace.staged_ops, next_version);

        Ok(next_version)
    }

    /// Submit validated operations to the group commit coordinator.
    pub fn group_commit(
        &self,
        db: &Database,
        ops: Vec<BatchOperation>,
        version: u64,
    ) -> Result<Vec<MutationResult>> {
        if ops.is_empty() {
            return Ok(Vec::new());
        }
        let res = self.coordinator.submit(db, ops, version, &self.stats)?;
        self.stats
            .transactions_committed
            .fetch_add(1, Ordering::Relaxed);
        Ok(res)
    }
}

/// Active in-memory optimistic concurrency control transaction.
///
/// Does NOT acquire any SQLite database locks upfront. All mutations are staged in RAM
/// with Read-Your-Own-Writes isolation, validated in microseconds at commit time,
/// and committed in automatically merged SQLite write batches.
#[derive(Debug)]
pub struct OccTransaction<'a> {
    db: &'a Database,
    manager: &'a OccManager,
    start_version: u64,
    workspace: OccWorkspace,
    committed: bool,
}

impl<'a> OccTransaction<'a> {
    pub(crate) fn new(db: &'a Database, manager: &'a OccManager) -> Self {
        manager
            .stats
            .transactions_started
            .fetch_add(1, Ordering::Relaxed);
        let start_version = manager.current_version();
        Self {
            db,
            manager,
            start_version,
            workspace: OccWorkspace::new(),
            committed: false,
        }
    }

    #[must_use]
    pub fn db(&self) -> &'a Database {
        self.db
    }

    #[must_use]
    pub fn start_version(&self) -> u64 {
        self.start_version
    }

    /// Returns the stack of active nested savepoint names from root to tip.
    #[must_use]
    pub fn active_savepoints(&self) -> Vec<&str> {
        self.workspace
            .savepoint_stack
            .iter()
            .map(|f| f.name.as_str())
            .collect()
    }

    /// Access the JSON sub-model facade within this OCC transaction.
    pub fn json(&mut self) -> OccTxJson<'_, 'a> {
        OccTxJson { tx: self }
    }

    /// Access the Key-Value sub-model facade within this OCC transaction.
    pub fn kv(&mut self) -> OccTxKv<'_, 'a> {
        OccTxKv { tx: self }
    }

    /// Access the Graph sub-model facade within this OCC transaction.
    pub fn graph(&mut self) -> OccTxGraph<'_, 'a> {
        OccTxGraph {
            tx: self,
            graph: "default".to_string(),
        }
    }

    /// Access a named Graph sub-model facade within this OCC transaction.
    pub fn graph_named(&mut self, graph: &str) -> OccTxGraph<'_, 'a> {
        OccTxGraph {
            tx: self,
            graph: graph.to_string(),
        }
    }

    /// Apply an arbitrary [`BatchOperation`] to the in-memory workspace.
    ///
    /// # Errors
    /// Returns `Conflict` error if transaction is already committed.
    pub fn apply_op(&mut self, operation: BatchOperation) -> Result<()> {
        if self.committed {
            return Err(LiliaError::new(
                ErrorCode::Conflict,
                "cannot apply operation on committed OCC transaction",
                false,
            ));
        }

        // Update local RYOW workspace state
        match &operation {
            BatchOperation::JsonPut {
                space, id, value, ..
            } => {
                let doc = lilia_core::BinaryDoc::from_json_value(value)?;
                let entry = JsonEntry::new(space, id, doc, self.start_version);
                self.workspace
                    .staged_json
                    .insert((space.clone(), id.clone()), WorkspaceValue::Put(entry));
            }
            BatchOperation::JsonDelete { space, id, .. } => {
                self.workspace
                    .staged_json
                    .insert((space.clone(), id.clone()), WorkspaceValue::Deleted);
            }
            BatchOperation::KvSet {
                namespace,
                key,
                value,
                ..
            } => {
                let entry = KvEntry {
                    namespace: namespace.clone(),
                    key: key.clone(),
                    value: value.clone(),
                    version: self.start_version,
                    expires_at_ms: None,
                };
                self.workspace
                    .staged_kv
                    .insert((namespace.clone(), key.clone()), WorkspaceValue::Put(entry));
            }
            BatchOperation::KvDelete { namespace, key, .. } => {
                self.workspace
                    .staged_kv
                    .insert((namespace.clone(), key.clone()), WorkspaceValue::Deleted);
            }
            BatchOperation::GraphAddEdge {
                graph,
                source,
                target,
                kind,
                weight,
            } => {
                self.workspace.staged_edges.insert(
                    (graph.clone(), source.clone(), target.clone(), kind.clone()),
                    WorkspaceValue::Put(*weight),
                );
            }
            BatchOperation::GraphRemoveEdge {
                graph,
                source,
                target,
                kind,
            } => {
                self.workspace.staged_edges.insert(
                    (graph.clone(), source.clone(), target.clone(), kind.clone()),
                    WorkspaceValue::Deleted,
                );
            }
            _ => {}
        }

        self.workspace.push_op(operation);
        Ok(())
    }

    /// Spans a speculative savepoint in RAM. If closure returns Err or panics, mutations inside
    /// are rolled back immediately without touching SQLite.
    ///
    /// # Errors
    /// Propagates errors from the closure or storage layer.
    pub fn spans_savepoint<F, R>(&mut self, name: &str, f: F) -> Result<R>
    where
        F: FnOnce(&mut OccTransaction<'_>) -> Result<R>,
    {
        if self.committed {
            return Err(LiliaError::new(
                ErrorCode::Conflict,
                "cannot open savepoint on committed OCC transaction",
                false,
            ));
        }

        let frame = OccSavepointFrame {
            name: name.to_string(),
            ops_len: self.workspace.staged_ops.len(),
            staged_json: self.workspace.staged_json.clone(),
            staged_kv: self.workspace.staged_kv.clone(),
            staged_edges: self.workspace.staged_edges.clone(),
            read_set: self.workspace.read_set.clone(),
            write_set: self.workspace.write_set.clone(),
        };
        self.workspace.savepoint_stack.push(frame);

        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));

        match res {
            Ok(Ok(val)) => {
                let _ = self.workspace.savepoint_stack.pop();
                Ok(val)
            }
            Ok(Err(err)) => {
                if let Some(frame) = self.workspace.savepoint_stack.pop() {
                    self.workspace.staged_ops.truncate(frame.ops_len);
                    self.workspace.staged_json = frame.staged_json;
                    self.workspace.staged_kv = frame.staged_kv;
                    self.workspace.staged_edges = frame.staged_edges;
                    self.workspace.read_set = frame.read_set;
                    self.workspace.write_set = frame.write_set;
                }
                Err(err)
            }
            Err(panic_payload) => {
                if let Some(frame) = self.workspace.savepoint_stack.pop() {
                    self.workspace.staged_ops.truncate(frame.ops_len);
                    self.workspace.staged_json = frame.staged_json;
                    self.workspace.staged_kv = frame.staged_kv;
                    self.workspace.staged_edges = frame.staged_edges;
                    self.workspace.read_set = frame.read_set;
                    self.workspace.write_set = frame.write_set;
                }
                let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                    (*s).to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic in OCC savepoint".to_string()
                };
                Err(LiliaError::new(
                    ErrorCode::Storage,
                    format!("OCC savepoint '{name}' aborted due to panic: {msg}"),
                    false,
                ))
            }
        }
    }

    /// Commit the OCC transaction.
    ///
    /// 1. Validates in RAM that no concurrent transaction modified any key in the Read Set.
    /// 2. If valid, merges writes into the automatic SQLite Group Commit block.
    ///
    /// # Errors
    /// Returns `Conflict` error (`retryable: true`) if a conflict is detected.
    pub fn commit(mut self) -> Result<()> {
        if self.committed {
            return Ok(());
        }
        if self.workspace.staged_ops.is_empty() {
            self.committed = true;
            return Ok(());
        }

        // 1. Ultra-fast RAM validation
        let version = self
            .manager
            .validate_and_record(&self.workspace, self.start_version)?;

        // 2. Submit to Automatic Group Commit
        let ops = std::mem::take(&mut self.workspace.staged_ops);
        let _ = self.manager.group_commit(self.db, ops, version)?;
        self.committed = true;
        Ok(())
    }

    /// Roll back this transaction, discarding all staged in-memory mutations.
    pub fn rollback(mut self) {
        self.workspace.staged_ops.clear();
        self.workspace.write_set.clear();
        self.workspace.read_set.clear();
        self.committed = true;
    }
}

impl Drop for OccTransaction<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.workspace.staged_ops.clear();
            self.workspace.write_set.clear();
            self.workspace.read_set.clear();
        }
    }
}

/// JSON facade for [`OccTransaction`].
#[derive(Debug)]
pub struct OccTxJson<'t, 'a> {
    tx: &'t mut OccTransaction<'a>,
}

impl OccTxJson<'_, '_> {
    /// Put a serializable document into the OCC workspace.
    ///
    /// # Errors
    /// Returns serialization or storage errors.
    pub fn put<T: serde::Serialize>(&mut self, space: &str, id: &str, value: &T) -> Result<()> {
        let val = serde_json::to_value(value).map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("failed to serialize JSON document: {e}"),
                false,
            )
        })?;
        self.put_value(space, id, val)
    }

    /// Put a raw `serde_json::Value` into the OCC workspace.
    ///
    /// # Errors
    /// Returns storage errors.
    pub fn put_value(&mut self, space: &str, id: &str, value: serde_json::Value) -> Result<()> {
        self.tx.apply_op(BatchOperation::JsonPut {
            space: space.to_owned(),
            id: id.to_owned(),
            value,
            if_version: None,
        })
    }

    /// Read a JSON document with Read-Your-Own-Writes semantics.
    ///
    /// # Errors
    /// Returns storage errors.
    pub fn get(&mut self, space: &str, id: &str) -> Result<Option<JsonEntry>> {
        // 1. Check workspace first (RYOW)
        let lookup = (space.to_owned(), id.to_owned());
        if let Some(staged) = self.tx.workspace.staged_json.get(&lookup) {
            return match staged {
                WorkspaceValue::Put(entry) => Ok(Some(entry.clone())),
                WorkspaceValue::Deleted => Ok(None),
            };
        }

        let key = OccKey::Json {
            space: space.to_owned(),
            id: id.to_owned(),
        };

        // 2. Check in-memory overlay (pending uncommitted writes from recently validated transactions)
        if let Some((ver, opt_entry)) = self.tx.manager.overlay.get_json(space, id) {
            self.tx.workspace.read_set.insert(key, ver);
            return Ok(opt_entry);
        }

        // 3. Read from database using concurrent reader pool
        let entry = self.tx.db.read_json_entry(space, id)?;
        let observed_version = self.tx.manager.key_version(&key);

        // 4. Record in Read Set
        self.tx.workspace.read_set.insert(key, observed_version);

        Ok(entry)
    }

    /// Delete a JSON document in the OCC workspace.
    ///
    /// # Errors
    /// Returns storage errors.
    pub fn delete(&mut self, space: &str, id: &str) -> Result<()> {
        self.tx.apply_op(BatchOperation::JsonDelete {
            space: space.to_owned(),
            id: id.to_owned(),
            if_version: None,
        })
    }
}

/// Key-Value facade for [`OccTransaction`].
#[derive(Debug)]
pub struct OccTxKv<'t, 'a> {
    tx: &'t mut OccTransaction<'a>,
}

impl OccTxKv<'_, '_> {
    /// Set a key-value entry in the OCC workspace.
    ///
    /// # Errors
    /// Returns storage errors.
    pub fn set(&mut self, namespace: &str, key: &[u8], value: &[u8]) -> Result<()> {
        self.tx.apply_op(BatchOperation::KvSet {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
            value: value.to_vec(),
            if_version: None,
            expires_at_ms: None,
        })
    }

    /// Set a key-value entry with TTL in the OCC workspace.
    ///
    /// # Errors
    /// Returns storage errors.
    pub fn set_with_ttl(
        &mut self,
        namespace: &str,
        key: &[u8],
        value: &[u8],
        expires_at_ms: i64,
    ) -> Result<()> {
        self.tx.apply_op(BatchOperation::KvSet {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
            value: value.to_vec(),
            if_version: None,
            expires_at_ms: Some(expires_at_ms),
        })
    }

    /// Read a key-value entry with Read-Your-Own-Writes semantics.
    ///
    /// # Errors
    /// Returns storage errors.
    pub fn get(&mut self, namespace: &str, key: &[u8]) -> Result<Option<KvEntry>> {
        // 1. Check workspace first (RYOW)
        let lookup = (namespace.to_owned(), key.to_vec());
        if let Some(staged) = self.tx.workspace.staged_kv.get(&lookup) {
            return match staged {
                WorkspaceValue::Put(entry) => Ok(Some(entry.clone())),
                WorkspaceValue::Deleted => Ok(None),
            };
        }

        let occ_key = OccKey::Kv {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
        };

        // 2. Check in-memory overlay (pending uncommitted writes from recently validated transactions)
        if let Some((ver, opt_entry)) = self.tx.manager.overlay.get_kv(namespace, key) {
            self.tx.workspace.read_set.insert(occ_key, ver);
            return Ok(opt_entry);
        }

        // 3. Read from database using concurrent reader pool
        let entry = self.tx.db.kv_get(namespace, key)?;
        let observed_version = self.tx.manager.key_version(&occ_key);

        // 4. Record in Read Set
        self.tx.workspace.read_set.insert(occ_key, observed_version);

        Ok(entry)
    }

    /// Delete a key in the OCC workspace.
    ///
    /// # Errors
    /// Returns storage errors.
    pub fn delete(&mut self, namespace: &str, key: &[u8]) -> Result<()> {
        self.tx.apply_op(BatchOperation::KvDelete {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
            if_version: None,
        })
    }
}

/// Graph facade for [`OccTransaction`].
#[derive(Debug)]
pub struct OccTxGraph<'t, 'a> {
    tx: &'t mut OccTransaction<'a>,
    graph: String,
}

impl OccTxGraph<'_, '_> {
    /// Add an unweighted edge to the graph in the OCC workspace.
    ///
    /// # Errors
    /// Returns storage errors.
    pub fn add_edge(&mut self, source: &str, target: &str, kind: &str) -> Result<()> {
        self.add_edge_weighted(source, target, kind, 1.0)
    }

    /// Add a weighted edge to the graph in the OCC workspace.
    ///
    /// # Errors
    /// Returns storage errors.
    pub fn add_edge_weighted(
        &mut self,
        source: &str,
        target: &str,
        kind: &str,
        weight: f64,
    ) -> Result<()> {
        self.tx.apply_op(BatchOperation::GraphAddEdge {
            graph: self.graph.clone(),
            source: source.to_owned(),
            target: target.to_owned(),
            kind: kind.to_owned(),
            weight,
        })
    }

    /// Remove an edge from the graph in the OCC workspace.
    ///
    /// # Errors
    /// Returns storage errors.
    pub fn remove_edge(&mut self, source: &str, target: &str, kind: &str) -> Result<()> {
        self.tx.apply_op(BatchOperation::GraphRemoveEdge {
            graph: self.graph.clone(),
            source: source.to_owned(),
            target: target.to_owned(),
            kind: kind.to_owned(),
        })
    }

    /// Set a node in the graph in the OCC workspace.
    ///
    /// # Errors
    /// Returns storage errors.
    pub fn set_node(&mut self, id: &str, kind: &str, metadata: &serde_json::Value) -> Result<()> {
        self.tx.apply_op(BatchOperation::GraphSetNode {
            graph: self.graph.clone(),
            id: id.to_owned(),
            kind: kind.to_owned(),
            metadata: metadata.clone(),
            doc_space: None,
            doc_id: None,
        })
    }

    /// Delete a node from the graph in the OCC workspace.
    ///
    /// # Errors
    /// Returns storage errors.
    pub fn delete_node(&mut self, id: &str) -> Result<()> {
        self.tx.apply_op(BatchOperation::GraphDeleteNode {
            graph: self.graph.clone(),
            id: id.to_owned(),
        })
    }

    /// Check if an edge exists with Read-Your-Own-Writes semantics.
    ///
    /// # Errors
    /// Returns storage errors.
    pub fn has_edge(&mut self, source: &str, target: &str, kind: &str) -> Result<bool> {
        let lookup = (
            self.graph.clone(),
            source.to_owned(),
            target.to_owned(),
            kind.to_owned(),
        );
        if let Some(staged) = self.tx.workspace.staged_edges.get(&lookup) {
            return match staged {
                WorkspaceValue::Put(_) => Ok(true),
                WorkspaceValue::Deleted => Ok(false),
            };
        }

        let exists = self
            .tx
            .db
            .graph_has_edge(&self.graph, source, target, kind)?;
        let occ_key = OccKey::GraphEdge {
            graph: self.graph.clone(),
            source: source.to_owned(),
            target: target.to_owned(),
            kind: kind.to_owned(),
        };
        let observed_version = self.tx.manager.key_version(&occ_key);
        self.tx.workspace.read_set.insert(occ_key, observed_version);
        Ok(exists)
    }
}
