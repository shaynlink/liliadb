#![allow(clippy::missing_errors_doc)]

use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[cfg(not(target_arch = "wasm32"))]
use lilia_sqlite_native::NativeBackup;
use lilia_sqlite_native::{InterruptHandle, NativeConnection};

use crate::checkpoint::{CheckpointStats, Checkpointer};
use crate::lifecycle::{Lifecycle, Operation};
use crate::storage::{migrate, prepare_parent, secure_database_file};
use crate::timeseries_worker::{TimeSeriesOptions, TimeSeriesWorker};
use crate::transaction::apply;
use crate::writer_queue::{Permit, WriterQueue};
use lilia_core::{
    BatchOperation, Durability, ErrorCode, IdentityCodec, LiliaError, MutationResult, Result,
    ValueCodec,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct TableStorageStat {
    pub name: String,
    pub page_count: i64,
    pub payload_bytes: i64,
    pub unused_bytes: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResourceProfile {
    #[default]
    Server,
    Mobile,
    Constrained,
}

#[derive(Debug, Clone)]
pub struct DatabaseOptions {
    pub path: PathBuf,
    pub durability: Durability,
    pub busy_timeout_ms: u64,
    pub writer_queue_capacity: usize,
    pub read_pool_size: usize,
    pub checkpoint_policy: CheckpointPolicy,
    pub codec: Arc<dyn ValueCodec>,
    pub mmap_size_bytes: u64,
    pub profile: ResourceProfile,
    pub cache_size_kib: i32,
    pub timeseries_options: TimeSeriesOptions,
    pub undo_log_capacity: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointPolicy {
    pub wal_bytes: u64,
    pub interval_ms: u64,
}

impl DatabaseOptions {
    pub fn durable(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            durability: Durability::Durable,
            busy_timeout_ms: 5_000,
            writer_queue_capacity: 64,
            read_pool_size: 4,
            checkpoint_policy: CheckpointPolicy {
                wal_bytes: 16 * 1024 * 1024,
                interval_ms: 5_000,
            },
            codec: Arc::new(IdentityCodec),
            mmap_size_bytes: 256 * 1024 * 1024,
            profile: ResourceProfile::Server,
            cache_size_kib: -65_536,
            timeseries_options: TimeSeriesOptions {
                auto_retention: true,
                retention_interval_ms: 60_000,
            },
            undo_log_capacity: 100,
        }
    }

    pub fn mobile(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            durability: Durability::Balanced,
            busy_timeout_ms: 5_000,
            writer_queue_capacity: 32,
            read_pool_size: 2,
            checkpoint_policy: CheckpointPolicy {
                wal_bytes: 4 * 1024 * 1024,
                interval_ms: 3_000,
            },
            codec: Arc::new(IdentityCodec),
            mmap_size_bytes: 32 * 1024 * 1024,
            profile: ResourceProfile::Mobile,
            cache_size_kib: -8_192,
            timeseries_options: TimeSeriesOptions {
                auto_retention: true,
                retention_interval_ms: 300_000,
            },
            undo_log_capacity: 100,
        }
    }

    pub fn constrained(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            durability: Durability::Balanced,
            busy_timeout_ms: 3_000,
            writer_queue_capacity: 16,
            read_pool_size: 1,
            checkpoint_policy: CheckpointPolicy {
                wal_bytes: 1024 * 1024,
                interval_ms: 1_500,
            },
            codec: Arc::new(IdentityCodec),
            mmap_size_bytes: 0,
            profile: ResourceProfile::Constrained,
            cache_size_kib: -2_048,
            timeseries_options: TimeSeriesOptions {
                auto_retention: false,
                retention_interval_ms: 600_000,
            },
            undo_log_capacity: 100,
        }
    }

    #[must_use]
    pub fn in_memory() -> Self {
        use uuid::Uuid;
        let mem_name = format!(
            "file:lilia_mem_{}?mode=memory&cache=shared",
            Uuid::new_v4().simple()
        );
        Self {
            path: PathBuf::from(mem_name),
            durability: Durability::Performance,
            busy_timeout_ms: 5_000,
            writer_queue_capacity: 64,
            read_pool_size: 2,
            checkpoint_policy: CheckpointPolicy {
                wal_bytes: 16 * 1024 * 1024,
                interval_ms: 5_000,
            },
            codec: Arc::new(IdentityCodec),
            mmap_size_bytes: 0,
            profile: ResourceProfile::Server,
            cache_size_kib: -65_536,
            timeseries_options: TimeSeriesOptions {
                auto_retention: false,
                retention_interval_ms: 0,
            },
            undo_log_capacity: 100,
        }
    }

    #[must_use]
    pub fn with_profile(mut self, profile: ResourceProfile) -> Self {
        self.profile = profile;
        match profile {
            ResourceProfile::Server => {
                self.cache_size_kib = -65_536;
                self.mmap_size_bytes = 256 * 1024 * 1024;
                self.read_pool_size = 4;
                self.checkpoint_policy = CheckpointPolicy {
                    wal_bytes: 16 * 1024 * 1024,
                    interval_ms: 5_000,
                };
                self.timeseries_options = TimeSeriesOptions {
                    auto_retention: true,
                    retention_interval_ms: 60_000,
                };
            }
            ResourceProfile::Mobile => {
                self.cache_size_kib = -8_192;
                self.mmap_size_bytes = 32 * 1024 * 1024;
                self.read_pool_size = 2;
                self.checkpoint_policy = CheckpointPolicy {
                    wal_bytes: 4 * 1024 * 1024,
                    interval_ms: 3_000,
                };
                self.timeseries_options = TimeSeriesOptions {
                    auto_retention: true,
                    retention_interval_ms: 300_000,
                };
            }
            ResourceProfile::Constrained => {
                self.cache_size_kib = -2_048;
                self.mmap_size_bytes = 0;
                self.read_pool_size = 1;
                self.checkpoint_policy = CheckpointPolicy {
                    wal_bytes: 1024 * 1024,
                    interval_ms: 1_500,
                };
                self.timeseries_options = TimeSeriesOptions {
                    auto_retention: false,
                    retention_interval_ms: 600_000,
                };
            }
        }
        self
    }

    #[must_use]
    pub fn with_timeseries_options(mut self, options: TimeSeriesOptions) -> Self {
        self.timeseries_options = options;
        self
    }

    #[must_use]
    pub fn with_cache_size_kib(mut self, kib: i32) -> Self {
        self.cache_size_kib = kib;
        self
    }

    #[must_use]
    pub fn with_codec(mut self, codec: Arc<dyn ValueCodec>) -> Self {
        self.codec = codec;
        self
    }

    #[must_use]
    pub fn with_mmap_size(mut self, bytes: u64) -> Self {
        self.mmap_size_bytes = bytes;
        self
    }

    #[must_use]
    pub fn with_undo_log_capacity(mut self, capacity: usize) -> Self {
        self.undo_log_capacity = capacity;
        self
    }
}

