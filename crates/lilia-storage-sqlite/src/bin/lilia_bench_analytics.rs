//! Compares analytical queries on the row store with the vectorized columnar engine.
//!
//! The same data is loaded as a JSON collection and as a typed SQL table, then each
//! query is timed on every engine: `SQLite` over JSON documents, `SQLite` over typed
//! columns, the streaming aggregation pipeline, and the columnar engine without its
//! chunk cache, with a warm cache, and with a pending delta.

use std::time::Instant;

use clap::Parser;
use lilia_core::BatchOperation;
use lilia_storage_sqlite::{
    doc, Aggregate, Analytics, AnalyticsOptions, ColumnType, ColumnarTableSpec, Database,
    DatabaseOptions,
};
use serde::Serialize;
use serde_json::json;

const CITY_COUNT: usize = 16;

#[derive(Debug, Parser)]
#[command(
    name = "lilia-bench-analytics",
    about = "Row store vs vectorized columnar engine on analytical queries"
)]
struct Arguments {
    /// Number of rows to load
    #[arg(long, default_value_t = 500_000)]
    rows: usize,

    /// Timed runs per scenario; the median is reported
    #[arg(long, default_value_t = 7)]
    iterations: usize,

    /// Rows changed after the build to measure the merge-on-read overlay
    #[arg(long, default_value_t = 2_000)]
    delta_rows: usize,

    /// Output results as JSON
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Serialize)]
struct Measurement {
    query: &'static str,
    engine: &'static str,
    median_ms: f64,
    rows_per_second: f64,
}

#[derive(Debug, Serialize)]
struct Report {
    rows: usize,
    arch: &'static str,
    simd: Vec<&'static str>,
    batch_size: usize,
    target_partitions: usize,
    build_ms: f64,
    segments: usize,
    stored_bytes: u64,
    database_bytes: u64,
    measurements: Vec<Measurement>,
}

/// SIMD instruction sets the vectorized kernels were compiled for.
fn compiled_simd() -> Vec<&'static str> {
    [
        ("neon", cfg!(target_feature = "neon")),
        ("sse2", cfg!(target_feature = "sse2")),
        ("avx2", cfg!(target_feature = "avx2")),
        ("avx512f", cfg!(target_feature = "avx512f")),
    ]
    .into_iter()
    .filter_map(|(name, enabled)| enabled.then_some(name))
    .collect()
}

