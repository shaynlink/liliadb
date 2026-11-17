//! Persistence of immutable columnar segments: one row per column chunk.

use lilia_analytics::arrow::array::{ArrayRef, RecordBatch};
use lilia_analytics::{decode_chunk, encode_chunk, ChunkKey, SegmentMeta};
use lilia_core::{ErrorCode, LiliaError, Result};
use lilia_sqlite_native::NativeConnection;

use super::catalog::{self, TableDef};
use super::core::{arrow_error, ColumnarCore, RowKey};

/// A stored segment and its metadata.
#[derive(Debug, Clone)]
pub(crate) struct Segment {
    pub(crate) id: i64,
    pub(crate) meta: SegmentMeta,
}

impl Segment {
    pub(crate) fn min_key(&self) -> Option<RowKey> {
        self.meta.min_key().and_then(RowKey::from_scalar)
    }

    pub(crate) fn max_key(&self) -> Option<RowKey> {
        self.meta.max_key().and_then(RowKey::from_scalar)
    }
}

fn corrupt(def: &TableDef, segment_id: i64, reason: impl std::fmt::Display) -> LiliaError {
    LiliaError::new(
        ErrorCode::Corrupt,
        format!(
            "segment {segment_id} of columnar table '{}' is corrupt: {reason}",
            def.name
        ),
        false,
    )
}

/// Loads every segment of a table, ordered by key range.
///
/// Segments never overlap, so ordering them by their smallest key orders all their rows.
pub(crate) fn load_segments(
    core: &ColumnarCore,
    connection: &NativeConnection,
    def: &TableDef,
) -> Result<Vec<Segment>> {
    let mut statement = connection.prepare(
        "SELECT segment_id, row_count, meta FROM _lilia_col_segments WHERE table_name = ?1;",
    )?;
    statement.bind_text_static(1, &def.name)?;
    let mut segments = Vec::new();
    while statement.step()? {
        let id = statement.column_int64(0);
        let plain = core.codec.decode(statement.column_blob(2))?;
        let meta = SegmentMeta::from_bytes(&plain)?;
        let stored_rows = usize::try_from(statement.column_int64(1)).ok();
        if stored_rows != Some(meta.row_count) || meta.zones.len() != def.schema.field_count() {
            return Err(corrupt(
                def,
                id,
                "metadata does not match the table definition",
            ));
        }
        segments.push(Segment { id, meta });
    }
    let keyed: Option<Vec<(RowKey, Segment)>> = segments
        .into_iter()
        .map(|segment| segment.min_key().map(|key| (key, segment)))
        .collect();
    let mut keyed = keyed.ok_or_else(|| {
        LiliaError::new(
            ErrorCode::Corrupt,
            format!(
                "columnar table '{}' holds a segment without key bounds",
                def.name
            ),
            false,
        )
    })?;
    keyed.sort_by(|(left, _), (right, _)| left.cmp(right));
    Ok(keyed.into_iter().map(|(_, segment)| segment).collect())
}

/// Returns one decoded column of a segment, from the cache when possible.
pub(crate) fn load_chunk(
    core: &ColumnarCore,
    connection: &NativeConnection,
    def: &TableDef,
    segment: &Segment,
    field: usize,
) -> Result<ArrayRef> {
    let key = ChunkKey {
        table_uid: def.uid,
        segment_id: segment.id,
        field,
    };
    if let Some(array) = core.cache.get(&key) {
        return Ok(array);
    }
    let schema = def.schema.arrow();
    let data_type = schema
        .fields()
        .get(field)
        .map(|f| f.data_type().clone())
        .ok_or_else(|| corrupt(def, segment.id, format!("unknown field {field}")))?;
    let column_idx = i64::try_from(field).unwrap_or(i64::MAX);
    let mut statement = connection.prepare(
        "SELECT data FROM _lilia_col_chunks \
         WHERE table_name = ?1 AND segment_id = ?2 AND column_idx = ?3;",
    )?;
    statement.bind_text_static(1, &def.name)?;
    statement.bind_int64(2, segment.id)?;
    statement.bind_int64(3, column_idx)?;
    if !statement.step()? {
        return Err(corrupt(
            def,
            segment.id,
            format!("column {field} is missing"),
        ));
    }
    let plain = core.codec.decode(statement.column_blob(0))?;
    let array = decode_chunk(&plain, &data_type, segment.meta.row_count)?;
    core.cache.insert(key, ArrayRef::clone(&array));
    Ok(array)
}