#[derive(Debug)]
pub struct Database {
    path: PathBuf,
    pub(crate) lifecycle: Arc<Lifecycle>,
    pub(crate) resources: Arc<Resources>,
    pub(crate) codec: Arc<dyn ValueCodec>,
    events: Arc<tokio::sync::broadcast::Sender<lilia_core::DatabaseEvent>>,
    pub(crate) hnsw_cache: Arc<
        std::sync::RwLock<
            std::collections::HashMap<String, Arc<std::sync::RwLock<lilia_core::HnswIndex>>>,
        >,
    >,
    #[allow(dead_code)]
    pub(crate) undo_log_capacity: usize,
}

#[derive(Debug)]
pub(crate) struct Resources {
    // Stop/join maintenance before dropping the database connections.
    checkpointer: Mutex<Option<Checkpointer>>,
    pub(crate) ts_worker: Mutex<Option<TimeSeriesWorker>>,
    writer: Mutex<Option<NativeConnection>>,
    readers: Vec<Mutex<Option<NativeConnection>>>,
    next_reader: AtomicUsize,
    writer_queue: WriterQueue,
}

#[doc(hidden)]
pub struct WriterGuard<'a> {
    // Drop the connection lock before making the next FIFO permit available.
    connection: MutexGuard<'a, Option<NativeConnection>>,
    _permit: Option<Permit<'a>>,
    _operation: Operation<'a>,
}

