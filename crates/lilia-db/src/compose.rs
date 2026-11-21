//! Searches composed from several models, in a session or transaction: a vector search
//! restricted to a part of a graph or to the documents an index selects, and a hybrid search
//! fusing full-text and vector rankings. Each part reads through the caller's policies.

use std::collections::HashMap;

use lilia_core::{Direction, HybridHit, VectorHit, VectorMetric};
use serde_json::{Map, Value};

use crate::context::{guard, Context};
use crate::error::Result;
use crate::fts::Fts;
use crate::graph::Graph;
use crate::keys::validate_name;
use crate::read::{self, Scan};
use crate::vectors::{Vector, Vectors};

/// The `limit` vectors of `collection` nearest to `query`, among the nodes reachable from
/// `start` in `direction` in graph `graph` (`start` included): a search over a neighbourhood of
/// a knowledge graph.
///
/// # Errors
///
/// Invalid names; as [`Vectors::search_within`].
#[allow(clippy::too_many_arguments)] // reason: the graph part and the vector part each need their own.
pub fn vectors_in_graph<C: Context>(
    context: &C,
    collection: &str,
    (graph, start, direction): (&str, &str, Direction),
    query: &Vector,
    metric: VectorMetric,
    limit: usize,
) -> Result<Vec<VectorHit>> {
    let reachable = Graph::new(context, graph).reachable(&[start], direction, None)?;
    let ids: Vec<&str> = reachable.iter().map(String::as_str).collect();
    Vectors::new(context, collection).search_within(query, metric, &ids, limit)
}

/// The `limit` vectors of `collection` nearest to `query`, among the ids of the documents of
/// `space` whose index `index` holds `value`.
///
/// # Errors
///
/// Invalid names, an unknown index; as [`Vectors::search_within`].
pub fn vectors_by_index<C: Context>(
    context: &C,
    collection: &str,
    (space, index, value): (&str, &str, &str),
    query: &Vector,
    metric: VectorMetric,
    limit: usize,
) -> Result<Vec<VectorHit>> {
    validate_name("space", space)?;
    let mut terms = Map::new();
    terms.insert(index.to_owned(), Value::String(value.to_owned()));
    let schema = context.database().schema(space);
    let documents = read::doc_find(
        &context.reader(true),
        &guard(context, space),
        schema.as_deref(),
        space,
        &terms,
        &Scan::all(),
    )?;
    let ids: Vec<&str> = documents.iter().map(|d| d.id.as_str()).collect();
    Vectors::new(context, collection).search_within(query, metric, &ids, limit)
}

/// How [`hybrid`] fuses its rankings: reciprocal rank fusion, a document scoring
/// `weight / (k + rank)` in each ranking it appears in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fusion {
    /// Smoothing of the ranks; 0 means the usual 60.
    pub k: u32,
    pub fts_weight: f64,
    pub vector_weight: f64,
}

impl Default for Fusion {
    fn default() -> Self {
        Self {
            k: 60,
            fts_weight: 1.0,
            vector_weight: 1.0,
        }
    }
}

#[derive(Default)]
struct Fused {
    score: f64,
    fts_rank: Option<usize>,
    fts_snippet: Option<String>,
    vector_rank: Option<usize>,
    vector_distance: Option<f64>,
}

/// The `limit` best documents of `collection` (a full-text collection and a vector collection
/// of the same name) for `text`, for `vector`, or for both, by reciprocal rank fusion of the
/// two rankings, each read to twice `limit`.
///
/// # Errors
///
/// Invalid names; as [`Fts::search`] and [`Vectors::search`].
pub fn hybrid<C: Context>(
    context: &C,
    collection: &str,
    text: Option<&str>,
    vector: Option<(&Vector, VectorMetric)>,
    fusion: Fusion,
    limit: usize,
) -> Result<Vec<HybridHit>> {
    validate_name("collection", collection)?;
    let k = if fusion.k == 0 {
        60.0
    } else {
        f64::from(fusion.k)
    };
    let candidates = limit.saturating_mul(2);
    // reason: ranks are below the candidate limit, far below 2^52.
    #[allow(clippy::cast_precision_loss)]
    let contribution = |weight: f64, rank: usize| weight / (k + rank as f64);
    let mut fused: HashMap<String, Fused> = HashMap::new();
    if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
        for (i, hit) in Fts::new(context, collection)
            .search(text, candidates)?
            .into_iter()
            .enumerate()
        {
            let entry = fused.entry(hit.doc_id).or_default();
            entry.score += contribution(fusion.fts_weight, i + 1);
            entry.fts_rank = Some(i + 1);
            entry.fts_snippet = Some(hit.snippet);
        }
    }
    if let Some((query, metric)) = vector {
        let hits = Vectors::new(context, collection).search(query, metric, candidates)?;
        for (i, hit) in hits.into_iter().enumerate() {
            let entry = fused.entry(hit.id).or_default();
            entry.score += contribution(fusion.vector_weight, i + 1);
            entry.vector_rank = Some(i + 1);
            entry.vector_distance = Some(hit.distance);
        }
    }
    let mut hits: Vec<HybridHit> = fused
        .into_iter()
        .map(|(id, f)| HybridHit {
            id,
            rrf_score: f.score,
            fts_rank: f.fts_rank,
            fts_snippet: f.fts_snippet,
            vector_rank: f.vector_rank,
            vector_distance: f.vector_distance,
        })
        .collect();
    hits.sort_by(|a, b| {
        b.rrf_score
            .total_cmp(&a.rrf_score)
            .then_with(|| a.id.cmp(&b.id))
    });
    hits.truncate(limit);
    Ok(hits)
}
