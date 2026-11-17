//! Persistent definitions of columnar tables and their change-tracking triggers.

use lilia_analytics::{ColumnSpec, KeyKind, TableSchema};
use lilia_core::{CollectionKind, ErrorCode, LiliaError, Result, StorageTier};
use lilia_sqlite_native::NativeConnection;

use super::core::{ColumnarCore, RowKey};
use super::ColumnarSourceKind;

/// Tables of the columnar store. They are created on first use so a database that
/// never asks for analytics keeps its original schema.
const SCHEMA_DDL: &str = "
CREATE TABLE IF NOT EXISTS _lilia_col_tables(
  name TEXT PRIMARY KEY NOT NULL,
  uid INTEGER NOT NULL,
  source_kind TEXT NOT NULL,
  source TEXT NOT NULL,
  columns TEXT NOT NULL,
  segment_rows INTEGER NOT NULL CHECK(segment_rows > 0),
  state TEXT NOT NULL,
  next_segment_id INTEGER NOT NULL DEFAULT 0,
  created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS _lilia_col_segments(
  table_name TEXT NOT NULL,
  segment_id INTEGER NOT NULL,
  row_count INTEGER NOT NULL,
  meta BLOB NOT NULL,
  PRIMARY KEY(table_name, segment_id)
) STRICT;
CREATE TABLE IF NOT EXISTS _lilia_col_chunks(
  table_name TEXT NOT NULL,
  segment_id INTEGER NOT NULL,
  column_idx INTEGER NOT NULL,
  data BLOB NOT NULL,
  PRIMARY KEY(table_name, segment_id, column_idx)
) STRICT;
CREATE TABLE IF NOT EXISTS _lilia_col_delta(
  table_name TEXT NOT NULL,
  key ANY NOT NULL,
  PRIMARY KEY(table_name, key)
) STRICT, WITHOUT ROWID;";

const DEF_COLUMNS: &str =
    "name, uid, source_kind, source, columns, segment_rows, state, created_at_ms";

/// Lifecycle of a columnar table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TableState {
    /// Triggers are installed and the initial segments are being written.
    Building,
    /// Segments cover the whole source; the table can be queried.
    Ready,
}

impl TableState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Building => "building",
            Self::Ready => "ready",
        }
    }
}

/// A columnar table as persisted in `_lilia_col_tables`.
#[derive(Debug, Clone)]
pub(crate) struct TableDef {
    pub(crate) name: String,
    /// Unique per creation, so a dropped and re-created table never reuses cached chunks.
    pub(crate) uid: i64,
    pub(crate) source_kind: ColumnarSourceKind,
    pub(crate) source: String,
    pub(crate) schema: TableSchema,
    pub(crate) segment_rows: usize,
    pub(crate) state: TableState,
    pub(crate) created_at_ms: i64,
}

impl TableDef {
    pub(crate) fn key_kind(kind: ColumnarSourceKind) -> KeyKind {
        match kind {
            ColumnarSourceKind::Json => KeyKind::Text,
            ColumnarSourceKind::Sql => KeyKind::Integer,
        }
    }
}

fn corrupt(name: &str, reason: impl std::fmt::Display) -> LiliaError {
    LiliaError::new(
        ErrorCode::Corrupt,
        format!("columnar table '{name}' has an invalid definition: {reason}"),
        false,
    )
}

