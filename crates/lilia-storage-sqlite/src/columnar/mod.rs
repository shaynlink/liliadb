//! Vectorized analytics over persistent columnar projections of the row store.
//!
//! A *columnar table* projects typed columns out of a JSON collection or a SQL table
//! into immutable Arrow segments stored in the database file. The row store stays the
//! source of truth: triggers record the key of every row that changes, scans overlay
//! those rows on top of the segments, and compaction folds them back in. Queries run
//! through `DataFusion` in fixed-size vectorized batches, inside the process.

mod catalog;
mod compaction;
mod core;
mod rows;
mod scan;
mod segments;
mod worker;

use std::marker::PhantomData;
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use lilia_analytics::{
    validate_identifier, AnalyticsEngine, EngineOptions, TableSchema, DEFAULT_BATCH_SIZE,
};
use lilia_core::{ErrorCode, LiliaError, Result, SqlQueryResult};

pub use lilia_analytics::{arrow, ColumnSpec, ColumnType, QueryOutput};

use self::catalog::{TableDef, TableState};
use self::core::ColumnarCore;
use self::scan::TableSource;
use self::worker::CompactionWorker;
use crate::Database;

/// Tuning of the analytics engine and of the columnar store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalyticsOptions {
    /// Rows evaluated per vectorized batch.
    pub batch_size: usize,
    /// Parallelism of a query.
    pub target_partitions: usize,
    /// Memory budget for decoded column chunks kept between queries. `0` disables it.
    pub cache_bytes: usize,
    /// Target number of rows per segment for tables that do not set their own.
    pub segment_rows: usize,
    /// Number of changed rows at which a table is compacted in the background.
    /// Below it, changed rows are simply overlaid on read.
    pub compaction_delta_rows: usize,
    /// How often the background worker looks for tables to compact, in milliseconds.
    /// `0` disables background compaction; [`Analytics::compact`] still works.
    pub compaction_interval_ms: u64,
}

impl Default for AnalyticsOptions {
    fn default() -> Self {
        Self {
            batch_size: DEFAULT_BATCH_SIZE,
            target_partitions: EngineOptions::default().target_partitions,
            cache_bytes: 256 * 1024 * 1024,
            segment_rows: 120 * DEFAULT_BATCH_SIZE,
            compaction_delta_rows: 4096,
            compaction_interval_ms: 1_000,
        }
    }
}

/// What a columnar table projects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnarSourceKind {
    /// A JSON collection; rows are keyed by document id, exposed as the `id` column.
    Json,
    /// A SQL table; rows are keyed by `rowid`, exposed as the `rowid` column.
    Sql,
}

impl ColumnarSourceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Sql => "sql",
        }
    }
}

/// Declaration of a columnar table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnarTableSpec {
    name: String,
    source_kind: ColumnarSourceKind,
    source: String,
    columns: Vec<ColumnSpec>,
    segment_rows: Option<usize>,
}

impl ColumnarTableSpec {
    /// Projects the JSON collection `space` as the columnar table `name`.
    #[must_use]
    pub fn json(name: impl Into<String>, space: impl Into<String>) -> Self {
        Self::new(name.into(), ColumnarSourceKind::Json, space.into())
    }

    /// Projects the SQL table `table` as the columnar table `name`.
    #[must_use]
    pub fn sql(name: impl Into<String>, table: impl Into<String>) -> Self {
        Self::new(name.into(), ColumnarSourceKind::Sql, table.into())
    }

    fn new(name: String, source_kind: ColumnarSourceKind, source: String) -> Self {
        Self {
            name,
            source_kind,
            source,
            columns: Vec::new(),
            segment_rows: None,
        }
    }

    /// Adds a column. `path` is a JSON path (`$.address.city`) for a JSON collection
    /// and a column name for a SQL table. A value whose type does not fit `ty` is
    /// stored as NULL; a `Float64` column also accepts integers.
    #[must_use]
    pub fn column(
        mut self,
        name: impl Into<String>,
        path: impl Into<String>,
        ty: ColumnType,
    ) -> Self {
        self.columns.push(ColumnSpec::new(name, path, ty));
        self
    }

    /// Overrides the target number of rows per segment.
    #[must_use]
    pub fn segment_rows(mut self, rows: usize) -> Self {
        self.segment_rows = Some(rows);
        self
    }
}

/// Description of an existing columnar table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnarTableInfo {
    pub name: String,
    pub source_kind: ColumnarSourceKind,
    pub source: String,
    pub columns: Vec<ColumnSpec>,
    /// `false` while the initial segments are being written, or when an interrupted
    /// build could not be resumed; such a table cannot be queried.
    pub ready: bool,
}

/// Storage figures of a columnar table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnarTableStats {
    pub name: String,
    pub ready: bool,
    /// Number of stored segments.
    pub segments: usize,
    /// Rows held in segments.
    pub segment_rows: usize,
    /// Rows changed since the last compaction, overlaid on read.
    pub delta_keys: usize,
    /// Bytes of column data stored in the database file.
    pub stored_bytes: u64,
}

