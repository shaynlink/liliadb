//! Routes of SQL, graphs, full-text search and vectors.

use axum::extract::State;
use axum::Json;
use lilia_core::{BatchOperation, VectorElementType, VectorMetric};
use lilia_protocol::{names, Operation, ResponseValue, StoredVector};
use serde_json::json;

use super::requests::{
    FtsFuzzySearchRequest, FtsIndexRequest, FtsPhoneticRequest, FtsSearchRequest,
    FtsSuggestRequest, GraphEdgeRequest, GraphNeighborsRequest, GraphNodeRequest,
    GraphReachabilityRequest, HybridSearchRequest, SqlRequest, VectorGetRequest,
    VectorSearchRequest, VectorSetRequest,
};
use super::{commit, map_error, run, vector_bytes, HttpResult, HttpState};

pub(super) async fn sql_handler(
    State(state): State<HttpState>,
    Json(payload): Json<SqlRequest>,
) -> HttpResult {
    let operation = Operation::SqlQuery {
        sql: payload.query,
        params: payload.params,
    };
    let result = answer!(run(&state, operation).await?, ResponseValue::Sql);
    Ok(Json(json!({
        "ok": true,
        "columns": result.columns,
        "rows": result.rows,
        "rows_affected": result.rows_affected
    })))
}