/// Quotes an SQL identifier.
pub(crate) fn quote_ident(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

/// Quotes an SQL string literal.
fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub(crate) fn tables_exist(connection: &NativeConnection) -> Result<bool> {
    let mut statement = connection.prepare(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_lilia_col_tables';",
    )?;
    statement.step()
}

pub(crate) fn ensure_tables(connection: &NativeConnection) -> Result<()> {
    connection.exec(SCHEMA_DDL)
}

fn read_def(statement: &lilia_sqlite_native::NativeStatement) -> Result<TableDef> {
    let name = statement.column_text(0).to_string();
    let source_kind = match statement.column_text(2) {
        "json" => ColumnarSourceKind::Json,
        "sql" => ColumnarSourceKind::Sql,
        other => return Err(corrupt(&name, format!("unknown source kind '{other}'"))),
    };
    let columns: Vec<ColumnSpec> =
        serde_json::from_str(statement.column_text(4)).map_err(|e| corrupt(&name, e))?;
    let schema = TableSchema::new(TableDef::key_kind(source_kind), columns)
        .map_err(|e| corrupt(&name, e))?;
    let segment_rows = usize::try_from(statement.column_int64(5))
        .ok()
        .filter(|rows| *rows > 0)
        .ok_or_else(|| corrupt(&name, "segment_rows must be positive"))?;
    let state = match statement.column_text(6) {
        "building" => TableState::Building,
        "ready" => TableState::Ready,
        other => return Err(corrupt(&name, format!("unknown state '{other}'"))),
    };
    Ok(TableDef {
        uid: statement.column_int64(1),
        source: statement.column_text(3).to_string(),
        created_at_ms: statement.column_int64(7),
        name,
        source_kind,
        schema,
        segment_rows,
        state,
    })
}

pub(crate) fn load_defs(connection: &NativeConnection) -> Result<Vec<TableDef>> {
    let mut statement = connection.prepare(&format!(
        "SELECT {DEF_COLUMNS} FROM _lilia_col_tables ORDER BY name;"
    ))?;
    let mut defs = Vec::new();
    while statement.step()? {
        defs.push(read_def(&statement)?);
    }
    Ok(defs)
}

pub(crate) fn load_def(connection: &NativeConnection, name: &str) -> Result<Option<TableDef>> {
    let mut statement = connection.prepare(&format!(
        "SELECT {DEF_COLUMNS} FROM _lilia_col_tables WHERE name = ?1;"
    ))?;
    statement.bind_text_static(1, name)?;
    if statement.step()? {
        read_def(&statement).map(Some)
    } else {
        Ok(None)
    }
}

/// Loads the definition a maintenance step started from, failing if the table was
/// dropped or re-created in the meantime.
pub(crate) fn load_same_def(
    connection: &NativeConnection,
    expected: &TableDef,
) -> Result<TableDef> {
    match load_def(connection, &expected.name)? {
        Some(current) if current.uid == expected.uid => Ok(current),
        _ => Err(LiliaError::new(
            ErrorCode::NotFound,
            format!("columnar table '{}' no longer exists", expected.name),
            false,
        )),
    }
}

fn trigger_names(name: &str) -> [String; 3] {
    ["ai", "au", "ad"].map(|suffix| format!("_lilia_col_trg_{name}_{suffix}"))
}

/// Statement recording `key_expr` in the delta of `name`, unless it is already there.
///
/// A plain `INSERT OR IGNORE` is not enough: inside a trigger SQLite replaces the
/// conflict policy with the one of the statement that fired it, so a user's
/// `INSERT OR FAIL` would turn an already-tracked key into a constraint failure.
fn track(name: &str, key_expr: &str, guard: Option<&str>) -> String {
    let table = quote_literal(name);
    let guard = guard.map_or_else(String::new, |condition| format!("{condition} AND "));
    format!(
        "INSERT INTO _lilia_col_delta(table_name, key) SELECT {table}, {key_expr} \
         WHERE {guard}NOT EXISTS (SELECT 1 FROM _lilia_col_delta \
         WHERE table_name = {table} AND key = {key_expr});"
    )
}

/// Triggers that record the key of every source row an insert, update or delete touches.
/// They cover every write path, including raw SQL and builds without analytics.
fn trigger_sql(def: &TableDef) -> [String; 3] {
    let [insert, update, delete] = trigger_names(&def.name).map(|name| quote_ident(&name));
    let name = def.name.as_str();
    match def.source_kind {
        ColumnarSourceKind::Json => {
            let space = quote_literal(&def.source);
            let old_in_space = format!("OLD.space = {space}");
            let new_in_space = format!("NEW.space = {space}");
            [
                format!(
                    "CREATE TRIGGER {insert} AFTER INSERT ON _lilia_json WHEN {new_in_space} \
                     BEGIN {} END;",
                    track(name, "NEW.id", None)
                ),
                format!(
                    "CREATE TRIGGER {update} AFTER UPDATE ON _lilia_json \
                     WHEN {old_in_space} OR {new_in_space} BEGIN {} {} END;",
                    track(name, "OLD.id", Some(&old_in_space)),
                    track(name, "NEW.id", Some(&new_in_space))
                ),
                format!(
                    "CREATE TRIGGER {delete} AFTER DELETE ON _lilia_json WHEN {old_in_space} \
                     BEGIN {} END;",
                    track(name, "OLD.id", None)
                ),
            ]
        }
        ColumnarSourceKind::Sql => {
            let source = quote_ident(&def.source);
            [
                format!(
                    "CREATE TRIGGER {insert} AFTER INSERT ON {source} BEGIN {} END;",
                    track(name, "NEW.rowid", None)
                ),
                format!(
                    "CREATE TRIGGER {update} AFTER UPDATE ON {source} BEGIN {} {} END;",
                    track(name, "OLD.rowid", None),
                    track(name, "NEW.rowid", None)
                ),
                format!(
                    "CREATE TRIGGER {delete} AFTER DELETE ON {source} BEGIN {} END;",
                    track(name, "OLD.rowid", None)
                ),
            ]
        }
    }
}

/// Checks that the source can back a columnar table.
pub(crate) fn validate_source(
    core: &ColumnarCore,
    connection: &NativeConnection,
    def: &TableDef,
) -> Result<()> {
    let invalid = |message: String| LiliaError::new(ErrorCode::InvalidInput, message, false);
    match def.source_kind {
        ColumnarSourceKind::Json => {
            crate::storage::validate_name("space", &def.source)?;
            if core.catalog.tier_for(CollectionKind::Json, &def.source) == StorageTier::Memory {
                return Err(invalid(format!(
                    "collection '{}' lives in the memory tier and has no persistent rows to project",
                    def.source
                )));
            }
            if core.sharding_enabled()? {
                return Err(invalid(
                    "columnar tables over JSON collections are not available when temporal \
                     sharding is enabled"
                        .into(),
                ));
            }
        }
        ColumnarSourceKind::Sql => {
            if def.source.starts_with("_lilia_") || def.source.starts_with("sqlite_") {
                return Err(invalid(format!(
                    "'{}' is an internal table and cannot be projected",
                    def.source
                )));
            }
            let mut statement = connection.prepare(
                "SELECT type, wr FROM pragma_table_list WHERE schema = 'main' AND name = ?1;",
            )?;
            statement.bind_text_static(1, &def.source)?;
            if !statement.step()? {
                return Err(invalid(format!(
                    "SQL table '{}' does not exist",
                    def.source
                )));
            }
            if statement.column_text(0) != "table" {
                return Err(invalid(format!(
                    "'{}' is a {}, only ordinary tables can be projected",
                    def.source,
                    statement.column_text(0)
                )));
            }
            if statement.column_int64(1) != 0 {
                return Err(invalid(format!(
                    "SQL table '{}' is WITHOUT ROWID and has no rowid to key its rows",
                    def.source
                )));
            }
            drop(statement);
            // Preparing the scan proves every projected column exists.
            connection.prepare(&super::rows::sql_select(def, "WHERE rowid = ?1"))?;
        }
    }
    Ok(())
}

/// Registers the table as `building` and installs its triggers. Runs in the caller's
/// write transaction, so the definition and the triggers appear atomically.
pub(crate) fn insert_def(connection: &NativeConnection, def: &TableDef) -> Result<()> {
    let columns = serde_json::to_string(def.schema.columns()).map_err(|e| corrupt(&def.name, e))?;
    let segment_rows = i64::try_from(def.segment_rows).map_err(|_| {
        LiliaError::new(ErrorCode::InvalidInput, "segment_rows is too large", false)
    })?;
    let mut statement = connection.prepare(
        "INSERT INTO _lilia_col_tables(name, uid, source_kind, source, columns, segment_rows, \
         state, created_at_ms) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8);",
    )?;
    statement.bind_text_static(1, &def.name)?;
    statement.bind_int64(2, def.uid)?;
    statement.bind_text_static(3, def.source_kind.as_str())?;
    statement.bind_text_static(4, &def.source)?;
    statement.bind_text_static(5, &columns)?;
    statement.bind_int64(6, segment_rows)?;
    statement.bind_text_static(7, def.state.as_str())?;
    statement.bind_int64(8, def.created_at_ms)?;
    statement.step()?;
    drop(statement);
    for trigger in trigger_sql(def) {
        connection.exec(&trigger)?;
    }
    Ok(())
}

pub(crate) fn set_state(
    connection: &NativeConnection,
    def: &TableDef,
    state: TableState,
) -> Result<()> {
    let mut statement = connection
        .prepare("UPDATE _lilia_col_tables SET state = ?1 WHERE name = ?2 AND uid = ?3;")?;
    statement.bind_text_static(1, state.as_str())?;
    statement.bind_text_static(2, &def.name)?;
    statement.bind_int64(3, def.uid)?;
    statement.step()?;
    Ok(())
}

fn delete_rows(connection: &NativeConnection, table: &str, name: &str) -> Result<()> {
    let mut statement =
        connection.prepare(&format!("DELETE FROM {table} WHERE table_name = ?1;"))?;
    statement.bind_text_static(1, name)?;
    statement.step()?;
    Ok(())
}

/// Removes every stored segment of a table, keeping its definition and its delta.
pub(crate) fn clear_segments(connection: &NativeConnection, name: &str) -> Result<()> {
    delete_rows(connection, "_lilia_col_chunks", name)?;
    delete_rows(connection, "_lilia_col_segments", name)
}

/// Removes a table entirely: triggers, segments, delta and definition.
pub(crate) fn delete_table(connection: &NativeConnection, name: &str) -> Result<()> {
    for trigger in trigger_names(name) {
        connection.exec(&format!(
            "DROP TRIGGER IF EXISTS {};",
            quote_ident(&trigger)
        ))?;
    }
    clear_segments(connection, name)?;
    delete_rows(connection, "_lilia_col_delta", name)?;
    let mut statement = connection.prepare("DELETE FROM _lilia_col_tables WHERE name = ?1;")?;
    statement.bind_text_static(1, name)?;
    statement.step()?;
    Ok(())
}

/// Reserves the next segment id of a table. Ids are never reused.
pub(crate) fn next_segment_id(connection: &NativeConnection, def: &TableDef) -> Result<i64> {
    let mut statement = connection.prepare(
        "UPDATE _lilia_col_tables SET next_segment_id = next_segment_id + 1 \
         WHERE name = ?1 AND uid = ?2 RETURNING next_segment_id - 1;",
    )?;
    statement.bind_text_static(1, &def.name)?;
    statement.bind_int64(2, def.uid)?;
    if statement.step()? {
        let id = statement.column_int64(0);
        // Run the statement to completion so the update is applied.
        while statement.step()? {}
        Ok(id)
    } else {
        Err(LiliaError::new(
            ErrorCode::NotFound,
            format!("columnar table '{}' no longer exists", def.name),
            false,
        ))
    }
}

/// Keys of the rows that changed since the last compaction, in ascending order.
pub(crate) fn load_delta(connection: &NativeConnection, def: &TableDef) -> Result<Vec<RowKey>> {
    let mut statement =
        connection.prepare("SELECT key FROM _lilia_col_delta WHERE table_name = ?1;")?;
    statement.bind_text_static(1, &def.name)?;
    let mut keys = Vec::new();
    while statement.step()? {
        keys.push(match def.schema.key() {
            KeyKind::Text => RowKey::Text(statement.column_text(0).to_string()),
            KeyKind::Integer => RowKey::Int(statement.column_int64(0)),
        });
    }
    keys.sort_unstable();
    Ok(keys)
}

pub(crate) fn delta_len(connection: &NativeConnection, name: &str) -> Result<usize> {
    let mut statement =
        connection.prepare("SELECT count(*) FROM _lilia_col_delta WHERE table_name = ?1;")?;
    statement.bind_text_static(1, name)?;
    statement.step()?;
    Ok(usize::try_from(statement.column_int64(0)).unwrap_or(0))
}

pub(crate) fn clear_delta(connection: &NativeConnection, name: &str) -> Result<()> {
    delete_rows(connection, "_lilia_col_delta", name)
}
