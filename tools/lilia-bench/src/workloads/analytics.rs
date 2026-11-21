//! Analytical queries over an event table (age, city, amount): filtered aggregate and group-by.
//! Every engine's answer is checked against the value computed from the generator.

use std::time::Instant;

use serde_json::json;

use super::{fresh_dir, nanos_since, Meter, Settings};
use crate::engines::Result;
use crate::os;
use crate::record::{params, EngineInfo, Line};

const CITIES: u64 = 16;
/// Timed runs per query; the record keeps every run, the report shows the median.
const RUNS: usize = 7;

#[derive(Debug, Clone, Copy)]
pub enum Query {
    /// `SELECT count(*), sum(amount), avg(amount) FROM events WHERE age > 40`
    FilterAggregate,
    /// `SELECT city, count(*), avg(amount) FROM events GROUP BY city`
    GroupBy,
}

impl Query {
    const fn name(self) -> &'static str {
        match self {
            Self::FilterAggregate => "filter_aggregate",
            Self::GroupBy => "group_by",
        }
    }

    const fn sql(self) -> &'static str {
        match self {
            Self::FilterAggregate => "SELECT count(*), sum(amount), avg(amount) FROM events WHERE age > 40",
            Self::GroupBy => "SELECT city, count(*), avg(amount) FROM events GROUP BY city",
        }
    }
}

fn age(i: u64) -> u64 {
    18 + (i * 7) % 60
}

#[cfg(any(feature = "lilia", feature = "sqlite", feature = "duckdb"))]
fn city(i: u64) -> String {
    format!("city{:02}", i % CITIES)
}

#[cfg(any(feature = "lilia", feature = "sqlite", feature = "duckdb"))]
fn amount(i: u64) -> f64 {
    (i % 1_000) as f64 * 0.5
}

pub trait Analytic {
    fn info(&self) -> EngineInfo;
    /// Execution modes over the same loaded data (LiliaDB: row pipeline, then columnar).
    fn modes(&self) -> Vec<&'static str>;
    fn load(&mut self, rows: u64) -> Result<()>;
    /// Prepares `mode` after loading (columnar build); returns whether it did any work.
    fn prepare(&mut self, mode: usize) -> Result<bool>;
    /// Runs `query`; returns the filtered row count or the number of groups.
    fn run(&mut self, mode: usize, query: Query) -> Result<u64>;
}

fn open(engine: &str, dir: &std::path::Path) -> Result<Box<dyn Analytic>> {
    match engine {
        #[cfg(feature = "lilia")]
        "lilia" => Ok(Box::new(lilia::Events::open(dir)?)),
        #[cfg(feature = "sqlite")]
        "sqlite" => Ok(Box::new(sqlite::Events::open(dir)?)),
        #[cfg(feature = "duckdb")]
        "duckdb" => Ok(Box::new(duckdb::Events::open(dir)?)),
        _ => Err(format!("engine `{engine}` has no analytics adapter in this binary ({})", dir.display()).into()),
    }
}

pub fn run(engine: &str, settings: &Settings, emit: &mut dyn FnMut(Line)) -> Result<()> {
    let rows = settings.analytics_rows;
    let expected_filter = (0..rows).filter(|&i| age(i) > 40).count() as u64;
    let expected_groups = rows.min(CITIES);
    let dir = fresh_dir(settings)?;
    let mut events = open(engine, dir.path())?;

    let meter = Meter::start();
    let started = Instant::now();
    events.load(rows)?;
    let mut load_ns = [nanos_since(started)];
    let mut record = meter.finish(events.info(), "analytics.load", params([("rows", json!(rows))]), rows, 0, &mut load_ns);
    record.disk_bytes = Some(os::dir_size(dir.path()));
    emit(Line::Record(Box::new(record)));

    for (mode, name) in events.modes().into_iter().enumerate() {
        let meter = Meter::start();
        let started = Instant::now();
        if events.prepare(mode)? {
            let mut ns = [nanos_since(started)];
            let record = meter.finish(events.info(), "analytics.build", params([("rows", json!(rows)), ("mode", json!(name))]), rows, 0, &mut ns);
            emit(Line::Record(Box::new(record)));
        }
        for query in [Query::FilterAggregate, Query::GroupBy] {
            let expected = match query {
                Query::FilterAggregate => expected_filter,
                Query::GroupBy => expected_groups,
            };
            let mut latencies = Vec::with_capacity(RUNS);
            let meter = Meter::start();
            for _ in 0..RUNS {
                let started = Instant::now();
                let answer = events.run(mode, query)?;
                latencies.push(nanos_since(started));
                if answer != expected {
                    return Err(format!("{engine} ({name}) {}: answered {answer}, expected {expected}", query.name()).into());
                }
            }
            let mut record = meter.finish(
                events.info(),
                &format!("analytics.{}", query.name()),
                params([("rows", json!(rows)), ("mode", json!(name)), ("runs", json!(RUNS))]),
                rows * RUNS as u64,
                0,
                &mut latencies,
            );
            record.quality = Some(json!({ "answer": expected, "sql": query.sql() }));
            emit(Line::Record(Box::new(record)));
        }
    }
    Ok(())
}

