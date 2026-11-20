#![allow(clippy::too_many_lines)]
//! Physical execution engine for `LiliaQL` multi-model queries.

use std::collections::{HashMap, HashSet};

use lilia_core::{Direction, GraphNode, Result, SortDirection, VectorMetric};
use serde_json::Value;

use super::plan::{LogicalQueryPlan, QuerySource};
use super::projection::ProjectionContext;
use super::types::{QueryPlanExplanation, QueryResult, QueryRow};
use crate::Database;

/// Internal candidate item tracking state during pipeline execution.
#[derive(Debug, Clone)]
struct Candidate {
    source_id: String,
    source_doc: Option<Value>,
    graph_source_id: Option<String>,
    graph_target_id: Option<String>,
    graph_target_node: Option<GraphNode>,
    graph_target_doc: Option<Value>,
    graph_edge_kind: Option<String>,
    graph_edge_weight: Option<f64>,
    fts_rank: Option<usize>,
    fts_snippet: Option<String>,
    fts_score: Option<f64>,
    vector_rank: Option<usize>,
    vector_distance: Option<f64>,
    vector_score: Option<f64>,
    rrf_score: Option<f64>,
}

impl Candidate {
    fn primary_target_id(&self) -> &str {
        self.graph_target_id.as_deref().unwrap_or(&self.source_id)
    }

    fn to_projection_context(&self) -> ProjectionContext<'_> {
        ProjectionContext {
            source_id: Some(&self.source_id),
            source_doc: self.source_doc.as_ref(),
            graph_source_id: self.graph_source_id.as_deref(),
            graph_target_id: self.graph_target_id.as_deref(),
            graph_target_node: self.graph_target_node.as_ref(),
            graph_target_doc: self.graph_target_doc.as_ref(),
            graph_edge_kind: self.graph_edge_kind.as_deref(),
            graph_edge_weight: self.graph_edge_weight,
            fts_rank: self.fts_rank,
            fts_snippet: self.fts_snippet.as_deref(),
            fts_score: self.fts_score,
            vector_rank: self.vector_rank,
            vector_distance: self.vector_distance,
            vector_score: self.vector_score,
            rrf_score: self.rrf_score,
        }
    }
}