/// Outcome of a compaction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompactionStats {
    /// Changed rows that were folded into segments.
    pub delta_keys: usize,
    /// Existing segments that had to be rewritten.
    pub segments_rewritten: usize,
    /// Segments written in their place.
    pub segments_written: usize,
    /// Rows contained in the written segments.
    pub rows_written: usize,
}

/// Process-wide analytics state of one database, created on first use.
#[derive(Debug)]
pub(crate) struct AnalyticsState {
    // Declared first so the worker thread is joined before the engine and the store
    // it works on are dropped.
    _worker: Option<CompactionWorker>,
    engine: AnalyticsEngine,
    core: Arc<ColumnarCore>,
}

impl AnalyticsState {
    fn start(database: &Database, options: AnalyticsOptions) -> Result<Arc<Self>> {
        let core = Arc::new(ColumnarCore::new(database, options));
        let engine = AnalyticsEngine::new(EngineOptions {
            batch_size: options.batch_size,
            target_partitions: options.target_partitions,
        })?;

        let defs = core.with_reader(|connection| {
            if catalog::tables_exist(connection)? {
                catalog::load_defs(connection)
            } else {
                Ok(Vec::new())
            }
        })?;
        for def in defs {
            // A build interrupted by a crash left partial segments: start it over. If
            // that fails (its source is gone, say) the table stays listed as not ready.
            if def.state == TableState::Building && compaction::rebuild(&core, &def).is_err() {
                continue;
            }
            register(&engine, &core, def)?;
        }

        let worker = CompactionWorker::start(
            Arc::downgrade(&core),
            Duration::from_millis(options.compaction_interval_ms),
        )?;
        Ok(Arc::new(Self {
            _worker: worker,
            engine,
            core,
        }))
    }
}

fn register(engine: &AnalyticsEngine, core: &Arc<ColumnarCore>, def: TableDef) -> Result<()> {
    let name = def.name.clone();
    let source = TableSource::new(Arc::clone(core), def);
    engine.register_table(&name, Arc::new(source))?;
    Ok(())
}

fn not_found(name: &str) -> LiliaError {
    LiliaError::new(
        ErrorCode::NotFound,
        format!("columnar table '{name}' does not exist"),
        false,
    )
}

impl Database {
    /// Opens the analytics engine of this database, starting it on first use.
    ///
    /// # Errors
    /// Returns an error when the engine cannot be started or the existing columnar
    /// tables cannot be loaded.
    pub fn analytics(&self) -> Result<Analytics<'_>> {
        let mut slot = self
            .analytics
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let state = if let Some(state) = slot.as_ref() {
            Arc::clone(state)
        } else {
            let state = AnalyticsState::start(self, self.analytics_options)?;
            *slot = Some(Arc::clone(&state));
            state
        };
        Ok(Analytics {
            state,
            _database: PhantomData,
        })
    }
}

/// Handle on the analytics engine of a [`Database`].
#[derive(Debug, Clone)]
pub struct Analytics<'a> {
    state: Arc<AnalyticsState>,
    _database: PhantomData<&'a Database>,
}

