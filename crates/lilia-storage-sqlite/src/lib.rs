#![allow(unsafe_code)]

mod bitemporal;
mod branch;
pub mod cache;
mod checkpoint;
mod engine;
mod fts;
mod geofence;
mod graph;
mod hybrid;
mod json;
mod kv;
mod lifecycle;
pub mod prefetch;
mod sql;
mod storage;
mod timeseries;
mod timeseries_worker;
mod transaction;
mod vector;
mod writer_queue;

pub use bitemporal::BitemporalStore;
pub use branch::Branch;
pub use cache::CacheStats;
pub use checkpoint::CheckpointStats;
#[doc(hidden)]
pub use engine::WriterGuard;
pub use engine::{CheckpointPolicy, Database, DatabaseOptions, ResourceProfile, TableStorageStat};
pub use json::JsonStore;
pub use lilia_core::{doc, h3, AccessTarget, BranchDiff, MarkovConfig, MarkovStats, MergeResult, MergeStrategy, TableDiff};
pub use prefetch::CacheOptions;
pub use sql::{CarrayBindable, SqlStore};
pub use timeseries_worker::TimeSeriesOptions;