#[cfg(feature = "lilia")]
mod lilia {
    use lilia_core::BatchOperation;
    use lilia_storage_sqlite::{doc, Aggregate, ColumnType, ColumnarTableSpec};
    use serde_json::json;

    use super::{age, amount, city, Query};
    use crate::engines::lilia::{info, Lilia};
    use crate::engines::Result;
    use crate::record::EngineInfo;

    pub struct Events(Lilia);

    impl Events {
        pub fn open(dir: &std::path::Path) -> Result<Self> {
            Ok(Self(Lilia::open("lilia", dir)?))
        }
    }

    impl super::Analytic for Events {
        fn info(&self) -> EngineInfo {
            info("lilia")
        }

        fn modes(&self) -> Vec<&'static str> {
            vec!["aggregation pipeline (JSON)", "columnar (DataFusion)"]
        }

        fn load(&mut self, rows: u64) -> Result<()> {
            let mut start = 0;
            while start < rows {
                let end = (start + 1_000).min(rows);
                let ops: Vec<_> = (start..end)
                    .map(|i| BatchOperation::JsonPut {
                        space: "events".into(),
                        id: format!("e{i:010}"),
                        value: json!({ "age": age(i), "city": city(i), "amount": amount(i) }),
                        if_version: None,
                    })
                    .collect();
                self.0.database().batch(&ops)?;
                start = end;
            }
            Ok(())
        }

        fn prepare(&mut self, mode: usize) -> Result<bool> {
            if mode == 0 {
                return Ok(false);
            }
            self.0.database().analytics()?.create_table(
                ColumnarTableSpec::json("events", "events")
                    .column("age", "$.age", ColumnType::Int64)
                    .column("city", "$.city", ColumnType::Utf8)
                    .column("amount", "$.amount", ColumnType::Float64),
            )?;
            Ok(true)
        }

        fn run(&mut self, mode: usize, query: Query) -> Result<u64> {
            let db = self.0.database();
            if mode == 1 {
                let result = db.analytics()?.query_json(query.sql())?;
                return Ok(match query {
                    Query::FilterAggregate => result.rows.first().and_then(|r| r.first()).and_then(serde_json::Value::as_u64).unwrap_or(0),
                    Query::GroupBy => result.rows.len() as u64,
                });
            }
            Ok(match query {
                Query::FilterAggregate => db
                    .aggregate_json("events")
                    .filter(doc! { "age": { "$gt": 40 } })
                    .aggregate_all(vec![Aggregate::count("n"), Aggregate::sum("amount", "total"), Aggregate::avg("amount", "mean")])
                    .execute()?
                    .first()
                    .and_then(|r| r.get("n"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                Query::GroupBy => db
                    .aggregate_json("events")
                    .group_by("city", vec![Aggregate::count("n"), Aggregate::avg("amount", "mean")])
                    .execute()?
                    .len() as u64,
            })
        }
    }
}

#[cfg(feature = "sqlite")]
mod sqlite {
    use rusqlite::{params, Connection};

    use super::{age, amount, city, Query};
    use crate::engines::sqlite::{info, Sqlite};
    use crate::engines::Result;
    use crate::record::EngineInfo;

