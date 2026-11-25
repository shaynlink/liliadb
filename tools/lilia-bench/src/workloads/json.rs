//! JSON documents: indexed inserts at three document sizes, then reads by id, field extraction and
//! queries served by a secondary index on `$.city`.

use std::time::Instant;

use serde_json::{json, Value};

use super::{fresh_dir, nanos_since, Meter, Settings};
use crate::engines::Result;
use crate::keys::{key, Rng};
use crate::os;
use crate::record::{params, EngineInfo, Line};

const CITIES: u64 = 100;
const QUERY_LIMIT: usize = 100;

/// A document store with one secondary index on `$.city`.
pub trait DocStore {
    fn info(&self) -> EngineInfo;
    fn put(&mut self, docs: &[(String, Value)]) -> Result<()>;
    /// Fetches and decodes the document; whether it exists.
    fn get(&mut self, id: &str) -> Result<bool>;
    /// `$.user.name` of the document.
    fn extract_name(&mut self, id: &str) -> Result<Option<String>>;
    /// Decoded documents whose `$.city` equals `city`, at most `limit`.
    fn find_city(&mut self, city: &str, limit: usize) -> Result<usize>;
}

/// Opens the store of `engine` in `dir`, index created.
fn open(engine: &str, dir: &std::path::Path) -> Result<Box<dyn DocStore>> {
    match engine {
        #[cfg(feature = "lilia")]
        "lilia" => Ok(Box::new(lilia::Store::open(dir)?)),
        #[cfg(feature = "sqlite")]
        "sqlite" => Ok(Box::new(sqlite::Store::open(dir)?)),
        _ => Err(format!("engine `{engine}` has no JSON store in this binary ({})", dir.display()).into()),
    }
}

#[must_use]
pub fn doc_id(i: u64) -> String {
    let k = key(i);
    format!("{:016x}{i}", u64::from_be_bytes(k[..8].try_into().unwrap_or_default()))
}

fn city(i: u64) -> String {
    format!("city{:03}", i % CITIES)
}

/// Document `i`, padded with a text field to about `size` bytes of JSON.
#[must_use]
pub fn document(i: u64, size: usize) -> Value {
    let mut doc = json!({
        "city": city(i),
        "age": 18 + i % 60,
        "score": (i % 1_000) as f64 / 10.0,
        "user": { "name": format!("user{i}"), "email": format!("user{i}@example.com") },
        "tags": [format!("t{}", i % 7), format!("t{}", i % 11), format!("t{}", i % 13)],
        "body": "",
    });
    let base = doc.to_string().len();
    let body = crate::keys::value(i, size.saturating_sub(base));
    doc["body"] = Value::String(String::from_utf8_lossy(&body).replace('"', "'"));
    doc
}

pub fn run(engine: &str, settings: &Settings, emit: &mut dyn FnMut(Line)) -> Result<()> {
    for size in [1_024, 4_096, 65_536] {
        let dir = fresh_dir(settings)?;
        let mut store = open(engine, dir.path())?;
        let cap = settings.documents.min((1u64 << 30) / size as u64);
        let budget = settings.budget();
        let (mut latencies, mut written) = (Vec::new(), 0u64);
        let meter = Meter::start();
        while written < cap && meter.elapsed() < budget {
            let end = (written + 100).min(cap);
            let docs: Vec<(String, Value)> = (written..end).map(|i| (doc_id(i), document(i, size))).collect();
            let started = Instant::now();
            store.put(&docs)?;
            latencies.push(nanos_since(started));
            written = end;
        }
        let mut record = meter.finish(
            store.info(),
            "json.put",
            params([("doc_size", json!(size)), ("batch", json!(100)), ("indexes", json!(1))]),
            written,
            written * size as u64,
            &mut latencies,
        );
        drop(store);
        record.disk_bytes = Some(os::dir_size(dir.path()));
        emit(Line::Record(Box::new(record)));
    }

    let dir = fresh_dir(settings)?;
    let mut store = open(engine, dir.path())?;
    let n = settings.documents;
    let mut i = 0;
    while i < n {
        let end = (i + 1_000).min(n);
        let docs: Vec<(String, Value)> = (i..end).map(|j| (doc_id(j), document(j, 1_024))).collect();
        store.put(&docs)?;
        i = end;
    }
    emit(Line::Record(Box::new(timed(&mut *store, settings, "json.get", |store, rng| {
        let id = rng.below(n);
        if store.get(&doc_id(id))? { Ok(1) } else { Err(format!("document {id} missing").into()) }
    })?)));
    emit(Line::Record(Box::new(timed(&mut *store, settings, "json.extract", |store, rng| {
        let id = rng.below(n);
        match store.extract_name(&doc_id(id))? {
            Some(name) if name == format!("user{id}") => Ok(1),
            other => Err(format!("document {id}: $.user.name was {other:?}").into()),
        }
    })?)));
    let expected = usize::try_from(n.div_ceil(CITIES).min(QUERY_LIMIT as u64))?;
    emit(Line::Record(Box::new(timed(&mut *store, settings, "json.find", |store, rng| {
        // Cities below `n % CITIES` hold one more document; query the others for an exact count.
        let c = rng.below(CITIES.min(n));
        let found = store.find_city(&city(c), QUERY_LIMIT)?;
        if found + 1 >= expected { Ok(1) } else { Err(format!("city {c}: {found} of {expected} documents").into()) }
    })?)));
    Ok(())
}

