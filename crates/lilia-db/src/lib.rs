//! `LiliaDB` data models over the native storage engine ([`lilia_engine`]).
//!
//! - **Key-value**: namespaced byte keys and values, with optional expiry
//!   ([`Session::kv_get`], [`Session::kv_scan`]).
//! - **JSON documents**: documents by id in spaces, stored in the binary document format, with
//!   secondary indexes over paths or expressions, partial indexes, and equality queries served
//!   by the indexes ([`Session::json_find`], [`Database::create_index`]).
//! - **Conditional batches**: atomic groups of writes over every model, each optionally
//!   conditional on the version of its record ([`Batch`], [`Write`]). A record's version is the
//!   engine sequence number of the write that produced it.
//! - **Transactions**: optimistic, over a snapshot, validated at commit ([`Transaction`]).
//! - **Row-level security and masking**: every read and write of a [`Session`] passes through
//!   one guard that applies the policies and masking rules of the collection for the caller's
//!   [`SecurityContext`] ([`guard`]).
//! - **Graph, full-text search, vectors, time series, geo, CRDTs, bitemporal entities**: model
//!   handles of a session or transaction ([`Graph`], [`Fts`], [`Vectors`], [`TimeSeries`],
//!   [`Geo`], [`Crdt`], [`Bitemporal`]), whose writes are batch writes like any other.
//! - **History**: reads in the past within a retention window and revisions of records
//!   ([`history`]).
//! - **SQL** (feature `sql`): read-only `DataFusion` queries over tables of the models, through
//!   the caller's policies ([`Session::sql`]).
//! - **Ledger**: an optional tamper-evident history of every record write, a Merkle Mountain
//!   Range with `O(log n)` appends and proofs ([`ledger`]).

mod batch;
pub mod bitemporal;
mod catalog;
mod context;
pub mod crdt;
mod db;
mod error;
pub mod find;
pub mod fts;
pub mod geo;
pub mod graph;
pub mod guard;
pub mod history;
mod keys;
pub mod ledger;
mod read;
mod record;
mod records;
mod session;
#[cfg(feature = "sql")]
pub mod sql;
pub mod timeseries;
mod txn;
pub mod vectors;
mod write;

pub use batch::{Batch, Outcome, Write};
pub use bitemporal::{Bitemporal, BitemporalWrite};
pub use context::Context;
pub use crdt::{Crdt, CrdtCounter, CrdtDoc, CrdtSet, CrdtWrite};
pub use db::{Database, Options, UndoStep, DEFAULT_HISTORY_RETENTION};
pub use error::{Error, Result};
pub use fts::{Fts, FtsWrite};
pub use geo::{Fence, Geo, GeoWrite, Polygon, TrackPoint};
pub use graph::{Graph, GraphWrite};
pub use guard::Access;
pub use history::{JsonRevision, KvRevision};
pub use ledger::mmr::MmrProof;
pub use ledger::{CommitProof, RecordProof};
pub use lilia_core::{
    BinaryDoc, Direction, DownsampleAggregation, FtsFuzzyHit, FtsHit, FtsPhoneticHit,
    FtsSuggestion, GeofenceMatch, GpsPoint, GraphEdge, GraphNode, HnswConfig, IndexOptions,
    JsonEntry, JsonIndexInfo, KvEntry, LedgerCommit, LedgerVerificationReport, LedgerVerifyMode,
    MaskFunction, MaskingRule, NamedPolicy, Policy, PolicyAction, SecurityContext,
    TimeSeriesLifecyclePolicy, TimeSeriesPoint, TimeSeriesRetentionRule, TimeSeriesRetentionStats,
    TimeSeriesStats, VectorElementType, VectorHit, VectorIndexInfo, VectorMetric,
};
pub use read::Scan;
pub use session::Session;
pub use timeseries::{TimeSeries, TimeSeriesWrite};
pub use txn::Transaction;
pub use vectors::{Vector, VectorWrite, Vectors};
