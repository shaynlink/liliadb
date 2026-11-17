#![allow(unsafe_code)]

pub mod aggregation;
mod autotune;
mod autotune_worker;
pub mod cas;
mod catalog;
mod checkpoint;
pub mod collection;
#[cfg(all(feature = "analytics", not(target_arch = "wasm32")))]
pub mod columnar;
mod crdt;
mod embed;
mod embed_worker;
mod engine;
mod fts;
mod geofence;
mod graph;
mod hybrid;
mod integrity;
pub mod ivm;
mod json;
mod kv;
mod ledger;
mod lifecycle;
pub mod live_query;
pub mod mem_engine;
mod ring_buffer;
pub mod rules;
pub mod sharding;
mod sql;
mod storage;
mod timeseries;
mod timeseries_worker;
mod transaction;
mod vector;
mod writer_queue;

pub use aggregation::{
    doc, Aggregate, AggregationPipeline, Sort, SortDirection, StreamDocument,
};
pub use cas::CasEngine;
pub use catalog::CollectionCatalog;
pub use checkpoint::CheckpointStats;
#[cfg(all(feature = "analytics", not(target_arch = "wasm32")))]
pub use columnar::{
    Analytics, AnalyticsOptions, ColumnSpec, ColumnType, ColumnarSourceKind, ColumnarTableInfo,
    ColumnarTableSpec, ColumnarTableStats, CompactionStats, QueryOutput,
};
pub use collection::{
    CollectionBuilder, CrdtCollection, CrossTierTransaction, FtsCollection, GraphCollection,
    JsonCollection, KvCollection, TimeSeriesCollection, VectorCollection,
};
pub use crdt::{CounterStore, CrdtStore, DocStore, SetStore};
#[doc(hidden)]
pub use engine::WriterGuard;
pub use engine::{
    CheckpointPolicy, Database, DatabaseOptions, JsonOptions, LiveQueryHandle, ResourceProfile,
    TableStorageStat,
};
pub use ivm::{
    create_materialized_view, drop_materialized_view, list_materialized_views,
    refresh_materialized_view,
};
pub use integrity::Integrity;
pub use json::JsonStore;
pub use lilia_core::{
    AutotuneOptions, AutotuneReport, CycleReport, IndexOrigin, IndexPhase, IndexRef, IndexUsageView,
    JsonQueryReport, PruneScope, Recommendation, SqlQueryReport,
    BlobDescriptor, BlobGcStats, CasHashAlgorithm, CasOptions, CollectionKind, CollectionSpec,
    CollectionOptions, CollectionTierEntry, CommitMode, ConstraintKind, EvictionPolicy, KeyMapping,
    AutoEmbedConfig, BuiltinModel, EmbedOptions, EmbedStatus, Embedder,
    InclusionProof, LedgerCommit, LedgerVerificationReport, LedgerVerifyMode,
    MemoryTierStats, OverflowPolicy, RefAction, Reference, ReferenceSource, RingBufferConfig,
    RingBufferStats, StorageTier,
};
pub use live_query::{LiveQueryCallback, LiveQueryManager};
pub use ring_buffer::RingBufferHandle;
pub use rules::{Action, Rule, RuleBuilder, RulesManager, SlidingWindow, TriggerMode, WindowCondition};
pub use sharding::ShardManager;
pub use transaction::{Transaction, TxFts, TxGraph, TxJson, TxKv, TxVector};
pub use lilia_core::sharding::{
    ShardArchiveResult, ShardGranularity, ShardInfo, ShardStatus, ShardingConfig,
};
pub use timeseries_worker::TimeSeriesOptions;