/// Loads the given fields of a segment as one record batch.
pub(crate) fn load_batch(
    core: &ColumnarCore,
    connection: &NativeConnection,
    def: &TableDef,
    segment: &Segment,
    fields: &[usize],
) -> Result<RecordBatch> {
    let schema = def.schema.arrow().project(fields).map_err(arrow_error)?;
    let columns = fields
        .iter()
        .map(|field| load_chunk(core, connection, def, segment, *field))
        .collect::<Result<Vec<_>>>()?;
    RecordBatch::try_new(std::sync::Arc::new(schema), columns).map_err(arrow_error)
}

/// Stores a full-width, key-sorted batch as a new segment. Runs in the caller's write
/// transaction. Empty batches are not stored.
pub(crate) fn write_segment(
    core: &ColumnarCore,
    connection: &NativeConnection,
    def: &TableDef,
    batch: &RecordBatch,
) -> Result<()> {
    if batch.num_rows() == 0 {
        return Ok(());
    }
    let segment_id = catalog::next_segment_id(connection, def)?;
    let meta = SegmentMeta::from_batch(batch);
    let meta_bytes = meta.to_bytes()?;
    let encoded_meta = core.codec.encode(&meta_bytes)?;
    let row_count = i64::try_from(batch.num_rows()).unwrap_or(i64::MAX);

    let mut statement = connection.prepare(
        "INSERT INTO _lilia_col_segments(table_name, segment_id, row_count, meta) \
         VALUES(?1, ?2, ?3, ?4);",
    )?;
    statement.bind_text_static(1, &def.name)?;
    statement.bind_int64(2, segment_id)?;
    statement.bind_int64(3, row_count)?;
    statement.bind_blob_static(4, &encoded_meta)?;
    statement.step()?;
    drop(statement);

    let mut statement = connection.prepare(
        "INSERT INTO _lilia_col_chunks(table_name, segment_id, column_idx, data) \
         VALUES(?1, ?2, ?3, ?4);",
    )?;
    for (column_idx, column) in (0_i64..).zip(batch.columns()) {
        let plain = encode_chunk(column)?;
        let encoded = core.codec.encode(&plain)?;
        statement.reset()?;
        statement.bind_text_static(1, &def.name)?;
        statement.bind_int64(2, segment_id)?;
        statement.bind_int64(3, column_idx)?;
        statement.bind_blob_static(4, &encoded)?;
        statement.step()?;
        // The bound buffers go out of scope at the end of this iteration.
        statement.clear_bindings()?;
    }
    Ok(())
}

/// Deletes a segment. Runs in the caller's write transaction.
pub(crate) fn delete_segment(
    connection: &NativeConnection,
    def: &TableDef,
    segment_id: i64,
) -> Result<()> {
    for table in ["_lilia_col_chunks", "_lilia_col_segments"] {
        let mut statement = connection.prepare(&format!(
            "DELETE FROM {table} WHERE table_name = ?1 AND segment_id = ?2;"
        ))?;
        statement.bind_text_static(1, &def.name)?;
        statement.bind_int64(2, segment_id)?;
        statement.step()?;
    }
    Ok(())
}

/// Drops the cached chunks of a segment that no longer exists.
pub(crate) fn evict_segment(core: &ColumnarCore, def: &TableDef, segment_id: i64) {
    for field in 0..def.schema.field_count() {
        core.cache.remove(&ChunkKey {
            table_uid: def.uid,
            segment_id,
            field,
        });
    }
}