/// Median wall-clock time of `work`, in milliseconds.
fn median_ms<T>(iterations: usize, mut work: impl FnMut() -> T) -> f64 {
    let mut timings: Vec<f64> = (0..iterations.max(1))
        .map(|_| {
            let started = Instant::now();
            std::hint::black_box(work());
            started.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    timings.sort_by(f64::total_cmp);
    timings[timings.len() / 2]
}

fn load(db: &Database, rows: usize) -> Result<(), Box<dyn std::error::Error>> {
    let mut batch = Vec::with_capacity(1_000);
    for index in 0..rows {
        batch.push(BatchOperation::JsonPut {
            space: "events".into(),
            id: format!("e{index:09}"),
            value: json!({
                "age": 18 + (index * 7) % 60,
                "city": format!("city{:02}", index % CITY_COUNT),
                "amount": f64::from(u32::try_from(index % 1000)?) * 0.5,
            }),
            if_version: None,
        });
        if batch.len() == batch.capacity() {
            db.batch(&batch)?;
            batch.clear();
        }
    }
    if !batch.is_empty() {
        db.batch(&batch)?;
    }

    db.sql_query(
        "CREATE TABLE events_sql(id INTEGER PRIMARY KEY, age INTEGER, city TEXT, amount REAL)",
        &[],
    )?;
    db.sql_query(
        "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i + 1 < ?1) \
         INSERT INTO events_sql SELECT i, 18 + (i * 7) % 60, printf('city%02d', i % 16), \
         (i % 1000) * 0.5 FROM n",
        &[json!(rows)],
    )?;
    Ok(())
}

/// Rewrites the first `count` documents so they sit in the delta of the columnar table.
fn touch(db: &Database, count: usize) -> Result<(), Box<dyn std::error::Error>> {
    let operations: Vec<BatchOperation> = (0..count)
        .map(|index| BatchOperation::JsonPut {
            space: "events".into(),
            id: format!("e{index:09}"),
            value: json!({ "age": 30, "city": "city00", "amount": 1.0 }),
            if_version: None,
        })
        .collect();
    for chunk in operations.chunks(1_000) {
        db.batch(chunk)?;
    }
    Ok(())
}

struct Bench<'a> {
    db: &'a Database,
    analytics: &'a Analytics<'a>,
    rows: usize,
    iterations: usize,
    measurements: Vec<Measurement>,
}

impl Bench<'_> {
    fn record<T>(&mut self, query: &'static str, engine: &'static str, work: impl FnMut() -> T) {
        let median_ms = median_ms(self.iterations, work);
        // Row counts are far below 2^53, where `f64` stops being exact.
        #[allow(clippy::cast_precision_loss)]
        let rows = self.rows as f64;
        self.measurements.push(Measurement {
            query,
            engine,
            median_ms,
            rows_per_second: rows / (median_ms / 1e3),
        });
    }

    fn row_store(&mut self) {
        let db = self.db;
        self.record("filter + sum/avg", "SQLite, JSON documents", || {
            db.sql_query(
                "SELECT count(*), sum(lilia_extract(value, '$.amount')), \
                 avg(lilia_extract(value, '$.amount')) FROM _lilia_json \
                 WHERE space = 'events' AND lilia_extract(value, '$.age') > 40",
                &[],
            )
            .unwrap()
        });
        self.record("filter + sum/avg", "SQLite, typed table", || {
            db.sql_query(
                "SELECT count(*), sum(amount), avg(amount) FROM events_sql WHERE age > 40",
                &[],
            )
            .unwrap()
        });
        self.record("filter + sum/avg", "aggregation pipeline", || {
            db.aggregate_json("events")
                .filter(doc! { "age": { "$gt": 40 } })
                .aggregate_all(vec![
                    Aggregate::count("n"),
                    Aggregate::sum("amount", "total"),
                    Aggregate::avg("amount", "mean"),
                ])
                .execute()
                .unwrap()
        });

        self.record("group by", "SQLite, JSON documents", || {
            db.sql_query(
                "SELECT lilia_extract(value, '$.city'), count(*), \
                 avg(lilia_extract(value, '$.amount')) FROM _lilia_json \
                 WHERE space = 'events' GROUP BY 1",
                &[],
            )
            .unwrap()
        });
        self.record("group by", "SQLite, typed table", || {
            db.sql_query(
                "SELECT city, count(*), avg(amount) FROM events_sql GROUP BY city",
                &[],
            )
            .unwrap()
        });
        self.record("group by", "aggregation pipeline", || {
            db.aggregate_json("events")
                .group_by(
                    "city",
                    vec![Aggregate::count("n"), Aggregate::avg("amount", "mean")],
                )
                .execute()
                .unwrap()
        });
    }

    fn columnar(&mut self, engine: &'static str) {
        let analytics = self.analytics;
        self.record("filter + sum/avg", engine, || {
            analytics
                .query("SELECT count(*), sum(amount), avg(amount) FROM events_col WHERE age > 40")
                .unwrap()
        });
        self.record("group by", engine, || {
            analytics
                .query("SELECT city, count(*), avg(amount) FROM events_col GROUP BY city")
                .unwrap()
        });
    }
}

/// Fails loudly if the engines disagree, so a fast wrong answer is never reported.
fn check_agreement(
    db: &Database,
    analytics: &Analytics<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    let query = "SELECT count(*), sum(amount) FROM {t} WHERE age > 40";
    let expected = db.sql_query(&query.replace("{t}", "events_sql"), &[])?;
    let actual = analytics.query_json(&query.replace("{t}", "events_col"))?;
    if actual.rows == expected.rows {
        Ok(())
    } else {
        Err(format!(
            "the columnar engine disagrees with SQLite: {:?} vs {:?}",
            actual.rows, expected.rows
        )
        .into())
    }
}

