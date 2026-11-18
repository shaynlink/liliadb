//! Builds the segments of a columnar table and folds changed rows back into them.

use lilia_analytics::arrow::array::RecordBatch;
use lilia_analytics::arrow::compute::{
    concat_batches, filter_record_batch, sort_to_indices, take_record_batch,
};
use lilia_core::{ErrorCode, LiliaError, Result};
use lilia_sqlite_native::NativeConnection;

use super::catalog::{self, TableDef, TableState};
use super::core::{arrow_error, lower_bound, ColumnarCore, RowKey};
use super::scan::{flush_buffered_source, stale_rows_filter};
use super::segments::{self, Segment};
use super::{rows, CompactionStats};

/// Writes the initial segments of a table that is in the `building` state.
///
/// The build never blocks writers for long: it reads the source one segment at a time
/// and commits each segment in its own short write transaction. This is correct
/// without a global snapshot because the change-tracking triggers were installed
/// before the first page was read: any row written since is in the delta, which every
/// scan overlays on top of the segments.
pub(crate) fn build(core: &ColumnarCore, def: &TableDef) -> Result<()> {
    flush_buffered_source(core, def)?;
    let mut after: Option<RowKey> = None;
    loop {
        let (page, last_key) = core.with_reader(|connection| {
            rows::read_page(core, connection, def, after.as_ref(), def.segment_rows)
        })?;
        if page.num_rows() > 0 {
            in_building_transaction(core, def, |connection| {
                segments::write_segment(core, connection, def, &page)
            })?;
        }
        match last_key {
            Some(key) => after = Some(key),
            None => break,
        }
    }
    in_building_transaction(core, def, |connection| {
        catalog::set_state(connection, def, TableState::Ready)
    })
}

/// Discards the partial segments of an interrupted build and builds the table again.
pub(crate) fn rebuild(core: &ColumnarCore, def: &TableDef) -> Result<()> {
    in_building_transaction(core, def, |connection| {
        catalog::clear_segments(connection, &def.name)
    })?;
    build(core, def)
}

/// Runs one write transaction of a build, refusing to continue if the table was
/// dropped, re-created or completed by someone else in the meantime.
fn in_building_transaction(
    core: &ColumnarCore,
    def: &TableDef,
    work: impl FnOnce(&NativeConnection) -> Result<()>,
) -> Result<()> {
    core.with_writer(|connection| {
        let transaction = connection.transaction()?;
        let current = catalog::load_same_def(transaction.conn(), def)?;
        if current.state != TableState::Building {
            return Err(LiliaError::new(
                ErrorCode::Conflict,
                format!("columnar table '{}' is not being built anymore", def.name),
                false,
            ));
        }
        work(transaction.conn())?;
        transaction.commit()
    })
}

/// Writes rewritten rows as segments close to the table's target size.
struct SegmentWriter<'a> {
    core: &'a ColumnarCore,
    connection: &'a NativeConnection,
    def: &'a TableDef,
    stats: CompactionStats,
}

impl SegmentWriter<'_> {
    fn write(&mut self, batch: &RecordBatch) -> Result<()> {
        segments::write_segment(self.core, self.connection, self.def, batch)?;
        self.stats.segments_written += 1;
        self.stats.rows_written += batch.num_rows();
        Ok(())
    }

    /// Cuts full segments off the front of `batch` and writes them. What remains is
    /// written too when it is at least half a segment or when `flush` is set;
    /// otherwise it is returned so it can be merged into the next segment instead of
    /// being stored as a tiny one.
    fn emit(&mut self, batch: RecordBatch, flush: bool) -> Result<Option<RecordBatch>> {
        let target = self.def.segment_rows;
        let mut rest = batch;
        while rest.num_rows() * 2 >= target * 3 {
            self.write(&rest.slice(0, target))?;
            rest = rest.slice(target, rest.num_rows() - target);
        }
        if rest.num_rows() == 0 {
            return Ok(None);
        }
        if flush || rest.num_rows() * 2 >= target {
            self.write(&rest)?;
            return Ok(None);
        }
        Ok(Some(rest))
    }
}

/// Sorted keys `>= lower` and `< upper`; a missing bound is unbounded.
fn keys_between<'k>(
    keys: &'k [RowKey],
    lower: Option<&RowKey>,
    upper: Option<&RowKey>,
) -> &'k [RowKey] {
    let start = lower.map_or(0, |bound| keys.partition_point(|key| key < bound));
    let end = upper.map_or(keys.len(), |bound| keys.partition_point(|key| key < bound));
    &keys[start..end]
}

/// Rows of a key-sorted batch whose key is `>= lower` and `< upper`.
fn rows_between(
    batch: &RecordBatch,
    lower: Option<&RowKey>,
    upper: Option<&RowKey>,
) -> RecordBatch {
    let keys = batch.column(0);
    let start = lower.map_or(0, |bound| lower_bound(keys, bound));
    let end = upper.map_or(batch.num_rows(), |bound| lower_bound(keys, bound));
    batch.slice(start, end - start)
}

