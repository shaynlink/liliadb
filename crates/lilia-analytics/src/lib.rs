//! Vectorized, in-process analytics engine for `LiliaDB`.
//!
//! The row store keeps serving transactional reads and writes; this crate adds a
//! columnar path for analytical queries. Columns are kept as contiguous Apache Arrow
//! buffers, persisted as immutable segments (one Arrow IPC chunk per column, plus zone
//! maps), and queried with `DataFusion` in fixed-size vectorized batches.
//!
//! The crate is storage-agnostic: a backend implements [`ColumnarSource`] and the
//! engine takes care of planning, pruning, parallelism and execution.

pub mod cache;
pub mod engine;
pub mod error;
pub mod output;
pub mod provider;
pub mod schema;
pub mod segment;
pub mod source;

/// The Arrow version the engine is built against, re-exported so callers manipulate
/// query results with matching types.
pub use datafusion::arrow;

pub use cache::{ChunkCache, ChunkKey};
pub use engine::{AnalyticsEngine, EngineOptions, QueryTable, DEFAULT_BATCH_SIZE};
pub use error::{AnalyticsError, Result};
pub use output::QueryOutput;
pub use provider::ColumnarTableProvider;
pub use schema::{validate_identifier, ColumnSpec, ColumnType, KeyKind, TableSchema};
pub use segment::{
    decode_chunk, encode_chunk, CmpOp, ColumnPredicate, Scalar, SegmentMeta, ZoneMap,
};
pub use source::{ColumnarSource, ScanRequest, ScanSnapshot};