fn open(
    path: &std::path::Path,
    options: AnalyticsOptions,
) -> Result<Database, Box<dyn std::error::Error>> {
    Ok(Database::open(
        DatabaseOptions::durable(path).with_analytics_options(options),
    )?)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = Arguments::parse();
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("bench.lilia");
    // Background compaction is off so the pending-delta scenario stays pending.
    let options = AnalyticsOptions {
        compaction_interval_ms: 0,
        ..AnalyticsOptions::default()
    };
    let mut measurements = Vec::new();

    // First pass, chunk cache disabled: every query reads and decodes its columns from
    // the database file.
    let (build_ms, stats) = {
        let db = open(
            &path,
            AnalyticsOptions {
                cache_bytes: 0,
                ..options
            },
        )?;
        load(&db, arguments.rows)?;
        let analytics = db.analytics()?;
        let started = Instant::now();
        analytics.create_table(
            ColumnarTableSpec::json("events_col", "events")
                .column("age", "$.age", ColumnType::Int64)
                .column("city", "$.city", ColumnType::Utf8)
                .column("amount", "$.amount", ColumnType::Float64),
        )?;
        let build_ms = started.elapsed().as_secs_f64() * 1e3;
        check_agreement(&db, &analytics)?;

        let mut bench = Bench {
            db: &db,
            analytics: &analytics,
            rows: arguments.rows,
            iterations: arguments.iterations,
            measurements: Vec::new(),
        };
        bench.row_store();
        bench.columnar("columnar, no cache");
        measurements.append(&mut bench.measurements);
        (build_ms, analytics.table_stats("events_col")?)
    };

    // Second pass on the reopened file, with the cache: steady-state queries, then the
    // same queries while changed rows are overlaid from the row store.
    let db = open(&path, options)?;
    let analytics = db.analytics()?;
    analytics.query("SELECT count(age), count(city), count(amount) FROM events_col")?;
    let mut bench = Bench {
        db: &db,
        analytics: &analytics,
        rows: arguments.rows,
        iterations: arguments.iterations,
        measurements: Vec::new(),
    };
    bench.columnar("columnar, warm cache");
    touch(&db, arguments.delta_rows.min(arguments.rows))?;
    bench.columnar("columnar, pending delta");
    measurements.append(&mut bench.measurements);

    measurements.sort_by(|a, b| a.query.cmp(b.query));
    let report = Report {
        rows: arguments.rows,
        arch: std::env::consts::ARCH,
        simd: compiled_simd(),
        batch_size: options.batch_size,
        target_partitions: options.target_partitions,
        build_ms,
        segments: stats.segments,
        stored_bytes: stats.stored_bytes,
        database_bytes: std::fs::metadata(&path)?.len(),
        measurements,
    };

    if arguments.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "\n=== LiliaDB analytics benchmark: {} rows, {} ({}), batch {} x {} partitions ===",
        report.rows,
        report.arch,
        report.simd.join("+"),
        report.batch_size,
        report.target_partitions
    );
    println!(
        "columnar build: {:.0} ms, {} segments, {:.1} MiB of columns in a {:.1} MiB database\n",
        report.build_ms,
        report.segments,
        mebibytes(report.stored_bytes),
        mebibytes(report.database_bytes)
    );
    println!(
        "{:<18} | {:<26} | {:>11} | {:>14}",
        "Query", "Engine", "Median (ms)", "Rows/s"
    );
    println!("{:-<18}-|-{:-<26}-|-{:->11}-|-{:->14}", "", "", "", "");
    for m in &report.measurements {
        println!(
            "{:<18} | {:<26} | {:>11.2} | {:>14.0}",
            m.query, m.engine, m.median_ms, m.rows_per_second
        );
    }
    println!();
    Ok(())
}

/// Storage sizes are far below 2^53 bytes, where `f64` stops being exact.
#[allow(clippy::cast_precision_loss)]
fn mebibytes(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}
