//! Vector search: HNSW index build through inserts, then top-10 cosine queries, with recall@10
//! measured against an exact scan. The exact scan is also reported, as the reference any index
//! must beat.

use std::time::Instant;

use serde_json::json;

use super::{fresh_dir, nanos_since, Meter, Settings};
use crate::engines::Result;
use crate::keys::Rng;
use crate::os;
use crate::record::{params, EngineInfo, Line, SyncPrimitive};

const TOP_K: usize = 10;
const CLUSTERS: usize = 64;
/// Queries whose exact answers are precomputed; the timed loop cycles through them.
const QUERIES: usize = 200;

pub trait VectorIndex {
    fn info(&self) -> EngineInfo;
    fn insert(&mut self, rows: &[(u64, &[f32])]) -> Result<()>;
    /// Identifiers of the `k` nearest rows by cosine distance, nearest first.
    fn search(&mut self, query: &[f32], k: usize) -> Result<Vec<u64>>;
}

fn open(engine: &str, dir: &std::path::Path, dim: usize) -> Result<Box<dyn VectorIndex>> {
    match engine {
        #[cfg(feature = "lilia")]
        "lilia" => Ok(Box::new(lilia::Index::open(dir, dim)?)),
        _ => Err(format!("engine `{engine}` has no vector index in this binary ({}, dim {dim})", dir.display()).into()),
    }
}

fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(f32::MIN_POSITIVE);
    v.iter_mut().for_each(|x| *x /= norm);
}

