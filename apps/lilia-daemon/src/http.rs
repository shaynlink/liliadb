//! The loopback HTTP gateway: JSON routes over the operations of the protocol, each run by
//! [`lilia_protocol::execute`] like an IPC request.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use lilia_core::{BatchOperation, ErrorCode, JsonEntry, KvEntry, LiliaError, VectorElementType};
use lilia_db::{Database, Vector};
use lilia_protocol::{Operation, ResponseValue};
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio::sync::Notify;

/// Most operations one batch request may carry.
const MAX_BATCH_OPERATIONS: usize = 10_000;

#[derive(Clone)]
pub struct HttpState {
    pub database: Arc<Database>,
    pub token: String,
    pub shutdown: Arc<Notify>,
}

type Failure = (StatusCode, Json<Value>);
type HttpResult = Result<Json<Value>, Failure>;

/// The inner value of a response of the expected variant; a failure otherwise.
macro_rules! answer {
    ($value:expr, $variant:path) => {
        match $value {
            $variant(inner) => inner,
            other => return Err(crate::http::unexpected(&other)),
        }
    };
}

mod batch;
mod documents;
mod models;
mod requests;

use batch::batch_handler;
use documents::{
    json_create_index_handler, json_delete_handler, json_drop_index_handler, json_find_handler,
    json_get_path_handler, json_get_post_handler, json_list_indexes_handler, json_put_handler,
    json_scan_by_index_handler, json_scan_handler, kv_delete_handler, kv_get_handler,
    kv_scan_handler, kv_set_handler,
};
use models::{
    fts_fuzzy_search_handler, fts_index_handler, fts_phonetic_handler, fts_search_handler,
    fts_suggest_handler, graph_edge_handler, graph_neighbors_handler, graph_node_handler,
    graph_reachability_handler, hybrid_search_handler, sql_handler, vector_get_handler,
    vector_search_handler, vector_set_handler,
};

#[allow(
    clippy::needless_pass_by_value,
    reason = "`map_err` hands the error over by value"
)]
fn map_error(error: LiliaError) -> Failure {
    let status = match error.code {
        ErrorCode::InvalidInput => StatusCode::BAD_REQUEST,
        ErrorCode::NotFound => StatusCode::NOT_FOUND,
        ErrorCode::Conflict => StatusCode::CONFLICT,
        ErrorCode::Timeout => StatusCode::REQUEST_TIMEOUT,
        ErrorCode::Unsupported => StatusCode::NOT_IMPLEMENTED,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(json!({
            "ok": false,
            "error": error
        })),
    )
}

fn unexpected(value: &ResponseValue) -> Failure {
    map_error(LiliaError::new(
        ErrorCode::Storage,
        format!("unexpected response: {}", value.name()),
        false,
    ))
}

/// Runs `operation` on a blocking worker.
async fn run(state: &HttpState, operation: Operation) -> Result<ResponseValue, Failure> {
    crate::run_blocking(Arc::clone(&state.database), operation, None)
        .await
        .map_err(map_error)
}

/// Commits `operations`; their results in order.
async fn commit(
    state: &HttpState,
    operations: Vec<BatchOperation>,
) -> Result<Vec<lilia_core::MutationResult>, Failure> {
    Ok(answer!(
        run(state, Operation::Batch { operations }).await?,
        ResponseValue::Mutations
    ))
}

fn invalid(message: String) -> Failure {
    map_error(LiliaError::new(ErrorCode::InvalidInput, message, false))
}

fn decode_bytes(input: &str, encoding: Option<&str>, field: &str) -> Result<Vec<u8>, Failure> {
    use base64::Engine;
    match encoding.unwrap_or("text") {
        "text" => Ok(input.as_bytes().to_vec()),
        "hex" => {
            hex::decode(input).map_err(|e| invalid(format!("invalid {field} hex encoding: {e}")))
        }
        "base64" => base64::engine::general_purpose::STANDARD
            .decode(input)
            .map_err(|e| invalid(format!("invalid {field} base64 encoding: {e}"))),
        other => Err(invalid(format!("unsupported encoding: {other}"))),
    }
}

fn encode_bytes(bytes: &[u8], encoding: Option<&str>) -> Value {
    use base64::Engine;
    match encoding.unwrap_or("text") {
        "hex" => Value::String(hex::encode(bytes)),
        "base64" => Value::String(base64::engine::general_purpose::STANDARD.encode(bytes)),
        _ => match std::str::from_utf8(bytes) {
            Ok(s) => Value::String(s.to_owned()),
            Err(_) => Value::String(base64::engine::general_purpose::STANDARD.encode(bytes)),
        },
    }
}

/// `values` as a vector of `element_type` (`f32` by default, quantized otherwise): its
/// dimension, element type and bytes.
fn vector_bytes(
    values: &[f32],
    element_type: Option<VectorElementType>,
) -> Result<(u32, VectorElementType, Vec<u8>), Failure> {
    let element_type = element_type.unwrap_or(VectorElementType::Float32);
    let vector = Vector::from_f32(values)
        .map_err(|e| map_error(e.into()))?
        .convert(element_type)
        .ok_or_else(|| invalid(format!("no conversion of f32 to {element_type:?}")))?;
    Ok((vector.dim(), element_type, vector.data().to_vec()))
}

