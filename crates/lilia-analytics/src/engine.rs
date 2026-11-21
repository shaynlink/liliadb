//! Query engine: a `DataFusion` session driven by a dedicated runtime.

use std::sync::mpsc;
use std::sync::Arc;

use datafusion::arrow::array::RecordBatch;
use datafusion::arrow::datatypes::SchemaRef;
use datafusion::common::ScalarValue;
use datafusion::datasource::MemTable;
use datafusion::execution::context::{SQLOptions, SessionConfig, SessionContext};
use datafusion::sql::parser::DFParser;
use serde_json::Value;
use tokio::runtime::{Builder, Runtime};

use crate::error::{AnalyticsError, Result};
use crate::output::QueryOutput;
use crate::provider::ColumnarTableProvider;
use crate::schema::validate_identifier;
use crate::source::ColumnarSource;

/// Rows evaluated per vectorized batch unless configured otherwise.
pub const DEFAULT_BATCH_SIZE: usize = 1024;

/// Tuning of the query engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineOptions {
    /// Rows per vectorized batch.
    pub batch_size: usize,
    /// Parallelism of a query: partitions per scan and worker threads of the runtime.
    pub target_partitions: usize,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            batch_size: DEFAULT_BATCH_SIZE,
            target_partitions: std::thread::available_parallelism().map_or(1, usize::from),
        }
    }
}

/// Executes read-only analytical SQL over registered columnar tables.
///
/// The engine owns its own multi-threaded runtime, so [`AnalyticsEngine::query`] is a
/// plain blocking call that is safe to invoke from any thread, including a thread
/// that is itself running inside another async runtime.
pub struct AnalyticsEngine {
    /// `None` only while the engine is being dropped.
    runtime: Option<Runtime>,
    context: SessionContext,
    config: SessionConfig,
    options: EngineOptions,
}

/// A table given to one query: its name, schema and rows.
#[derive(Debug, Clone)]
pub struct QueryTable {
    pub name: String,
    pub schema: SchemaRef,
    pub batches: Vec<RecordBatch>,
}

/// A query context with the JSON functions (`json_get_str`, `->>`, ...).
fn context(config: SessionConfig) -> Result<SessionContext> {
    let mut context = SessionContext::new_with_config(config);
    datafusion_functions_json::register_all(&mut context)?;
    Ok(context)
}