/// Combines the rows carried over from the previous segment, the surviving rows of
/// this segment and the changed rows routed to it, in key order.
fn merge(
    def: &TableDef,
    carried: Option<RecordBatch>,
    survivors: &RecordBatch,
    changed: &RecordBatch,
) -> Result<RecordBatch> {
    let schema = def.schema.arrow();
    let body = if changed.num_rows() == 0 {
        survivors.clone()
    } else {
        let combined = concat_batches(&schema, [survivors, changed]).map_err(arrow_error)?;
        let order = sort_to_indices(combined.column(0), None, None).map_err(arrow_error)?;
        take_record_batch(&combined, &order).map_err(arrow_error)?
    };
    match carried {
        // Carried rows come from the previous key range: they all sort before `body`.
        Some(head) => concat_batches(&schema, [&head, &body]).map_err(arrow_error),
        None => Ok(body),
    }
}

/// Folds the delta of a table into its segments, in one atomic write transaction.
///
/// Only the segments that own a changed key are rewritten. Each changed key is routed
/// to the segment whose key range starts at or before it, which keeps segments sorted
/// and non-overlapping.
pub(crate) fn compact(core: &ColumnarCore, name: &str) -> Result<CompactionStats> {
    let not_found = || {
        LiliaError::new(
            ErrorCode::NotFound,
            format!("columnar table '{name}' does not exist"),
            false,
        )
    };
    let def = core
        .with_reader(|connection| {
            if catalog::tables_exist(connection)? {
                catalog::load_def(connection, name)
            } else {
                Ok(None)
            }
        })?
        .ok_or_else(not_found)?;
    flush_buffered_source(core, &def)?;

    let (stats, rewritten) = core.with_writer(|connection| {
        let transaction = connection.transaction()?;
        let outcome = compact_in_transaction(core, transaction.conn(), &def)?;
        transaction.commit()?;
        Ok(outcome)
    })?;
    for segment_id in rewritten {
        segments::evict_segment(core, &def, segment_id);
    }
    Ok(stats)
}

fn compact_in_transaction(
    core: &ColumnarCore,
    connection: &NativeConnection,
    started_from: &TableDef,
) -> Result<(CompactionStats, Vec<i64>)> {
    let def = catalog::load_same_def(connection, started_from)?;
    if def.state != TableState::Ready {
        return Err(LiliaError::new(
            ErrorCode::Busy,
            format!("columnar table '{}' is still being built", def.name),
            true,
        ));
    }
    let delta = catalog::load_delta(connection, &def)?;
    if delta.is_empty() {
        return Ok((CompactionStats::default(), Vec::new()));
    }
    let stored = segments::load_segments(core, connection, &def)?;
    let changed = rows::read_keys(core, connection, &def, &delta)?;
    let all_fields: Vec<usize> = (0..def.schema.field_count()).collect();
    let range_starts: Vec<Option<RowKey>> = stored.iter().map(Segment::min_key).collect();

    let mut writer = SegmentWriter {
        core,
        connection,
        def: &def,
        stats: CompactionStats {
            delta_keys: delta.len(),
            ..CompactionStats::default()
        },
    };
    let mut rewritten = Vec::new();
    let mut carried: Option<RecordBatch> = None;

    if stored.is_empty() {
        carried = writer.emit(changed.clone(), true)?;
    }
    for (index, segment) in stored.iter().enumerate() {
        // The first segment also owns every key before it, the last one every key after.
        let lower = if index == 0 {
            None
        } else {
            range_starts[index].as_ref()
        };
        let upper = range_starts.get(index + 1).and_then(Option::as_ref);
        let routed = rows_between(&changed, lower, upper);
        let keys = segments::load_chunk(core, connection, &def, segment, 0)?;
        let stale = stale_rows_filter(&keys, keys_between(&delta, lower, upper));
        if stale.is_none() && routed.num_rows() == 0 && carried.is_none() {
            continue;
        }

        let rows = segments::load_batch(core, connection, &def, segment, &all_fields)?;
        let survivors = match &stale {
            Some(filter) => filter_record_batch(&rows, filter).map_err(arrow_error)?,
            None => rows,
        };
        let merged = merge(&def, carried.take(), &survivors, &routed)?;
        segments::delete_segment(connection, &def, segment.id)?;
        rewritten.push(segment.id);
        writer.stats.segments_rewritten += 1;
        carried = writer.emit(merged, index + 1 == stored.len())?;
    }
    debug_assert!(
        carried.is_none(),
        "the last segment flushes every pending row"
    );

    catalog::clear_delta(connection, &def.name)?;
    Ok((writer.stats, rewritten))
}

/// Compacts every ready table whose delta reached the configured threshold.
pub(crate) fn compact_due(core: &ColumnarCore) -> Result<()> {
    let threshold = core.options.compaction_delta_rows.max(1);
    let due: Vec<String> = core.with_reader(|connection| {
        if !catalog::tables_exist(connection)? {
            return Ok(Vec::new());
        }
        let mut due = Vec::new();
        for def in catalog::load_defs(connection)? {
            if def.state == TableState::Ready
                && catalog::delta_len(connection, &def.name)? >= threshold
            {
                due.push(def.name);
            }
        }
        Ok(due)
    })?;
    for name in due {
        match compact(core, &name) {
            // A table dropped or rebuilt since the check needs no compaction.
            Err(error) if matches!(error.code, ErrorCode::NotFound | ErrorCode::Busy) => {}
            other => {
                other?;
            }
        }
    }
    Ok(())
}
