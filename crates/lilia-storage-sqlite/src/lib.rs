#![allow(unsafe_code)]

pub mod aggregation;
mod autotune;
mod autotune_worker;
mod bitemporal;
mod branch;
pub mod cache;
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
pub mod parity;
#[cfg(not(target_arch = "wasm32"))]
pub mod polyglot;
pub mod prefetch;
pub mod reconciliation;
mod ring_buffer;
pub mod rules;
pub mod security;
pub mod sharding;
mod sql;
mod storage;
mod timeseries;
mod timeseries_worker;
mod transaction;
mod vector;
pub mod wasm_udf;
pub mod wear;
mod writer_queue;

pub use aggregation::{doc, Aggregate, AggregationPipeline, Sort, SortDirection, StreamDocument};
pub use bitemporal::BitemporalStore;
pub use branch::Branch;
pub use cache::CacheStats;
pub use cas::CasEngine;
pub use catalog::CollectionCatalog;
pub use checkpoint::CheckpointStats;
pub use collection::{
    CollectionBuilder, CrdtCollection, CrossTierTransaction, FtsCollection, GraphCollection,
    JsonCollection, KvCollection, TimeSeriesCollection, VectorCollection,
};
#[cfg(all(feature = "analytics", not(target_arch = "wasm32")))]
pub use columnar::{
    Analytics, AnalyticsOptions, ColumnSpec, ColumnType, ColumnarSourceKind, ColumnarTableInfo,
    ColumnarTableSpec, ColumnarTableStats, CompactionStats, QueryOutput,
};
pub use crdt::{CounterStore, CrdtStore, DocStore, SetStore};
#[doc(hidden)]
pub use engine::WriterGuard;
pub use engine::{
    CheckpointPolicy, Database, DatabaseOptions, JsonOptions, LiveQueryHandle, ResourceProfile,
    TableStorageStat,
};
pub use integrity::Integrity;
pub use ivm::{
    create_materialized_view, drop_materialized_view, list_materialized_views,
    refresh_materialized_view,
};
pub use json::{FindTrace, JsonStore};
pub use lilia_core::parity::{PageChecksum, PageRepairOutcome, ParityConfig, RepairSummary};
pub use lilia_core::security::{
    MaskFunction, MaskingRule, NamedPolicy, Policy, PolicyAction, SecurityContext,
};
pub use lilia_core::sharding::{
    ShardArchiveResult, ShardGranularity, ShardInfo, ShardStatus, ShardingConfig,
};
pub use lilia_core::wear::{FlashWearTelemetry, FlashWearTier, WearLevelingPolicy};
pub use lilia_core::{
    h3, AccessTarget, AutoEmbedConfig, AutotuneOptions, AutotuneReport, BlobDescriptor,
    BlobGcStats, BranchDiff, BuiltinModel, CasHashAlgorithm, CasOptions, CollectionKind,
    CollectionOptions, CollectionSpec, CollectionTierEntry, CommitMode, ConstraintKind,
    CycleReport, EmbedOptions, EmbedStatus, Embedder, EvictionPolicy, InclusionProof, IndexOrigin,
    IndexPhase, IndexRef, IndexUsageView, JsonQueryReport, KeyMapping, LedgerCommit,
    LedgerVerificationReport, LedgerVerifyMode, MarkovConfig, MarkovStats, MemoryTierStats,
    MergeResult, MergeStrategy, OverflowPolicy, PruneScope, Recommendation, RefAction, Reference,
    ReferenceSource, RingBufferConfig, RingBufferStats, SqlQueryReport, StorageTier, TableDiff,
};
pub use live_query::{LiveQueryCallback, LiveQueryManager};
pub use parity::ParityManager;
pub use prefetch::CacheOptions;
pub use reconciliation::{P2pApplyStats, P2pDocumentPayload, ReconciliationSummary};
pub use ring_buffer::RingBufferHandle;
pub use rules::{
    Action, Rule, RuleBuilder, RulesManager, SlidingWindow, TriggerMode, WindowCondition,
};
pub use security::{
    SecuredDatabase, SecuredDatabaseArc, SecurityContextGuard, SecurityManager, SecurityRegistry,
};
pub use sharding::ShardManager;
pub use sql::{CarrayBindable, SqlStore};
pub use timeseries_worker::TimeSeriesOptions;
pub use transaction::{Transaction, TxFts, TxGraph, TxJson, TxKv, TxVector};
pub use wasm_udf::{WasmUdfConfig, WasmUdfInfo, WasmUdfRegistry};
pub use wear::FlashWearManager;
