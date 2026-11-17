#![allow(clippy::missing_errors_doc)]

use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[cfg(not(target_arch = "wasm32"))]
use lilia_sqlite_native::NativeBackup;
use lilia_sqlite_native::{InterruptHandle, NativeConnection};

use crate::autotune::{AutotuneEngine, IndexEnv};
use crate::autotune_worker::AutotuneWorker;
use crate::catalog::CollectionCatalog;
use crate::checkpoint::{CheckpointStats, Checkpointer};
use crate::embed::Embedders;
use crate::embed_worker::EmbedWorker;
use crate::lifecycle::{Lifecycle, Operation};
use crate::mem_engine::MemEngine;
use crate::ring_buffer::{RingBufferEngine, WriteRingBuffer};
use crate::storage::{migrate, prepare_parent, secure_database_file};
use crate::timeseries_worker::{TimeSeriesOptions, TimeSeriesWorker};
use crate::transaction::apply;
use crate::writer_queue::{Permit, WriterQueue};
pub use lilia_core::{ResourceProfile, TableStorageStat};
use lilia_core::{
    AutotuneOptions, BatchOperation, CollectionKind, CollectionSpec, DatabaseEvent, Durability, ErrorCode,
    IdentityCodec, LiliaError, MutationResult, Result, RingBufferConfig, StorageTier, ValueCodec,
};
use lilia_core::sharding::{ShardArchiveResult, ShardInfo, ShardingConfig};

#[derive(Debug, Clone)]
pub struct JsonOptions {
    pub binary_storage: bool,
    pub auto_train_dict: bool,
    pub dict_sample_threshold: usize,
    pub dict_target_size_bytes: usize,
    pub disabled_auto_train_spaces: Vec<String>,
}

impl Default for JsonOptions {
    fn default() -> Self {
        Self {
            binary_storage: true,
            auto_train_dict: true,
            dict_sample_threshold: 100,
            dict_target_size_bytes: 16 * 1024,
            disabled_auto_train_spaces: Vec::new(),
        }
    }
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
    pub json_options: JsonOptions,
    pub node_id: Option<String>,
    pub max_drift_ms: Option<i64>,
    pub tier_rules: Vec<CollectionSpec>,
    pub ring_buffer: RingBufferConfig,
    pub key_registry: Arc<lilia_core::KeyRegistry>,
    pub cas_options: lilia_core::cas::CasOptions,
    /// Optional local temporal sharding configuration. When `Some`, enables multi-file partition engine.
    pub sharding_config: Option<ShardingConfig>,
    /// Auto-tuning engine configuration (disabled by default).
    pub autotune_options: AutotuneOptions,
    /// Text embedding backends for auto-embedded JSON collections, one per model.
    pub embedders: Vec<Arc<dyn lilia_core::Embedder>>,
    /// Tuning of the background embedding worker.
    pub embed_options: lilia_core::EmbedOptions,
    /// Whether the cryptographic Merkle Ledger is enabled (default `true`).
    pub ledger_enabled: bool,
    /// Ledger verification mode executed on database open (default `Strict`).
    pub ledger_verify_on_open: lilia_core::ledger::LedgerVerifyMode,
    /// Tuning of the vectorized analytics engine and of its columnar store.
    #[cfg(all(feature = "analytics", not(target_arch = "wasm32")))]
    pub analytics_options: crate::columnar::AnalyticsOptions,
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
            json_options: JsonOptions::default(),
            node_id: None,
            max_drift_ms: None,
            tier_rules: Vec::new(),
            ring_buffer: RingBufferConfig::default(),
            key_registry: Arc::new(lilia_core::KeyRegistry::new()),
            cas_options: lilia_core::cas::CasOptions::default(),
            sharding_config: None,
            autotune_options: AutotuneOptions::default(),
            embedders: Vec::new(),
            embed_options: lilia_core::EmbedOptions::default(),
            ledger_enabled: true,
            ledger_verify_on_open: lilia_core::ledger::LedgerVerifyMode::Strict,
            #[cfg(all(feature = "analytics", not(target_arch = "wasm32")))]
            analytics_options: crate::columnar::AnalyticsOptions::default(),
        }
    }

    pub fn mobile(path: impl Into<PathBuf>) -> Self {        Self {
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
            json_options: JsonOptions::default(),
            node_id: None,
            max_drift_ms: None,
            tier_rules: Vec::new(),
            ring_buffer: RingBufferConfig::default(),
            key_registry: Arc::new(lilia_core::KeyRegistry::new()),
            cas_options: lilia_core::cas::CasOptions::default(),
            sharding_config: None,
            autotune_options: AutotuneOptions::default(),
            embedders: Vec::new(),
            embed_options: lilia_core::EmbedOptions::default(),
            ledger_enabled: true,
            ledger_verify_on_open: lilia_core::ledger::LedgerVerifyMode::Strict,
            #[cfg(all(feature = "analytics", not(target_arch = "wasm32")))]
            analytics_options: crate::columnar::AnalyticsOptions::default(),
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
            json_options: JsonOptions::default(),
            node_id: None,
            max_drift_ms: None,
            tier_rules: Vec::new(),
            ring_buffer: RingBufferConfig::default(),
            key_registry: Arc::new(lilia_core::KeyRegistry::new()),
            cas_options: lilia_core::cas::CasOptions::default(),
            sharding_config: None,
            autotune_options: AutotuneOptions::default(),
            embedders: Vec::new(),
            embed_options: lilia_core::EmbedOptions::default(),
            ledger_enabled: true,
            ledger_verify_on_open: lilia_core::ledger::LedgerVerifyMode::Strict,
            #[cfg(all(feature = "analytics", not(target_arch = "wasm32")))]
            analytics_options: crate::columnar::AnalyticsOptions::default(),
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
            json_options: JsonOptions::default(),
            node_id: None,
            max_drift_ms: None,
            tier_rules: Vec::new(),
            ring_buffer: RingBufferConfig::default(),
            key_registry: Arc::new(lilia_core::KeyRegistry::new()),
            cas_options: lilia_core::cas::CasOptions::default(),
            sharding_config: None,
            autotune_options: AutotuneOptions::default(),
            embedders: Vec::new(),
            embed_options: lilia_core::EmbedOptions::default(),
            ledger_enabled: false,
            ledger_verify_on_open: lilia_core::ledger::LedgerVerifyMode::None,
            #[cfg(all(feature = "analytics", not(target_arch = "wasm32")))]
            analytics_options: crate::columnar::AnalyticsOptions::default(),
        }
    }

    #[must_use]
    pub fn with_ledger(mut self, enabled: bool) -> Self {
        self.ledger_enabled = enabled;
        self
    }

    #[must_use]
    pub fn with_ledger_verification(mut self, mode: lilia_core::ledger::LedgerVerifyMode) -> Self {
        self.ledger_verify_on_open = mode;
        self
    }

    #[must_use]
    pub fn with_key_registry(mut self, key_registry: Arc<lilia_core::KeyRegistry>) -> Self {
        self.key_registry = key_registry;
        self
    }

    #[must_use]
    pub fn with_compartment(self, id: impl Into<String>, key: lilia_core::KeyMaterial) -> Self {
        self.key_registry.register_key(id, key);
        self
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

    #[must_use]
    pub fn with_json_options(mut self, options: JsonOptions) -> Self {
        self.json_options = options;
        self
    }

    #[must_use]
    pub fn with_auto_train_dict(mut self, enabled: bool) -> Self {
        self.json_options.auto_train_dict = enabled;
        self
    }

    #[must_use]
    pub fn with_node_id(mut self, node_id: impl Into<String>) -> Self {
        self.node_id = Some(node_id.into());
        self
    }

    #[must_use]
    pub fn with_max_drift(mut self, max_drift_ms: i64) -> Self {
        self.max_drift_ms = Some(max_drift_ms);
        self
    }

    #[must_use]
    pub fn with_collection_spec(mut self, spec: CollectionSpec) -> Self {
        self.tier_rules.push(spec);
        self
    }

    #[must_use]
    pub fn with_memory_collection(self, kind: CollectionKind, name: impl Into<String>) -> Self {
        self.with_collection_spec(CollectionSpec::new(kind, name, StorageTier::Memory))
    }

    #[must_use]
    pub fn with_memory_namespace(self, name: impl Into<String>) -> Self {
        self.with_memory_collection(CollectionKind::Kv, name)
    }

    #[must_use]
    pub fn with_memory_json(self, name: impl Into<String>) -> Self {
        self.with_memory_collection(CollectionKind::Json, name)
    }

    #[must_use]
    pub fn with_buffered_collection(self, kind: CollectionKind, name: impl Into<String>) -> Self {
        self.with_collection_spec(CollectionSpec::new(kind, name, StorageTier::Buffered))
    }

    #[must_use]
    pub fn with_buffered_kv(self, name: impl Into<String>) -> Self {
        self.with_buffered_collection(CollectionKind::Kv, name)
    }

    #[must_use]
    pub fn with_buffered_series(self, series_id: impl Into<String>) -> Self {
        self.with_buffered_collection(CollectionKind::TimeSeries, series_id)
    }

    #[must_use]
    pub fn with_ring_buffer(mut self, config: RingBufferConfig) -> Self {
        self.ring_buffer = config;
        self
    }

    #[must_use]
    pub fn with_cas_options(mut self, options: lilia_core::cas::CasOptions) -> Self {
        self.cas_options = options;
        self
    }

    #[must_use]
    pub fn with_cas_enabled(mut self, enabled: bool) -> Self {
        self.cas_options.enabled = enabled;
        self
    }

    #[must_use]
    pub fn with_cas_threshold(mut self, threshold_bytes: usize) -> Self {
        self.cas_options.threshold_bytes = threshold_bytes;
        self
    }

    #[must_use]
    pub fn with_cas_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cas_options.blobs_dir = Some(dir.into());
        self
    }

    /// Configures the auto-tuning engine (workload statistics, index synthesis and pruning).
    #[must_use]
    pub fn with_autotune(mut self, options: AutotuneOptions) -> Self {
        self.autotune_options = options;
        self
    }

    /// Registers the backend that embeds the documents and text queries of auto-embedded JSON
    /// collections. One backend is kept per model; a later one replaces an earlier one.
    #[must_use]
    pub fn with_embedder(mut self, embedder: Arc<dyn lilia_core::Embedder>) -> Self {
        self.embedders.push(embedder);
        self
    }

    /// Tunes the background embedding worker.
    #[must_use]
    pub const fn with_embed_options(mut self, options: lilia_core::EmbedOptions) -> Self {
        self.embed_options = options;
        self
    }

    /// Enables the local temporal sharding engine with the given configuration.
    #[must_use]
    pub fn with_sharding(mut self, config: ShardingConfig) -> Self {
        self.sharding_config = Some(config);
        self
    }

    /// Tunes the vectorized analytics engine and its columnar store.
    #[cfg(all(feature = "analytics", not(target_arch = "wasm32")))]
    #[must_use]
    pub const fn with_analytics_options(
        mut self,
        options: crate::columnar::AnalyticsOptions,
    ) -> Self {
        self.analytics_options = options;
        self
    }
}

#[derive(Debug)]
pub struct Database {
    path: PathBuf,
    // Declared first: dropping it joins the compaction worker while the connections it
    // may still be using are alive.
    #[cfg(all(feature = "analytics", not(target_arch = "wasm32")))]
    pub(crate) analytics: Mutex<Option<Arc<crate::columnar::AnalyticsState>>>,
    #[cfg(all(feature = "analytics", not(target_arch = "wasm32")))]
    pub(crate) analytics_options: crate::columnar::AnalyticsOptions,
    // Declared BEFORE `resources` so Drop drains buffered writes while connections remain alive.
    pub(crate) ring_buffer: Arc<RingBufferEngine>,
    pub(crate) ring_core: Arc<WriteRingBuffer>,
    pub(crate) lifecycle: Arc<Lifecycle>,
    pub(crate) resources: Arc<Resources>,
    pub(crate) cas: Arc<crate::cas::CasEngine>,
    pub(crate) codec: Arc<dyn ValueCodec>,
    events: Arc<tokio::sync::broadcast::Sender<DatabaseEvent>>,
    pub(crate) hnsw_cache: Arc<
        std::sync::RwLock<
            std::collections::HashMap<String, Arc<std::sync::RwLock<lilia_core::HnswIndex>>>,
        >,
    >,
    pub(crate) fts_trie_cache: Arc<
        std::sync::RwLock<
            std::collections::HashMap<String, Arc<std::sync::RwLock<lilia_core::TermTrie>>>,
        >,
    >,
    pub(crate) live_query_manager: Arc<crate::live_query::LiveQueryManager>,
    pub(crate) rule_engine: Arc<crate::rules::RuleEngine>,
    #[allow(dead_code)]
    pub(crate) undo_log_capacity: usize,
    pub(crate) json_options: JsonOptions,
    pub(crate) dict_registry: Arc<lilia_core::DictionaryRegistry>,
    pub(crate) hlc: Arc<lilia_core::HybridClock>,
    pub(crate) catalog: Arc<CollectionCatalog>,
    pub(crate) integrity: Arc<crate::integrity::IntegrityRegistry>,
    pub(crate) mem_engine: Arc<MemEngine>,
    pub(crate) key_registry: Arc<lilia_core::KeyRegistry>,
    pub(crate) autotune: Arc<AutotuneEngine>,
    /// Optional local temporal sharding manager (enabled via `DatabaseOptions::with_sharding`).
    pub(crate) shard_manager: Option<Arc<crate::sharding::ShardManager>>,
    pub(crate) embedders: Arc<Embedders>,
    pub(crate) embed_options: lilia_core::EmbedOptions,
    pub(crate) ledger: Arc<crate::ledger::LedgerManager>,
    pub(crate) ledger_enabled: bool,
}

#[derive(Debug)]
pub(crate) struct Resources {
    // Stop/join maintenance before dropping the database connections.
    checkpointer: Mutex<Option<Checkpointer>>,
    pub(crate) ts_worker: Mutex<Option<TimeSeriesWorker>>,
    autotune_worker: Mutex<Option<AutotuneWorker>>,
    embed_worker: Mutex<Option<EmbedWorker>>,
    writer: Mutex<Option<NativeConnection>>,
    readers: Vec<Mutex<Option<NativeConnection>>>,
    next_reader: AtomicUsize,
    writer_queue: WriterQueue,
    /// Set once at open when temporal sharding is enabled; background writers
    /// (ring-buffer flush, time-series maintenance) route through it.
    shards: std::sync::OnceLock<Arc<crate::sharding::ShardManager>>,
}

#[doc(hidden)]
pub struct WriterGuard<'a> {
    // Drop the connection lock before making the next FIFO permit available.
    connection: MutexGuard<'a, Option<NativeConnection>>,
    _permit: Option<Permit<'a>>,
    _operation: Option<Operation<'a>>,
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

