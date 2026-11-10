#![allow(
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::collections::HashMap;

use lilia_core::{Direction, HybridHit, Result, VectorElementType, VectorHit, VectorMetric};

use crate::storage::{normalize_limit, validate_name};
use crate::Database;

impl Database {
    /// Performs Reciprocal Rank Fusion (RRF) combining full-text search (BM25) and dense vector search (KNN).
    ///
    /// Score formula:
    /// `RRF_Score(d) = (fts_weight / (k + rank_fts)) + (vector_weight / (k + rank_vec))`
    ///
    /// The industry standard smoothing factor `rrf_k` defaults to `60`.
    #[allow(clippy::too_many_arguments)]
    pub fn hybrid_search(
        &self,
        collection: &str,
        fts_query: Option<&str>,
        vector_query: Option<(&[u8], u32, VectorElementType, VectorMetric)>,
        rrf_k: u32,
        fts_weight: f64,
        vector_weight: f64,
        limit: u32,
    ) -> Result<Vec<HybridHit>> {
        validate_name("collection", collection)?;
        let k = if rrf_k == 0 { 60.0 } else { f64::from(rrf_k) };
        let candidate_limit = normalize_limit(limit).saturating_mul(2) as u32;

        struct Accumulator {
            score: f64,
            fts_rank: Option<usize>,
            fts_snippet: Option<String>,
            vector_rank: Option<usize>,
            vector_distance: Option<f64>,
        }

        let mut map: HashMap<String, Accumulator> = HashMap::new();

        // 1. Run Lexical BM25 FTS if requested
        if let Some(query) = fts_query {
            if !query.trim().is_empty() {
                let fts_hits = self.fts_search(collection, query, candidate_limit)?;
                for (idx, hit) in fts_hits.into_iter().enumerate() {
                    let rank_1based = idx + 1;
                    let rrf_contrib = fts_weight / (k + rank_1based as f64);
                    let entry = map.entry(hit.doc_id).or_insert_with(|| Accumulator {
                        score: 0.0,
                        fts_rank: None,
                        fts_snippet: None,
                        vector_rank: None,
                        vector_distance: None,
                    });
                    entry.score += rrf_contrib;
                    entry.fts_rank = Some(rank_1based);
                    entry.fts_snippet = Some(hit.snippet);
                }
            }
        }

        // 2. Run Dense KNN Vector Search if requested
        if let Some((vec_bytes, dim, elem_type, metric)) = vector_query {
            let vec_hits = self.vector_search(
                collection,
                vec_bytes,
                dim,
                elem_type,
                metric,
                candidate_limit,
            )?;
            for (idx, hit) in vec_hits.into_iter().enumerate() {
                let rank_1based = idx + 1;
                let rrf_contrib = vector_weight / (k + rank_1based as f64);
                let entry = map.entry(hit.id).or_insert_with(|| Accumulator {
                    score: 0.0,
                    fts_rank: None,
                    fts_snippet: None,
                    vector_rank: None,
                    vector_distance: None,
                });
                entry.score += rrf_contrib;
                entry.vector_rank = Some(rank_1based);
                entry.vector_distance = Some(hit.distance);
            }
        }

        let mut results: Vec<HybridHit> = map
            .into_iter()
            .map(|(id, acc)| HybridHit {
                id,
                rrf_score: acc.score,
                fts_rank: acc.fts_rank,
                fts_snippet: acc.fts_snippet,
                vector_rank: acc.vector_rank,
                vector_distance: acc.vector_distance,
            })
            .collect();

        // Sort descending by RRF score (highest score first)
        results.sort_by(|a, b| {
            b.rrf_score
                .partial_cmp(&a.rrf_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let norm_limit = normalize_limit(limit) as usize;
        if results.len() > norm_limit {
            results.truncate(norm_limit);
        }
        Ok(results)
    }

    /// Performs a semantic vector search restricted to neighboring nodes in a graph.
    ///
    /// Ideal for Knowledge Graph RAG: traverses reachable nodes from `start_node`
    /// and finds the most semantically relevant entities within that subgraph.
    #[allow(clippy::too_many_arguments)]
    pub fn vector_search_in_graph_neighbors(
        &self,
        collection: &str,
        graph: &str,
        start_node: &str,
        direction: Direction,
        query_vector: &[u8],
        dim: u32,
        element_type: VectorElementType,
        metric: VectorMetric,
        limit: u32,
    ) -> Result<Vec<VectorHit>> {
        validate_name("collection", collection)?;
        validate_name("graph", graph)?;
        let start_nodes = vec![start_node.to_string()];
        let reachable = self.graph_reachability(graph, &start_nodes, direction)?;
        let candidate_refs: Vec<&str> = reachable.iter().map(String::as_str).collect();
        self.vector_search_filtered(
            collection,
            query_vector,
            dim,
            element_type,
            metric,
            &candidate_refs,
            limit,
        )
    }

    /// Performs a semantic vector search restricted to documents matching a JSON secondary index value.
    ///
    /// Ideal for fast tenant-partitioned or category-filtered vector retrieval.
    #[allow(clippy::too_many_arguments)]
    pub fn vector_search_by_json_index(
        &self,
        collection: &str,
        space: &str,
        index_name: &str,
        index_value: &str,
        query_vector: &[u8],
        dim: u32,
        element_type: VectorElementType,
        metric: VectorMetric,
        limit: u32,
    ) -> Result<Vec<VectorHit>> {
        validate_name("collection", collection)?;
        validate_name("space", space)?;
        validate_name("index_name", index_name)?;

        let conn = self.read_lock()?;
        let mut stmt = conn.prepare(
            "SELECT doc_id FROM _lilia_json_index_entries \
             WHERE space = ?1 AND index_name = ?2 AND idx_value = ?3;",
        )?;
        stmt.bind_text_static(1, space)?;
        stmt.bind_text_static(2, index_name)?;
        stmt.bind_text_static(3, index_value)?;

        let mut candidate_ids = Vec::new();
        while stmt.step()? {
            candidate_ids.push(stmt.column_text(0).to_string());
        }
        drop(stmt);
        drop(conn);

        let candidate_refs: Vec<&str> = candidate_ids.iter().map(String::as_str).collect();
        self.vector_search_filtered(
            collection,
            query_vector,
            dim,
            element_type,
            metric,
            &candidate_refs,
            limit,
        )
    }
}
