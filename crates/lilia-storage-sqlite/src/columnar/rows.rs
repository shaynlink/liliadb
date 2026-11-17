//! Reads source rows from the row store and turns them into Arrow record batches.

use std::sync::Arc;

use lilia_analytics::arrow::array::{
    ArrayRef, BooleanBuilder, Float64Builder, Int64Builder, RecordBatch, StringBuilder,
    TimestampMillisecondBuilder,
};
use lilia_analytics::{ColumnType, KeyKind};
use lilia_core::json::{BinaryDoc, BinaryValueRef};
use lilia_core::Result;
use lilia_sqlite_native::{ffi, NativeConnection, NativeStatement};

use super::catalog::{quote_ident, TableDef};
use super::core::{arrow_error, ColumnarCore, RowKey};
use super::ColumnarSourceKind;

const JSON_SELECT: &str = "SELECT id, value, format, dict_id FROM _lilia_json WHERE space = ?1";

enum KeyBuilder {
    Text(StringBuilder),
    Int(Int64Builder),
}

enum ColumnBuilder {
    Int64(Int64Builder),
    Float64(Float64Builder),
    Boolean(BooleanBuilder),
    Utf8(StringBuilder),
    Timestamp(TimestampMillisecondBuilder),
}

impl ColumnBuilder {
    fn new(ty: ColumnType, capacity: usize) -> Self {
        match ty {
            ColumnType::Int64 => Self::Int64(Int64Builder::with_capacity(capacity)),
            ColumnType::Float64 => Self::Float64(Float64Builder::with_capacity(capacity)),
            ColumnType::Boolean => Self::Boolean(BooleanBuilder::with_capacity(capacity)),
            ColumnType::Utf8 => Self::Utf8(StringBuilder::with_capacity(capacity, capacity * 8)),
            ColumnType::TimestampMillis => {
                Self::Timestamp(TimestampMillisecondBuilder::with_capacity(capacity))
            }
        }
    }

    /// Appends a JSON value, or NULL when its type does not fit the column.
    /// A float column also accepts integers; no other conversion is attempted.
    fn push_json(&mut self, value: Option<BinaryValueRef<'_>>) {
        match (self, value) {
            (Self::Int64(builder), Some(BinaryValueRef::Int64(v))) => builder.append_value(v),
            (Self::Float64(builder), Some(BinaryValueRef::Float64(v))) => builder.append_value(v),
            (Self::Float64(builder), Some(BinaryValueRef::Int64(v))) => {
                builder.append_value(int_to_float(v));
            }
            (Self::Boolean(builder), Some(BinaryValueRef::Bool(v))) => builder.append_value(v),
            (Self::Utf8(builder), Some(BinaryValueRef::String(v))) => builder.append_value(v),
            (Self::Timestamp(builder), Some(BinaryValueRef::Int64(v))) => builder.append_value(v),
            (builder, _) => builder.push_null(),
        }
    }

    /// Appends the value of a SQL result column, or NULL when its storage class does
    /// not fit the column.
    fn push_sql(&mut self, statement: &NativeStatement, column: i32) {
        let storage_class = statement.column_type(column);
        match (self, storage_class) {
            (Self::Int64(builder), ffi::SQLITE_INTEGER) => {
                builder.append_value(statement.column_int64(column));
            }
            (Self::Float64(builder), ffi::SQLITE_FLOAT) => {
                builder.append_value(statement.column_double(column));
            }
            (Self::Float64(builder), ffi::SQLITE_INTEGER) => {
                builder.append_value(int_to_float(statement.column_int64(column)));
            }
            (Self::Boolean(builder), ffi::SQLITE_INTEGER) => {
                builder.append_value(statement.column_int64(column) != 0);
            }
            (Self::Utf8(builder), ffi::SQLITE_TEXT) => {
                builder.append_value(statement.column_text(column));
            }
            (Self::Timestamp(builder), ffi::SQLITE_INTEGER) => {
                builder.append_value(statement.column_int64(column));
            }
            (builder, _) => builder.push_null(),
        }
    }