pub(super) async fn graph_node_handler(
    State(state): State<HttpState>,
    Json(payload): Json<GraphNodeRequest>,
) -> HttpResult {
    let operation = BatchOperation::GraphSetNode {
        graph: payload.graph,
        id: payload.id,
        kind: payload.kind,
        metadata: payload.metadata.unwrap_or_else(|| json!({})),
        doc_space: None,
        doc_id: None,
    };
    commit(&state, vec![operation]).await?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn graph_edge_handler(
    State(state): State<HttpState>,
    Json(payload): Json<GraphEdgeRequest>,
) -> HttpResult {
    let operation = BatchOperation::GraphAddEdge {
        graph: payload.graph,
        source: payload.source,
        target: payload.target,
        kind: payload.kind,
        weight: payload.weight.unwrap_or(1.0),
    };
    commit(&state, vec![operation]).await?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn graph_neighbors_handler(
    State(state): State<HttpState>,
    Json(payload): Json<GraphNeighborsRequest>,
) -> HttpResult {
    let operation = Operation::GraphNeighbors {
        graph: payload.graph,
        node: payload.node,
        direction: names::direction(payload.direction.as_deref()).map_err(map_error)?,
        kind: payload.kind,
    };
    let neighbors = answer!(run(&state, operation).await?, ResponseValue::Ids);
    Ok(Json(json!({ "ok": true, "neighbors": neighbors })))
}

pub(super) async fn graph_reachability_handler(
    State(state): State<HttpState>,
    Json(payload): Json<GraphReachabilityRequest>,
) -> HttpResult {
    let operation = Operation::GraphReachability {
        graph: payload.graph,
        start_nodes: payload.start_nodes,
        direction: names::direction(payload.direction.as_deref()).map_err(map_error)?,
    };
    let reachable = answer!(run(&state, operation).await?, ResponseValue::Ids);
    Ok(Json(json!({ "ok": true, "reachable": reachable })))
}

pub(super) async fn fts_index_handler(
    State(state): State<HttpState>,
    Json(payload): Json<FtsIndexRequest>,
) -> HttpResult {
    let operation = BatchOperation::FtsIndex {
        collection: payload.collection,
        doc_id: payload.doc_id,
        content: payload.content,
    };
    commit(&state, vec![operation]).await?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn fts_search_handler(
    State(state): State<HttpState>,
    Json(payload): Json<FtsSearchRequest>,
) -> HttpResult {
    let operation = Operation::FtsSearch {
        collection: payload.collection,
        query: payload.query,
        limit: payload.limit.unwrap_or(10),
    };
    let hits = answer!(run(&state, operation).await?, ResponseValue::FtsHits);
    Ok(Json(json!({ "ok": true, "hits": hits })))
}

pub(super) async fn fts_suggest_handler(
    State(state): State<HttpState>,
    Json(payload): Json<FtsSuggestRequest>,
) -> HttpResult {
    let operation = Operation::FtsSuggest {
        collection: payload.collection,
        prefix: payload.prefix,
        max_distance: payload.max_distance.unwrap_or(1),
        limit: payload.limit.unwrap_or(10),
    };
    let suggestions = answer!(run(&state, operation).await?, ResponseValue::FtsSuggestions);
    Ok(Json(json!({ "ok": true, "suggestions": suggestions })))
}

pub(super) async fn fts_fuzzy_search_handler(
    State(state): State<HttpState>,
    Json(payload): Json<FtsFuzzySearchRequest>,
) -> HttpResult {
    let operation = Operation::FtsFuzzySearch {
        collection: payload.collection,
        query: payload.query,
        max_distance: payload.max_distance.unwrap_or(1),
        limit: payload.limit.unwrap_or(10),
    };
    let hits = answer!(run(&state, operation).await?, ResponseValue::FtsFuzzyHits);
    Ok(Json(json!({ "ok": true, "hits": hits })))
}

pub(super) async fn fts_phonetic_handler(
    State(state): State<HttpState>,
    Json(payload): Json<FtsPhoneticRequest>,
) -> HttpResult {
    let operation = Operation::FtsPhoneticSearch {
        collection: payload.collection,
        query: payload.query,
        limit: payload.limit.unwrap_or(10),
    };
    let hits = answer!(
        run(&state, operation).await?,
        ResponseValue::FtsPhoneticHits
    );
    Ok(Json(json!({ "ok": true, "hits": hits })))
}

pub(super) async fn vector_set_handler(
    State(state): State<HttpState>,
    Json(payload): Json<VectorSetRequest>,
) -> HttpResult {
    let (dim, element_type, vector) = vector_bytes(&payload.vector, payload.element_type)?;
    let operation = BatchOperation::VectorSet {
        collection: payload.collection,
        id: payload.id,
        dim,
        element_type,
        vector,
    };
    commit(&state, vec![operation]).await?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn vector_get_handler(
    State(state): State<HttpState>,
    Json(payload): Json<VectorGetRequest>,
) -> HttpResult {
    let operation = Operation::VectorGet {
        collection: payload.collection,
        id: payload.id,
    };
    let Some(StoredVector {
        dim,
        element_type,
        data: bytes,
    }) = answer!(run(&state, operation).await?, ResponseValue::Vector)
    else {
        return Ok(Json(json!({ "ok": true, "found": false })));
    };
    let values: Vec<f32> = match element_type {
        VectorElementType::Float32 => bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect(),
        VectorElementType::Int8 => bytes.iter().map(|&x| f32::from(x.cast_signed())).collect(),
        VectorElementType::Bit => bytes.iter().map(|&x| f32::from(x)).collect(),
    };
    Ok(Json(json!({
        "ok": true,
        "found": true,
        "dim": dim,
        "element_type": element_type,
        "vector": values
    })))
}

pub(super) async fn vector_search_handler(
    State(state): State<HttpState>,
    Json(payload): Json<VectorSearchRequest>,
) -> HttpResult {
    let (dim, element_type, vector) = vector_bytes(&payload.vector, payload.element_type)?;
    let operation = Operation::VectorSearch {
        collection: payload.collection,
        vector,
        dim,
        element_type,
        metric: payload.metric.unwrap_or(VectorMetric::Cosine),
        limit: payload.limit.unwrap_or(10),
    };
    let hits = answer!(run(&state, operation).await?, ResponseValue::VectorHits);
    Ok(Json(json!({ "ok": true, "hits": hits })))
}

pub(super) async fn hybrid_search_handler(
    State(state): State<HttpState>,
    Json(payload): Json<HybridSearchRequest>,
) -> HttpResult {
    let query = payload
        .vector
        .as_deref()
        .map(|values| vector_bytes(values, payload.element_type))
        .transpose()?;
    let operation = Operation::HybridSearch {
        collection: payload.collection,
        fts_query: payload.fts_query,
        dim: query.as_ref().map(|(dim, _, _)| *dim),
        element_type: query.as_ref().map(|(_, element_type, _)| *element_type),
        vector: query.map(|(_, _, bytes)| bytes),
        metric: payload.metric,
        rrf_k: payload.rrf_k.unwrap_or(60),
        fts_weight: payload.fts_weight.unwrap_or(1.0),
        vector_weight: payload.vector_weight.unwrap_or(1.0),
        limit: payload.limit.unwrap_or(10),
    };
    let hits = answer!(run(&state, operation).await?, ResponseValue::HybridHits);
    Ok(Json(json!({ "ok": true, "hits": hits })))
}