/// Runs `op` on random documents until the budget is spent.
fn timed(
    store: &mut dyn DocStore,
    settings: &Settings,
    workload: &str,
    mut op: impl FnMut(&mut dyn DocStore, &mut Rng) -> Result<u64>,
) -> Result<crate::record::Record> {
    let mut rng = Rng::new(0xD0C5);
    let budget = settings.budget();
    let (mut latencies, mut ops) = (Vec::new(), 0u64);
    let meter = Meter::start();
    while meter.elapsed() < budget {
        let started = Instant::now();
        ops += op(store, &mut rng)?;
        latencies.push(nanos_since(started));
    }
    Ok(meter.finish(
        store.info(),
        workload,
        params([("documents", json!(settings.documents)), ("doc_size", json!(1_024))]),
        ops,
        0,
        &mut latencies,
    ))
}

#[cfg(feature = "lilia")]
mod lilia {
    use lilia_core::BatchOperation;
    use lilia_protocol::{Operation, ResponseValue};
    use serde_json::Value;

    use crate::engines::lilia_db::LiliaDb;
    use crate::engines::{Engine, Result};
    use crate::record::EngineInfo;

    const SPACE: &str = "docs";

    pub struct Store(LiliaDb);

    impl Store {
        pub fn open(dir: &std::path::Path) -> Result<Self> {
            let db = LiliaDb::open("lilia", dir)?;
            db.run(Operation::JsonCreateIndex { space: SPACE.into(), name: "by_city".into(), path: "$.city".into(), filter: None })?;
            Ok(Self(db))
        }
    }

    impl super::DocStore for Store {
        fn info(&self) -> EngineInfo {
            self.0.info()
        }

        fn put(&mut self, docs: &[(String, Value)]) -> Result<()> {
            let ops = docs
                .iter()
                .map(|(id, value)| BatchOperation::JsonPut { space: SPACE.into(), id: id.clone(), value: value.clone(), if_version: None })
                .collect();
            self.0.batch(ops)
        }

        fn get(&mut self, id: &str) -> Result<bool> {
            let found = self.0.run(Operation::JsonGet { space: SPACE.into(), id: id.to_owned() })?;
            Ok(matches!(found, ResponseValue::Json(Some(_))))
        }

        fn extract_name(&mut self, id: &str) -> Result<Option<String>> {
            let path = "$.user.name".to_owned();
            Ok(match self.0.run(Operation::JsonExtract { space: SPACE.into(), id: id.to_owned(), path })? {
                ResponseValue::JsonExtracted(Some(Value::String(name))) => Some(name),
                _ => None,
            })
        }

        fn find_city(&mut self, city: &str, limit: usize) -> Result<usize> {
            let query = serde_json::json!({ "by_city": city });
            Ok(match self.0.run(Operation::JsonFind { space: SPACE.into(), query, after: None, limit: u32::try_from(limit)? })? {
                ResponseValue::JsonPage(documents) => documents.len(),
                other => return Err(format!("json_find answered {}", other.name()).into()),
            })
        }
    }
}

#[cfg(feature = "sqlite")]
mod sqlite {
    use rusqlite::{params, Connection, OptionalExtension};
    use serde_json::Value;

    use crate::engines::sqlite::{info, Sqlite};
    use crate::engines::Result;
    use crate::record::EngineInfo;

    /// Documents stored as SQLite's binary JSON (`jsonb`), with an expression index on `$.city`.
    pub struct Store(Connection);

    impl Store {
        pub fn open(dir: &std::path::Path) -> Result<Self> {
            let conn = Sqlite::open("sqlite", dir)?.connect()?;
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS docs(id TEXT PRIMARY KEY, body BLOB NOT NULL) STRICT, WITHOUT ROWID;
                 CREATE INDEX IF NOT EXISTS docs_city ON docs(body ->> '$.city');",
            )?;
            Ok(Self(conn))
        }
    }

    impl super::DocStore for Store {
        fn info(&self) -> EngineInfo {
            let mut info = info("sqlite");
            info.name = "SQLite (WAL, JSONB)".into();
            info
        }

        fn put(&mut self, docs: &[(String, Value)]) -> Result<()> {
            let tx = self.0.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            {
                let mut stmt = tx.prepare_cached("INSERT OR REPLACE INTO docs(id, body) VALUES(?1, jsonb(?2))")?;
                for (id, value) in docs {
                    stmt.execute(params![id, value.to_string()])?;
                }
            }
            tx.commit()?;
            Ok(())
        }

        fn get(&mut self, id: &str) -> Result<bool> {
            let mut stmt = self.0.prepare_cached("SELECT json(body) FROM docs WHERE id = ?1")?;
            let text: Option<String> = stmt.query_row([id], |r| r.get(0)).optional()?;
            Ok(match text {
                Some(text) => serde_json::from_str::<Value>(&text).is_ok(),
                None => false,
            })
        }

        fn extract_name(&mut self, id: &str) -> Result<Option<String>> {
            let mut stmt = self.0.prepare_cached("SELECT body ->> '$.user.name' FROM docs WHERE id = ?1")?;
            Ok(stmt.query_row([id], |r| r.get::<_, Option<String>>(0)).optional()?.flatten())
        }

        fn find_city(&mut self, city: &str, limit: usize) -> Result<usize> {
            let mut stmt = self.0.prepare_cached("SELECT json(body) FROM docs WHERE body ->> '$.city' = ?1 LIMIT ?2")?;
            let mut rows = stmt.query(params![city, i64::try_from(limit)?])?;
            let mut count = 0;
            while let Some(row) = rows.next()? {
                let text: String = row.get(0)?;
                count += usize::from(serde_json::from_str::<Value>(&text).is_ok());
            }
            Ok(count)
        }
    }
}