    fn push_null(&mut self) {
        match self {
            Self::Int64(builder) => builder.append_null(),
            Self::Float64(builder) => builder.append_null(),
            Self::Boolean(builder) => builder.append_null(),
            Self::Utf8(builder) => builder.append_null(),
            Self::Timestamp(builder) => builder.append_null(),
        }
    }

    fn finish(&mut self) -> ArrayRef {
        match self {
            Self::Int64(builder) => Arc::new(builder.finish()),
            Self::Float64(builder) => Arc::new(builder.finish()),
            Self::Boolean(builder) => Arc::new(builder.finish()),
            Self::Utf8(builder) => Arc::new(builder.finish()),
            Self::Timestamp(builder) => Arc::new(builder.finish()),
        }
    }
}

/// Integers stored in a float column follow IEEE-754 rounding beyond 2^53, exactly as
/// SQLite and JSON numbers do.
#[allow(clippy::cast_precision_loss)]
fn int_to_float(value: i64) -> f64 {
    value as f64
}

/// Accumulates source rows into one full-width record batch of a columnar table.
struct BatchBuilder<'d> {
    def: &'d TableDef,
    key: KeyBuilder,
    columns: Vec<ColumnBuilder>,
    last_key: Option<RowKey>,
    rows: usize,
}

impl<'d> BatchBuilder<'d> {
    fn new(def: &'d TableDef, capacity: usize) -> Self {
        let key = match def.schema.key() {
            KeyKind::Text => {
                KeyBuilder::Text(StringBuilder::with_capacity(capacity, capacity * 16))
            }
            KeyKind::Integer => KeyBuilder::Int(Int64Builder::with_capacity(capacity)),
        };
        let columns = def
            .schema
            .columns()
            .iter()
            .map(|column| ColumnBuilder::new(column.ty, capacity))
            .collect();
        Self {
            def,
            key,
            columns,
            last_key: None,
            rows: 0,
        }
    }

    /// Appends the row the statement is positioned on.
    fn push(&mut self, core: &ColumnarCore, statement: &NativeStatement) -> Result<()> {
        match (&mut self.key, self.def.source_kind) {
            (KeyBuilder::Text(key), ColumnarSourceKind::Json) => {
                let id = statement.column_text(0);
                // Field-level encrypted values are deliberately left encrypted: segments
                // must never hold plaintext the row store itself does not hold.
                let document: BinaryDoc = crate::json::decode_stored_doc(
                    statement,
                    1,
                    statement.column_int64(2),
                    statement.column_int64(3),
                    &self.def.source,
                    core.codec.as_ref(),
                    &core.dict_registry,
                    None,
                )?;
                key.append_value(id);
                self.last_key = Some(RowKey::Text(id.to_string()));
                for (builder, column) in self.columns.iter_mut().zip(self.def.schema.columns()) {
                    builder.push_json(document.extract(&column.path));
                }
            }
            (KeyBuilder::Int(key), ColumnarSourceKind::Sql) => {
                let rowid = statement.column_int64(0);
                key.append_value(rowid);
                self.last_key = Some(RowKey::Int(rowid));
                for (builder, column) in self.columns.iter_mut().zip(1..) {
                    builder.push_sql(statement, column);
                }
            }
            // The key kind is derived from the source kind when a definition is loaded.
            (KeyBuilder::Text(_), ColumnarSourceKind::Sql)
            | (KeyBuilder::Int(_), ColumnarSourceKind::Json) => {
                unreachable!("key kind always matches the source kind")
            }
        }
        self.rows += 1;
        Ok(())
    }

    fn finish(mut self) -> Result<RecordBatch> {
        let mut arrays: Vec<ArrayRef> = Vec::with_capacity(self.columns.len() + 1);
        arrays.push(match &mut self.key {
            KeyBuilder::Text(builder) => Arc::new(builder.finish()),
            KeyBuilder::Int(builder) => Arc::new(builder.finish()),
        });
        arrays.extend(self.columns.iter_mut().map(ColumnBuilder::finish));
        RecordBatch::try_new(self.def.schema.arrow(), arrays).map_err(arrow_error)
    }
}

