use std::fmt::Debug;

use datafusion::arrow::array::RecordBatch;
use datafusion::arrow::datatypes::SchemaRef;

use crate::error::Result;
use crate::segment::ColumnPredicate;

/// What a query needs from a columnar table.
#[derive(Debug, Clone, Copy)]
pub struct ScanRequest<'a> {
    /// Fields to return, as indexes into the table schema, in output order.
    /// Never empty.
    pub projection: &'a [usize],
    /// Conjunctive predicates a source may use to skip whole segments. They are a
    /// hint: the engine re-applies every filter on the returned rows.
    pub predicates: &'a [ColumnPredicate],
}

/// A transactionally consistent view of a columnar table at scan time.
#[derive(Debug, Clone, Default)]
pub struct ScanSnapshot {
    /// Rows of the table, restricted to the requested projection.
    pub batches: Vec<RecordBatch>,
    /// Segments the table holds.
    pub segments_total: usize,
    /// Segments skipped thanks to zone maps.
    pub segments_pruned: usize,
    /// Rows served from the row store because they changed since the last compaction.
    pub delta_rows: usize,
}

/// Storage backend of a columnar table.
///
/// Implementations are called from blocking threads of the analytics runtime and may
/// perform synchronous I/O.
pub trait ColumnarSource: Debug + Send + Sync {
    /// Full table schema, key column first.
    fn schema(&self) -> SchemaRef;

    /// Reads a consistent snapshot of the requested columns.
    ///
    /// # Errors
    /// Returns an error when the underlying storage cannot be read or decoded.
    fn scan(&self, request: &ScanRequest<'_>) -> Result<ScanSnapshot>;
}