/// Clustered unit vectors: real embeddings are clustered, uniform random vectors are not.
fn dataset(n: usize, dim: usize) -> Vec<Vec<f32>> {
    let mut rng = Rng::new(0x7EC7);
    let centroids: Vec<Vec<f32>> = (0..CLUSTERS).map(|_| (0..dim).map(|_| rng.normal() as f32).collect()).collect();
    (0..n)
        .map(|_| {
            let c = &centroids[rng.below(CLUSTERS as u64) as usize];
            let mut v: Vec<f32> = c.iter().map(|x| x + 0.35 * rng.normal() as f32).collect();
            normalize(&mut v);
            v
        })
        .collect()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Exact top-`k` by cosine similarity (vectors are unit length).
fn exact(data: &[Vec<f32>], query: &[f32], k: usize) -> Vec<u64> {
    let mut scored: Vec<(f32, u64)> = data.iter().enumerate().map(|(i, v)| (dot(v, query), i as u64)).collect();
    let nth = k.min(scored.len()) - 1;
    scored.select_nth_unstable_by(nth, |a, b| b.0.total_cmp(&a.0));
    scored.truncate(k);
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.into_iter().map(|(_, i)| i).collect()
}

pub fn run(engine: &str, settings: &Settings, emit: &mut dyn FnMut(Line)) -> Result<()> {
    let n = usize::try_from(settings.vectors)?;
    let dim = settings.vector_dim;
    let data = dataset(n, dim);
    let mut rng = Rng::new(0x9E5);
    let queries: Vec<Vec<f32>> = (0..QUERIES)
        .map(|_| {
            let base = &data[rng.below(n as u64) as usize];
            let mut q: Vec<f32> = base.iter().map(|x| x + 0.1 * rng.normal() as f32).collect();
            normalize(&mut q);
            q
        })
        .collect();
    let truth: Vec<Vec<u64>> = queries.iter().map(|q| exact(&data, q, TOP_K)).collect();

    let dir = fresh_dir(settings)?;
    let mut index = open(engine, dir.path(), dim)?;
    let (mut latencies, mut done) = (Vec::new(), 0usize);
    let meter = Meter::start();
    while done < n {
        let end = (done + 100).min(n);
        let rows: Vec<(u64, &[f32])> = (done..end).map(|i| (i as u64, &data[i][..])).collect();
        let started = Instant::now();
        index.insert(&rows)?;
        latencies.push(nanos_since(started));
        done = end;
    }
    let shape = || params([("vectors", json!(n)), ("dim", json!(dim)), ("metric", json!("cosine"))]);
    let mut record = meter.finish(index.info(), "vector.insert", shape(), n as u64, (n * dim * 4) as u64, &mut latencies);
    record.disk_bytes = Some(os::dir_size(dir.path()));
    emit(Line::Record(Box::new(record)));

    let (record, recall) = search_loop(settings, &queries, &truth, |q| index.search(q, TOP_K))?;
    let mut record = record.finish_as(index.info(), shape());
    record.quality = Some(json!({ "recall_at_10": recall }));
    emit(Line::Record(Box::new(record)));

    let (record, recall) = search_loop(settings, &queries, &truth, |q| Ok(exact(&data, q, TOP_K)))?;
    let reference = EngineInfo {
        id: "exact-scan".into(),
        name: "Exact scan (in-process reference)".into(),
        version: "lilia-bench".into(),
        sync: SyncPrimitive::None,
    };
    let mut record = record.finish_as(reference, shape());
    record.quality = Some(json!({ "recall_at_10": recall }));
    emit(Line::Record(Box::new(record)));
    Ok(())
}

struct Searches {
    meter: Meter,
    latencies: Vec<u64>,
}

impl Searches {
    fn finish_as(mut self, info: EngineInfo, params: serde_json::Map<String, serde_json::Value>) -> crate::record::Record {
        let ops = self.latencies.len() as u64;
        self.meter.finish(info, "vector.search", params, ops, 0, &mut self.latencies)
    }
}

/// Cycles through the queries until the budget is spent; returns the timings and mean recall@10.
fn search_loop(
    settings: &Settings,
    queries: &[Vec<f32>],
    truth: &[Vec<u64>],
    mut search: impl FnMut(&[f32]) -> Result<Vec<u64>>,
) -> Result<(Searches, f64)> {
    let budget = settings.budget();
    let (mut latencies, mut hits, mut asked) = (Vec::new(), 0usize, 0usize);
    let meter = Meter::start();
    let mut q = 0;
    while meter.elapsed() < budget || q < queries.len() {
        let started = Instant::now();
        let found = search(&queries[q % queries.len()])?;
        latencies.push(nanos_since(started));
        if q < queries.len() {
            hits += found.iter().filter(|id| truth[q].contains(id)).count();
            asked += TOP_K;
        }
        q += 1;
    }
    Ok((Searches { meter, latencies }, hits as f64 / asked.max(1) as f64))
}

#[cfg(feature = "lilia")]
mod lilia {
    use lilia_core::{BatchOperation, HnswConfig, VectorElementType, VectorMetric};
    use lilia_protocol::{Operation, ResponseValue};

    use crate::engines::lilia_db::LiliaDb;
    use crate::engines::{Engine, Result};
    use crate::record::EngineInfo;

    const COLLECTION: &str = "embeddings";

    pub struct Index {
        db: LiliaDb,
        dim: u32,
    }

    impl Index {
        pub fn open(dir: &std::path::Path, dim: usize) -> Result<Self> {
            let db = LiliaDb::open("lilia", dir)?;
            let dim = u32::try_from(dim)?;
            db.run(Operation::VectorCreateIndex {
                collection: COLLECTION.into(),
                config: HnswConfig { dim, metric: VectorMetric::Cosine, element_type: VectorElementType::Float32, ..HnswConfig::default() },
            })?;
            Ok(Self { db, dim })
        }
    }

    fn bytes(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    impl super::VectorIndex for Index {
        fn info(&self) -> EngineInfo {
            let mut info = self.db.info();
            info.name = "LiliaDB (HNSW, M=16, ef_c=64)".into();
            info
        }

        fn insert(&mut self, rows: &[(u64, &[f32])]) -> Result<()> {
            let ops = rows
                .iter()
                .map(|(id, v)| BatchOperation::VectorSet {
                    collection: COLLECTION.into(),
                    id: format!("v{id}"),
                    dim: self.dim,
                    element_type: VectorElementType::Float32,
                    vector: bytes(v),
                })
                .collect();
            self.db.batch(ops)
        }

        fn search(&mut self, query: &[f32], k: usize) -> Result<Vec<u64>> {
            let search = Operation::VectorSearch {
                collection: COLLECTION.into(),
                vector: bytes(query),
                dim: self.dim,
                element_type: VectorElementType::Float32,
                metric: VectorMetric::Cosine,
                limit: u32::try_from(k)?,
            };
            let ResponseValue::VectorHits(hits) = self.db.run(search)? else {
                return Err("vector_search answered another response".into());
            };
            hits.iter()
                .map(|hit| hit.id.strip_prefix('v').and_then(|n| n.parse().ok()).ok_or_else(|| format!("unexpected vector id {}", hit.id).into()))
                .collect()
        }
    }
}
