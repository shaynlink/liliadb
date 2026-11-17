//! Merge-on-read scan of a columnar table.

use std::sync::Arc;

use lilia_analytics::arrow::array::{ArrayRef, BooleanArray, RecordBatch};
use lilia_analytics::arrow::compute::filter_record_batch;
use lilia_analytics::arrow::datatypes::SchemaRef;
use lilia_analytics::{ColumnarSource, ScanRequest, ScanSnapshot};
use lilia_core::{CollectionKind, ErrorCode, LiliaError, Result, StorageTier};

use super::catalog::{self, TableDef, TableState};
use super::core::{arrow_error, find_key, ColumnarCore, ReadSnapshot, RowKey};
use super::segments::{self, Segment};
use super::{rows, ColumnarSourceKind};

/// A columnar table as seen by the query engine.
#[derive(Debug)]
pub(crate) struct TableSource {
    core: Arc<ColumnarCore>,
    def: TableDef,
}

impl TableSource {
    pub(crate) fn new(core: Arc<ColumnarCore>, def: TableDef) -> Self {
        Self { core, def }
    }
}

impl ColumnarSource for TableSource {
    fn schema(&self) -> SchemaRef {
        self.def.schema.arrow()
    }

    fn scan(&self, request: &ScanRequest<'_>) -> lilia_analytics::Result<ScanSnapshot> {
        Ok(scan(&self.core, &self.def, request)?)
    }
}

/// Makes writes that are acknowledged but still buffered visible to the row store.
pub(crate) fn flush_buffered_source(core: &ColumnarCore, def: &TableDef) -> Result<()> {
    if def.source_kind == ColumnarSourceKind::Json
        && core.catalog.tier_for(CollectionKind::Json, &def.source) == StorageTier::Buffered
    {
        core.flush_ring()?;
    }
    Ok(())
}

/// The slice of sorted `delta` keys that fall inside the segment's key range.
fn delta_in_range<'k>(delta: &'k [RowKey], segment: &Segment) -> &'k [RowKey] {
    let (Some(low), Some(high)) = (segment.min_key(), segment.max_key()) else {
        return &[];
    };
    let start = delta.partition_point(|key| *key < low);
    let end = delta.partition_point(|key| *key <= high);
    &delta[start..end]
}

/// Builds the filter that drops the segment rows whose key changed since compaction.
/// Returns `None` when no row of the segment is affected.
pub(crate) fn stale_rows_filter(keys: &ArrayRef, delta: &[RowKey]) -> Option<BooleanArray> {
    let mut keep: Option<Vec<bool>> = None;
    for key in delta {
        if let Some(index) = find_key(keys, key) {
            keep.get_or_insert_with(|| vec![true; keys.len()])[index] = false;
        }
    }
    keep.map(BooleanArray::from)
}

/// Loads the requested fields of a segment, without the rows listed in `delta`.
pub(crate) fn load_live_rows(
    core: &ColumnarCore,
    connection: &lilia_sqlite_native::NativeConnection,
    def: &TableDef,
    segment: &Segment,
    fields: &[usize],
    delta: &[RowKey],
) -> Result<RecordBatch> {
    let batch = segments::load_batch(core, connection, def, segment, fields)?;
    let affected = delta_in_range(delta, segment);
    if affected.is_empty() {
        return Ok(batch);
    }
    let keys = segments::load_chunk(core, connection, def, segment, 0)?;
    match stale_rows_filter(&keys, affected) {
        Some(filter) => filter_record_batch(&batch, &filter).map_err(arrow_error),
        None => Ok(batch),
    }
}

/// Reads one transactionally consistent view of the table: compacted segments, minus
/// the rows that changed since, plus the current version of those rows.
fn scan(core: &ColumnarCore, def: &TableDef, request: &ScanRequest<'_>) -> Result<ScanSnapshot> {
    flush_buffered_source(core, def)?;
    core.with_reader(|connection| {
        // One read transaction: segments, delta and source rows are mutually consistent.
        let _snapshot = ReadSnapshot::begin(connection)?;
        let current = catalog::load_same_def(connection, def)?;
        if current.state != TableState::Ready {
            return Err(LiliaError::new(
                ErrorCode::Busy,
                format!("columnar table '{}' is still being built", def.name),
                true,
            ));
        }

        let delta = catalog::load_delta(connection, def)?;
        let stored = segments::load_segments(core, connection, def)?;
        let mut snapshot = ScanSnapshot {
            segments_total: stored.len(),
            ..ScanSnapshot::default()
        };
        for segment in &stored {
            if !segment.meta.may_match(request.predicates) {
                snapshot.segments_pruned += 1;
                continue;
            }
            let batch = load_live_rows(core, connection, def, segment, request.projection, &delta)?;
            if batch.num_rows() > 0 {
                snapshot.batches.push(batch);
            }
        }

        if !delta.is_empty() {
            let changed = rows::read_keys(core, connection, def, &delta)?;
            snapshot.delta_rows = changed.num_rows();
            if changed.num_rows() > 0 {
                snapshot
                    .batches
                    .push(changed.project(request.projection).map_err(arrow_error)?);
            }
        }
        Ok(snapshot)
    })
}