impl std::fmt::Debug for AnalyticsEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnalyticsEngine")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl AnalyticsEngine {
    /// Starts the engine and its runtime.
    ///
    /// # Errors
    /// Returns [`AnalyticsError::InvalidInput`] when an option is zero, or
    /// [`AnalyticsError::Runtime`] when the worker threads cannot be spawned.
    pub fn new(options: EngineOptions) -> Result<Self> {
        if options.batch_size == 0 || options.target_partitions == 0 {
            return Err(AnalyticsError::InvalidInput(
                "batch_size and target_partitions must be greater than zero".into(),
            ));
        }
        let runtime = Builder::new_multi_thread()
            .worker_threads(options.target_partitions)
            .thread_name("lilia-analytics")
            .build()
            .map_err(|e| AnalyticsError::Runtime(format!("cannot start runtime: {e}")))?;
        let config = SessionConfig::new()
            .with_batch_size(options.batch_size)
            .with_target_partitions(options.target_partitions)
            .with_information_schema(false);
        Ok(Self {
            runtime: Some(runtime),
            context: context(config.clone())?,
            config,
            options,
        })
    }

    /// Options the engine was started with.
    #[must_use]
    pub fn options(&self) -> EngineOptions {
        self.options
    }

    /// Makes a columnar table queryable under `name`, replacing any previous
    /// registration of that name.
    ///
    /// # Errors
    /// Returns [`AnalyticsError::InvalidInput`] when `name` is not a lowercase
    /// identifier, or a query error if the catalog rejects the table.
    pub fn register_table(&self, name: &str, source: Arc<dyn ColumnarSource>) -> Result<()> {
        validate_identifier("table name", name)?;
        let provider = ColumnarTableProvider::new(
            source,
            self.options.batch_size,
            self.options.target_partitions,
        );
        self.context.deregister_table(name)?;
        self.context.register_table(name, Arc::new(provider))?;
        Ok(())
    }

    /// Removes a table from the query catalog. Returns whether it was registered.
    ///
    /// # Errors
    /// Returns a query error if the catalog cannot be updated.
    pub fn deregister_table(&self, name: &str) -> Result<bool> {
        Ok(self.context.deregister_table(name)?.is_some())
    }

    /// Runs a read-only SQL query and collects its result.
    ///
    /// DDL, DML and session statements are rejected: the engine can only read the
    /// registered columnar tables.
    ///
    /// # Errors
    /// Returns a query error when the SQL is invalid, references an unknown table or
    /// column, or fails during execution, and [`AnalyticsError::Runtime`] when the
    /// engine is shutting down.
    pub fn query(&self, sql: &str) -> Result<QueryOutput> {
        self.execute(self.context.clone(), sql, Vec::new())
    }

    /// The names of the tables `sql`, one statement, reads.
    ///
    /// # Errors
    /// Returns a query error when `sql` does not parse or holds several statements.
    pub fn table_references(&self, sql: &str) -> Result<Vec<String>> {
        let mut statements = DFParser::parse_sql(sql)?;
        let (Some(statement), true) = (statements.pop_front(), statements.is_empty()) else {
            return Err(AnalyticsError::InvalidInput(
                "a query holds exactly one statement".into(),
            ));
        };
        let references = self.context.state().resolve_table_references(&statement)?;
        Ok(references.iter().map(|r| r.table().to_owned()).collect())
    }

    /// Runs a read-only SQL query over `tables`, which exist for this query only (the
    /// registered columnar tables are not visible to it), with `params` bound to its
    /// placeholders `$1`, `$2`, ... (null, booleans, numbers and strings).
    ///
    /// # Errors
    /// As [`AnalyticsError::InvalidInput`] for an array or object parameter; as
    /// [`AnalyticsEngine::query`].
    pub fn query_tables(
        &self,
        tables: Vec<QueryTable>,
        sql: &str,
        params: &[Value],
    ) -> Result<QueryOutput> {
        let params = params.iter().map(scalar).collect::<Result<Vec<_>>>()?;
        let context = context(self.config.clone())?;
        for table in tables {
            let rows = MemTable::try_new(table.schema, vec![table.batches])?;
            context.register_table(table.name.as_str(), Arc::new(rows))?;
        }
        self.execute(context, sql, params)
    }

    fn execute(
        &self,
        context: SessionContext,
        sql: &str,
        params: Vec<ScalarValue>,
    ) -> Result<QueryOutput> {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| AnalyticsError::Runtime("engine is shutting down".into()))?;
        let sql = sql.to_owned();
        // The result travels over a channel instead of `block_on`, which would panic
        // when the caller is already inside an async runtime.
        let (sender, receiver) = mpsc::sync_channel(1);
        runtime.spawn(async move {
            // The receiver only disappears if the calling thread is gone.
            let _ = sender.send(run(&context, &sql, params).await);
        });
        receiver
            .recv()
            .map_err(|_| AnalyticsError::Runtime("query task terminated unexpectedly".into()))?
    }
}

/// A query parameter as a SQL value.
fn scalar(value: &Value) -> Result<ScalarValue> {
    Ok(match value {
        Value::Null => ScalarValue::Null,
        Value::Bool(b) => ScalarValue::Boolean(Some(*b)),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => ScalarValue::Int64(Some(i)),
            (None, Some(f)) => ScalarValue::Float64(Some(f)),
            (None, None) => {
                return Err(AnalyticsError::InvalidInput(format!(
                    "parameter {n} is out of range"
                )))
            }
        },
        Value::String(s) => ScalarValue::Utf8(Some(s.clone())),
        Value::Array(_) | Value::Object(_) => {
            return Err(AnalyticsError::InvalidInput(
                "a parameter is null, a boolean, a number or a string".into(),
            ))
        }
    })
}

async fn run(context: &SessionContext, sql: &str, params: Vec<ScalarValue>) -> Result<QueryOutput> {
    let options = SQLOptions::new()
        .with_allow_ddl(false)
        .with_allow_dml(false)
        .with_allow_statements(false);
    let mut frame = context.sql_with_options(sql, options).await?;
    if !params.is_empty() {
        frame = frame.with_param_values(params)?;
    }
    let schema = Arc::clone(frame.schema().inner());
    let batches = frame.collect().await?;
    Ok(QueryOutput { schema, batches })
}

impl Drop for AnalyticsEngine {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            // Dropping a runtime blocks, which panics inside an async context; a
            // background shutdown is safe from anywhere.
            runtime.shutdown_background();
        }
    }
}