fn kv_json(entry: &KvEntry, encoding: Option<&str>) -> Value {
    json!({
        "namespace": entry.namespace,
        "key": encode_bytes(&entry.key, encoding),
        "value": encode_bytes(&entry.value, encoding),
        "version": entry.version.to_string(),
        "expires_at_ms": entry.expires_at_ms
    })
}

fn document_json(document: &JsonEntry) -> Value {
    json!({
        "space": document.space,
        "id": document.id,
        "value": document.value(),
        "version": document.version.to_string()
    })
}

fn documents_json(documents: &[JsonEntry]) -> Json<Value> {
    let formatted: Vec<Value> = documents.iter().map(document_json).collect();
    Json(json!({
        "ok": true,
        "documents": formatted,
        "count": formatted.len()
    }))
}

fn written(results: &[lilia_core::MutationResult]) -> Json<Value> {
    Json(json!({
        "ok": true,
        "version": results.first().and_then(|r| r.version).map(|v| v.to_string())
    }))
}

fn deleted(results: &[lilia_core::MutationResult]) -> Json<Value> {
    Json(json!({
        "ok": true,
        "deleted": results.first().is_some_and(|r| r.deleted)
    }))
}

async fn auth_middleware(
    State(state): State<HttpState>,
    request: axum::extract::Request,
    next: Next,
) -> Result<Response, Failure> {
    let authenticated = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| token.as_bytes().ct_eq(state.token.as_bytes()).into());
    if !authenticated {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "ok": false,
                "error": {
                    "code": "AUTHENTICATION",
                    "message": "invalid or missing bearer token",
                    "retryable": false
                }
            })),
        ));
    }
    Ok(next.run(request).await)
}

async fn health_handler() -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "status": "running",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

async fn doctor_handler(State(state): State<HttpState>) -> HttpResult {
    let integrity = answer!(
        run(&state, Operation::IntegrityCheck).await?,
        ResponseValue::Bool
    );
    Ok(Json(json!({
        "ok": integrity,
        "integrity": integrity
    })))
}

async fn shutdown_handler(State(state): State<HttpState>) -> impl IntoResponse {
    state.shutdown.notify_one();
    Json(json!({
        "ok": true,
        "message": "daemon shutting down"
    }))
}

pub fn router(state: HttpState) -> Router {
    let public_routes = Router::new().route("/health", get(health_handler));
    let protected_routes = Router::new()
        .route("/v1/kv/get", post(kv_get_handler))
        .route("/v1/kv/set", post(kv_set_handler))
        .route("/v1/kv/delete", post(kv_delete_handler))
        .route("/v1/kv/scan", post(kv_scan_handler))
        .route("/v1/json/{space}/{id}", get(json_get_path_handler))
        .route("/v1/json/get", post(json_get_post_handler))
        .route("/v1/json/put", post(json_put_handler))
        .route("/v1/json/delete", post(json_delete_handler))
        .route("/v1/json/scan", post(json_scan_handler))
        .route("/v1/json/indexes/create", post(json_create_index_handler))
        .route("/v1/json/indexes/drop", post(json_drop_index_handler))
        .route("/v1/json/indexes/list", post(json_list_indexes_handler))
        .route("/v1/json/scan_by_index", post(json_scan_by_index_handler))
        .route("/v1/json/find", post(json_find_handler))
        .route("/v1/graph/node", post(graph_node_handler))
        .route("/v1/graph/edge", post(graph_edge_handler))
        .route("/v1/graph/neighbors", post(graph_neighbors_handler))
        .route("/v1/graph/reachability", post(graph_reachability_handler))
        .route("/v1/fts/index", post(fts_index_handler))
        .route("/v1/fts/search", post(fts_search_handler))
        .route("/v1/fts/suggest", post(fts_suggest_handler))
        .route("/v1/fts/fuzzy-search", post(fts_fuzzy_search_handler))
        .route("/v1/fts/phonetic", post(fts_phonetic_handler))
        .route("/v1/vector/set", post(vector_set_handler))
        .route("/v1/vector/get", post(vector_get_handler))
        .route("/v1/vector/search", post(vector_search_handler))
        .route("/v1/hybrid/search", post(hybrid_search_handler))
        .route("/v1/batch", post(batch_handler))
        .route("/v1/sql", post(sql_handler))
        .route("/v1/doctor", get(doctor_handler))
        .route("/v1/shutdown", post(shutdown_handler))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));
    public_routes.merge(protected_routes).with_state(state)
}

pub async fn serve_http(
    listener: tokio::net::TcpListener,
    database: Arc<Database>,
    token: String,
    shutdown: Arc<Notify>,
) -> std::io::Result<()> {
    let state = HttpState {
        database,
        token,
        shutdown: Arc::clone(&shutdown),
    };
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async move { shutdown.notified().await })
        .await
}

#[cfg(test)]
mod error_mapping_tests {
    use super::*;

    #[test]
    fn a_conflict_maps_to_409_and_keeps_its_details() {
        let error = LiliaError::new(ErrorCode::Conflict, "version conflict", true)
            .details(json!({ "expected": 3, "found": 5 }));
        let (status, Json(body)) = map_error(error);
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "CONFLICT");
        assert_eq!(body["error"]["details"]["found"], 5);
    }
}