pub(crate) fn op_collection(op: &BatchOperation) -> (lilia_core::CollectionKind, &str) {
    use lilia_core::CollectionKind;
    match op {
        BatchOperation::KvSet { namespace, .. } | BatchOperation::KvDelete { namespace, .. } => {
            (CollectionKind::Kv, namespace.as_str())
        }
        BatchOperation::JsonPut { space, .. }
        | BatchOperation::JsonPutRaw { space, .. }
        | BatchOperation::JsonPutHlc { space, .. }
        | BatchOperation::JsonDelete { space, .. } => (CollectionKind::Json, space.as_str()),
        BatchOperation::GraphSetNode { graph, .. }
        | BatchOperation::GraphDeleteNode { graph, .. }
        | BatchOperation::GraphAddEdge { graph, .. }
        | BatchOperation::GraphRemoveEdge { graph, .. } => (CollectionKind::Graph, graph.as_str()),
        BatchOperation::FtsIndex { collection, .. }
        | BatchOperation::FtsDelete { collection, .. } => (CollectionKind::Fts, collection.as_str()),
        BatchOperation::VectorSet { collection, .. }
        | BatchOperation::VectorDelete { collection, .. } => {
            (CollectionKind::Vector, collection.as_str())
        }
        BatchOperation::CrdtPnCounterIncrement { space, .. }
        | BatchOperation::CrdtPnCounterDecrement { space, .. }
        | BatchOperation::CrdtOrSetAdd { space, .. }
        | BatchOperation::CrdtOrSetRemove { space, .. }
        | BatchOperation::CrdtJsonSetField { space, .. }
        | BatchOperation::CrdtJsonDeleteField { space, .. }
        | BatchOperation::CrdtJsonMerge { space, .. } => (CollectionKind::Crdt, space.as_str()),
    }
}

pub(crate) fn op_requires_sync(op: &BatchOperation) -> bool {
    matches!(
        op,
        BatchOperation::KvSet {
            if_version: Some(_),
            ..
        } | BatchOperation::KvDelete {
            if_version: Some(_),
            ..
        } | BatchOperation::JsonPut {
            if_version: Some(_),
            ..
        } | BatchOperation::JsonPutRaw {
            if_version: Some(_),
            ..
        } | BatchOperation::JsonDelete {
            if_version: Some(_),
            ..
        } | BatchOperation::JsonPutHlc { .. }
    )
}

fn register_connection_tier_views(
    conn: &NativeConnection,
    codec: &Arc<dyn ValueCodec>,
    mem_engine: &Arc<crate::mem_engine::MemEngine>,
    ring_overlay: &Arc<crate::ring_buffer::PendingOverlay>,
    catalog: &Arc<crate::catalog::CollectionCatalog>,
) -> Result<()> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        conn.register_vtab_module(
            "_vtab_mem_kv",
            crate::mem_engine::build_mem_kv_vtab(mem_engine, codec),
        )?;
        conn.register_vtab_module(
            "_vtab_mem_json",
            crate::mem_engine::build_mem_json_vtab(mem_engine),
        )?;
        conn.register_vtab_module(
            "_vtab_mem_ts",
            crate::mem_engine::build_mem_ts_vtab(mem_engine),
        )?;
        conn.register_vtab_module(
            "_vtab_mem_graph_nodes",
            crate::mem_engine::build_mem_graph_nodes_vtab(mem_engine),
        )?;
        conn.register_vtab_module(
            "_vtab_mem_graph_edges",
            crate::mem_engine::build_mem_graph_edges_vtab(mem_engine),
        )?;
        conn.register_vtab_module(
            "_vtab_mem_vectors",
            crate::mem_engine::build_mem_vectors_vtab(mem_engine),
        )?;
        conn.register_vtab_module(
            "_vtab_mem_vector_indexes",
            crate::mem_engine::build_mem_vector_indexes_vtab(mem_engine),
        )?;
        conn.register_vtab_module(
            "_vtab_mem_crdt_counters",
            crate::mem_engine::build_mem_crdt_counters_vtab(mem_engine),
        )?;
        conn.register_vtab_module(
            "_vtab_mem_crdt_orsets",
            crate::mem_engine::build_mem_crdt_orsets_vtab(mem_engine),
        )?;
        conn.register_vtab_module(
            "_vtab_mem_crdt_json",
            crate::mem_engine::build_mem_crdt_json_vtab(mem_engine),
        )?;
        conn.register_vtab_module(
            "_vtab_buffered_kv",
            crate::ring_buffer::build_buffered_kv_vtab(ring_overlay, codec),
        )?;
        conn.register_vtab_module(
            "_vtab_buffered_json",
            crate::ring_buffer::build_buffered_json_vtab(ring_overlay),
        )?;
        conn.register_vtab_module(
            "_vtab_catalog",
            crate::mem_engine::build_catalog_vtab(catalog),
        )?;
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (codec, mem_engine, ring_overlay, catalog);
    }
    crate::storage::init_unified_temp_views(conn, !cfg!(target_arch = "wasm32"), true)
}