#[cfg(test)]
mod tests {
    use datafusion::arrow::array::{AsArray, Int64Array, RecordBatch, StringArray};
    use datafusion::arrow::datatypes::{
        DataType, Field, Float64Type, Int64Type, Schema, SchemaRef,
    };
    use serde_json::json;

    use super::*;
    use crate::source::{ScanRequest, ScanSnapshot};

    /// In-memory source that records what the engine asked for.
    #[derive(Debug)]
    struct FixedSource {
        schema: SchemaRef,
        batch: RecordBatch,
        requests: std::sync::Mutex<Vec<(Vec<usize>, usize)>>,
    }

    impl FixedSource {
        fn people() -> Arc<Self> {
            let schema = Arc::new(Schema::new(vec![
                Field::new("id", DataType::Utf8, false),
                Field::new("age", DataType::Int64, true),
                Field::new("city", DataType::Utf8, true),
            ]));
            let ids: Vec<String> = (0..3000).map(|i| format!("u{i:05}")).collect();
            let ages: Vec<i64> = (0..3000).map(|i| 20 + i % 50).collect();
            let cities: Vec<&str> = (0..3000)
                .map(|i| if i % 3 == 0 { "paris" } else { "lyon" })
                .collect();
            let batch = RecordBatch::try_new(
                Arc::clone(&schema),
                vec![
                    Arc::new(StringArray::from(ids)),
                    Arc::new(Int64Array::from(ages)),
                    Arc::new(StringArray::from(cities)),
                ],
            )
            .unwrap();
            Arc::new(Self {
                schema,
                batch,
                requests: std::sync::Mutex::new(Vec::new()),
            })
        }
    }

    impl ColumnarSource for FixedSource {
        fn schema(&self) -> SchemaRef {
            Arc::clone(&self.schema)
        }

        fn scan(&self, request: &ScanRequest<'_>) -> Result<ScanSnapshot> {
            self.requests
                .lock()
                .unwrap()
                .push((request.projection.to_vec(), request.predicates.len()));
            Ok(ScanSnapshot {
                batches: vec![self.batch.project(request.projection)?],
                segments_total: 1,
                segments_pruned: 0,
                delta_rows: 0,
            })
        }
    }

    fn engine_with_people() -> (AnalyticsEngine, Arc<FixedSource>) {
        let engine = AnalyticsEngine::new(EngineOptions {
            batch_size: DEFAULT_BATCH_SIZE,
            target_partitions: 2,
        })
        .unwrap();
        let source = FixedSource::people();
        engine
            .register_table("people", Arc::clone(&source) as Arc<dyn ColumnarSource>)
            .unwrap();
        (engine, source)
    }