impl Analytics<'_> {
    /// Creates a columnar table and builds its segments from the current rows.
    ///
    /// Writers are never blocked for the duration of the build: rows written while it
    /// runs are tracked and served from the row store until the next compaction.
    ///
    /// # Errors
    /// Returns `InvalidInput` when the declaration is invalid or its source cannot be
    /// projected (memory-tier collection, sharded JSON, `WITHOUT ROWID` or missing SQL
    /// table, unknown column), and `Conflict` when the name is already taken.
    pub fn create_table(&self, spec: ColumnarTableSpec) -> Result<()> {
        let core = &self.state.core;
        validate_identifier("table name", &spec.name)?;
        let schema = TableSchema::new(TableDef::key_kind(spec.source_kind), spec.columns)?;
        let segment_rows = spec.segment_rows.unwrap_or(core.options.segment_rows);
        if segment_rows == 0 {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "segment_rows must be greater than zero",
                false,
            ));
        }
        let def = TableDef {
            name: spec.name,
            uid: rand::random(),
            source_kind: spec.source_kind,
            source: spec.source,
            schema,
            segment_rows,
            state: TableState::Building,
            created_at_ms: crate::storage::now_ms(),
        };

        core.with_writer(|connection| {
            let transaction = connection.transaction()?;
            let connection = transaction.conn();
            catalog::ensure_tables(connection)?;
            if catalog::load_def(connection, &def.name)?.is_some() {
                return Err(LiliaError::new(
                    ErrorCode::Conflict,
                    format!("columnar table '{}' already exists", def.name),
                    false,
                ));
            }
            catalog::validate_source(core, connection, &def)?;
            catalog::insert_def(connection, &def)?;
            transaction.commit()
        })?;

        if let Err(error) = compaction::build(core, &def) {
            // Leave nothing behind: neither triggers nor partial segments.
            let _ = core.with_writer(|connection| {
                let transaction = connection.transaction()?;
                if catalog::load_same_def(transaction.conn(), &def).is_ok() {
                    catalog::delete_table(transaction.conn(), &def.name)?;
                }
                transaction.commit()
            });
            return Err(error);
        }
        let ready = TableDef {
            state: TableState::Ready,
            ..def
        };
        register(&self.state.engine, core, ready)
    }

    /// Drops a columnar table: its segments, its change tracking and its definition.
    /// The source rows are untouched. Returns whether the table existed.
    ///
    /// # Errors
    /// Returns an error when the storage cannot be updated.
    pub fn drop_table(&self, name: &str) -> Result<bool> {
        let core = &self.state.core;
        let dropped = core.with_writer(|connection| {
            let transaction = connection.transaction()?;
            if !catalog::tables_exist(transaction.conn())? {
                return Ok(None);
            }
            let Some(def) = catalog::load_def(transaction.conn(), name)? else {
                return Ok(None);
            };
            catalog::delete_table(transaction.conn(), name)?;
            transaction.commit()?;
            Ok(Some(def))
        })?;
        let Some(def) = dropped else {
            return Ok(false);
        };
        self.state.engine.deregister_table(name)?;
        core.cache.remove_table(def.uid);
        Ok(true)
    }

    /// Lists the columnar tables of the database, by name.
    ///
    /// # Errors
    /// Returns an error when the catalog cannot be read.
    pub fn list_tables(&self) -> Result<Vec<ColumnarTableInfo>> {
        let defs = self.state.core.with_reader(|connection| {
            if catalog::tables_exist(connection)? {
                catalog::load_defs(connection)
            } else {
                Ok(Vec::new())
            }
        })?;
        Ok(defs
            .into_iter()
            .map(|def| ColumnarTableInfo {
                ready: def.state == TableState::Ready,
                columns: def.schema.columns().to_vec(),
                name: def.name,
                source_kind: def.source_kind,
                source: def.source,
            })
            .collect())
    }

    /// Reports how much of a table sits in segments and how much awaits compaction.
    ///
    /// # Errors
    /// Returns `NotFound` when the table does not exist.
    pub fn table_stats(&self, name: &str) -> Result<ColumnarTableStats> {
        self.state.core.with_reader(|connection| {
            if !catalog::tables_exist(connection)? {
                return Err(not_found(name));
            }
            let def = catalog::load_def(connection, name)?.ok_or_else(|| not_found(name))?;

            let mut statement = connection.prepare(
                "SELECT count(*), coalesce(sum(row_count), 0) FROM _lilia_col_segments \
                 WHERE table_name = ?1;",
            )?;
            statement.bind_text_static(1, name)?;
            statement.step()?;
            let segments = usize::try_from(statement.column_int64(0)).unwrap_or(0);
            let segment_rows = usize::try_from(statement.column_int64(1)).unwrap_or(0);
            drop(statement);

            let mut statement = connection.prepare(
                "SELECT coalesce(sum(length(data)), 0) FROM _lilia_col_chunks \
                 WHERE table_name = ?1;",
            )?;
            statement.bind_text_static(1, name)?;
            statement.step()?;
            let stored_bytes = u64::try_from(statement.column_int64(0)).unwrap_or(0);
            drop(statement);

            Ok(ColumnarTableStats {
                ready: def.state == TableState::Ready,
                segments,
                segment_rows,
                delta_keys: catalog::delta_len(connection, name)?,
                stored_bytes,
                name: def.name,
            })
        })
    }

    /// Folds the rows changed since the last compaction into the table's segments.
    ///
    /// Compaction is an optimization, never a requirement for correctness: queries
    /// always see the current rows.
    ///
    /// # Errors
    /// Returns `NotFound` when the table does not exist and `Busy` while it is still
    /// being built.
    pub fn compact(&self, name: &str) -> Result<CompactionStats> {
        compaction::compact(&self.state.core, name)
    }

    /// Runs a read-only analytical SQL query over the columnar tables and returns
    /// Arrow record batches.
    ///
    /// # Errors
    /// Returns `InvalidInput` for SQL that is invalid, references unknown tables or
    /// columns, or tries to write; other codes report execution or storage failures.
    pub fn query(&self, sql: &str) -> Result<QueryOutput> {
        Ok(self.state.engine.query(sql)?)
    }

    /// Runs a read-only analytical SQL query and returns row-oriented JSON values, in
    /// the same shape as [`Database::sql_query`].
    ///
    /// # Errors
    /// Same as [`Analytics::query`].
    pub fn query_json(&self, sql: &str) -> Result<SqlQueryResult> {
        let output = self.state.engine.query(sql)?;
        Ok(SqlQueryResult {
            columns: output.column_names(),
            rows: output.to_json_rows()?,
            rows_affected: 0,
        })
    }
}