impl std::fmt::Debug for WriterGuard<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriterGuard").finish_non_exhaustive()
    }
}

impl Deref for WriterGuard<'_> {
    type Target = NativeConnection;
    fn deref(&self) -> &NativeConnection {
        self.connection
            .as_ref()
            .expect("admitted connection must remain open")
    }
}

impl DerefMut for WriterGuard<'_> {
    fn deref_mut(&mut self) -> &mut NativeConnection {
        self.connection
            .as_mut()
            .expect("admitted connection must remain open")
    }
}

fn open_connection(
    path: &Path,
    synchronous: &str,
    mmap_size: u64,
    busy_timeout_ms: i32,
    cache_size_kib: i32,
    codec: &Arc<dyn ValueCodec>,
) -> Result<NativeConnection> {
    let conn = NativeConnection::open_with_config(
        path,
        synchronous,
        mmap_size,
        busy_timeout_ms,
        cache_size_kib,
    )?;
    crate::sql::register_sql_functions(&conn, codec)?;
    Ok(conn)
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
struct Watchdog {
    stop_tx: Option<std::sync::mpsc::SyncSender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Watchdog {
    fn arm(interrupt: InterruptHandle, duration: Duration) -> Self {
        let (stop_tx, stop_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let worker = std::thread::Builder::new()
            .name("lilia-deadline-watchdog".into())
            .spawn(move || {
                if matches!(
                    stop_rx.recv_timeout(duration),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                ) {
                    interrupt.interrupt();
                }
            })
            .ok();
        Self {
            stop_tx: Some(stop_tx),
            worker,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Watchdog {
    fn drop(&mut self) {
        drop(self.stop_tx.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
struct Watchdog;

#[cfg(target_arch = "wasm32")]
impl Watchdog {
    #[allow(clippy::unnecessary_wraps)]
    fn arm(_interrupt: InterruptHandle, _duration: Duration) -> Self {
        Self
    }
}

impl Database {
    pub fn open(options: DatabaseOptions) -> Result<Self> {
        if options.checkpoint_policy.wal_bytes == 0
            || !(1..=u64::from(u32::MAX)).contains(&options.checkpoint_policy.interval_ms)
        {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "checkpoint wal_bytes must be positive and interval_ms must be in 1..=4294967295",
                false,
            ));
        }
        prepare_parent(&options.path)?;
        let synchronous = match options.durability {
            Durability::Durable => "FULL",
            Durability::Balanced => "NORMAL",
            Durability::Performance => "OFF",
        };
        let busy_ms = i32::try_from(options.busy_timeout_ms).unwrap_or(5000);
        let connection = open_connection(
            &options.path,
            synchronous,
            options.mmap_size_bytes,
            busy_ms,
            options.cache_size_kib,
            &options.codec,
        )?;
        secure_database_file(&options.path)?;
        // Disable SQLite's commit-path checkpoint in favor of background work.
        connection.pragma_update("wal_autocheckpoint", 0)?;
        migrate(&connection)?;
        let reader_count = if cfg!(target_arch = "wasm32") {
            0
        } else {
            options.read_pool_size.max(1)
        };
        let mut readers = Vec::with_capacity(reader_count);
        for _ in 0..reader_count {
            let reader = open_connection(
                &options.path,
                synchronous,
                options.mmap_size_bytes,
                busy_ms,
                options.cache_size_kib,
                &options.codec,
            )?;
            reader.pragma_update("query_only", 1)?;
            readers.push(Mutex::new(Some(reader)));
        }
        let lifecycle = Arc::new(Lifecycle::default());
        let resources = Arc::new(Resources {
            checkpointer: Mutex::new(Some(Checkpointer::start(
                &options.path,
                options.checkpoint_policy,
                synchronous,
            )?)),
            ts_worker: Mutex::new(None),
            writer: Mutex::new(Some(connection)),
            readers,
            next_reader: AtomicUsize::new(0),
            writer_queue: WriterQueue::new(options.writer_queue_capacity),
        });

        #[cfg(not(target_arch = "wasm32"))]
        {
            let ts_worker = TimeSeriesWorker::start(
                Arc::downgrade(&resources),
                Arc::downgrade(&lifecycle),
                options.timeseries_options,
            )?;
            *resources
                .ts_worker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ts_worker);
        }

        Ok(Self {
            lifecycle,
            resources,
            path: options.path,
            codec: options.codec,
            events: Arc::new(tokio::sync::broadcast::channel(1024).0),
            hnsw_cache: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
            undo_log_capacity: options.undo_log_capacity,
        })
    }

    #[must_use]
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<lilia_core::DatabaseEvent> {
        self.events.subscribe()
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn codec(&self) -> &Arc<dyn ValueCodec> {
        &self.codec
    }

    pub fn checkpoint_stats(&self) -> CheckpointStats {
        self.resources
            .checkpointer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(Checkpointer::stats)
            .unwrap_or_default()
    }

    /// Stop admission and wait at most `timeout`; cleanup continues after TIMEOUT.
    pub fn close(&self, timeout: Duration) -> Result<()> {
        let resources = Arc::clone(&self.resources);
        self.lifecycle.close(timeout, move || resources.close())
    }

    pub fn integrity_check(&self) -> Result<bool> {
        let conn = self.read_lock()?;
        let mut stmt = conn.prepare("PRAGMA integrity_check;")?;
        if stmt.step()? {
            Ok(stmt.column_text(0) == "ok")
        } else {
            Ok(false)
        }
    }

    pub fn quick_check(&self) -> Result<bool> {
        let conn = self.read_lock()?;
        let mut stmt = conn.prepare("PRAGMA quick_check;")?;
        if stmt.step()? {
            Ok(stmt.column_text(0) == "ok")
        } else {
            Ok(false)
        }
    }

    /// Retrieve physical page and payload statistics for all tables in the database using `dbstat`.
    pub fn storage_stats(&self) -> Result<Vec<TableStorageStat>> {
        let conn = self.read_lock()?;
        let mut stmt = conn.prepare(
            "SELECT name, count(*) AS page_count, coalesce(sum(payload), 0) AS total_payload, coalesce(sum(unused), 0) AS total_unused
             FROM dbstat
             GROUP BY name
             ORDER BY name;",
        )?;
        let mut stats = Vec::new();
        while stmt.step()? {
            stats.push(TableStorageStat {
                name: stmt.column_text(0).to_string(),
                page_count: stmt.column_int64(1),
                payload_bytes: stmt.column_int64(2),
                unused_bytes: stmt.column_int64(3),
            });
        }
        Ok(stats)
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.lock_writer()?
            .exec("PRAGMA wal_checkpoint(PASSIVE);")?;
        Ok(())
    }

    pub fn backup(&self, destination: impl AsRef<Path>) -> Result<()> {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = destination;
            Err(LiliaError::new(
                ErrorCode::Storage,
                "backup is not supported on WebAssembly",
                false,
            ))
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            // Admission covers validation, snapshotting and publication, not only SQL.
            let source = self.lock_writer()?;
            let destination = destination.as_ref();
            if destination == self.path {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "backup destination must differ from the database path",
                    false,
                ));
            }
            // Refuse every existing entry, including dangling symlinks and source aliases.
            // Publication below also enforces this atomically against concurrent creators.
            match std::fs::symlink_metadata(destination) {
                Ok(_) => {
                    return Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        "backup destination already exists",
                        false,
                    ))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(backup_io(error)),
            }
            prepare_parent(destination)?;
            let parent = destination
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            // A private sibling directory also protects SQLite's temporary sidecars.
            let staging = tempfile::Builder::new()
                .prefix(".lilia-backup-")
                .tempdir_in(parent)
                .map_err(backup_io)?;
            #[cfg(windows)]
            lilia_core::windows_security::secure_directory(staging.path()).map_err(backup_io)?;
            let temporary = staging.path().join("snapshot.lilia");
            let output = NativeConnection::open_with_config(&temporary, "NORMAL", 0, 5000, -2048)?;
            secure_database_file(&temporary)?;
            let mut backup = NativeBackup::new(&output, &source)?;
            let mut done = false;
            while !done {
                done = backup.step(128)?;
                if !done {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
            drop(backup);
            let mut stmt = output.prepare("PRAGMA integrity_check;")?;
            let mut integrity = String::new();
            if stmt.step()? {
                integrity = stmt.column_text(0).to_string();
            }
            drop(stmt);
            drop(output);
            if integrity != "ok" {
                return Err(LiliaError::new(
                    ErrorCode::Corrupt,
                    "backup integrity check failed",
                    false,
                ));
            }
            // Windows FlushFileBuffers requires a handle opened with write access.
            std::fs::OpenOptions::new()
                .write(true)
                .open(&temporary)
                .map_err(backup_io)?
                .sync_all()
                .map_err(backup_io)?;
            // Unlike rename on Unix, hard_link never replaces an existing destination.
            // Both names are on the same filesystem; only a complete snapshot becomes visible.
            std::fs::hard_link(&temporary, destination).map_err(backup_io)?;
            lilia_core::sync_directory(parent).map_err(backup_io)?;
            Ok(())
        }
    }

    pub fn batch(&self, operations: &[BatchOperation]) -> Result<Vec<MutationResult>> {
        self.batch_with_deadline(operations, None)
    }

    /// Deadline applies to writer admission and transaction execution.
    #[allow(clippy::too_many_lines)]
    pub fn batch_with_deadline(
        &self,
        operations: &[BatchOperation],
        deadline: Option<Instant>,
    ) -> Result<Vec<MutationResult>> {
        if operations.is_empty() {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "batch cannot be empty",
                false,
            ));
        }
        if operations.len() > 1_000 {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "batch exceeds 1000 operations",
                false,
            ));
        }
        let mut connection = self.writer_lock(deadline)?;
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(LiliaError::new(
                ErrorCode::Timeout,
                "request expired before transaction",
                true,
            ));
        }
        let watchdog = deadline.and_then(|deadline| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                None
            } else {
                Some(Watchdog::arm(connection.interrupt_handle(), remaining))
            }
        });

        let session = if self.undo_log_capacity > 0 {
            if let Ok(sess) = lilia_sqlite_native::NativeSession::create(&connection, "main") {
                let _ = sess.attach_all();
                Some(sess)
            } else {
                None
            }
        } else {
            None
        };

        let mut transaction = connection.transaction()?;
        let results = operations
            .iter()
            .map(|operation| apply(&mut transaction, operation, self.codec.as_ref()))
            .collect::<Result<Vec<_>>>()?;

        for operation in operations {
            match operation {
                BatchOperation::VectorSet {
                    collection,
                    id,
                    dim,
                    element_type,
                    vector,
                } => {
                    self.sync_hnsw_insert(
                        collection,
                        id,
                        *dim,
                        *element_type,
                        vector,
                        &mut transaction,
                    )?;
                }
                BatchOperation::VectorDelete { collection, id } => {
                    self.sync_hnsw_delete(collection, id, &mut transaction)?;
                }
                _ => {}
            }
        }

        transaction.commit()?;

        if let Some(sess) = session {
            if !sess.is_empty() {
                if let Ok(changeset) = sess.changeset() {
                    if !changeset.is_empty() {
                        if let Ok(inverse) =
                            lilia_sqlite_native::NativeSession::invert_changeset(&changeset)
                        {
                            drop(sess);

                            let now = crate::storage::now_ms();
                            let summary = format!("batch of {} operations", operations.len());
                            let fwd_compressed = lilia_core::compress_bytes(&changeset);
                            let rev_compressed = lilia_core::compress_bytes(&inverse);

                            if let Ok(undo_tx) = connection.transaction() {
                                if let Ok(mut ins_stmt) = undo_tx.prepare(
                                    "INSERT INTO _lilia_undo_log(timestamp_ms, summary, forward_changeset, reverse_changeset, undone) \
                                     VALUES(?1, ?2, ?3, ?4, 0);",
                                ) {
                                    let _ = ins_stmt.bind_int64(1, now);
                                    let _ = ins_stmt.bind_text_static(2, &summary);
                                    let _ = ins_stmt.bind_blob_static(3, &fwd_compressed);
                                    let _ = ins_stmt.bind_blob_static(4, &rev_compressed);
                                    let _ = ins_stmt.step();
                                }

                                let _ =
                                    undo_tx.exec("DELETE FROM _lilia_undo_log WHERE undone = 1;");

                                if let Ok(mut prune_stmt) = undo_tx.prepare(
                                    "DELETE FROM _lilia_undo_log WHERE id NOT IN (\
                                       SELECT id FROM _lilia_undo_log ORDER BY id DESC LIMIT ?1\
                                     );",
                                ) {
                                    let _ = prune_stmt.bind_int64(
                                        1,
                                        i64::try_from(self.undo_log_capacity).unwrap_or(100),
                                    );
                                    let _ = prune_stmt.step();
                                }

                                let _ = undo_tx.commit();
                            }
                        }
                    }
                }
            }
        }
        if self.events.receiver_count() > 0 {
            for (op, res) in operations.iter().zip(&results) {
                let evt = match op {
                    BatchOperation::KvSet { namespace, key, .. } => {
                        Some(lilia_core::DatabaseEvent::KvMutated {
                            namespace: namespace.clone(),
                            key: key.clone(),
                            version: res.version,
                            deleted: false,
                        })
                    }
                    BatchOperation::KvDelete { namespace, key, .. } => {
                        Some(lilia_core::DatabaseEvent::KvMutated {
                            namespace: namespace.clone(),
                            key: key.clone(),
                            version: None,
                            deleted: true,
                        })
                    }
                    BatchOperation::JsonPut { space, id, .. } => {
                        Some(lilia_core::DatabaseEvent::JsonMutated {
                            space: space.clone(),
                            id: id.clone(),
                            version: res.version,
                            deleted: false,
                        })
                    }
                    BatchOperation::JsonDelete { space, id, .. } => {
                        Some(lilia_core::DatabaseEvent::JsonMutated {
                            space: space.clone(),
                            id: id.clone(),
                            version: None,
                            deleted: true,
                        })
                    }
                    BatchOperation::GraphSetNode { graph, id, .. } => {
                        Some(lilia_core::DatabaseEvent::GraphNodeMutated {
                            graph: graph.clone(),
                            id: id.clone(),
                            deleted: false,
                        })
                    }
                    BatchOperation::GraphDeleteNode { graph, id } => {
                        Some(lilia_core::DatabaseEvent::GraphNodeMutated {
                            graph: graph.clone(),
                            id: id.clone(),
                            deleted: true,
                        })
                    }
                    BatchOperation::GraphAddEdge {
                        graph,
                        source,
                        target,
                        kind,
                        ..
                    } => Some(lilia_core::DatabaseEvent::GraphEdgeMutated {
                        graph: graph.clone(),
                        source: source.clone(),
                        target: target.clone(),
                        kind: kind.clone(),
                        deleted: false,
                    }),
                    BatchOperation::GraphRemoveEdge {
                        graph,
                        source,
                        target,
                        kind,
                    } => Some(lilia_core::DatabaseEvent::GraphEdgeMutated {
                        graph: graph.clone(),
                        source: source.clone(),
                        target: target.clone(),
                        kind: kind.clone(),
                        deleted: true,
                    }),
                    BatchOperation::FtsIndex {
                        collection, doc_id, ..
                    } => Some(lilia_core::DatabaseEvent::FtsMutated {
                        collection: collection.clone(),
                        doc_id: doc_id.clone(),
                        deleted: false,
                    }),
                    BatchOperation::FtsDelete { collection, doc_id } => {
                        Some(lilia_core::DatabaseEvent::FtsMutated {
                            collection: collection.clone(),
                            doc_id: doc_id.clone(),
                            deleted: true,
                        })
                    }
                    BatchOperation::VectorSet { collection, id, .. } => {
                        Some(lilia_core::DatabaseEvent::VectorMutated {
                            collection: collection.clone(),
                            id: id.clone(),
                            deleted: false,
                        })
                    }
                    BatchOperation::VectorDelete { collection, id } => {
                        Some(lilia_core::DatabaseEvent::VectorMutated {
                            collection: collection.clone(),
                            id: id.clone(),
                            deleted: true,
                        })
                    }
                };
                if let Some(event) = evt {
                    let _ = self.events.send(event);
                }
            }
        }
        drop(watchdog);
        drop(connection);
        if let Some(checkpointer) = self
            .resources
            .checkpointer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            checkpointer.notify();
        }
        Ok(results)
    }

    #[doc(hidden)]
    pub fn lock_writer(&self) -> Result<WriterGuard<'_>> {
        self.writer_lock(None)
    }

    fn writer_lock(&self, deadline: Option<Instant>) -> Result<WriterGuard<'_>> {
        self.resources.lock_writer(&self.lifecycle, deadline)
    }

    pub(crate) fn read_lock(&self) -> Result<WriterGuard<'_>> {
        self.resources.read_lock(&self.lifecycle)
    }

    /// Roll back the most recent database mutation using native changeset inversion.
    pub fn rollback_last_mutation(&self) -> Result<Option<lilia_core::UndoResult>> {
        let mut connection = self.writer_lock(None)?;
        let tx = connection.transaction()?;

        let entry = {
            let mut stmt = tx.prepare(
                "SELECT id, summary, timestamp_ms, reverse_changeset \
                 FROM _lilia_undo_log \
                 WHERE undone = 0 \
                 ORDER BY id DESC LIMIT 1;",
            )?;
            if stmt.step()? {
                let id = stmt.column_int64(0);
                let summary = stmt.column_text(1).to_string();
                let ts = stmt.column_int64(2);
                let reverse_blob = stmt.column_blob(3).to_vec();
                Some((id, summary, ts, reverse_blob))
            } else {
                None
            }
        };

        let Some((id, summary, ts, reverse_blob)) = entry else {
            return Ok(None);
        };

        let raw_inverse = lilia_core::decompress_bytes(&reverse_blob)?;
        lilia_sqlite_native::NativeSession::apply(
            tx.conn(),
            &raw_inverse,
            lilia_sqlite_native::ConflictAction::Replace,
        )?;

        let mut update_stmt = tx.prepare("UPDATE _lilia_undo_log SET undone = 1 WHERE id = ?1;")?;
        update_stmt.bind_int64(1, id)?;
        update_stmt.step()?;
        drop(update_stmt);

        tx.commit()?;

        Ok(Some(lilia_core::UndoResult {
            id,
            summary,
            timestamp_ms: ts,
        }))
    }

    /// Alias for `rollback_last_mutation`.
    pub fn undo(&self) -> Result<Option<lilia_core::UndoResult>> {
        self.rollback_last_mutation()
    }

    /// Re-apply the most recently undone database mutation.
    pub fn redo(&self) -> Result<Option<lilia_core::RedoResult>> {
        let mut connection = self.writer_lock(None)?;
        let tx = connection.transaction()?;

        let entry = {
            let mut stmt = tx.prepare(
                "SELECT id, summary, timestamp_ms, forward_changeset \
                 FROM _lilia_undo_log \
                 WHERE undone = 1 \
                 ORDER BY id DESC LIMIT 1;",
            )?;
            if stmt.step()? {
                let id = stmt.column_int64(0);
                let summary = stmt.column_text(1).to_string();
                let ts = stmt.column_int64(2);
                let forward_blob = stmt.column_blob(3).to_vec();
                Some((id, summary, ts, forward_blob))
            } else {
                None
            }
        };

        let Some((id, summary, ts, forward_blob)) = entry else {
            return Ok(None);
        };

        let raw_forward = lilia_core::decompress_bytes(&forward_blob)?;
        lilia_sqlite_native::NativeSession::apply(
            tx.conn(),
            &raw_forward,
            lilia_sqlite_native::ConflictAction::Replace,
        )?;

        let mut update_stmt = tx.prepare("UPDATE _lilia_undo_log SET undone = 0 WHERE id = ?1;")?;
        update_stmt.bind_int64(1, id)?;
        update_stmt.step()?;
        drop(update_stmt);

        tx.commit()?;

        Ok(Some(lilia_core::RedoResult {
            id,
            summary,
            timestamp_ms: ts,
        }))
    }

    /// Retrieve the recent transaction undo/redo history.
    pub fn undo_history(&self) -> Result<Vec<lilia_core::UndoEntry>> {
        let conn = self.read_lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, timestamp_ms, summary, undone \
             FROM _lilia_undo_log \
             ORDER BY id DESC;",
        )?;
        let mut entries = Vec::new();
        while stmt.step()? {
            entries.push(lilia_core::UndoEntry {
                id: stmt.column_int64(0),
                timestamp_ms: stmt.column_int64(1),
                summary: stmt.column_text(2).to_string(),
                undone: stmt.column_int64(3) != 0,
            });
        }
        Ok(entries)
    }
}