/// `SELECT rowid, <projected columns> FROM <source> <tail>` for a SQL source.
pub(crate) fn sql_select(def: &TableDef, tail: &str) -> String {
    let mut columns = String::new();
    for column in def.schema.columns() {
        columns.push_str(", ");
        columns.push_str(&quote_ident(&column.path));
    }
    format!(
        "SELECT rowid{columns} FROM {} {tail};",
        quote_ident(&def.source)
    )
}

fn bind_key(statement: &mut NativeStatement, index: i32, key: &RowKey) -> Result<()> {
    match key {
        RowKey::Text(text) => statement.bind_text_static(index, text),
        RowKey::Int(value) => statement.bind_int64(index, *value),
    }
}

/// Reads up to `limit` source rows in key order, starting strictly after `after`.
/// Returns the batch and the key of its last row, or `None` when the source is exhausted.
pub(crate) fn read_page(
    core: &ColumnarCore,
    connection: &NativeConnection,
    def: &TableDef,
    after: Option<&RowKey>,
    limit: usize,
) -> Result<(RecordBatch, Option<RowKey>)> {
    let limit_param = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut statement = match (def.source_kind, after) {
        (ColumnarSourceKind::Json, None) => {
            let mut statement =
                connection.prepare(&format!("{JSON_SELECT} ORDER BY id LIMIT ?2;"))?;
            statement.bind_int64(2, limit_param)?;
            statement
        }
        (ColumnarSourceKind::Json, Some(key)) => {
            let mut statement =
                connection.prepare(&format!("{JSON_SELECT} AND id > ?2 ORDER BY id LIMIT ?3;"))?;
            bind_key(&mut statement, 2, key)?;
            statement.bind_int64(3, limit_param)?;
            statement
        }
        (ColumnarSourceKind::Sql, None) => {
            let mut statement = connection.prepare(&sql_select(def, "ORDER BY rowid LIMIT ?1"))?;
            statement.bind_int64(1, limit_param)?;
            statement
        }
        (ColumnarSourceKind::Sql, Some(key)) => {
            let mut statement =
                connection.prepare(&sql_select(def, "WHERE rowid > ?1 ORDER BY rowid LIMIT ?2"))?;
            bind_key(&mut statement, 1, key)?;
            statement.bind_int64(2, limit_param)?;
            statement
        }
    };
    if def.source_kind == ColumnarSourceKind::Json {
        statement.bind_text_static(1, &def.source)?;
    }
    let mut builder = BatchBuilder::new(def, limit.min(8192));
    while statement.step()? {
        builder.push(core, &statement)?;
    }
    let last_key = if builder.rows < limit {
        None
    } else {
        builder.last_key.clone()
    };
    Ok((builder.finish()?, last_key))
}

/// Reads the current version of the rows with the given keys, in the given order.
/// Keys whose row no longer exists are skipped.
pub(crate) fn read_keys(
    core: &ColumnarCore,
    connection: &NativeConnection,
    def: &TableDef,
    keys: &[RowKey],
) -> Result<RecordBatch> {
    let mut builder = BatchBuilder::new(def, keys.len());
    if keys.is_empty() {
        return builder.finish();
    }
    let (mut statement, key_param) = match def.source_kind {
        ColumnarSourceKind::Json => (
            connection.prepare(&format!("{JSON_SELECT} AND id = ?2;"))?,
            2,
        ),
        ColumnarSourceKind::Sql => (connection.prepare(&sql_select(def, "WHERE rowid = ?1"))?, 1),
    };
    for key in keys {
        statement.reset()?;
        if def.source_kind == ColumnarSourceKind::Json {
            statement.bind_text_static(1, &def.source)?;
        }
        bind_key(&mut statement, key_param, key)?;
        if statement.step()? {
            builder.push(core, &statement)?;
        }
    }
    builder.finish()
}