/// Executes the given logical plan against the database instance.
///
/// # Errors
/// Returns [`LiliaError`] if storage or query operations fail.
pub fn execute_plan(db: &Database, plan: LogicalQueryPlan) -> Result<QueryResult> {
    // -------------------------------------------------------------
    // Step 1: Base Source Ingestion & Filter Pushdown
    // -------------------------------------------------------------
    let mut candidates = Vec::new();

    match &plan.source {
        QuerySource::Json { space } => {
            let entries = if let Some(ref fe) = plan.filter {
                if let Value::Object(map) = fe.as_value() {
                    // Check if map contains simple key-values suitable for index lookup
                    let is_simple = map.values().all(|v| !v.is_object() && !v.is_array());
                    if is_simple && !map.is_empty() {
                        let (found, _) = db.json_find_with_trace(space, map, None, u32::MAX)?;
                        found
                    } else {
                        db.json_scan(space, None, u32::MAX)?
                    }
                } else {
                    db.json_scan(space, None, u32::MAX)?
                }
            } else {
                db.json_scan(space, None, u32::MAX)?
            };

            for entry in entries {
                let matches = if let Some(ref fe) = plan.filter {
                    fe.matches_binary(&entry.doc)
                } else {
                    true
                };
                if matches {
                    let doc_val = entry.value();
                    candidates.push(Candidate {
                        source_id: entry.id.clone(),
                        source_doc: Some(doc_val),
                        graph_source_id: None,
                        graph_target_id: None,
                        graph_target_node: None,
                        graph_target_doc: None,
                        graph_edge_kind: None,
                        graph_edge_weight: None,
                        fts_rank: None,
                        fts_snippet: None,
                        fts_score: None,
                        vector_rank: None,
                        vector_distance: None,
                        vector_score: None,
                        rrf_score: None,
                    });
                }
            }
        }
        QuerySource::Graph { graph } => {
            let nodes = db.graph_list_nodes(graph, None, u32::MAX)?;
            for node in nodes {
                let doc = if let Some(ref sp) = node.doc_space {
                    let doc_id = node.doc_id.as_deref().unwrap_or(&node.id);
                    db.json_get(sp, doc_id)?.map(|e| e.value())
                } else {
                    None
                };
                let id = node.id.clone();
                candidates.push(Candidate {
                    source_id: id.clone(),
                    source_doc: doc.clone(),
                    graph_source_id: Some(id.clone()),
                    graph_target_id: Some(id),
                    graph_target_node: Some(node),
                    graph_target_doc: doc,
                    graph_edge_kind: None,
                    graph_edge_weight: None,
                    fts_rank: None,
                    fts_snippet: None,
                    fts_score: None,
                    vector_rank: None,
                    vector_distance: None,
                    vector_score: None,
                    rrf_score: None,
                });
            }
        }
        QuerySource::Fts { collection, query } => {
            let hits = db.fts_search(collection, query, u32::MAX)?;
            for (idx, hit) in hits.into_iter().enumerate() {
                candidates.push(Candidate {
                    source_id: hit.doc_id.clone(),
                    source_doc: None,
                    graph_source_id: None,
                    graph_target_id: Some(hit.doc_id),
                    graph_target_node: None,
                    graph_target_doc: None,
                    graph_edge_kind: None,
                    graph_edge_weight: None,
                    fts_rank: Some(idx + 1),
                    fts_snippet: Some(hit.snippet),
                    fts_score: Some(hit.rank),
                    vector_rank: None,
                    vector_distance: None,
                    vector_score: None,
                    rrf_score: None,
                });
            }
        }
        QuerySource::Vector {
            collection,
            vector,
            dim,
            element_type,
        } => {
            let hits = db.vector_search(
                collection,
                vector,
                *dim,
                *element_type,
                VectorMetric::Cosine,
                u32::MAX,
            )?;
            for (idx, hit) in hits.into_iter().enumerate() {
                let sim = 1.0 - hit.distance;
                candidates.push(Candidate {
                    source_id: hit.id.clone(),
                    source_doc: None,
                    graph_source_id: None,
                    graph_target_id: Some(hit.id),
                    graph_target_node: None,
                    graph_target_doc: None,
                    graph_edge_kind: None,
                    graph_edge_weight: None,
                    fts_rank: None,
                    fts_snippet: None,
                    fts_score: None,
                    vector_rank: Some(idx + 1),
                    vector_distance: Some(hit.distance),
                    vector_score: Some(sim),
                    rrf_score: None,
                });
            }
        }
        QuerySource::Kv { namespace } => {
            let entries = db.kv_scan(namespace, None, u32::MAX)?;
            for entry in entries {
                let id = String::from_utf8_lossy(&entry.key).to_string();
                let doc = serde_json::from_slice::<Value>(&entry.value).ok();
                candidates.push(Candidate {
                    source_id: id,
                    source_doc: doc,
                    graph_source_id: None,
                    graph_target_id: None,
                    graph_target_node: None,
                    graph_target_doc: None,
                    graph_edge_kind: None,
                    graph_edge_weight: None,
                    fts_rank: None,
                    fts_snippet: None,
                    fts_score: None,
                    vector_rank: None,
                    vector_distance: None,
                    vector_score: None,
                    rrf_score: None,
                });
            }
        }
        QuerySource::Sql { query } => {
            let res = db.sql_query(query, &[])?;
            for row in res.rows {
                let mut map = serde_json::Map::new();
                for (col, val) in res.columns.iter().zip(row) {
                    map.insert(col.clone(), val);
                }
                let val = Value::Object(map);
                let id = val
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                candidates.push(Candidate {
                    source_id: id,
                    source_doc: Some(val),
                    graph_source_id: None,
                    graph_target_id: None,
                    graph_target_node: None,
                    graph_target_doc: None,
                    graph_edge_kind: None,
                    graph_edge_weight: None,
                    fts_rank: None,
                    fts_snippet: None,
                    fts_score: None,
                    vector_rank: None,
                    vector_distance: None,
                    vector_score: None,
                    rrf_score: None,
                });
            }
        }
    }

    if candidates.is_empty() {
        return Ok(QueryResult::new(Vec::new()));
    }

    // -------------------------------------------------------------
    // Step 2: Graph Traversal Join (Inner Join)
    // -------------------------------------------------------------
    if let Some(ref gstage) = plan.graph_traversal {
        let conn = db.read_lock()?;
        let mut traversed_candidates = Vec::new();

        for cand in candidates {
            let source_id = cand.source_id.clone();

            // Query edges from database
            let (sql, bind_graph) = match (gstage.direction, gstage.graph.as_deref()) {
                (Direction::Outbound, Some(_)) => (
                    "SELECT graph, source, target, kind, weight FROM _lilia_graph_edges \
                     WHERE graph = ?1 AND source = ?2 AND kind = ?3 ORDER BY target;",
                    true,
                ),
                (Direction::Outbound, None) => (
                    "SELECT graph, source, target, kind, weight FROM _lilia_graph_edges \
                     WHERE source = ?1 AND kind = ?2 ORDER BY target;",
                    false,
                ),
                (Direction::Inbound, Some(_)) => (
                    "SELECT graph, source, target, kind, weight FROM _lilia_graph_edges \
                     WHERE graph = ?1 AND target = ?2 AND kind = ?3 ORDER BY source;",
                    true,
                ),
                (Direction::Inbound, None) => (
                    "SELECT graph, source, target, kind, weight FROM _lilia_graph_edges \
                     WHERE target = ?1 AND kind = ?2 ORDER BY source;",
                    false,
                ),
                (Direction::Both, Some(_)) => (
                    "SELECT graph, source, target, kind, weight FROM _lilia_graph_edges \
                     WHERE graph = ?1 AND (source = ?2 OR target = ?2) AND kind = ?3 ORDER BY target;",
                    true,
                ),
                (Direction::Both, None) => (
                    "SELECT graph, source, target, kind, weight FROM _lilia_graph_edges \
                     WHERE (source = ?1 OR target = ?1) AND kind = ?2 ORDER BY target;",
                    false,
                ),
            };

            let mut stmt = conn.prepare(sql)?;
            if bind_graph {
                stmt.bind_text_static(1, gstage.graph.as_deref().unwrap_or(""))?;
                stmt.bind_text_static(2, &source_id)?;
                stmt.bind_text_static(3, &gstage.kind)?;
            } else {
                stmt.bind_text_static(1, &source_id)?;
                stmt.bind_text_static(2, &gstage.kind)?;
            }

            struct EdgeRow {
                graph: String,
                source: String,
                target: String,
                kind: String,
                weight: f64,
            }

            let mut edges = Vec::new();
            while stmt.step()? {
                edges.push(EdgeRow {
                    graph: stmt.column_text(0).to_string(),
                    source: stmt.column_text(1).to_string(),
                    target: stmt.column_text(2).to_string(),
                    kind: stmt.column_text(3).to_string(),
                    weight: stmt.column_double(4),
                });
            }
            drop(stmt);

            for edge in edges {
                let target_id = if gstage.direction == Direction::Inbound {
                    edge.source.clone()
                } else if edge.source == source_id {
                    edge.target.clone()
                } else {
                    edge.source.clone()
                };

                // Look up target node metadata
                let mut node_stmt = conn.prepare(
                    "SELECT kind, metadata, doc_space, doc_id FROM _lilia_graph_nodes WHERE graph = ?1 AND id = ?2;",
                )?;
                node_stmt.bind_text_static(1, &edge.graph)?;
                node_stmt.bind_text_static(2, &target_id)?;

                let target_node = if node_stmt.step()? {
                    let n_kind = node_stmt.column_text(0).to_string();
                    let n_meta: Value =
                        serde_json::from_str(node_stmt.column_text(1)).unwrap_or(Value::Null);
                    let doc_space = if node_stmt.column_type(2) == 5 {
                        None
                    } else {
                        Some(node_stmt.column_text(2).to_string())
                    };
                    let doc_id = if node_stmt.column_type(3) == 5 {
                        None
                    } else {
                        Some(node_stmt.column_text(3).to_string())
                    };
                    Some(GraphNode {
                        graph: edge.graph.clone(),
                        id: target_id.clone(),
                        kind: n_kind,
                        metadata: n_meta,
                        doc_space,
                        doc_id,
                    })
                } else {
                    None
                };
                drop(node_stmt);

                // Resolve target document if linked or in source space
                let target_doc = if let Some(ref tn) = target_node {
                    if let Some(ref d_space) = tn.doc_space {
                        let d_id = tn.doc_id.as_deref().unwrap_or(&target_id);
                        db.json_get(d_space, d_id)?.map(|e| e.value())
                    } else if let QuerySource::Json { ref space } = plan.source {
                        db.json_get(space, &target_id)?.map(|e| e.value())
                    } else {
                        None
                    }
                } else if let QuerySource::Json { ref space } = plan.source {
                    db.json_get(space, &target_id)?.map(|e| e.value())
                } else {
                    None
                };

                let mut new_cand = cand.clone();
                new_cand.graph_source_id = Some(edge.source);
                new_cand.graph_target_id = Some(target_id);
                new_cand.graph_target_node = target_node;
                new_cand.graph_target_doc = target_doc;
                new_cand.graph_edge_kind = Some(edge.kind);
                new_cand.graph_edge_weight = Some(edge.weight);
                traversed_candidates.push(new_cand);
            }
        }
        drop(conn);

        candidates = traversed_candidates;
    }

    if candidates.is_empty() {
        return Ok(QueryResult::new(Vec::new()));
    }

    // -------------------------------------------------------------
    // Step 3: Lexical FTS Search Join (Inner Join)
    // -------------------------------------------------------------
    if let Some(ref fts_stage) = plan.fts_search {
        let fts_hits = db.fts_search(&fts_stage.collection, &fts_stage.query, u32::MAX)?;
        let mut hit_map: HashMap<String, (usize, String, f64)> = HashMap::new();
        for (idx, hit) in fts_hits.into_iter().enumerate() {
            hit_map.insert(hit.doc_id, (idx + 1, hit.snippet, hit.rank));
        }

        candidates.retain_mut(|cand| {
            let target_id = cand.primary_target_id();
            if let Some((rank, snippet, score)) = hit_map.get(target_id) {
                cand.fts_rank = Some(*rank);
                cand.fts_snippet = Some(snippet.clone());
                cand.fts_score = Some(*score);
                true
            } else {
                false
            }
        });
    }

    if candidates.is_empty() {
        return Ok(QueryResult::new(Vec::new()));
    }

    // -------------------------------------------------------------
    // Step 4: Dense Vector Search Join (HNSW Pre-filtered, Inner Join)
    // -------------------------------------------------------------
    if let Some(ref vstage) = plan.vector_search {
        let target_id_set: HashSet<String> = candidates
            .iter()
            .map(|c| c.primary_target_id().to_string())
            .collect();
        let candidate_refs: Vec<&str> = target_id_set.iter().map(String::as_str).collect();

        let vec_hits = db.vector_search_filtered(
            &vstage.collection,
            &vstage.vector,
            vstage.dim,
            vstage.element_type,
            vstage.metric,
            &candidate_refs,
            u32::MAX,
        )?;

        let mut hit_map: HashMap<String, (usize, f64, f64)> = HashMap::new();
        let mut rank = 1;
        for hit in vec_hits {
            let (meets_threshold, similarity) = match vstage.metric {
                VectorMetric::Cosine => {
                    let sim = 1.0 - hit.distance;
                    (sim >= vstage.threshold - 1e-6, sim)
                }
                VectorMetric::L2 | VectorMetric::Hamming => (
                    hit.distance <= vstage.threshold + 1e-6,
                    1.0 / (1.0 + hit.distance),
                ),
                VectorMetric::Dot => (hit.distance >= vstage.threshold - 1e-6, hit.distance),
            };

            if meets_threshold {
                hit_map.insert(hit.id, (rank, hit.distance, similarity));
                rank += 1;
            }
        }

        candidates.retain_mut(|cand| {
            let target_id = cand.primary_target_id();
            if let Some((r, dist, sim)) = hit_map.get(target_id) {
                cand.vector_rank = Some(*r);
                cand.vector_distance = Some(*dist);
                cand.vector_score = Some(*sim);
                true
            } else {
                false
            }
        });
    }

    if candidates.is_empty() {
        return Ok(QueryResult::new(Vec::new()));
    }

    // -------------------------------------------------------------
    // Step 5: Reciprocal Rank Fusion (RRF) & Relevance Scoring
    // -------------------------------------------------------------
    let k = f64::from(plan.rrf.k);
    let fts_w = plan.rrf.fts_weight;
    let vec_w = plan.rrf.vector_weight;

    for cand in &mut candidates {
        let fts_contrib = cand.fts_rank.map_or(0.0, |r| {
            fts_w / (k + f64::from(u32::try_from(r).unwrap_or(u32::MAX)))
        });
        let vec_contrib = cand.vector_rank.map_or(0.0, |r| {
            vec_w / (k + f64::from(u32::try_from(r).unwrap_or(u32::MAX)))
        });

        let score = if cand.fts_rank.is_some() && cand.vector_rank.is_some() {
            fts_contrib + vec_contrib
        } else if let Some(sim) = cand.vector_score {
            sim
        } else {
            cand.fts_score.unwrap_or(0.0)
        };

        cand.rrf_score = Some(score);
    }

    // -------------------------------------------------------------
    // Step 6: Ordering & Deduplication
    // -------------------------------------------------------------
    if plan.distinct {
        let mut seen = HashSet::new();
        candidates.retain(|c| seen.insert(c.primary_target_id().to_string()));
    }

    if let Some((ref field, dir)) = plan.order_by {
        candidates.sort_by(|a, b| {
            let va = a.to_projection_context();
            let vb = b.to_projection_context();
            let row_a = plan.projection.evaluate(&va);
            let row_b = plan.projection.evaluate(&vb);
            let val_a = row_a.get(field);
            let val_b = row_b.get(field);
            let cmp = compare_values(val_a, val_b);
            if dir == SortDirection::Desc {
                cmp.reverse()
            } else {
                cmp
            }
        });
    } else if plan.fts_search.is_some() || plan.vector_search.is_some() {
        // Default ranking: highest RRF relevance score first
        candidates.sort_by(|a, b| {
            let sa = a.rrf_score.unwrap_or(0.0);
            let sb = b.rrf_score.unwrap_or(0.0);
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    // -------------------------------------------------------------
    // Step 7: Offset, Limit & Projection
    // -------------------------------------------------------------
    let offset = plan.offset.unwrap_or(0);
    let rows_iter = candidates.into_iter().skip(offset);
    let final_iter: Box<dyn Iterator<Item = Candidate>> = if let Some(lim) = plan.limit {
        Box::new(rows_iter.take(lim))
    } else {
        Box::new(rows_iter)
    };

    let rows: Vec<QueryRow> = final_iter
        .map(|c| {
            let ctx = c.to_projection_context();
            plan.projection.evaluate(&ctx)
        })
        .collect();

    Ok(QueryResult::new(rows))
}

/// Produces an explanation of the physical query execution plan.
#[must_use]
pub fn explain_plan(plan: &LogicalQueryPlan) -> QueryPlanExplanation {
    let mut stages = Vec::new();

    let src_desc = match &plan.source {
        QuerySource::Json { space } => format!("Source: JSON documents from space '{space}'"),
        QuerySource::Graph { graph } => format!("Source: Graph nodes from graph '{graph}'"),
        QuerySource::Fts { collection, query } => {
            format!("Source: FTS index '{collection}' query '{query}'")
        }
        QuerySource::Vector {
            collection, dim, ..
        } => {
            format!("Source: Vector collection '{collection}' (dim={dim})")
        }
        QuerySource::Kv { namespace } => format!("Source: KV namespace '{namespace}'"),
        QuerySource::Sql { query } => format!("Source: SQL query \"{query}\""),
    };
    stages.push(src_desc);

    if let Some(ref fe) = plan.filter {
        stages.push(format!("Filter: predicate expression {:?}", fe.as_value()));
    }

    if let Some(ref gstage) = plan.graph_traversal {
        stages.push(format!(
            "Traverse Graph: edge kind '{}' direction {:?} in graph {:?}",
            gstage.kind, gstage.direction, gstage.graph
        ));
    }

    if let Some(ref fstage) = plan.fts_search {
        stages.push(format!(
            "FTS Search: BM25 matching in collection '{}' with query '{}'",
            fstage.collection, fstage.query
        ));
    }

    let mut vector_prefilter = false;
    if let Some(ref vstage) = plan.vector_search {
        vector_prefilter = true;
        stages.push(format!(
            "Vector Search: HNSW pre-filtered in '{}' (threshold >= {}, metric={:?})",
            vstage.collection, vstage.threshold, vstage.metric
        ));
    }

    let rrf_active = plan.fts_search.is_some() && plan.vector_search.is_some();
    if rrf_active {
        stages.push(format!(
            "Fusion: Reciprocal Rank Fusion (k={}, fts_weight={}, vector_weight={})",
            plan.rrf.k, plan.rrf.fts_weight, plan.rrf.vector_weight
        ));
    }

    if !plan.projection.is_empty() {
        stages.push(format!(
            "Project: {} fields ({})",
            plan.projection.fields.len(),
            plan.projection
                .fields
                .iter()
                .map(|(k, v)| format!("{k} => {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    QueryPlanExplanation {
        stages,
        index_hint: None,
        vector_prefilter,
        rrf_active,
    }
}

fn compare_values(a: Option<&Value>, b: Option<&Value>) -> std::cmp::Ordering {
    match (a, b) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(va), Some(vb)) => match (va, vb) {
            (Value::Number(na), Value::Number(nb)) => {
                if let (Some(fa), Some(fb)) = (na.as_f64(), nb.as_f64()) {
                    fa.partial_cmp(&fb).unwrap_or(std::cmp::Ordering::Equal)
                } else {
                    std::cmp::Ordering::Equal
                }
            }
            (Value::String(sa), Value::String(sb)) => sa.cmp(sb),
            (Value::Bool(ba), Value::Bool(bb)) => ba.cmp(bb),
            _ => std::cmp::Ordering::Equal,
        },
    }
}