impl Resources {
    pub(crate) fn lock_writer<'a>(
        &'a self,
        lifecycle: &'a Lifecycle,
        deadline: Option<Instant>,
    ) -> Result<WriterGuard<'a>> {
        let operation = lifecycle.enter()?;
        let permit = self.writer_queue.acquire(deadline)?;
        let connection = self
            .writer
            .lock()
            .map_err(|_| LiliaError::new(ErrorCode::Storage, "database lock poisoned", false))?;
        Ok(WriterGuard {
            connection,
            _permit: Some(permit),
            _operation: operation,
        })
    }

    pub(crate) fn read_lock<'a>(&'a self, lifecycle: &'a Lifecycle) -> Result<WriterGuard<'a>> {
        if self.readers.is_empty() {
            return self.lock_writer(lifecycle, None);
        }
        let operation = lifecycle.enter()?;
        let index = self.next_reader.fetch_add(1, Ordering::Relaxed) % self.readers.len();
        let connection = self.readers[index].lock().map_err(|_| {
            LiliaError::new(ErrorCode::Storage, "read database lock poisoned", false)
        })?;
        Ok(WriterGuard {
            connection,
            _permit: None,
            _operation: operation,
        })
    }

    fn close(&self) -> Result<()> {
        // No admitted operations remain. Potentially slow I/O runs on the cleanup
        // thread so the caller's timeout is independent of filesystem progress.
        let ts_worker = self
            .ts_worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(ts_worker);
        let worker = self
            .checkpointer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(worker);
        let mut result = Ok(());
        for reader in &self.readers {
            if let Some(reader) = reader
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                drop(reader);
            }
        }
        if let Some(writer) = self
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let checkpoint = writer.exec("PRAGMA wal_checkpoint(PASSIVE);");
            result = result.and(checkpoint);
            drop(writer);
        }
        result
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod recovery_tests;

// Match Result::map_err's owned error callback without repeating conversion closures.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::needless_pass_by_value)]
fn backup_io(error: std::io::Error) -> LiliaError {
    LiliaError::new(ErrorCode::Io, error.to_string(), false)
}