fn open_connection(
    path: &Path,
    synchronous: &str,
    mmap_size: u64,
    busy_timeout_ms: i32,
    cache_size_kib: i32,
    codec: &Arc<dyn ValueCodec>,
    dict_registry: &Arc<lilia_core::DictionaryRegistry>,
    key_registry: &Arc<lilia_core::KeyRegistry>,
) -> Result<NativeConnection> {
    let conn = NativeConnection::open_with_config(
        path,
        synchronous,
        mmap_size,
        busy_timeout_ms,
        cache_size_kib,
    )?;
    crate::sql::register_sql_functions(&conn, codec, dict_registry, key_registry)?;
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
        let dict_registry = Arc::new(lilia_core::DictionaryRegistry::new());
        let connection = open_connection(
            &options.path,
            synchronous,
            options.mmap_size_bytes,
            busy_ms,
            options.cache_size_kib,
            &options.codec,
            &dict_registry,
            &options.key_registry,
        )?;
        secure_database_file(&options.path)?;
        // Disable SQLite's commit-path checkpoint in favor of background work.
        connection.pragma_update("wal_autocheckpoint", 0)?;
        migrate(&connection)?;

        let ledger = Arc::new(crate::ledger::LedgerManager::open(
            &connection,
            options.ledger_enabled,
            options.ledger_verify_on_open,
        )?);

        // Preload active collection dictionaries from _lilia_json_dictionaries
        if let Ok(mut stmt_dict) = connection.prepare(
            "SELECT space, dict_id, dict_data FROM _lilia_json_dictionaries;"
        ) {
            while let Ok(true) = stmt_dict.step() {
                let space = stmt_dict.column_text(0);
                let dict_id = stmt_dict.column_int64(1) as u32;
                let dict_data = stmt_dict.column_blob(2).to_vec();
                dict_registry.register(space, dict_id, dict_data);
            }
        }

        let catalog = Arc::new(crate::catalog::CollectionCatalog::new(
            options.tier_rules.clone(),
        ));
        catalog.load_from_connection(&connection)?;
        let integrity = Arc::new(crate::integrity::IntegrityRegistry::load(&connection, &catalog)?);
        let mem_engine = Arc::new(crate::mem_engine::MemEngine::new(Arc::clone(&catalog)));
        let mem_db_uri = format!(
            "file:lilia_ephemeral_{}?mode=memory&cache=shared",
            uuid::Uuid::new_v4().simple()
        );
        connection.attach_database("_lilia_mem", &mem_db_uri)?;
        crate::storage::migrate_ephemeral_sql(&connection, "_lilia_mem")?;

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
                &dict_registry,
                &options.key_registry,
            )?;
            reader.attach_database("_lilia_mem", &mem_db_uri)?;
            readers.push(Mutex::new(Some(reader)));
        }
        let node_id = match &options.node_id {
            Some(id) => id.clone(),
            None => {
                let mut node_id = String::new();
                if let Ok(mut stmt) = connection.prepare("SELECT value FROM _lilia_metadata WHERE key = 'node_id';") {
                    if let Ok(true) = stmt.step() {
                        node_id = stmt.column_text(0).to_string();
                    }
                }
                if node_id.is_empty() {
                    node_id = uuid::Uuid::new_v4().to_string();
                    let _ = connection.exec(&format!(
                        "INSERT INTO _lilia_metadata(key, value) VALUES('node_id', '{node_id}') ON CONFLICT(key) DO UPDATE SET value = '{node_id}';"
                    ));
                }
                node_id
            }
        };
        let hlc = Arc::new(match options.max_drift_ms {
            Some(drift) => lilia_core::HybridClock::with_max_drift(node_id, drift),
            None => lilia_core::HybridClock::new(node_id),
        });

        let blobs_dir = options.cas_options.blobs_dir.clone().unwrap_or_else(|| {
            let mut dir = options.path.clone();
            let file_name = dir.file_name().unwrap_or_default().to_string_lossy().to_string();
            dir.set_file_name(format!("{file_name}.blobs"));
            dir
        });
        let cas = Arc::new(crate::cas::CasEngine::new(blobs_dir, options.cas_options.clone()));
        if options.cas_options.enabled && !crate::storage::is_uri_or_memory(&options.path) {
            cas.ensure_dirs()?;
        }

        let lifecycle = Arc::new(Lifecycle::default());
        let resources = Arc::new(Resources {
            checkpointer: Mutex::new(Some(Checkpointer::start(
                &options.path,
                options.checkpoint_policy,
                synchronous,
                Some(Arc::clone(&cas)),
            )?)),
            ts_worker: Mutex::new(None),
            autotune_worker: Mutex::new(None),
            embed_worker: Mutex::new(None),
            writer: Mutex::new(Some(connection)),
            readers,
            next_reader: AtomicUsize::new(0),
            writer_queue: WriterQueue::new(options.writer_queue_capacity),
            shards: std::sync::OnceLock::new(),
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

        let events = Arc::new(tokio::sync::broadcast::channel(1024).0);
        let hnsw_cache = Arc::new(std::sync::RwLock::new(std::collections::HashMap::new()));
        let fts_trie_cache = Arc::new(std::sync::RwLock::new(std::collections::HashMap::new()));
        let ring_buffer = Arc::new(crate::ring_buffer::RingBufferEngine::start(
            options.ring_buffer.clone(),
            Arc::clone(&resources),
            Arc::clone(&lifecycle),
            Arc::clone(&options.codec),
            Arc::clone(&dict_registry),
            options.json_options.clone(),
            Arc::clone(&hlc),
            Arc::clone(&hnsw_cache),
            Arc::clone(&events),
            Arc::clone(&cas),
        )?);
        let ring_core = Arc::clone(&ring_buffer.core);

        {
            let writer_guard = resources
                .writer
                .lock()
                .map_err(|_| LiliaError::new(ErrorCode::Storage, "database lock poisoned", false))?;
            if let Some(writer_conn) = writer_guard.as_ref() {
                register_connection_tier_views(
                    writer_conn,
                    &options.codec,
                    &mem_engine,
                    &ring_core.overlay,
                    &catalog,
                )?;
            }
        }
        for reader_mutex in &resources.readers {
            let reader_guard = reader_mutex.lock().map_err(|_| {
                LiliaError::new(ErrorCode::Storage, "read database lock poisoned", false)
            })?;
            if let Some(reader_conn) = reader_guard.as_ref() {
                register_connection_tier_views(
                    reader_conn,
                    &options.codec,
                    &mem_engine,
                    &ring_core.overlay,
                    &catalog,
                )?;
                reader_conn.pragma_update("query_only", 1)?;
            }
        }

        let rule_engine = Arc::new(crate::rules::RuleEngine::new());
        {
            let writer_guard = resources
                .writer
                .lock()
                .map_err(|_| LiliaError::new(ErrorCode::Storage, "database lock poisoned", false))?;
            if let Some(writer_conn) = writer_guard.as_ref() {
                rule_engine.load_from_connection(writer_conn)?;
            }
        }

        // ── Sharding: initialise ShardManager when sharding is configured ────────
        #[cfg(not(target_arch = "wasm32"))]
        let shard_manager: Option<Arc<crate::sharding::ShardManager>> =
            if let Some(shard_cfg) = options.sharding_config.clone() {
                let manager = Arc::new(crate::sharding::ShardManager::new(
                    &options.path,
                    shard_cfg,
                )?);
                // Load shard catalogue from coordinator (writer) connection.
                {
                    let writer_guard = resources.writer.lock().map_err(|_| {
                        LiliaError::new(ErrorCode::Storage, "database lock poisoned", false)
                    })?;
                    if let Some(w) = writer_guard.as_ref() {
                        manager.load_from_connection(w)?;
                    }
                }
                // Attach every warm/active shard to writer, migrate schema, refresh views.
                let pairs = manager.attached_shard_paths();
                let aliases: Vec<String> = pairs.iter().map(|(a, _)| a.clone()).collect();
                {
                    let writer_guard = resources.writer.lock().map_err(|_| {
                        LiliaError::new(ErrorCode::Storage, "database lock poisoned", false)
                    })?;
                    if let Some(w) = writer_guard.as_ref() {
                        for (alias, path) in &pairs {
                            w.attach_database(alias, &path.to_string_lossy())?;
                            crate::storage::migrate_shard_database(w, alias)?;
                        }
                        crate::sharding::views::refresh_temp_views(
                            w, &aliases, true, true,
                        )?;
                    }
                }
                // Attach to each reader connection (views only, no schema migration needed).
                for reader_mutex in &resources.readers {
                    let reader_guard = reader_mutex.lock().map_err(|_| {
                        LiliaError::new(
                            ErrorCode::Storage,
                            "read database lock poisoned",
                            false,
                        )
                    })?;
                    if let Some(r) = reader_guard.as_ref() {
                        for (alias, path) in &pairs {
                            r.attach_database(alias, &path.to_string_lossy())?;
                        }
                        crate::sharding::views::refresh_temp_views_on_reader(r, &aliases)?;
                    }
                }
                let _ = resources.shards.set(Arc::clone(&manager));
                Some(manager)
            } else {
                None
            };
        #[cfg(target_arch = "wasm32")]
        let shard_manager: Option<Arc<crate::sharding::ShardManager>> = None;
        // ── End sharding init ─────────────────────────────────────────────────────

        let autotune = Arc::new(AutotuneEngine::new(options.autotune_options)?);
        autotune.initialize(&IndexEnv {
            resources: &resources,
            lifecycle: &lifecycle,
            catalog: &catalog,
            codec: options.codec.as_ref(),
            dict_registry: &dict_registry,
        })?;
        #[cfg(not(target_arch = "wasm32"))]
        if autotune.enabled() {
            let worker = AutotuneWorker::start(
                Arc::clone(&autotune),
                Arc::downgrade(&resources),
                Arc::downgrade(&lifecycle),
                Arc::clone(&catalog),
                Arc::clone(&options.codec),
                Arc::clone(&dict_registry),
                Duration::from_millis(options.autotune_options.interval_ms),
            )?;
            *resources
                .autotune_worker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(worker);
        }

        let embedders = Arc::new(Embedders::new(&options.embedders));
        #[cfg(not(target_arch = "wasm32"))]
        if !embedders.is_empty() {
            let worker = EmbedWorker::start(crate::embed_worker::EmbedCore {
                resources: Arc::downgrade(&resources),
                lifecycle: Arc::downgrade(&lifecycle),
                integrity: Arc::clone(&integrity),
                embedders: Arc::clone(&embedders),
                options: options.embed_options,
                hnsw_cache: Arc::clone(&hnsw_cache),
                codec: Arc::clone(&options.codec),
                dict_registry: Arc::clone(&dict_registry),
                json_options: options.json_options.clone(),
                hlc: Arc::clone(&hlc),
                cas: Arc::clone(&cas),
                events: Arc::clone(&events),
            })?;
            *resources
                .embed_worker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(worker);
        }

        Ok(Self {
            path: options.path,
            #[cfg(all(feature = "analytics", not(target_arch = "wasm32")))]
            analytics: Mutex::new(None),
            #[cfg(all(feature = "analytics", not(target_arch = "wasm32")))]
            analytics_options: options.analytics_options,
            ring_buffer,
            ring_core,
            lifecycle,
            resources,
            cas,
            codec: options.codec,
            events,
            hnsw_cache,
            fts_trie_cache,
            live_query_manager: Arc::new(crate::live_query::LiveQueryManager::new()),
            rule_engine,
            undo_log_capacity: options.undo_log_capacity,
            json_options: options.json_options,
            dict_registry,
            hlc,
            catalog,
            integrity,
            mem_engine,
            key_registry: options.key_registry,
            autotune,
            shard_manager,
            embedders,
            embed_options: options.embed_options,
            ledger,
            ledger_enabled: options.ledger_enabled,
        })
    }

    #[must_use]
    pub fn hlc(&self) -> &Arc<lilia_core::HybridClock> {
        &self.hlc
    }

    #[must_use]
    pub fn next_hlc(&self) -> lilia_core::Hlc {
        self.hlc.now()
    }

    pub fn observe_hlc(&self, remote: &lilia_core::Hlc) -> Result<lilia_core::Hlc> {
        self.hlc.update(remote).map_err(Into::into)
    }

    // ── Temporal Sharding Public API ──────────────────────────────────────────

    /// Schemas that hold sharded data: a single unqualified entry without
    /// sharding, otherwise every attached shard followed by the coordinator.
    pub(crate) fn data_schemas(&self) -> Vec<Option<String>> {
        self.shard_manager.as_ref().map_or_else(
            || vec![None],
            |m| m.data_schemas().into_iter().map(Some).collect(),
        )
    }

    /// Rolls over to the next period's shard when the active one has expired and
    /// `auto_rollover` is enabled. Cheap (one clock read) when nothing is due.
    pub(crate) fn roll_shard_if_due(&self) -> Result<()> {
        if let Some(manager) = &self.shard_manager {
            if manager.config.auto_rollover && manager.rollover_due(crate::storage::now_ms()) {
                self.rollover_shard()?;
            }
        }
        Ok(())
    }

    /// The schema new records are written to: the active shard, or `None`
    /// (unqualified) without sharding.
    pub(crate) fn write_schema(&self) -> Option<String> {
        self.shard_manager
            .as_ref()
            .map(|m| m.resolve_target_shard_alias(None))
    }

    fn shard_manager(&self) -> Result<&Arc<crate::sharding::ShardManager>> {
        self.shard_manager.as_ref().ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "sharding is not enabled on this database",
                false,
            )
        })
    }

    /// Runs `f` on the writer connection, then on every pooled reader connection.
    ///
    /// Reader failures are ignored when `strict_readers` is false (used for
    /// `DETACH`, which is a no-op on readers that never attached the shard).
    fn for_each_connection(
        &self,
        strict_readers: bool,
        mut f: impl FnMut(&mut NativeConnection, bool) -> Result<()>,
    ) -> Result<()> {
        {
            let mut writer_guard = self.resources.writer.lock().map_err(|_| {
                LiliaError::new(ErrorCode::Storage, "database lock poisoned", false)
            })?;
            let writer = writer_guard.as_mut().ok_or_else(|| {
                LiliaError::new(ErrorCode::Storage, "writer connection is closed", false)
            })?;
            f(writer, false)?;
        }
        for reader_mutex in &self.resources.readers {
            let mut reader_guard = reader_mutex.lock().map_err(|_| {
                LiliaError::new(ErrorCode::Storage, "read database lock poisoned", false)
            })?;
            if let Some(reader) = reader_guard.as_mut() {
                let outcome = f(reader, true);
                if strict_readers {
                    outcome?;
                }
            }
        }
        Ok(())
    }

    /// Attaches `shard_id` to every connection, migrates its schema on the
    /// writer, and rebuilds the unified views. The catalogue must already list
    /// the shard as attached so that the rebuilt views include it.
    fn attach_shard_everywhere(
        &self,
        manager: &crate::sharding::ShardManager,
        shard_id: &str,
        path: &Path,
    ) -> Result<()> {
        let alias = crate::sharding::ShardLayout::shard_alias(shard_id);
        let path_str = path.to_string_lossy().to_string();
        let aliases = manager.attached_aliases();
        self.for_each_connection(true, |conn, is_reader| {
            conn.attach_database(&alias, &path_str)?;
            if is_reader {
                crate::sharding::views::refresh_temp_views_on_reader(conn, &aliases)
            } else {
                crate::storage::migrate_shard_database(conn, &alias)?;
                crate::sharding::views::refresh_temp_views(conn, &aliases, true, true)
            }
        })
    }

    /// Detaches `shard_id` from every connection and rebuilds the unified
    /// views. The catalogue must already list the shard as not attached so that
    /// the rebuilt views exclude it.
    fn detach_shard_everywhere(
        &self,
        manager: &crate::sharding::ShardManager,
        shard_id: &str,
    ) -> Result<()> {
        let alias = crate::sharding::ShardLayout::shard_alias(shard_id);
        let aliases = manager.attached_aliases();
        self.for_each_connection(false, |conn, is_reader| {
            // Views referencing the shard must go before the schema can be detached.
            if is_reader {
                crate::sharding::views::refresh_temp_views_on_reader(conn, &aliases)?;
            } else {
                crate::sharding::views::refresh_temp_views(conn, &aliases, true, true)?;
            }
            conn.forget_schema_slots(&alias);
            conn.detach_database(&alias)
        })
    }

    /// Returns info on all known temporal shards.
    pub fn list_shards(&self) -> Result<Vec<ShardInfo>> {
        Ok(self.shard_manager()?.list_shards())
    }

    /// Returns info about the currently active (write-target) shard.
    pub fn active_shard(&self) -> Result<ShardInfo> {
        self.shard_manager()?.active_shard()
    }

    /// Manually triggers a rollover: creates a new active partition for the
    /// current wall-clock time and demotes the previous one to `Warm`.
    ///
    /// When `auto_archive_oldest` is enabled and more than `max_attached_shards`
    /// shards are attached afterwards, the oldest warm shards are archived.
    ///
    /// Returns the new active `ShardInfo`.
    pub fn rollover_shard(&self) -> Result<ShardInfo> {
        let manager = self.shard_manager()?;
        let now_ms = self.hlc.now().millis;
        let previous = manager.active_shard()?;
        let new_info = {
            let writer_guard = self.resources.writer.lock().map_err(|_| {
                LiliaError::new(ErrorCode::Storage, "database lock poisoned", false)
            })?;
            let writer = writer_guard.as_ref().ok_or_else(|| {
                LiliaError::new(ErrorCode::Storage, "writer connection is closed", false)
            })?;
            manager.rollover(writer, now_ms)?
        };

        if new_info.shard_id != previous.shard_id {
            self.attach_shard_everywhere(manager, &new_info.shard_id, &new_info.file_path)?;
        }
        self.enforce_attached_shard_limit(manager)?;
        Ok(new_info)
    }

    /// Archives the oldest warm shards while the attached count exceeds the
    /// configured maximum and `auto_archive_oldest` is enabled.
    fn enforce_attached_shard_limit(&self, manager: &crate::sharding::ShardManager) -> Result<()> {
        if !manager.config.auto_archive_oldest {
            return Ok(());
        }
        while manager.attached_aliases().len() > manager.config.max_attached_shards {
            let oldest_warm = manager
                .list_shards()
                .into_iter()
                .find(|s| s.status == lilia_core::sharding::ShardStatus::Warm);
            match oldest_warm {
                Some(info) => {
                    self.detach_and_compress(&info.shard_id)?;
                }
                None => break,
            }
        }
        Ok(())
    }

    /// WAL-checkpoint, compress the shard to a `.lilia.zst` cold archive,
    /// detach it from all connections, and remove the uncompressed `.lilia` file.
    ///
    /// The active shard cannot be archived; roll over first.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn detach_and_compress(&self, shard_id: &str) -> Result<ShardArchiveResult> {
        use lilia_core::sharding::ShardStatus;

        let manager = self.shard_manager()?;
        let mut info = manager.get_shard_info(shard_id)?;
        match info.status {
            ShardStatus::Active => {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("shard '{shard_id}' is the active shard and cannot be archived"),
                    false,
                ));
            }
            ShardStatus::Cold => {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("shard '{shard_id}' is already archived"),
                    false,
                ));
            }
            ShardStatus::Warm | ShardStatus::Detached => {}
        }
        let alias = crate::sharding::ShardLayout::shard_alias(shard_id);
        let db_path = manager.layout.shard_db_path(shard_id);
        let archive_path = manager.layout.archive_path(shard_id);

        // 1. Flush the shard WAL into its main file while still attached, then
        //    take it out of every connection.
        if info.status == ShardStatus::Warm {
            {
                let writer_guard = self.resources.writer.lock().map_err(|_| {
                    LiliaError::new(ErrorCode::Storage, "database lock poisoned", false)
                })?;
                if let Some(writer) = writer_guard.as_ref() {
                    writer.exec(&format!("PRAGMA {alias}.wal_checkpoint(TRUNCATE);"))?;
                }
            }
            info.status = ShardStatus::Detached;
            {
                let writer_guard = self.resources.writer.lock().map_err(|_| {
                    LiliaError::new(ErrorCode::Storage, "database lock poisoned", false)
                })?;
                let writer = writer_guard.as_ref().ok_or_else(|| {
                    LiliaError::new(ErrorCode::Storage, "writer connection is closed", false)
                })?;
                manager.update_shard_info(writer, info.clone())?;
            }
            self.detach_shard_everywhere(manager, shard_id)?;
        }

        // 2. Compress (verifies the archive before removing the source file).
        let result = crate::sharding::archive::compress_shard(
            &db_path,
            &archive_path,
            shard_id,
            manager.config.zstd_level,
        )?;

        // 3. Record the cold tier state in the catalogue.
        info.status = ShardStatus::Cold;
        info.file_path = archive_path;
        info.is_compressed = true;
        info.uncompressed_bytes = result.original_size_bytes;
        info.compressed_bytes = result.compressed_size_bytes;
        info.checksum = Some(result.checksum.clone());
        info.archived_at_ms = Some(result.archived_at_ms);
        let writer_guard = self.resources.writer.lock().map_err(|_| {
            LiliaError::new(ErrorCode::Storage, "database lock poisoned", false)
        })?;
        let writer = writer_guard.as_ref().ok_or_else(|| {
            LiliaError::new(ErrorCode::Storage, "writer connection is closed", false)
        })?;
        manager.update_shard_info(writer, info)?;
        Ok(result)
    }

    /// Decompresses a cold `.lilia.zst` archive back to a `.lilia` file and
    /// re-attaches it to all connections as a `Warm` shard.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn decompress_and_attach(&self, shard_id: &str) -> Result<ShardInfo> {
        use lilia_core::sharding::ShardStatus;

        let manager = self.shard_manager()?;
        let mut info = manager.get_shard_info(shard_id)?;
        if info.status != ShardStatus::Cold {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!("shard '{shard_id}' is not archived"),
                false,
            ));
        }
        let db_path = manager.layout.shard_db_path(shard_id);
        crate::sharding::archive::decompress_shard(&manager.layout.archive_path(shard_id), &db_path)?;

        info.status = ShardStatus::Warm;
        info.file_path = db_path.clone();
        info.is_compressed = false;
        {
            let writer_guard = self.resources.writer.lock().map_err(|_| {
                LiliaError::new(ErrorCode::Storage, "database lock poisoned", false)
            })?;
            let writer = writer_guard.as_ref().ok_or_else(|| {
                LiliaError::new(ErrorCode::Storage, "writer connection is closed", false)
            })?;
            manager.update_shard_info(writer, info.clone())?;
        }
        self.attach_shard_everywhere(manager, shard_id, &db_path)?;
        Ok(info)
    }

    /// Detaches a warm shard without compressing it. Its status becomes `Detached`.
    ///
    /// The active shard cannot be detached; roll over first.
    pub fn detach_shard(&self, shard_id: &str) -> Result<()> {
        use lilia_core::sharding::ShardStatus;

        let manager = self.shard_manager()?;
        let mut info = manager.get_shard_info(shard_id)?;
        if info.status != ShardStatus::Warm {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!("shard '{shard_id}' is not a warm shard"),
                false,
            ));
        }
        info.status = ShardStatus::Detached;
        {
            let writer_guard = self.resources.writer.lock().map_err(|_| {
                LiliaError::new(ErrorCode::Storage, "database lock poisoned", false)
            })?;
            let writer = writer_guard.as_ref().ok_or_else(|| {
                LiliaError::new(ErrorCode::Storage, "writer connection is closed", false)
            })?;
            manager.update_shard_info(writer, info)?;
        }
        self.detach_shard_everywhere(manager, shard_id)
    }

    /// Attaches a shard database file as a `Warm` shard.
    ///
    /// For an already-known detached shard, `path` must be its catalogued file.
    /// Otherwise `path` is an external `.lilia` file whose period is named by
    /// `shard_id`; it is copied into the shards directory.
    pub fn attach_shard(&self, shard_id: &str, path: impl AsRef<Path>) -> Result<()> {
        use lilia_core::sharding::ShardStatus;

        let manager = self.shard_manager()?;
        let source = path.as_ref();
        if !source.is_file() {
            return Err(LiliaError::new(
                ErrorCode::NotFound,
                format!("shard database file not found at {}", source.display()),
                false,
            ));
        }
        let info = match manager.get_shard_info(shard_id) {
            Ok(mut existing) => {
                if existing.status != ShardStatus::Detached {
                    return Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!("shard '{shard_id}' is not detached"),
                        false,
                    ));
                }
                existing.status = ShardStatus::Warm;
                existing
            }
            Err(_) => {
                let (start, end) = manager.config.granularity.parse_period_range(shard_id)?;
                let destination = manager.layout.shard_db_path(shard_id);
                if destination != source {
                    if destination.exists() {
                        return Err(LiliaError::new(
                            ErrorCode::InvalidInput,
                            format!("shard file already exists at {}", destination.display()),
                            false,
                        ));
                    }
                    std::fs::copy(source, &destination).map_err(|e| {
                        LiliaError::new(
                            ErrorCode::Storage,
                            format!("failed to copy shard into shards directory: {e}"),
                            false,
                        )
                    })?;
                }
                let size = std::fs::metadata(&destination).map_or(0, |m| m.len());
                ShardInfo {
                    shard_id: shard_id.to_string(),
                    status: ShardStatus::Warm,
                    period_start_ms: start,
                    period_end_ms: end,
                    file_path: destination,
                    is_compressed: false,
                    uncompressed_bytes: size,
                    compressed_bytes: 0,
                    checksum: None,
                    created_at_ms: self.hlc.now().millis,
                    archived_at_ms: None,
                }
            }
        };
        let file_path = info.file_path.clone();
        {
            let writer_guard = self.resources.writer.lock().map_err(|_| {
                LiliaError::new(ErrorCode::Storage, "database lock poisoned", false)
            })?;
            let writer = writer_guard.as_ref().ok_or_else(|| {
                LiliaError::new(ErrorCode::Storage, "writer connection is closed", false)
            })?;
            manager.update_shard_info(writer, info)?;
        }
        self.attach_shard_everywhere(manager, shard_id, &file_path)
    }


    /// Put a JSON document with HLC and causal concurrency detection.
    pub fn json_put_hlc(
        &self,
        space: &str,
        id: &str,
        value: serde_json::Value,
        hlc: Option<lilia_core::Hlc>,
        parent_hlc: Option<lilia_core::Hlc>,
        strategy: lilia_core::ConflictResolutionStrategy,
    ) -> Result<lilia_core::HlcMutationOutcome> {
        let ops = vec![BatchOperation::JsonPutHlc {
            space: space.to_string(),
            id: id.to_string(),
            value,
            hlc,
            parent_hlc,
            strategy,
        }];
        let results = self.batch(&ops)?;
        let first = results.first().ok_or_else(|| {
            LiliaError::new(ErrorCode::Storage, "empty result from json_put_hlc", false)
        })?;

        let conflict_detected = first.conflict_detected.unwrap_or(false);
        let final_hlc = first.hlc.clone().unwrap_or_else(|| self.hlc.now());

        let (conflict_id, causal_relation) = if conflict_detected {
            let conn = self.read_lock()?;
            let mut stmt = conn.prepare(
                "SELECT id FROM _lilia_conflicts WHERE model = 'json' AND space = ?1 AND doc_id = ?2 ORDER BY id DESC LIMIT 1;",
            )?;
            stmt.bind_text_static(1, space)?;
            stmt.bind_text_static(2, id)?;
            let cid = if stmt.step()? {
                Some(stmt.column_int64(0))
            } else {
                None
            };
            (cid, lilia_core::CausalRelation::Concurrent)
        } else {
            (None, lilia_core::CausalRelation::Descendant)
        };

        Ok(lilia_core::HlcMutationOutcome {
            hlc: final_hlc,
            version: first.version.unwrap_or(1),
            causal_relation,
            conflict_detected,
            conflict_id,
            resolved_by: Some(strategy),
        })
    }

    /// Retrieves the current HLC and causal parent for a document.
    pub fn json_get_hlc(&self, space: &str, id: &str) -> Result<Option<lilia_core::HlcRecord>> {
        let conn = self.read_lock()?;
        let mut stmt = conn.prepare(
            "SELECT hlc, parent_hlc, node_id, updated_at_ms FROM _lilia_record_hlc WHERE model = 'json' AND space = ?1 AND id = ?2;",
        )?;
        stmt.bind_text_static(1, space)?;
        stmt.bind_text_static(2, id)?;
        if stmt.step()? {
            let hlc_str = stmt.column_text(0);
            let parent_str = if stmt.column_is_null(1) {
                None
            } else {
                Some(stmt.column_text(1))
            };
            let node_id = stmt.column_text(2).to_string();
            let updated_at_ms = stmt.column_int64(3);
            let hlc = hlc_str
                .parse::<lilia_core::Hlc>()
                .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
            let parent_hlc = parent_str
                .map(str::parse::<lilia_core::Hlc>)
                .transpose()
                .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
            Ok(Some(lilia_core::HlcRecord {
                model: "json".to_string(),
                space: space.to_string(),
                id: id.to_string(),
                hlc,
                parent_hlc,
                node_id,
                updated_at_ms,
            }))
        } else {
            Ok(None)
        }
    }

    /// Lists recorded conflicts from persistent audit log.
    pub fn list_conflicts(&self, space: Option<&str>) -> Result<Vec<lilia_core::ConflictRecord>> {
        let conn = self.read_lock()?;
        let sql = match space {
            Some(_) => {
                "SELECT id, model, space, doc_id, base_hlc, ours_hlc, theirs_hlc, ours_value, theirs_value, resolved_value, resolution_strategy, status, detected_at_ms                  FROM _lilia_conflicts WHERE space = ?1 ORDER BY id ASC;"
            }
            None => {
                "SELECT id, model, space, doc_id, base_hlc, ours_hlc, theirs_hlc, ours_value, theirs_value, resolved_value, resolution_strategy, status, detected_at_ms                  FROM _lilia_conflicts ORDER BY id ASC;"
            }
        };
        let mut stmt = conn.prepare(sql)?;
        if let Some(sp) = space {
            stmt.bind_text_static(1, sp)?;
        }
        let mut conflicts = Vec::new();
        while stmt.step()? {
            let id = Some(stmt.column_int64(0));
            let model = stmt.column_text(1).to_string();
            let space_name = stmt.column_text(2).to_string();
            let doc_id = stmt.column_text(3).to_string();
            let base_hlc = if stmt.column_is_null(4) {
                None
            } else {
                stmt.column_text(4).parse::<lilia_core::Hlc>().ok()
            };
            let ours_hlc = stmt.column_text(5)
                .parse::<lilia_core::Hlc>()
                .unwrap_or_else(|_| lilia_core::Hlc::new(0, 0, ""));
            let theirs_hlc = stmt.column_text(6)
                .parse::<lilia_core::Hlc>()
                .unwrap_or_else(|_| lilia_core::Hlc::new(0, 0, ""));
            let ours_val: serde_json::Value = serde_json::from_str(stmt.column_text(7))
                .unwrap_or(serde_json::Value::Null);
            let theirs_val: serde_json::Value = serde_json::from_str(stmt.column_text(8))
                .unwrap_or(serde_json::Value::Null);
            let resolved_val: Option<serde_json::Value> = if stmt.column_is_null(9) {
                None
            } else {
                serde_json::from_str(stmt.column_text(9)).ok()
            };
            let strat_str = stmt.column_text(10);
            let strategy = match strat_str {
                s if s.contains("ThreeWayMerge") => lilia_core::ConflictResolutionStrategy::ThreeWayMerge,
                s if s.contains("Manual") => lilia_core::ConflictResolutionStrategy::Manual,
                _ => lilia_core::ConflictResolutionStrategy::LastWriteWins,
            };
            let status = stmt.column_text(11).to_string();
            let detected_at_ms = stmt.column_int64(12);

            conflicts.push(lilia_core::ConflictRecord {
                id,
                model,
                space: space_name,
                doc_id,
                base_hlc,
                ours_hlc,
                theirs_hlc,
                ours_value: ours_val,
                theirs_value: theirs_val,
                resolved_value: resolved_val,
                resolution_strategy: strategy,
                status,
                detected_at_ms,
            });
        }
        Ok(conflicts)
    }

    /// Manually resolves a recorded conflict by updating the document to `resolved_value`.
    pub fn resolve_conflict(&self, conflict_id: i64, resolved_value: serde_json::Value) -> Result<()> {
        let mut writer = self.lock_writer()?;
        let tx = writer.transaction()?;

        let (space, doc_id) = {
            let mut stmt = tx.prepare("SELECT space, doc_id FROM _lilia_conflicts WHERE id = ?1;")?;
            stmt.bind_int64(1, conflict_id)?;
            if stmt.step()? {
                (stmt.column_text(0).to_string(), stmt.column_text(1).to_string())
            } else {
                return Err(LiliaError::new(ErrorCode::NotFound, "conflict not found", false));
            }
        };

        let resolved_str = serde_json::to_string(&resolved_value)
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
        let mut upd = tx.prepare("UPDATE _lilia_conflicts SET status = 'resolved', resolved_value = ?1 WHERE id = ?2;")?;
        upd.bind_text_static(1, &resolved_str)?;
        upd.bind_int64(2, conflict_id)?;
        upd.step()?;
        drop(upd);
        tx.commit()?;
        drop(writer);

        self.json_put_hlc(&space, &doc_id, resolved_value, None, None, lilia_core::ConflictResolutionStrategy::LastWriteWins)?;
        Ok(())
    }

    #[must_use]
    pub fn json_options(&self) -> &JsonOptions {
        &self.json_options
    }

    #[must_use]
    pub fn dict_registry(&self) -> &Arc<lilia_core::DictionaryRegistry> {
        &self.dict_registry
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

    #[must_use]
    pub fn key_registry(&self) -> &Arc<lilia_core::KeyRegistry> {
        &self.key_registry
    }

    /// Creates and persists a new cryptographic compartment in `_lilia_compartments`,
    /// generating a salt and verification canary, and unlocks it in memory.
    pub fn create_compartment(
        &self,
        id: &str,
        passphrase_or_key: &str,
    ) -> Result<lilia_core::KeyMaterial> {
        crate::storage::validate_name("compartment_id", id)?;
        let salt: [u8; 16] = rand::random();
        let key_material = lilia_core::KeyMaterial::from_passphrase(passphrase_or_key, &salt);
        let verify_tag = key_material.compute_verify_tag(&salt);

        let writer = self.lock_writer()?;
        let mut stmt = writer.prepare(
            "INSERT INTO _lilia_compartments(id, salt, verify_hash, created_at_ms) \
             VALUES(?1, ?2, ?3, ?4) \
             ON CONFLICT(id) DO UPDATE SET \
               salt = excluded.salt, \
               verify_hash = excluded.verify_hash, \
               created_at_ms = excluded.created_at_ms;",
        )?;
        stmt.bind_text_static(1, id)?;
        stmt.bind_blob_static(2, &salt)?;
        stmt.bind_blob_static(3, &verify_tag)?;
        stmt.bind_int64(4, crate::storage::now_ms())?;
        stmt.step()?;
        drop(stmt);

        self.key_registry.register_key(id, key_material.clone());
        Ok(key_material)
    }

    /// Unlocks a compartment using provided key material.
    ///
    /// Validates against the stored canary if the compartment was previously registered on disk.
    pub fn unlock_compartment(&self, id: &str, key: lilia_core::KeyMaterial) -> Result<()> {
        crate::storage::validate_name("compartment_id", id)?;
        let conn = self.read_lock()?;
        let mut stmt =
            conn.prepare("SELECT salt, verify_hash FROM _lilia_compartments WHERE id = ?1;")?;
        stmt.bind_text_static(1, id)?;
        if stmt.step()? {
            let salt = stmt.column_blob(0);
            let expected_tag = stmt.column_blob(1);
            let calculated = key.compute_verify_tag(salt);
            if calculated != expected_tag {
                return Err(LiliaError::new(
                    ErrorCode::Unauthorized,
                    format!("invalid key for compartment '{id}'"),
                    false,
                ));
            }
        }
        drop(stmt);
        drop(conn);

        self.key_registry.register_key(id, key);
        Ok(())
    }

    /// Unlocks a compartment using a passphrase.
    pub fn unlock_compartment_with_passphrase(&self, id: &str, passphrase: &str) -> Result<()> {
        crate::storage::validate_name("compartment_id", id)?;
        let conn = self.read_lock()?;
        let mut stmt =
            conn.prepare("SELECT salt, verify_hash FROM _lilia_compartments WHERE id = ?1;")?;
        stmt.bind_text_static(1, id)?;
        if stmt.step()? {
            let salt = stmt.column_blob(0);
            let expected_tag = stmt.column_blob(1);
            let key = lilia_core::KeyMaterial::from_passphrase(passphrase, salt);
            let calculated = key.compute_verify_tag(salt);
            if calculated != expected_tag {
                return Err(LiliaError::new(
                    ErrorCode::Unauthorized,
                    format!("invalid passphrase for compartment '{id}'"),
                    false,
                ));
            }
            drop(stmt);
            drop(conn);
            self.key_registry.register_key(id, key);
            Ok(())
        } else {
            drop(stmt);
            drop(conn);
            let key = lilia_core::KeyMaterial::from_passphrase(passphrase, b"liliadb:ephemeral");
            self.key_registry.register_key(id, key);
            Ok(())
        }
    }

    /// Locks a compartment, removing its key material from memory.
    pub fn lock_compartment(&self, id: &str) -> bool {
        self.key_registry.lock_key(id)
    }

    /// Checks if a compartment is currently unlocked in memory.
    #[must_use]
    pub fn is_compartment_unlocked(&self, id: &str) -> bool {
        self.key_registry.is_unlocked(id)
    }

    /// Lists all known compartments and their unlocked status.
    pub fn list_compartments(&self) -> Result<Vec<lilia_core::CompartmentInfo>> {
        let conn = self.read_lock()?;
        let mut stmt = conn.prepare("SELECT id FROM _lilia_compartments;")?;
        let mut set = std::collections::BTreeSet::new();
        while stmt.step()? {
            set.insert(stmt.column_text(0).to_string());
        }
        drop(stmt);
        drop(conn);

        for comp in self.key_registry.list() {
            set.insert(comp.id);
        }

        let mut out = Vec::new();
        for id in set {
            out.push(lilia_core::CompartmentInfo {
                is_unlocked: self.key_registry.is_unlocked(&id),
                id,
            });
        }
        Ok(out)
    }

    /// Configures the Field-Level Encryption policy for a document collection.
    pub fn set_fle_policy(&self, space: &str, policy: lilia_core::FlePolicy) -> Result<()> {
        crate::storage::validate_name("space", space)?;
        let existing = self.catalog.spec_for(CollectionKind::Json, space);
        self.set_collection_spec(existing.with_fle_policy(policy))
    }

    /// Returns the Field-Level Encryption policy for a document collection if configured.
    #[must_use]
    pub fn fle_policy(&self, space: &str) -> Option<lilia_core::FlePolicy> {
        self.catalog.spec_for(CollectionKind::Json, space).fle_policy
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

    /// Drain pending ring-buffer writes cleanly before stopping admission and closing SQLite.
    pub fn close(&self, timeout: Duration) -> Result<()> {
        // Best effort: statistics are advisory and are rebuilt from the workload if lost.
        let _ = self.autotune.flush(&self.index_env());
        self.ring_buffer.shutdown_and_drain();
        let resources = Arc::clone(&self.resources);
        self.lifecycle.close(timeout, move || resources.close())
    }

    pub(crate) fn index_env(&self) -> IndexEnv<'_> {
        IndexEnv {
            resources: &self.resources,
            lifecycle: &self.lifecycle,
            catalog: &self.catalog,
            codec: self.codec.as_ref(),
            dict_registry: &self.dict_registry,
        }
    }

    /// Snapshot of the workload the auto-tuner observed: query shapes, full scans, index usage
    /// and footprint, and the actions it recommends. Empty when autotune is disabled.
    pub fn autotune_report(&self) -> Result<lilia_core::AutotuneReport> {
        self.autotune_report_at(crate::storage::now_ms())
    }

    /// Runs one tuning cycle immediately: indexes needed by the observed workload are built, and
    /// unused ones are dropped, as far as the options allow; the rest is reported as pending.
    pub fn autotune_run_now(&self) -> Result<lilia_core::CycleReport> {
        self.autotune_run_at(crate::storage::now_ms())
    }

    /// Forgets all recorded workload statistics.
    pub fn autotune_reset_stats(&self) -> Result<()> {
        self.autotune
            .reset(&self.index_env(), crate::storage::now_ms())
    }

    #[doc(hidden)]
    pub fn autotune_report_at(&self, now_ms: i64) -> Result<lilia_core::AutotuneReport> {
        self.autotune.report(&self.index_env(), now_ms)
    }

    #[doc(hidden)]
    pub fn autotune_run_at(&self, now_ms: i64) -> Result<lilia_core::CycleReport> {
        self.autotune.run_cycle(&self.index_env(), now_ms)
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

    /// Returns the current 32-byte cryptographic Merkle Root of the database.
    #[must_use]
    pub fn ledger_root(&self) -> [u8; 32] {
        self.ledger.root()
    }

    /// Returns the latest commit block recorded in the Merkle Ledger, if any.
    pub fn ledger_latest_commit(&self) -> Result<Option<lilia_core::ledger::LedgerCommit>> {
        let conn = self.read_lock()?;
        self.ledger.latest_commit(&conn)
    }

    /// Returns a specific commit block by its 1-based sequence number.
    pub fn ledger_commit(&self, seq: u64) -> Result<Option<lilia_core::ledger::LedgerCommit>> {
        let conn = self.read_lock()?;
        self.ledger.commit_by_seq(&conn, seq)
    }

    /// Exports a mathematical Inclusion Proof for a JSON document.
    pub fn ledger_prove_doc(
        &self,
        space: &str,
        id: &str,
        version: Option<u64>,
    ) -> Result<lilia_core::ledger::InclusionProof> {
        let conn = self.read_lock()?;
        self.ledger.prove_doc(&conn, space, id, version)
    }

    /// Exports a mathematical Inclusion Proof for a KV key-value entry.
    pub fn ledger_prove_kv(
        &self,
        namespace: &str,
        key: &[u8],
        version: Option<u64>,
    ) -> Result<lilia_core::ledger::InclusionProof> {
        let conn = self.read_lock()?;
        self.ledger.prove_kv(&conn, namespace, key, version)
    }

    /// Exports a mathematical Inclusion Proof for a commit block.
    pub fn ledger_prove_commit(&self, commit_seq: u64) -> Result<lilia_core::ledger::InclusionProof> {
        let conn = self.read_lock()?;
        self.ledger.prove_commit(&conn, commit_seq)
    }

    /// Audits the entire Merkle DAG and live records, verifying that no tampering or corruption occurred.
    pub fn verify_ledger(&self) -> Result<lilia_core::ledger::LedgerVerificationReport> {
        let conn = self.read_lock()?;
        self.ledger.verify_ledger(&conn)
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
        let writer = self.lock_writer()?;
        writer.exec("PRAGMA wal_checkpoint(PASSIVE);")?;
        if self.cas.options().enabled && self.cas.options().auto_purge_on_checkpoint {
            let _ = crate::cas::purge_orphan_blobs(&writer, &self.cas);
        }
        Ok(())
    }

    #[must_use]
    pub fn cas(&self) -> &Arc<crate::cas::CasEngine> {
        &self.cas
    }

    pub fn blob_store(&self, data: &[u8]) -> Result<lilia_core::BlobDescriptor> {
        let desc = self.cas.store(data, None)?;
        let now = crate::storage::now_ms();
        let mut writer = self.lock_writer()?;
        let mut tx = writer.transaction()?;
        crate::cas::register_blob_in_db(&mut tx, &desc, now)?;
        tx.commit()?;
        Ok(desc)
    }

    pub fn blob_store_stream<R: std::io::Read>(
        &self,
        reader: &mut R,
        mime_type: Option<String>,
    ) -> Result<lilia_core::BlobDescriptor> {
        let desc = self.cas.store_reader(reader, mime_type)?;
        let now = crate::storage::now_ms();
        let mut writer = self.lock_writer()?;
        let mut tx = writer.transaction()?;
        crate::cas::register_blob_in_db(&mut tx, &desc, now)?;
        tx.commit()?;
        Ok(desc)
    }

    pub fn blob_get(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        self.cas.get(hash)
    }

    pub fn blob_open(&self, hash: &str) -> Result<Option<std::fs::File>> {
        self.cas.open(hash)
    }

    #[must_use]
    pub fn blob_path(&self, hash: &str) -> PathBuf {
        self.cas.path_for_hash(hash)
    }

    #[must_use]
    pub fn blob_exists(&self, hash: &str) -> bool {
        self.cas.exists(hash)
    }

    pub fn blob_gc(&self) -> Result<lilia_core::BlobGcStats> {
        let writer = self.lock_writer()?;
        crate::cas::purge_orphan_blobs(&writer, &self.cas)
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
        use lilia_core::{CollectionKind, StorageTier};

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

        self.roll_shard_if_due()?;

        let effective_ops_cow: std::borrow::Cow<'_, [BatchOperation]> = {
            let mut any_fle = false;
            for op in operations {
                if let BatchOperation::JsonPut { space, .. } = op {
                    let spec = self.catalog.spec_for(CollectionKind::Json, space);
                    if spec.fle_policy.is_some() {
                        any_fle = true;
                        break;
                    }
                }
            }
            if any_fle {
                let mut transformed = Vec::with_capacity(operations.len());
                for op in operations {
                    match op {
                        BatchOperation::JsonPut { space, id, value, if_version } => {
                            let spec = self.catalog.spec_for(CollectionKind::Json, space);
                            if let Some(policy) = &spec.fle_policy {
                                let mut encrypted_val = value.clone();
                                lilia_core::encrypt_json_fields(&mut encrypted_val, policy, &self.key_registry)?;
                                transformed.push(BatchOperation::JsonPut {
                                    space: space.clone(),
                                    id: id.clone(),
                                    value: encrypted_val,
                                    if_version: *if_version,
                                });
                            } else {
                                transformed.push(op.clone());
                            }
                        }
                        _ => transformed.push(op.clone()),
                    }
                }
                std::borrow::Cow::Owned(transformed)
            } else {
                std::borrow::Cow::Borrowed(operations)
            }
        };
        let operations = effective_ops_cow.as_ref();

        let mut tiers = Vec::with_capacity(operations.len());
        let mut is_mem_fts = Vec::with_capacity(operations.len());
        let mut all_mem_non_fts = true;
        let mut all_buffered_async = true;
        let mut any_buffered = false;

        for op in operations {
            let (kind, name) = op_collection(op);
            let tier = self.catalog.tier_for(kind, name);
            let mem_fts = tier == StorageTier::Memory && kind == CollectionKind::Fts;
            if tier != StorageTier::Memory || mem_fts {
                all_mem_non_fts = false;
            }
            if tier != StorageTier::Buffered || op_requires_sync(op) {
                all_buffered_async = false;
            }
            if tier == StorageTier::Buffered {
                any_buffered = true;
            }
            tiers.push(tier);
            is_mem_fts.push(mem_fts);
        }

        // Fast Path 1: pure RAM MemEngine atomic batch (0 SQLite locks, 0 WAL writes).
        if all_mem_non_fts {
            let _op = self.lifecycle.enter()?;
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(LiliaError::new(
                    ErrorCode::Timeout,
                    "request expired before in-memory batch",
                    true,
                ));
            }
            let op_refs: Vec<&BatchOperation> = operations.iter().collect();
            let mut guards = self.mem_engine.lock_kv_json_shards(&op_refs);
            let now = crate::storage::now_ms();
            self.mem_engine
                .preflight_with_guards(&op_refs, &guards, now)?;
            let mut results = Vec::with_capacity(operations.len());
            for op in operations {
                results.push(self.mem_engine.apply_op_with_guards(
                    op,
                    &mut guards,
                    self.codec.as_ref(),
                    self.hlc.as_ref(),
                    now,
                )?);
            }
            self.mem_engine.finish_guards(guards, now);
            self.emit_batch_events(operations, &results, None);
            return Ok(results);
        }

        // Fast Path 2: lock-free WriteRingBuffer / GroupSync path for StorageTier::Buffered.
        if all_buffered_async {
            let _op = self.lifecycle.enter()?;
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(LiliaError::new(
                    ErrorCode::Timeout,
                    "request expired before buffered enqueue",
                    true,
                ));
            }
            let use_group_sync = operations.iter().any(|op| {
                let (kind, name) = op_collection(op);
                self.catalog.spec_for(kind, name).durability == Some(Durability::Durable)
            });
            if use_group_sync {
                let timeout = deadline.map(|d| d.saturating_duration_since(Instant::now()));
                let results = self
                    .ring_core
                    .push_group_sync(operations.to_vec(), timeout)?;
                self.emit_batch_events(operations, &results, None);
                return Ok(results);
            }
            let mut results = Vec::with_capacity(operations.len());
            for op in operations {
                let deleted = matches!(
                    op,
                    BatchOperation::KvDelete { .. }
                        | BatchOperation::JsonDelete { .. }
                        | BatchOperation::GraphDeleteNode { .. }
                        | BatchOperation::GraphRemoveEdge { .. }
                        | BatchOperation::FtsDelete { .. }
                        | BatchOperation::VectorDelete { .. }
                );
                self.ring_core.push_op(op.clone())?;
                results.push(MutationResult {
                    version: None,
                    deleted,
                    conflict_detected: None,
                    hlc: None,
                    tier: StorageTier::Buffered,
                    buffered: true,
                });
            }
            return Ok(results);
        }

        // Before acquiring writer_lock, flush any buffered writes if synchronous ops touch a Buffered space.
        if any_buffered {
            self.ring_core.flush()?;
        }

        // General / Mixed Cross-Tier (Disk + Memory) / Memory-FTS / CAS Synchronous Path.
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

        let session = if self.undo_log_capacity > 0 || self.live_query_manager.has_subscribers() {
            if let Ok(mut sess) = lilia_sqlite_native::NativeSession::create(&connection, "main") {
                // The embedding queue is drained by its worker outside any recorded batch:
                // replaying it from a changeset would conflict with rows already consumed.
                // The columnar store is derived data maintained by triggers and compaction:
                // undoing a batch must re-fire those triggers, not rewind their bookkeeping.
                sess.set_table_filter(|table| {
                    table != "_lilia_embed_queue" && !table.starts_with("_lilia_col_")
                });
                let _ = sess.attach_all();
                Some(sess)
            } else {
                None
            }
        } else {
            None
        };

        let mem_non_fts_refs: Vec<&BatchOperation> = operations
            .iter()
            .zip(&tiers)
            .zip(&is_mem_fts)
            .filter_map(|((op, tier), fts)| {
                if *tier == StorageTier::Memory && !*fts {
                    Some(op)
                } else {
                    None
                }
            })
            .collect();

        let mut transaction = connection.transaction()?;
        let now = crate::storage::now_ms();
        // Strict lock ordering: SQLite writer -> MemEngine shards.
        let mut mem_guards = if mem_non_fts_refs.is_empty() {
            None
        } else {
            let guards = self.mem_engine.lock_kv_json_shards(&mem_non_fts_refs);
            self.mem_engine
                .preflight_with_guards(&mem_non_fts_refs, &guards, now)?;
            Some(guards)
        };

        let mut results = Vec::with_capacity(operations.len());
        for ((operation, &tier), &mem_fts) in operations.iter().zip(&tiers).zip(&is_mem_fts) {
            if tier == StorageTier::Memory {
                if mem_fts {
                    let mut res = match operation {
                        BatchOperation::FtsIndex {
                            collection,
                            doc_id,
                            content,
                        } => crate::transaction::apply_fts_index(
                            &mut transaction,
                            collection,
                            doc_id,
                            content,
                            true,
                        )?,
                        BatchOperation::FtsDelete { collection, doc_id } => {
                            crate::transaction::apply_fts_delete(
                                &mut transaction,
                                collection,
                                doc_id,
                                true,
                            )?
                        }
                        _ => unreachable!("non-FTS operation marked mem_fts"),
                    };
                    res.tier = StorageTier::Memory;
                    results.push(res);
                } else {
                    results.push(crate::mem_engine::mem_result(None, false));
                }
            } else {
                let mut res = apply(
                    &mut transaction,
                    operation,
                    self.codec.as_ref(),
                    &self.dict_registry,
                    &self.json_options,
                    self.hlc.as_ref(),
                    &self.cas,

                    self.shard_manager.as_deref(),
                    Some(self.integrity.as_ref()),
                )?;
                if tier == StorageTier::Buffered {
                    res.tier = StorageTier::Buffered;
                }
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
                results.push(res);
            }
        }

        // In-transaction reactive rules and Complex Event Processing (CEP) evaluation
        if !self.rule_engine.is_empty() {
            let mut pending_ops = operations.to_vec();
            let mut cascade_depth = 0;
            const MAX_CASCADE_DEPTH: usize = 8;

            while !pending_ops.is_empty() {
                if cascade_depth >= MAX_CASCADE_DEPTH {
                    return Err(LiliaError::new(
                        ErrorCode::Conflict,
                        "maximum reactive rule cascade depth exceeded (possible cycle)",
                        false,
                    ));
                }
                cascade_depth += 1;
                let current_eval_ops = std::mem::take(&mut pending_ops);
                for op in &current_eval_ops {
                    let triggered_ops =
                        self.rule_engine
                            .evaluate_in_transaction(&mut transaction, op, now)?;
                    for trig_op in triggered_ops {
                        apply(
                            &mut transaction,
                            &trig_op,
                            self.codec.as_ref(),
                            &self.dict_registry,
                            &self.json_options,
                            self.hlc.as_ref(),
                            &self.cas,

                            self.shard_manager.as_deref(),
                            Some(self.integrity.as_ref()),
                        )?;
                        pending_ops.push(trig_op);
                    }
                }
            }
        }

        if self.ledger_enabled && !operations.is_empty() {
            self.ledger
                .record_commit(&mut transaction, operations, &results, now)?;
        }

        transaction.commit()?;

        if let Some(mut guards) = mem_guards.take() {
            for ((idx, op), (&tier, &mem_fts)) in operations
                .iter()
                .enumerate()
                .zip(tiers.iter().zip(&is_mem_fts))
            {
                if tier == StorageTier::Memory && !mem_fts {
                    results[idx] = self.mem_engine.apply_op_with_guards(
                        op,
                        &mut guards,
                        self.codec.as_ref(),
                        self.hlc.as_ref(),
                        now,
                    )?;
                }
            }
            self.mem_engine.finish_guards(guards, now);
        }

        if let Some(sess) = session {
            if !sess.is_empty() {
                if let Ok(changeset) = sess.changeset() {
                    if !changeset.is_empty() {
                        self.live_query_manager
                            .process_changeset(&changeset, &connection);

                        if self.undo_log_capacity > 0 {
                            if let Ok(inverse) =
                                lilia_sqlite_native::NativeSession::invert_changeset(&changeset)
                            {
                                drop(sess);

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

                                    let _ = undo_tx
                                        .exec("DELETE FROM _lilia_undo_log WHERE undone = 1;");

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
        }

        self.emit_batch_events(operations, &results, Some(&connection));

        let mut auto_train_candidates = Vec::new();
        if self.json_options.auto_train_dict {
            for (op, &tier) in operations.iter().zip(&tiers) {
                if tier != StorageTier::Memory {
                    if let BatchOperation::JsonPut { space, .. } = op {
                        if !self.dict_registry.contains(space)
                            && !self.json_options.disabled_auto_train_spaces.contains(space)
                            && !auto_train_candidates.contains(space)
                        {
                            if let Ok(mut count_stmt) =
                                connection.prepare("SELECT count(*) FROM _lilia_json WHERE space = ?1;")
                            {
                                let _ = count_stmt.bind_text_static(1, space);
                                if let Ok(true) = count_stmt.step() {
                                    let count = count_stmt.column_int64(0) as usize;
                                    if count >= self.json_options.dict_sample_threshold {
                                        auto_train_candidates.push(space.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        drop(watchdog);
        drop(connection);

        if crate::embed::may_queue(&self.integrity, operations, !self.rule_engine.is_empty()) {
            self.resources.notify_embed_worker();
        }

        for space in auto_train_candidates {
            let _ = self.train_json_dictionary(
                &space,
                self.json_options.dict_sample_threshold,
                self.json_options.dict_target_size_bytes,
            );
        }
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
        let mut tx = connection.transaction()?;

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
        let queued = crate::embed::enqueue_changeset(&mut tx, &self.integrity, &raw_inverse)?;

        let mut update_stmt = tx.prepare("UPDATE _lilia_undo_log SET undone = 1 WHERE id = ?1;")?;
        update_stmt.bind_int64(1, id)?;
        update_stmt.step()?;
        drop(update_stmt);

        tx.commit()?;
        drop(connection);
        if queued {
            self.resources.notify_embed_worker();
        }

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
        let mut tx = connection.transaction()?;

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
        let queued = crate::embed::enqueue_changeset(&mut tx, &self.integrity, &raw_forward)?;

        let mut update_stmt = tx.prepare("UPDATE _lilia_undo_log SET undone = 0 WHERE id = ?1;")?;
        update_stmt.bind_int64(1, id)?;
        update_stmt.step()?;
        drop(update_stmt);

        tx.commit()?;
        drop(connection);
        if queued {
            self.resources.notify_embed_worker();
        }

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

    #[must_use]
    pub fn has_live_subscribers(&self) -> bool {
        self.live_query_manager.has_subscribers()
    }

    /// Subscribe to live differential query results.
    /// Emits initial snapshot immediately, followed by `{ added, updated, removed }` diffs on every commit.
    pub fn live_query<F>(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        callback: F,
    ) -> Result<LiveQueryHandle>
    where
        F: Fn(lilia_core::LiveQueryDiff) + Send + Sync + 'static,
    {
        let cb: Arc<dyn Fn(lilia_core::LiveQueryDiff) + Send + Sync + 'static> = Arc::new(callback);
        let (id, _parsed) = self
            .live_query_manager
            .register(sql, params, Arc::clone(&cb))?;

        let initial_res = self.sql_query(sql, params)?;
        let mut added = Vec::with_capacity(initial_res.rows.len());
        for row in initial_res.rows {
            let mut obj = serde_json::Map::with_capacity(initial_res.columns.len());
            for (col, val) in initial_res.columns.iter().zip(row) {
                obj.insert(col.clone(), val);
            }
            added.push(serde_json::Value::Object(obj));
        }

        let initial_diff = lilia_core::LiveQueryDiff {
            subscription_id: id,
            added,
            updated: Vec::new(),
            removed: Vec::new(),
            is_initial: true,
        };
        cb(initial_diff);

        Ok(LiveQueryHandle {
            id,
            manager: Arc::clone(&self.live_query_manager),
        })
    }

    /// Define and materialize an incremental materialized view updated in O(1) on mutations.
    pub fn create_materialized_view(&self, name: &str, query: &str) -> Result<()> {
        let writer = self.lock_writer()?;
        crate::ivm::create_materialized_view(&writer, name, query)
    }

    /// Drop an existing incremental materialized view and its associated triggers.
    pub fn drop_materialized_view(&self, name: &str) -> Result<()> {
        let writer = self.lock_writer()?;
        crate::ivm::drop_materialized_view(&writer, name)
    }

    /// Explicitly refresh an incremental materialized view from source table.
    pub fn refresh_materialized_view(&self, name: &str) -> Result<()> {
        let writer = self.lock_writer()?;
        crate::ivm::refresh_materialized_view(&writer, name)
    }

    /// List all registered incremental materialized views.
    pub fn list_materialized_views(&self) -> Result<Vec<lilia_core::MaterializedViewInfo>> {
        let writer = self.lock_writer()?;
        crate::ivm::list_materialized_views(&writer)
    }

    /// Open the rules manager to register, list, and unregister reactive CEP rules.
    #[must_use]
    pub fn rules(&self) -> crate::rules::RulesManager<'_> {
        crate::rules::RulesManager::new(self)
    }

    /// Open a fluent builder for declaring, configuring, or accessing a tier-routed collection.
    #[must_use]
    pub fn collection(&self, name: &str) -> crate::collection::CollectionBuilder<'_> {
        crate::collection::CollectionBuilder::new(self, name)
    }

    /// Configure or update the routing specification for a collection (`Disk`, `Buffered`, or `Memory`).
    ///
    /// Pure `Memory` collection configurations stay strictly in RAM unless replacing a previously
    /// persistent specification, guaranteeing zero WAL/disk I/O for ephemeral spaces.
    pub fn set_collection_spec(&self, spec: lilia_core::CollectionSpec) -> Result<()> {
        crate::storage::validate_name("collection", &spec.name)?;
        let spec = self.reconcile_spec(spec)?;
        let prev = self
            .catalog
            .list_specs()
            .into_iter()
            .find(|s| s.kind == spec.kind && s.name == spec.name);

        if spec.tier == lilia_core::StorageTier::Memory {
            if prev.is_some_and(|p| p.tier != lilia_core::StorageTier::Memory) {
                let writer = self.lock_writer()?;
                CollectionCatalog::delete_persisted_spec(&writer, spec.kind, &spec.name)?;
            }
        } else {
            let writer = self.lock_writer()?;
            crate::catalog::CollectionCatalog::persist_spec(&writer, &spec)?;
        }

        self.catalog.set_spec(spec);
        Ok(())
    }

    /// Return the effective [`lilia_core::CollectionSpec`] for `(kind, name)`.
    #[must_use]
    pub fn get_collection_spec(
        &self,
        kind: lilia_core::CollectionKind,
        name: &str,
    ) -> lilia_core::CollectionSpec {
        self.catalog.spec_for(kind, name)
    }

    /// Return all explicitly configured collection specifications.
    #[must_use]
    pub fn list_collections(&self) -> Vec<lilia_core::CollectionSpec> {
        self.catalog.list_specs()
    }

    /// Open an active multi-model transaction with declarative savepoint tree support.
    pub fn transaction(&self) -> Result<crate::transaction::Transaction<'_>> {
        let writer = self.lock_writer()?;
        writer.begin_immediate()?;
        Ok(crate::transaction::Transaction::new(self, writer))
    }

    /// Execute an atomic multi-model transaction via closure.
    ///
    /// Automatically commits on `Ok` and rolls back on `Err` or panic.
    pub fn with_transaction<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&mut crate::transaction::Transaction<'_>) -> Result<R>,
    {
        let mut tx = self.transaction()?;
        let outcome = f(&mut tx)?;
        tx.commit()?;
        Ok(outcome)
    }

    /// Execute an atomic cross-tier batch transaction across Disk and Memory tiers.
    pub fn cross_tier_transaction<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&mut crate::collection::CrossTierTransaction<'_>) -> Result<R>,
    {
        let mut tx = crate::collection::CrossTierTransaction::new(self);
        let outcome = f(&mut tx)?;
        tx.commit()?;
        Ok(outcome)
    }

    /// Promote an ephemeral (`Memory`) or `Buffered` collection to durable `Disk` storage.
    pub fn promote_collection(
        &self,
        kind: lilia_core::CollectionKind,
        name: &str,
    ) -> Result<usize> {
        use lilia_core::{CollectionKind, StorageTier};
        crate::storage::validate_name("collection", name)?;
        let current = self.catalog.spec_for(kind, name);
        if current.tier == StorageTier::Disk {
            return Ok(0);
        }
        let mut next = current.clone();
        next.tier = StorageTier::Disk;

        if current.tier == StorageTier::Buffered {
            self.ring_core.flush()?;
            self.set_collection_spec(next)?;
            return Ok(0);
        }

        let now = crate::storage::now_ms();
        let mut moved = 0usize;
        let mut vector_hnsw_cfg = None;

        {
            let mut writer = self.lock_writer()?;
            let mut tx = writer.transaction()?;

            match kind {
                CollectionKind::Kv => {
                    let items = self.mem_engine.kv.scan(Some(name), None, usize::MAX, now);
                    moved = items.len();
                    let kv_table = crate::sharding::qualified(self.write_schema().as_deref(), "_lilia_kv");
                    for (_, key, snap) in items {
                        let mut stmt = tx.prepare(&format!(
                            "INSERT INTO {kv_table}(namespace, key, value, version, expires_at_ms) \
                             VALUES(?1, ?2, ?3, ?4, ?5) \
                             ON CONFLICT(namespace, key) DO UPDATE SET \
                               value = excluded.value, \
                               version = MAX(_lilia_kv.version + 1, excluded.version), \
                               expires_at_ms = excluded.expires_at_ms;"
                        ))?;
                        stmt.bind_text_static(1, name)?;
                        stmt.bind_blob_static(2, &key)?;
                        stmt.bind_blob_static(3, &snap.value)?;
                        stmt.bind_int64(
                            4,
                            i64::try_from(snap.version.max(1)).unwrap_or(i64::MAX),
                        )?;
                        match snap.expires_at_ms {
                            Some(exp) => stmt.bind_int64(5, exp)?,
                            None => stmt.bind_null(5)?,
                        }
                        stmt.step()?;
                    }
                }
                CollectionKind::Json => {
                    let items = self.mem_engine.json.scan(Some(name), None, usize::MAX, now);
                    moved = items.len();
                    for (_, id_bytes, snap) in items {
                        let id = String::from_utf8_lossy(&id_bytes).into_owned();
                        let op = BatchOperation::JsonPut {
                            space: name.to_owned(),
                            id,
                            value: snap.value,
                            if_version: None,
                        };
                        crate::transaction::apply(
                            &mut tx,
                            &op,
                            self.codec.as_ref(),
                            &self.dict_registry,
                            &self.json_options,
                            self.hlc.as_ref(),
                            &self.cas,

                            self.shard_manager.as_deref(),
                            Some(self.integrity.as_ref()),
                        )?;
                    }
                }
                CollectionKind::TimeSeries => {
                    let items =
                        self.mem_engine
                            .timeseries
                            .scan(Some(name), None, usize::MAX, now);
                    for (_, key_bytes, snap) in items {
                        let resolution_ms = if key_bytes.len() == 8 {
                            let mut arr = [0u8; 8];
                            arr.copy_from_slice(&key_bytes);
                            i64::from_be_bytes(arr)
                        } else {
                            0
                        };
                        moved += snap.value.len();
                        crate::timeseries::append_chunk_in_tx(
                            &tx,
                            name,
                            resolution_ms,
                            &snap.value,
                            true,
                            self.shard_manager.as_deref(),
                        )?;
                    }
                }
                CollectionKind::Graph => {
                    let nodes =
                        self.mem_engine
                            .graph_nodes
                            .scan(Some(name), None, usize::MAX, now);
                    let edges =
                        self.mem_engine
                            .graph_edges
                            .scan(Some(name), None, usize::MAX, now);
                    moved = nodes.len() + edges.len();
                    for (_, _, snap) in nodes {
                        let n = snap.value;
                        let op = BatchOperation::GraphSetNode {
                            graph: n.graph,
                            id: n.id,
                            kind: n.kind,
                            metadata: n.metadata,
                            doc_space: n.doc_space,
                            doc_id: n.doc_id,
                        };
                        crate::transaction::apply(
                            &mut tx,
                            &op,
                            self.codec.as_ref(),
                            &self.dict_registry,
                            &self.json_options,
                            self.hlc.as_ref(),
                            &self.cas,

                            self.shard_manager.as_deref(),
                            Some(self.integrity.as_ref()),
                        )?;
                    }
                    for (_, _, snap) in edges {
                        let e = snap.value;
                        let op = BatchOperation::GraphAddEdge {
                            graph: e.graph,
                            source: e.source,
                            target: e.target,
                            kind: e.kind,
                            weight: e.weight,
                        };
                        crate::transaction::apply(
                            &mut tx,
                            &op,
                            self.codec.as_ref(),
                            &self.dict_registry,
                            &self.json_options,
                            self.hlc.as_ref(),
                            &self.cas,

                            self.shard_manager.as_deref(),
                            Some(self.integrity.as_ref()),
                        )?;
                    }
                }
                CollectionKind::Vector => {
                    vector_hnsw_cfg = self
                        .mem_engine
                        .hnsw_meta
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .get(name)
                        .cloned();
                    let vecs = self.mem_engine.vectors.scan(Some(name), None, usize::MAX, now);
                    moved = vecs.len();
                    for (_, id_bytes, snap) in vecs {
                        let id = String::from_utf8_lossy(&id_bytes).into_owned();
                        let (dim, element_type, vector) = snap.value;
                        let op = BatchOperation::VectorSet {
                            collection: name.to_owned(),
                            id,
                            dim,
                            element_type,
                            vector,
                        };
                        crate::transaction::apply(
                            &mut tx,
                            &op,
                            self.codec.as_ref(),
                            &self.dict_registry,
                            &self.json_options,
                            self.hlc.as_ref(),
                            &self.cas,

                            self.shard_manager.as_deref(),
                            Some(self.integrity.as_ref()),
                        )?;
                    }
                }
                CollectionKind::Fts => {
                    let mut rows = Vec::new();
                    {
                        let mut stmt = tx.prepare(
                            "SELECT doc_id, content FROM _lilia_mem._lilia_mem_fts WHERE collection = ?1;",
                        )?;
                        stmt.bind_text_static(1, name)?;
                        while stmt.step()? {
                            rows.push((
                                stmt.column_text(0).to_string(),
                                stmt.column_text(1).to_string(),
                            ));
                        }
                    }
                    moved = rows.len();
                    for (doc_id, content) in rows {
                        crate::transaction::apply_fts_index(&mut tx, name, &doc_id, &content, false)?;
                    }
                    let mut del = tx.prepare(
                        "DELETE FROM _lilia_mem._lilia_mem_fts WHERE collection = ?1;",
                    )?;
                    del.bind_text_static(1, name)?;
                    del.step()?;
                }
                CollectionKind::Crdt => {
                    let counters = self
                        .mem_engine
                        .crdt_counters
                        .scan(Some(name), None, usize::MAX, now);
                    let orsets = self
                        .mem_engine
                        .crdt_orsets
                        .scan(Some(name), None, usize::MAX, now);
                    let jsons = self
                        .mem_engine
                        .crdt_json
                        .scan(Some(name), None, usize::MAX, now);
                    moved = counters.len() + orsets.len() + jsons.len();
                    for (_, id_bytes, snap) in counters {
                        let id = String::from_utf8_lossy(&id_bytes).into_owned();
                        let counter = snap.value;
                        for (actor, p) in &counter.p {
                            let n = counter.n.get(actor).copied().unwrap_or(0);
                            let mut stmt = tx.prepare(
                                "INSERT INTO _lilia_crdt_counters(space, id, actor_id, p_count, n_count) \
                                 VALUES(?1, ?2, ?3, ?4, ?5) \
                                 ON CONFLICT(space, id, actor_id) DO UPDATE SET \
                                   p_count = MAX(_lilia_crdt_counters.p_count, excluded.p_count), \
                                   n_count = MAX(_lilia_crdt_counters.n_count, excluded.n_count);",
                            )?;
                            stmt.bind_text_static(1, name)?;
                            stmt.bind_text_static(2, &id)?;
                            stmt.bind_text_static(3, actor)?;
                            stmt.bind_int64(4, i64::try_from(*p).unwrap_or(i64::MAX))?;
                            stmt.bind_int64(5, i64::try_from(n).unwrap_or(i64::MAX))?;
                            stmt.step()?;
                        }
                        for (actor, n) in &counter.n {
                            if !counter.p.contains_key(actor) {
                                let mut stmt = tx.prepare(
                                    "INSERT INTO _lilia_crdt_counters(space, id, actor_id, p_count, n_count) \
                                     VALUES(?1, ?2, ?3, 0, ?4) \
                                     ON CONFLICT(space, id, actor_id) DO UPDATE SET \
                                       n_count = MAX(_lilia_crdt_counters.n_count, excluded.n_count);",
                                )?;
                                stmt.bind_text_static(1, name)?;
                                stmt.bind_text_static(2, &id)?;
                                stmt.bind_text_static(3, actor)?;
                                stmt.bind_int64(4, i64::try_from(*n).unwrap_or(i64::MAX))?;
                                stmt.step()?;
                            }
                        }
                    }
                    for (_, id_bytes, snap) in orsets {
                        let id = String::from_utf8_lossy(&id_bytes).into_owned();
                        let set = snap.value;
                        for (elem, tags) in set.entries {
                            for tag in tags {
                                let tomb = set.tombstones.get(&tag).copied();
                                let mut stmt = tx.prepare(
                                    "INSERT OR REPLACE INTO _lilia_crdt_orsets(space, id, element, tag, tombstone_ms) \
                                     VALUES(?1, ?2, ?3, ?4, ?5);",
                                )?;
                                stmt.bind_text_static(1, name)?;
                                stmt.bind_text_static(2, &id)?;
                                stmt.bind_text_static(3, &elem)?;
                                stmt.bind_text_static(4, &tag)?;
                                match tomb {
                                    Some(t) => stmt.bind_int64(5, t)?,
                                    None => stmt.bind_null(5)?,
                                }
                                stmt.step()?;
                            }
                        }
                    }
                    for (_, id_bytes, snap) in jsons {
                        let id = String::from_utf8_lossy(&id_bytes).into_owned();
                        let (doc, materialized, _) = snap.value;
                        let raw = serde_json::to_vec(&doc)
                            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
                        let state = lilia_core::compress_bytes(&raw);
                        let mat_str = serde_json::to_string(&materialized)
                            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
                        let mut stmt = tx.prepare(
                            "INSERT INTO _lilia_crdt_json(space, id, state, materialized, version) \
                             VALUES(?1, ?2, ?3, ?4, ?5) \
                             ON CONFLICT(space, id) DO UPDATE SET \
                               state = excluded.state, \
                               materialized = excluded.materialized, \
                               version = MAX(_lilia_crdt_json.version + 1, excluded.version);",
                        )?;
                        stmt.bind_text_static(1, name)?;
                        stmt.bind_text_static(2, &id)?;
                        stmt.bind_blob_static(3, &state)?;
                        stmt.bind_text_static(4, &mat_str)?;
                        stmt.bind_int64(
                            5,
                            i64::try_from(snap.version.max(1)).unwrap_or(i64::MAX),
                        )?;
                        stmt.step()?;
                    }
                }
                CollectionKind::Geofence | CollectionKind::SqlTable => {
                    return Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!("{} collections cannot be promoted automatically", kind.as_str()),
                        false,
                    ));
                }
            }

            tx.commit()?;
        }

        // Clear memory structures after SQLite commit and persist Disk catalog spec.
        match kind {
            CollectionKind::Kv => {
                self.mem_engine.kv.clear_space(name);
            }
            CollectionKind::Json => {
                self.mem_engine.json.clear_space(name);
            }
            CollectionKind::TimeSeries => {
                self.mem_engine.timeseries.clear_space(name);
            }
            CollectionKind::Graph => {
                self.mem_engine.graph_nodes.clear_space(name);
                self.mem_engine.graph_edges.clear_space(name);
            }
            CollectionKind::Vector => {
                self.mem_engine.vectors.clear_space(name);
                self.mem_engine
                    .hnsw_meta
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(name);
                self.mem_engine
                    .hnsw_indices
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(name);
            }
            CollectionKind::Fts | CollectionKind::Geofence | CollectionKind::SqlTable => {}
            CollectionKind::Crdt => {
                self.mem_engine.crdt_counters.clear_space(name);
                self.mem_engine.crdt_orsets.clear_space(name);
                self.mem_engine.crdt_json.clear_space(name);
            }
        }

        self.set_collection_spec(next)?;
        if let Some(cfg) = vector_hnsw_cfg {
            self.vector_create_index(name, cfg)?;
        }
        Ok(moved)
    }

    /// Demote a persistent collection to [`StorageTier::Memory`] or [`StorageTier::Buffered`].
    #[allow(clippy::too_many_lines)]
    pub fn demote_collection(
        &self,
        kind: lilia_core::CollectionKind,
        name: &str,
        target: lilia_core::StorageTier,
    ) -> Result<usize> {
        use lilia_core::{CollectionKind, StorageTier};
        crate::storage::validate_name("collection", name)?;
        self.ensure_tier_allowed(kind, name, target)?;
        let current = self.catalog.spec_for(kind, name);
        if current.tier == target {
            return Ok(0);
        }
        if target == StorageTier::Disk {
            return self.promote_collection(kind, name);
        }
        if target == StorageTier::Buffered {
            let moved = if current.tier == StorageTier::Memory {
                self.promote_collection(kind, name)?
            } else {
                0
            };
            let mut next = self.catalog.spec_for(kind, name);
            next.tier = StorageTier::Buffered;
            self.set_collection_spec(next)?;
            return Ok(moved);
        }

        // Target is StorageTier::Memory. Flush buffered writes first so nothing is left behind.
        if current.tier == StorageTier::Buffered {
            self.ring_core.flush()?;
        }

        let now = crate::storage::now_ms();
        let mut next = current.clone();
        next.tier = StorageTier::Memory;
        let mut moved = 0usize;

        {
            let mut writer = self.lock_writer()?;
            let tx = writer.transaction()?;

            match kind {
                CollectionKind::Kv => {
                    let mut rows = Vec::new();
                    {
                        let mut stmt = tx.prepare(
                            "SELECT key, value, version, expires_at_ms FROM _lilia_kv \
                             WHERE namespace = ?1 AND (expires_at_ms IS NULL OR expires_at_ms > ?2);",
                        )?;
                        stmt.bind_text_static(1, name)?;
                        stmt.bind_int64(2, now)?;
                        while stmt.step()? {
                            let key = stmt.column_blob(0).to_vec();
                            let value = stmt.column_blob(1).to_vec();
                            let ver = u64::try_from(stmt.column_int64(2).max(1)).unwrap_or(1);
                            let exp = if stmt.column_is_null(3) {
                                None
                            } else {
                                Some(stmt.column_int64(3))
                            };
                            rows.push((key, value, ver, exp));
                        }
                    }
                    moved = rows.len();
                    for schema in self.data_schemas() {
                        let kv_table = crate::sharding::qualified(schema.as_deref(), "_lilia_kv");
                        let mut del = tx.prepare(&format!("DELETE FROM {kv_table} WHERE namespace = ?1;"))?;
                        del.bind_text_static(1, name)?;
                        del.step()?;
                    }
                    CollectionCatalog::delete_persisted_spec(tx.conn(), kind, name)?;
                    tx.commit()?;
                    self.catalog.set_spec(next);
                    for (key, value, _ver, exp) in rows {
                        let _ = self.mem_engine.kv_put_direct(name, &key, value, None, exp);
                    }
                }
                CollectionKind::Json => {
                    let mut rows = Vec::new();
                    {
                        let mut stmt = tx.prepare(
                            "SELECT id, value, format, dict_id FROM _lilia_json WHERE space = ?1;",
                        )?;
                        stmt.bind_text_static(1, name)?;
                        while stmt.step()? {
                            let id = stmt.column_text(0).to_string();
                            let format = stmt.column_int64(2);
                            let dict_id = stmt.column_int64(3);
                            let doc = crate::json::decode_stored_doc(
                                &stmt,
                                1,
                                format,
                                dict_id,
                                name,
                                self.codec.as_ref(),
                                &self.dict_registry,
                                Some(self.key_registry.as_ref()),
                            )?;
                            rows.push((id, doc.to_json_value()));
                        }
                    }
                    moved = rows.len();
                    for schema in self.data_schemas() {
                        for table in ["_lilia_json", "_lilia_json_index_entries"] {
                            let table = crate::sharding::qualified(schema.as_deref(), table);
                            let mut del = tx.prepare(&format!("DELETE FROM {table} WHERE space = ?1;"))?;
                            del.bind_text_static(1, name)?;
                            del.step()?;
                        }
                    }
                    CollectionCatalog::delete_persisted_spec(tx.conn(), kind, name)?;
                    tx.commit()?;
                    self.catalog.set_spec(next);
                    for (id, val) in rows {
                        let _ = self.mem_engine.json_put_direct(name, &id, val, None);
                    }
                }
                CollectionKind::TimeSeries => {
                    let mut rows = Vec::new();
                    {
                        let mut stmt = tx.prepare(
                            "SELECT resolution_ms, data FROM _lilia_timeseries_chunks WHERE series_id = ?1;",
                        )?;
                        stmt.bind_text_static(1, name)?;
                        while stmt.step()? {
                            let res = stmt.column_int64(0);
                            let pts = lilia_core::decompress_timeseries(stmt.column_blob(1))?;
                            moved += pts.len();
                            rows.push((res, pts));
                        }
                    }
                    for schema in self.data_schemas() {
                        let chunks = crate::sharding::qualified(schema.as_deref(), "_lilia_timeseries_chunks");
                        let mut del = tx.prepare(&format!(
                            "DELETE FROM {chunks} WHERE series_id = ?1;"
                        ))?;
                        del.bind_text_static(1, name)?;
                        del.step()?;
                    }
                    crate::catalog::CollectionCatalog::delete_persisted_spec(
                        tx.conn(),
                        kind,
                        name,
                    )?;
                    tx.commit()?;
                    self.catalog.set_spec(next);
                    for (res, pts) in rows {
                        self.mem_engine.ts_append_direct(name, res, &pts)?;
                    }
                }
                CollectionKind::Graph => {
                    let mut nodes = Vec::new();
                    let mut edges = Vec::new();
                    {
                        let mut stmt = tx.prepare(
                            "SELECT id, kind, metadata, doc_space, doc_id FROM _lilia_graph_nodes WHERE graph = ?1;",
                        )?;
                        stmt.bind_text_static(1, name)?;
                        while stmt.step()? {
                            let meta: serde_json::Value =
                                serde_json::from_str(stmt.column_text(2)).map_err(|e| {
                                    LiliaError::new(ErrorCode::Storage, e.to_string(), false)
                                })?;
                            nodes.push(lilia_core::GraphNode {
                                graph: name.to_owned(),
                                id: stmt.column_text(0).to_string(),
                                kind: stmt.column_text(1).to_string(),
                                metadata: meta,
                                doc_space: if stmt.column_is_null(3) {
                                    None
                                } else {
                                    Some(stmt.column_text(3).to_string())
                                },
                                doc_id: if stmt.column_is_null(4) {
                                    None
                                } else {
                                    Some(stmt.column_text(4).to_string())
                                },
                            });
                        }
                    }
                    {
                        let mut stmt = tx.prepare(
                            "SELECT source, target, kind, weight FROM _lilia_graph_edges WHERE graph = ?1;",
                        )?;
                        stmt.bind_text_static(1, name)?;
                        while stmt.step()? {
                            edges.push(lilia_core::GraphEdge {
                                graph: name.to_owned(),
                                source: stmt.column_text(0).to_string(),
                                target: stmt.column_text(1).to_string(),
                                kind: stmt.column_text(2).to_string(),
                                weight: stmt.column_double(3),
                            });
                        }
                    }
                    moved = nodes.len() + edges.len();
                    for sql in [
                        "DELETE FROM _lilia_graph_edges WHERE graph = ?1;",
                        "DELETE FROM _lilia_graph_nodes WHERE graph = ?1;",
                    ] {
                        let mut del = tx.prepare(sql)?;
                        del.bind_text_static(1, name)?;
                        del.step()?;
                    }
                    CollectionCatalog::delete_persisted_spec(tx.conn(), kind, name)?;
                    tx.commit()?;
                    self.catalog.set_spec(next);
                    for n in nodes {
                        let _ = self.mem_engine.graph_set_node(n)?;
                    }
                    for e in edges {
                        let _ = self.mem_engine.graph_add_edge(e)?;
                    }
                }
                CollectionKind::Vector => {
                    let hnsw_cfg = {
                        let mut stmt = tx.prepare(
                            "SELECT m, ef_construction, ef_search, metric, element_type, dim \
                             FROM _lilia_hnsw_meta WHERE collection = ?1;",
                        )?;
                        stmt.bind_text_static(1, name)?;
                        if stmt.step()? {
                            let metric: lilia_core::VectorMetric = stmt.column_text(3).parse()?;
                            let element_type =
                                lilia_core::VectorElementType::try_from(stmt.column_int64(4))?;
                            Some(lilia_core::HnswConfig {
                                m: usize::try_from(stmt.column_int64(0).max(2)).unwrap_or(16),
                                ef_construction: usize::try_from(stmt.column_int64(1).max(1))
                                    .unwrap_or(64),
                                ef_search: usize::try_from(stmt.column_int64(2).max(1))
                                    .unwrap_or(32),
                                metric,
                                element_type,
                                dim: u32::try_from(stmt.column_int64(5).max(0)).unwrap_or(0),
                            })
                        } else {
                            None
                        }
                    };
                    let mut vecs = Vec::new();
                    {
                        let mut stmt = tx.prepare(
                            "SELECT id, dim, element_type, vector FROM _lilia_vectors WHERE collection = ?1;",
                        )?;
                        stmt.bind_text_static(1, name)?;
                        while stmt.step()? {
                            let id = stmt.column_text(0).to_string();
                            let dim = u32::try_from(stmt.column_int64(1).max(0)).unwrap_or(0);
                            let el =
                                lilia_core::VectorElementType::try_from(stmt.column_int64(2))?;
                            let blob = stmt.column_blob(3).to_vec();
                            vecs.push((id, dim, el, blob));
                        }
                    }
                    moved = vecs.len();
                    for sql in [
                        "DELETE FROM _lilia_vectors WHERE collection = ?1;",
                        "DELETE FROM _lilia_hnsw_edges WHERE collection = ?1;",
                        "DELETE FROM _lilia_hnsw_meta WHERE collection = ?1;",
                    ] {
                        let mut del = tx.prepare(sql)?;
                        del.bind_text_static(1, name)?;
                        del.step()?;
                    }
                    CollectionCatalog::delete_persisted_spec(tx.conn(), kind, name)?;
                    tx.commit()?;
                    self.hnsw_cache
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(name);
                    self.catalog.set_spec(next);
                    if let Some(cfg) = hnsw_cfg {
                        let idx = Arc::new(std::sync::RwLock::new(lilia_core::HnswIndex::new(
                            cfg.clone(),
                        )));
                        self.mem_engine
                            .hnsw_meta
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .insert(name.to_owned(), cfg);
                        self.mem_engine
                            .hnsw_indices
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .insert(name.to_owned(), idx);
                    }
                    for (id, dim, el, blob) in vecs {
                        self.mem_engine.vector_set(name, &id, dim, el, &blob)?;
                    }
                }
                CollectionKind::Fts => {
                    let mut docs = Vec::new();
                    {
                        let mut stmt = tx.prepare(
                            "SELECT doc_id, content FROM _lilia_fts WHERE collection = ?1;",
                        )?;
                        stmt.bind_text_static(1, name)?;
                        while stmt.step()? {
                            docs.push((
                                stmt.column_text(0).to_string(),
                                stmt.column_text(1).to_string(),
                            ));
                        }
                    }
                    moved = docs.len();
                    {
                        let mut del = tx.prepare("DELETE FROM _lilia_fts WHERE collection = ?1;")?;
                        del.bind_text_static(1, name)?;
                        del.step()?;
                    }
                    {
                        let mut clear_mem = tx.prepare(
                            "DELETE FROM _lilia_mem._lilia_mem_fts WHERE collection = ?1;",
                        )?;
                        clear_mem.bind_text_static(1, name)?;
                        clear_mem.step()?;
                    }
                    for (doc_id, content) in docs {
                        let mut ins = tx.prepare(
                            "INSERT INTO _lilia_mem._lilia_mem_fts(collection, doc_id, content) VALUES(?1, ?2, ?3);",
                        )?;
                        ins.bind_text_static(1, name)?;
                        ins.bind_text_static(2, &doc_id)?;
                        ins.bind_text_static(3, &content)?;
                        ins.step()?;
                    }
                    CollectionCatalog::delete_persisted_spec(tx.conn(), kind, name)?;
                    tx.commit()?;
                    self.catalog.set_spec(next);
                }
                CollectionKind::Crdt => {
                    let mut counters = Vec::new();
                    let mut orsets = Vec::new();
                    let mut jsons = Vec::new();
                    {
                        let mut stmt = tx.prepare(
                            "SELECT id, actor_id, p_count, n_count FROM _lilia_crdt_counters WHERE space = ?1;",
                        )?;
                        stmt.bind_text_static(1, name)?;
                        while stmt.step()? {
                            counters.push((
                                stmt.column_text(0).to_string(),
                                stmt.column_text(1).to_string(),
                                u64::try_from(stmt.column_int64(2).max(0)).unwrap_or(0),
                                u64::try_from(stmt.column_int64(3).max(0)).unwrap_or(0),
                            ));
                        }
                    }
                    {
                        let mut stmt = tx.prepare(
                            "SELECT id, element, tombstone_ms FROM _lilia_crdt_orsets WHERE space = ?1;",
                        )?;
                        stmt.bind_text_static(1, name)?;
                        while stmt.step()? {
                            let is_active = stmt.column_is_null(2);
                            if is_active {
                                orsets.push((
                                    stmt.column_text(0).to_string(),
                                    stmt.column_text(1).to_string(),
                                ));
                            }
                        }
                    }
                    {
                        let mut stmt = tx.prepare(
                            "SELECT id, state FROM _lilia_crdt_json WHERE space = ?1;",
                        )?;
                        stmt.bind_text_static(1, name)?;
                        while stmt.step()? {
                            let id = stmt.column_text(0).to_string();
                            let raw = lilia_core::decompress_bytes(stmt.column_blob(1))?;
                            let doc: lilia_core::crdt::LwwJsonDocument =
                                serde_json::from_slice(&raw).map_err(|e| {
                                    LiliaError::new(ErrorCode::Storage, e.to_string(), false)
                                })?;
                            jsons.push((id, doc));
                        }
                    }
                    moved = counters.len() + orsets.len() + jsons.len();
                    for sql in [
                        "DELETE FROM _lilia_crdt_counters WHERE space = ?1;",
                        "DELETE FROM _lilia_crdt_orsets WHERE space = ?1;",
                        "DELETE FROM _lilia_crdt_json WHERE space = ?1;",
                    ] {
                        let mut del = tx.prepare(sql)?;
                        del.bind_text_static(1, name)?;
                        del.step()?;
                    }
                    CollectionCatalog::delete_persisted_spec(tx.conn(), kind, name)?;
                    tx.commit()?;
                    self.catalog.set_spec(next);
                    for (id, actor, p, n) in counters {
                        if p > 0 {
                            self.mem_engine.crdt_counter_incr(name, &id, &actor, p, false);
                        }
                        if n > 0 {
                            self.mem_engine.crdt_counter_incr(name, &id, &actor, n, true);
                        }
                    }
                    for (id, elem) in orsets {
                        let tag = format!("demote-{}", uuid::Uuid::new_v4().simple());
                        self.mem_engine
                            .crdt_orset_mutate(name, &id, Some(&tag), &elem, true, 0);
                    }
                    for (id, doc) in jsons {
                        self.mem_engine.crdt_json_merge(name, &id, &doc);
                    }
                }
                CollectionKind::Geofence | CollectionKind::SqlTable => {
                    return Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!("{} collections cannot be demoted automatically", kind.as_str()),
                        false,
                    ));
                }
            }
        }

        Ok(moved)
    }

    /// Create an ephemeral SQL table inside the RAM-only `_lilia_mem` shared-cache database.
    ///
    /// Accepts `CREATE TABLE ...` or `CREATE EPHEMERAL TABLE ...`. Zero disk/WAL bytes are written
    /// and the table disappears cleanly when the database is closed and reopened.
    pub fn create_memory_table(&self, sql: &str) -> Result<()> {
        let (table_name, rewritten) = crate::storage::rewrite_ephemeral_create_sql(sql)?;
        let writer = self.lock_writer()?;
        {
            let mut check = writer.prepare(
                "SELECT 1 FROM main.sqlite_master WHERE name = ?1 LIMIT 1;",
            )?;
            check.bind_text_static(1, &table_name)?;
            if check.step()? {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("ephemeral table '{table_name}' conflicts with persistent table in main"),
                    false,
                ));
            }
        }
        writer.exec(&rewritten)?;
        self.catalog.register_ephemeral_table(table_name);
        Ok(())
    }

    /// Acquire a cloneable high-speed producer handle into LiliaDB's lock-free [`crate::ring_buffer::WriteRingBuffer`].
    #[must_use]
    pub fn ring_buffer(&self) -> crate::ring_buffer::RingBufferHandle {
        crate::ring_buffer::RingBufferHandle {
            core: Arc::clone(&self.ring_core),
        }
    }

    /// Append TimeSeries points via the lock-free [`crate::ring_buffer::WriteRingBuffer`] (or RAM for [`StorageTier::Memory`]).
    pub fn ts_append_buffered(
        &self,
        series_id: &str,
        points: &[lilia_core::TimeSeriesPoint],
    ) -> Result<()> {
        crate::storage::validate_name("series_id", series_id)?;
        if self.catalog.tier_for(lilia_core::CollectionKind::TimeSeries, series_id)
            == lilia_core::StorageTier::Memory
        {
            self.mem_engine.ts_append_direct(series_id, 0, points)?;
            return Ok(());
        }
        self.ring_core.push_ts_points(series_id, 0, points.to_vec())
    }

    /// Enqueue a batch of operations into the group-commit worker and wait for SQLite commit completion.
    pub fn commit_group_batch(
        &self,
        ops: Vec<BatchOperation>,
        timeout: Option<Duration>,
    ) -> Result<Vec<MutationResult>> {
        self.ring_core.push_group_sync(ops, timeout)
    }

    /// Synchronously flush all pending operations queued in the [`crate::ring_buffer::WriteRingBuffer`].
    pub fn flush_writes(&self) -> Result<lilia_core::RingBufferStats> {
        self.ring_core.flush()
    }

    /// Return current diagnostics and throughput counters from the [`crate::ring_buffer::WriteRingBuffer`].
    #[must_use]
    pub fn ring_buffer_stats(&self) -> lilia_core::RingBufferStats {
        self.ring_core.stats()
    }

    /// Return current entry counts, byte estimates, and TTL/LRU eviction counters for the RAM tier.
    #[must_use]
    pub fn memory_tier_stats(&self) -> lilia_core::MemoryTierStats {
        self.mem_engine.stats()
    }

    fn emit_batch_events(
        &self,
        operations: &[BatchOperation],
        results: &[MutationResult],
        connection: Option<&NativeConnection>,
    ) {
        if self.events.receiver_count() == 0 {
            return;
        }
        for (op, res) in operations.iter().zip(results) {
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
                BatchOperation::JsonPut { space, id, .. }
                | BatchOperation::JsonPutRaw { space, id, .. } => {
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
                BatchOperation::CrdtPnCounterIncrement { space, id, .. }
                | BatchOperation::CrdtPnCounterDecrement { space, id, .. } => {
                    let val = if self.catalog.tier_for(lilia_core::CollectionKind::Crdt, space)
                        == lilia_core::StorageTier::Memory
                    {
                        self.mem_engine
                            .crdt_counters
                            .get(space, id.as_bytes(), crate::storage::now_ms(), false)
                            .map_or(0, |s| s.value.value())
                    } else {
                        self.crdt().counter(space, id).get().unwrap_or(0)
                    };
                    Some(lilia_core::DatabaseEvent::CrdtCounterMutated {
                        space: space.clone(),
                        id: id.clone(),
                        value: val,
                    })
                }
                BatchOperation::CrdtOrSetAdd {
                    space, id, element, ..
                } => Some(lilia_core::DatabaseEvent::CrdtSetMutated {
                    space: space.clone(),
                    id: id.clone(),
                    element: element.clone(),
                    deleted: false,
                }),
                BatchOperation::CrdtOrSetRemove {
                    space, id, element, ..
                } => Some(lilia_core::DatabaseEvent::CrdtSetMutated {
                    space: space.clone(),
                    id: id.clone(),
                    element: element.clone(),
                    deleted: true,
                }),
                BatchOperation::CrdtJsonSetField { space, id, .. }
                | BatchOperation::CrdtJsonDeleteField { space, id, .. }
                | BatchOperation::CrdtJsonMerge { space, id, .. } => {
                    Some(lilia_core::DatabaseEvent::CrdtJsonMutated {
                        space: space.clone(),
                        id: id.clone(),
                        version: res.version,
                    })
                }
                BatchOperation::JsonPutHlc { space, id, .. } => {
                    if res.conflict_detected == Some(true) {
                        if let Some(conn) = connection {
                            if let Ok(mut stmt_conf) = conn.prepare(
                                "SELECT base_hlc, ours_hlc, theirs_hlc, resolution_strategy \
                                 FROM _lilia_conflicts WHERE model = 'json' AND space = ?1 AND doc_id = ?2 \
                                 ORDER BY id DESC LIMIT 1;",
                            ) {
                                let _ = stmt_conf.bind_text_static(1, space);
                                let _ = stmt_conf.bind_text_static(2, id);
                                if let Ok(true) = stmt_conf.step() {
                                    let base_h = if stmt_conf.column_is_null(0) {
                                        None
                                    } else {
                                        Some(stmt_conf.column_text(0).to_string())
                                    };
                                    let _ = self.events.send(
                                        lilia_core::DatabaseEvent::ConflictDetected {
                                            model: "json".to_string(),
                                            space: space.clone(),
                                            id: id.clone(),
                                            base_hlc: base_h,
                                            ours_hlc: stmt_conf.column_text(1).to_string(),
                                            theirs_hlc: stmt_conf.column_text(2).to_string(),
                                            resolution_strategy: stmt_conf.column_text(3).to_string(),
                                        },
                                    );
                                }
                            }
                        }
                    }
                    Some(lilia_core::DatabaseEvent::JsonMutated {
                        space: space.clone(),
                        id: id.clone(),
                        version: res.version,
                        deleted: false,
                    })
                }
            };
            if let Some(event) = evt {
                let _ = self.events.send(event);
            }
        }
    }
}

/// Active subscription handle for a live differential query.
/// Automatically unregisters on drop or explicit call to `unsubscribe()`.
#[derive(Clone)]
pub struct LiveQueryHandle {
    pub id: u64,
    manager: Arc<crate::live_query::LiveQueryManager>,
}

impl std::fmt::Debug for LiveQueryHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveQueryHandle")
            .field("id", &self.id)
            .finish()
    }
}

impl LiveQueryHandle {
    pub fn unsubscribe(&self) {
        self.manager.unregister(self.id);
    }
}

impl Drop for LiveQueryHandle {
    fn drop(&mut self) {
        self.manager.unregister(self.id);
    }
}

impl Resources {
    pub(crate) fn shards(&self) -> Option<&crate::sharding::ShardManager> {
        self.shards.get().map(AsRef::as_ref)
    }

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
        if connection.is_none() {
            return Err(LiliaError::new(
                ErrorCode::Closed,
                "database writer connection is closed",
                false,
            ));
        }
        Ok(WriterGuard {
            connection,
            _permit: Some(permit),
            _operation: Some(operation),
        })
    }

    pub(crate) fn lock_writer_for_drain<'a>(
        &'a self,
        lifecycle: &'a Lifecycle,
        closing: bool,
    ) -> Result<WriterGuard<'a>> {
        let operation = if closing {
            None
        } else {
            Some(lifecycle.enter()?)
        };
        let permit = if closing {
            None
        } else {
            Some(self.writer_queue.acquire(None)?)
        };
        let connection = self
            .writer
            .lock()
            .map_err(|_| LiliaError::new(ErrorCode::Storage, "database lock poisoned", false))?;
        if connection.is_none() {
            return Err(LiliaError::new(
                ErrorCode::Closed,
                "database writer connection is closed",
                false,
            ));
        }
        Ok(WriterGuard {
            connection,
            _permit: permit,
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
        if connection.is_none() {
            return Err(LiliaError::new(
                ErrorCode::Closed,
                "database reader connection is closed",
                false,
            ));
        }
        Ok(WriterGuard {
            connection,
            _permit: None,
            _operation: Some(operation),
        })
    }

    pub(crate) fn notify_checkpointer(&self) {
        if let Some(cp) = self
            .checkpointer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            cp.notify();
        }
    }

    pub(crate) fn notify_embed_worker(&self) {
        if let Some(worker) = self
            .embed_worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            worker.notify();
        }
    }

    fn close(&self) -> Result<()> {
        // No admitted operations remain. Potentially slow I/O runs on the cleanup
        // thread so the caller's timeout is independent of filesystem progress.
        let embed_worker = self
            .embed_worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(embed_worker);
        let autotune_worker = self
            .autotune_worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(autotune_worker);
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
