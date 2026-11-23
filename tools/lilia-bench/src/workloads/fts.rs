//! Full-text search: indexing a synthetic corpus (Zipf-distributed vocabulary), then ranked
//! top-10 queries of one and two terms.

use std::time::Instant;

use serde_json::json;

use super::{fresh_dir, nanos_since, Meter, Settings};
use crate::engines::Result;
use crate::keys::{Rng, Zipf};
use crate::os;
use crate::record::{params, EngineInfo, Line};

const VOCABULARY: u64 = 20_000;
const WORDS_PER_DOC: usize = 60;
const TOP_K: usize = 10;

pub trait TextIndex {
    fn info(&self) -> EngineInfo;
    fn index(&mut self, docs: &[(String, String)]) -> Result<()>;
    /// Ranked hits for `query` (terms separated by spaces), at most `k`.
    fn search(&mut self, query: &str, k: usize) -> Result<usize>;
}

fn open(engine: &str, dir: &std::path::Path) -> Result<Box<dyn TextIndex>> {
    match engine {
        #[cfg(feature = "lilia")]
        "lilia" => Ok(Box::new(lilia::Index::open(dir)?)),
        #[cfg(feature = "sqlite")]
        "sqlite" => Ok(Box::new(sqlite::Index::open(dir)?)),
        _ => Err(format!("engine `{engine}` has no full-text index in this binary ({})", dir.display()).into()),
    }
}

/// Distinct lowercase pseudo-word for vocabulary rank `r` (base-80 syllables).
fn word(r: u64) -> String {
    const CONSONANTS: &[u8; 16] = b"bcdfghjklmnprstv";
    const VOWELS: &[u8; 5] = b"aeiou";
    let mut x = r + 1;
    let mut w = String::new();
    while x > 0 {
        w.push(char::from(CONSONANTS[(x % 16) as usize]));
        w.push(char::from(VOWELS[((x / 16) % 5) as usize]));
        x /= 80;
    }
    w
}

fn text(i: u64, zipf: &mut Zipf) -> String {
    let mut rng = Rng::new(i ^ 0xF75);
    (0..WORDS_PER_DOC).map(|_| word(zipf.next(&mut rng))).collect::<Vec<_>>().join(" ")
}

pub fn run(engine: &str, settings: &Settings, emit: &mut dyn FnMut(Line)) -> Result<()> {
    let dir = fresh_dir(settings)?;
    let mut index = open(engine, dir.path())?;
    let mut zipf = Zipf::new(VOCABULARY);
    let n = settings.documents;
    let (mut latencies, mut done, mut text_bytes) = (Vec::new(), 0u64, 0u64);
    let meter = Meter::start();
    while done < n {
        let end = (done + 100).min(n);
        let docs: Vec<(String, String)> = (done..end).map(|i| (format!("doc{i}"), text(i, &mut zipf))).collect();
        text_bytes += docs.iter().map(|(_, t)| t.len() as u64).sum::<u64>();
        let started = Instant::now();
        index.index(&docs)?;
        latencies.push(nanos_since(started));
        done = end;
    }
    let mut record = meter.finish(
        index.info(),
        "fts.index",
        params([("documents", json!(n)), ("words_per_doc", json!(WORDS_PER_DOC)), ("batch", json!(100))]),
        n,
        text_bytes,
        &mut latencies,
    );
    drop(index);
    record.disk_bytes = Some(os::dir_size(dir.path()));
    emit(Line::Record(Box::new(record)));
    let mut index = open(engine, dir.path())?;

    for terms in [1usize, 2] {
        let mut rng = Rng::new(0x5EA2C4 + terms as u64);
        let budget = settings.budget();
        let (mut latencies, mut queries, mut hits) = (Vec::new(), 0u64, 0u64);
        let meter = Meter::start();
        while meter.elapsed() < budget {
            // Mid-frequency terms: present in many documents, far from stop-word frequency.
            let query = (0..terms).map(|_| word(50 + rng.below(950))).collect::<Vec<_>>().join(" ");
            let started = Instant::now();
            hits += index.search(&query, TOP_K)? as u64;
            latencies.push(nanos_since(started));
            queries += 1;
        }
        let mut record = meter.finish(
            index.info(),
            "fts.search",
            params([("terms", json!(terms)), ("top_k", json!(TOP_K)), ("documents", json!(n))]),
            queries,
            0,
            &mut latencies,
        );
        record.quality = Some(json!({ "mean_hits": hits as f64 / queries.max(1) as f64 }));
        emit(Line::Record(Box::new(record)));
    }
    Ok(())
}

#[cfg(feature = "lilia")]
mod lilia {
    use lilia_core::BatchOperation;
    use lilia_protocol::{Operation, ResponseValue};

    use crate::engines::lilia_db::LiliaDb;
    use crate::engines::{Engine, Result};
    use crate::record::EngineInfo;

    const COLLECTION: &str = "corpus";

    pub struct Index(LiliaDb);

    impl Index {
        pub fn open(dir: &std::path::Path) -> Result<Self> {
            Ok(Self(LiliaDb::open("lilia", dir)?))
        }
    }

    impl super::TextIndex for Index {
        fn info(&self) -> EngineInfo {
            self.0.info()
        }

        fn index(&mut self, docs: &[(String, String)]) -> Result<()> {
            let ops = docs
                .iter()
                .map(|(id, content)| BatchOperation::FtsIndex { collection: COLLECTION.into(), doc_id: id.clone(), content: content.clone() })
                .collect();
            self.0.batch(ops)
        }

        fn search(&mut self, query: &str, k: usize) -> Result<usize> {
            let search = Operation::FtsSearch { collection: COLLECTION.into(), query: query.to_owned(), limit: u32::try_from(k)? };
            Ok(match self.0.run(search)? {
                ResponseValue::FtsHits(hits) => hits.len(),
                other => return Err(format!("fts_search answered {}", other.name()).into()),
            })
        }
    }
}

#[cfg(feature = "sqlite")]
mod sqlite {
    use rusqlite::{params, Connection};

    use crate::engines::sqlite::{info, Sqlite};
    use crate::engines::Result;
    use crate::record::EngineInfo;

    /// FTS5 with its default tokenizer, ranked by `bm25`.
    pub struct Index(Connection);

    impl Index {
        pub fn open(dir: &std::path::Path) -> Result<Self> {
            let conn = Sqlite::open("sqlite", dir)?.connect()?;
            conn.execute_batch("CREATE VIRTUAL TABLE IF NOT EXISTS corpus USING fts5(id UNINDEXED, body);")?;
            Ok(Self(conn))
        }
    }

    impl super::TextIndex for Index {
        fn info(&self) -> EngineInfo {
            let mut info = info("sqlite");
            info.name = "SQLite (WAL, FTS5)".into();
            info
        }

        fn index(&mut self, docs: &[(String, String)]) -> Result<()> {
            let tx = self.0.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            {
                let mut stmt = tx.prepare_cached("INSERT INTO corpus(id, body) VALUES(?1, ?2)")?;
                for (id, body) in docs {
                    stmt.execute(params![id, body])?;
                }
            }
            tx.commit()?;
            Ok(())
        }

        fn search(&mut self, query: &str, k: usize) -> Result<usize> {
            let mut stmt = self.0.prepare_cached("SELECT id FROM corpus WHERE corpus MATCH ?1 ORDER BY rank LIMIT ?2")?;
            let mut rows = stmt.query(params![query, i64::try_from(k)?])?;
            let mut count = 0;
            while rows.next()?.is_some() {
                count += 1;
            }
            Ok(count)
        }
    }
}