    pub struct Events(Connection);

    impl Events {
        pub fn open(dir: &std::path::Path) -> Result<Self> {
            let conn = Sqlite::open("sqlite", dir)?.connect()?;
            conn.execute_batch("CREATE TABLE events(id INTEGER PRIMARY KEY, age INTEGER, city TEXT, amount REAL) STRICT;")?;
            Ok(Self(conn))
        }
    }

    impl super::Analytic for Events {
        fn info(&self) -> EngineInfo {
            let mut info = info("sqlite");
            info.name = "SQLite (typed table)".into();
            info
        }

        fn modes(&self) -> Vec<&'static str> {
            vec!["row store (typed table)"]
        }

        fn load(&mut self, rows: u64) -> Result<()> {
            let mut start = 0;
            while start < rows {
                let end = (start + 10_000).min(rows);
                let tx = self.0.transaction()?;
                {
                    let mut stmt = tx.prepare_cached("INSERT INTO events(id, age, city, amount) VALUES(?1, ?2, ?3, ?4)")?;
                    for i in start..end {
                        stmt.execute(params![i64::try_from(i)?, i64::try_from(age(i))?, city(i), amount(i)])?;
                    }
                }
                tx.commit()?;
                start = end;
            }
            Ok(())
        }

        fn prepare(&mut self, _mode: usize) -> Result<bool> {
            Ok(false)
        }

        fn run(&mut self, _mode: usize, query: Query) -> Result<u64> {
            let mut stmt = self.0.prepare_cached(query.sql())?;
            Ok(match query {
                Query::FilterAggregate => u64::try_from(stmt.query_row([], |r| r.get::<_, i64>(0))?)?,
                Query::GroupBy => {
                    let mut rows = stmt.query([])?;
                    let mut groups = 0;
                    while rows.next()?.is_some() {
                        groups += 1;
                    }
                    groups
                }
            })
        }
    }
}

#[cfg(feature = "duckdb")]
mod duckdb {
    use duckdb::{params, Connection};

    use super::{age, amount, city, Query};
    use crate::engines::Result;
    use crate::record::EngineInfo;

    pub struct Events {
        conn: Connection,
        version: String,
    }

    impl Events {
        pub fn open(dir: &std::path::Path) -> Result<Self> {
            let conn = Connection::open(dir.join("bench.duckdb"))?;
            conn.execute_batch("CREATE TABLE events(id BIGINT, age INTEGER, city VARCHAR, amount DOUBLE);")?;
            let version: String = conn.query_row("SELECT version()", [], |r| r.get(0))?;
            Ok(Self { conn, version })
        }
    }

    impl super::Analytic for Events {
        fn info(&self) -> EngineInfo {
            EngineInfo {
                id: "duckdb".into(),
                name: "DuckDB".into(),
                version: format!("{} (duckdb crate {})", self.version, env!("LILIA_BENCH_DEP_DUCKDB")),
                sync: crate::engines::std_file_sync(),
            }
        }

        fn modes(&self) -> Vec<&'static str> {
            vec!["columnar table"]
        }

        fn load(&mut self, rows: u64) -> Result<()> {
            let mut appender = self.conn.appender("events")?;
            for i in 0..rows {
                appender.append_row(params![i64::try_from(i)?, i32::try_from(age(i))?, city(i), amount(i)])?;
            }
            appender.flush()?;
            drop(appender);
            self.conn.execute_batch("CHECKPOINT;")?;
            Ok(())
        }

        fn prepare(&mut self, _mode: usize) -> Result<bool> {
            Ok(false)
        }

        fn run(&mut self, _mode: usize, query: Query) -> Result<u64> {
            let mut stmt = self.conn.prepare_cached(query.sql())?;
            Ok(match query {
                Query::FilterAggregate => u64::try_from(stmt.query_row([], |r| r.get::<_, i64>(0))?)?,
                Query::GroupBy => {
                    let mut rows = stmt.query([])?;
                    let mut groups = 0;
                    while rows.next()?.is_some() {
                        groups += 1;
                    }
                    groups
                }
            })
        }
    }
}