    #[test]
    fn aggregates_with_filter_and_group_by() {
        let (engine, source) = engine_with_people();
        let output = engine
            .query(
                "SELECT city, count(*) AS n, avg(age) AS mean, sum(age) AS total \
                 FROM people WHERE age >= 60 GROUP BY city ORDER BY city",
            )
            .unwrap();

        assert_eq!(output.column_names(), ["city", "n", "mean", "total"]);
        let rows = output.to_json_rows().unwrap();
        // Ages cycle over 20..=69, so 10 of every 50 rows are >= 60: 600 rows, mean 64.5.
        let lyon = (0..3000)
            .filter(|i| i % 3 != 0 && 20 + i % 50 >= 60)
            .count();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], json!("lyon"));
        assert_eq!(rows[0][1], json!(lyon));
        assert_eq!(rows[1][0], json!("paris"));
        assert_eq!(rows[1][1], json!(600 - lyon));
        let total: i64 = rows.iter().map(|row| row[3].as_i64().unwrap()).sum();
        assert_eq!(total, 600 * 129 / 2);

        // Only `age` and `city` were read, and the comparison was pushed down.
        let requests = source.requests.lock().unwrap();
        assert_eq!(requests.as_slice(), [(vec![1, 2], 1)]);
    }

    #[test]
    fn count_star_reads_only_the_key_column() {
        let (engine, source) = engine_with_people();
        let output = engine.query("SELECT count(*) FROM people").unwrap();
        let counts = output.batches[0].column(0).as_primitive::<Int64Type>();
        assert_eq!(counts.value(0), 3000);
        assert_eq!(source.requests.lock().unwrap().as_slice(), [(vec![0], 0)]);
    }

    #[test]
    fn batches_reach_the_engine_at_the_configured_size() {
        let (engine, _source) = engine_with_people();
        let output = engine.query("SELECT age FROM people").unwrap();
        assert_eq!(output.row_count(), 3000);
        assert!(output
            .batches
            .iter()
            .all(|batch| batch.num_rows() <= DEFAULT_BATCH_SIZE));
    }

    #[test]
    fn rejects_writes_ddl_and_unknown_objects() {
        let (engine, _source) = engine_with_people();
        for sql in [
            "CREATE TABLE t AS SELECT 1",
            "INSERT INTO people VALUES ('x', 1, 'y')",
            "DROP TABLE people",
            "SET datafusion.execution.batch_size = 1",
            "SELECT * FROM missing",
            "SELECT nope FROM people",
            "SELECT * FROM 'data.csv'",
            "not sql at all",
        ] {
            assert!(engine.query(sql).is_err(), "{sql} should be rejected");
        }
        // The table is untouched by the rejected statements.
        assert_eq!(
            engine.query("SELECT id FROM people").unwrap().row_count(),
            3000
        );
    }

    #[test]
    fn empty_results_keep_their_schema() {
        let (engine, _source) = engine_with_people();
        let output = engine
            .query("SELECT id, age FROM people WHERE age > 1000")
            .unwrap();
        assert_eq!(output.row_count(), 0);
        assert_eq!(output.column_names(), ["id", "age"]);
    }

    #[test]
    fn deregistered_tables_disappear() {
        let (engine, _source) = engine_with_people();
        assert!(engine.deregister_table("people").unwrap());
        assert!(!engine.deregister_table("people").unwrap());
        assert!(engine.query("SELECT 1 FROM people").is_err());
        assert!(engine
            .register_table("Bad Name", FixedSource::people())
            .is_err());
    }

    #[test]
    fn can_be_queried_and_dropped_from_inside_another_runtime() {
        let outer = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .build()
            .unwrap();
        let mean = outer.block_on(async {
            let (engine, _source) = engine_with_people();
            let output = engine.query("SELECT avg(age) FROM people").unwrap();
            let mean = output.batches[0]
                .column(0)
                .as_primitive::<Float64Type>()
                .value(0);
            // Dropping the engine here must not panic either.
            drop(engine);
            mean
        });
        assert!((mean - 44.5).abs() < 1e-9);
    }

    #[test]
    fn queries_run_over_their_own_tables_with_json_functions() {
        let engine = AnalyticsEngine::new(EngineOptions {
            batch_size: DEFAULT_BATCH_SIZE,
            target_partitions: 2,
        })
        .unwrap();
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("value", DataType::Utf8, false),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(StringArray::from(vec!["a", "b"])),
                Arc::new(StringArray::from(vec![
                    r#"{"n": 1, "t": "x"}"#,
                    r#"{"n": 2}"#,
                ])),
            ],
        )
        .unwrap();
        let table = QueryTable {
            name: "docs".into(),
            schema,
            batches: vec![batch],
        };
        let sql =
            "SELECT id, json_get_int(value, 'n') AS n, value->>'t' AS t FROM docs ORDER BY id";
        assert_eq!(engine.table_references(sql).unwrap(), ["docs"]);
        let rows = engine
            .query_tables(vec![table.clone()], sql, &[])
            .unwrap()
            .to_json_rows()
            .unwrap();
        assert_eq!(
            rows,
            [
                vec![json!("a"), json!(1), json!("x")],
                vec![json!("b"), json!(2), serde_json::Value::Null]
            ]
        );
        assert!(
            engine.query("SELECT * FROM docs").is_err(),
            "a query's tables are its own"
        );
        let bound = engine
            .query_tables(
                vec![table],
                "SELECT id FROM docs WHERE json_get_int(value, 'n') = $1 AND id <> $2",
                &[json!(2), json!("z")],
            )
            .unwrap();
        assert_eq!(bound.to_json_rows().unwrap(), [vec![json!("b")]]);
        assert!(engine
            .query_tables(Vec::new(), "SELECT $1", &[json!([1])])
            .is_err());
        assert!(engine.table_references("SELECT 1; SELECT 2").is_err());
    }

    #[test]
    fn zero_options_are_rejected() {
        let zero_batch = EngineOptions {
            batch_size: 0,
            target_partitions: 1,
        };
        assert!(AnalyticsEngine::new(zero_batch).is_err());
        let zero_partitions = EngineOptions {
            batch_size: 1,
            target_partitions: 0,
        };
        assert!(AnalyticsEngine::new(zero_partitions).is_err());
    }
}
