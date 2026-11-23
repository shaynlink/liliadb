//! The loopback HTTP gateway: JSON routes over the operations of the protocol, each run by
//! [`lilia_protocol::execute`] like an IPC request.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use lilia_core::{
    BatchOperation, ErrorCode, JsonEntry, KvEntry, LiliaError, VectorElementType, VectorMetric,
};
use lilia_db::{Database, Vector};
use lilia_protocol::{names, Operation, ResponseValue, StoredVector};
use serde::Deserialize;
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
            other => return Err(unexpected(&other)),
        }
    };
}

#[derive(Debug, Deserialize)]
pub struct KvGetRequest {
    pub namespace: String,
    pub key: String,
    #[serde(default)]
    pub encoding: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KvSetRequest {
    pub namespace: String,
    pub key: String,
    pub value: String,
    #[serde(default)]
    pub if_version: Option<u64>,
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
    #[serde(default)]
    pub encoding: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KvDeleteRequest {
    pub namespace: String,
    pub key: String,
    #[serde(default)]
    pub if_version: Option<u64>,
    #[serde(default)]
    pub encoding: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KvScanRequest {
    pub namespace: String,
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub encoding: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct JsonGetRequest {
    pub space: String,
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct JsonPutRequest {
    pub space: String,
    pub id: String,
    pub value: Value,
    #[serde(default)]
    pub if_version: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct JsonDeleteRequest {
    pub space: String,
    pub id: String,
    #[serde(default)]
    pub if_version: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct JsonScanRequest {
    pub space: String,
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct JsonCreateIndexRequest {
    pub space: String,
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub filter: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct JsonDropIndexRequest {
    pub space: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct JsonListIndexesRequest {
    pub space: String,
}

#[derive(Debug, Deserialize)]
pub struct JsonScanByIndexRequest {
    pub space: String,
    pub index_name: String,
    pub value: String,
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct JsonFindRequest {
    pub space: String,
    pub query: serde_json::Map<String, Value>,
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct GraphNodeRequest {
    pub graph: String,
    pub id: String,
    #[serde(default = "default_node_kind")]
    pub kind: String,
    #[serde(default)]
    pub metadata: Option<Value>,
}

fn default_node_kind() -> String {
    "default".into()
}

#[derive(Debug, Deserialize)]
pub struct GraphEdgeRequest {
    pub graph: String,
    pub source: String,
    pub target: String,
    #[serde(default = "default_edge_kind")]
    pub kind: String,
    #[serde(default)]
    pub weight: Option<f64>,
}

fn default_edge_kind() -> String {
    "edge".into()
}

#[derive(Debug, Deserialize)]
pub struct GraphNeighborsRequest {
    pub graph: String,
    pub node: String,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GraphReachabilityRequest {
    pub graph: String,
    pub start_nodes: Vec<String>,
    #[serde(default)]
    pub direction: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FtsIndexRequest {
    pub collection: String,
    pub doc_id: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct FtsSearchRequest {
    pub collection: String,
    pub query: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct FtsSuggestRequest {
    pub collection: String,
    pub prefix: String,
    #[serde(default)]
    pub max_distance: Option<u8>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct FtsFuzzySearchRequest {
    pub collection: String,
    pub query: String,
    #[serde(default)]
    pub max_distance: Option<u8>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct FtsPhoneticRequest {
    pub collection: String,
    pub query: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// A write of a batch request; keys, values and vectors in readable forms.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HttpBatchOperation {
    KvSet {
        #[serde(default, alias = "space")]
        namespace: String,
        key: String,
        value: String,
        #[serde(default)]
        if_version: Option<u64>,
        #[serde(default)]
        expires_at_ms: Option<i64>,
        #[serde(default)]
        encoding: Option<String>,
    },
    KvDelete {
        #[serde(default, alias = "space")]
        namespace: String,
        key: String,
        #[serde(default)]
        if_version: Option<u64>,
        #[serde(default)]
        encoding: Option<String>,
    },
    JsonPut {
        space: String,
        id: String,
        value: Value,
        #[serde(default)]
        if_version: Option<u64>,
    },
    JsonDelete {
        space: String,
        id: String,
        #[serde(default)]
        if_version: Option<u64>,
    },
    GraphSetNode {
        graph: String,
        id: String,
        kind: String,
        #[serde(default)]
        metadata: Option<Value>,
        #[serde(default)]
        doc_space: Option<String>,
        #[serde(default)]
        doc_id: Option<String>,
    },
    GraphDeleteNode {
        graph: String,
        id: String,
    },
    GraphAddEdge {
        graph: String,
        source: String,
        target: String,
        kind: String,
        #[serde(default)]
        weight: Option<f64>,
    },
    GraphRemoveEdge {
        graph: String,
        source: String,
        target: String,
        kind: String,
    },
    FtsIndex {
        collection: String,
        doc_id: String,
        content: String,
    },
    FtsDelete {
        collection: String,
        doc_id: String,
    },
    VectorSet {
        collection: String,
        id: String,
        #[serde(default)]
        element_type: Option<VectorElementType>,
        vector: Vec<f32>,
    },
    VectorDelete {
        collection: String,
        id: String,
    },
    CrdtPnCounterIncrement {
        space: String,
        id: String,
        actor_id: String,
        amount: u64,
    },
    CrdtPnCounterDecrement {
        space: String,
        id: String,
        actor_id: String,
        amount: u64,
    },
    CrdtOrSetAdd {
        space: String,
        id: String,
        actor_id: String,
        element: String,
    },
    CrdtOrSetRemove {
        space: String,
        id: String,
        element: String,
        #[serde(default)]
        timestamp_ms: Option<i64>,
    },
    CrdtJsonSetField {
        space: String,
        id: String,
        actor_id: String,
        path: String,
        value: Value,
        #[serde(default)]
        timestamp_ms: Option<i64>,
    },
    CrdtJsonDeleteField {
        space: String,
        id: String,
        actor_id: String,
        path: String,
        #[serde(default)]
        timestamp_ms: Option<i64>,
    },
    CrdtJsonMerge {
        space: String,
        id: String,
        #[serde(with = "serde_bytes")]
        state: Vec<u8>,
    },
}

#[derive(Debug, Deserialize)]
pub struct VectorSetRequest {
    pub collection: String,
    pub id: String,
    #[serde(default)]
    pub element_type: Option<VectorElementType>,
    pub vector: Vec<f32>,
}

#[derive(Debug, Deserialize)]
pub struct VectorGetRequest {
    pub collection: String,
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct VectorSearchRequest {
    pub collection: String,
    pub vector: Vec<f32>,
    #[serde(default)]
    pub element_type: Option<VectorElementType>,
    #[serde(default)]
    pub metric: Option<VectorMetric>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct HybridSearchRequest {
    pub collection: String,
    #[serde(default)]
    pub fts_query: Option<String>,
    #[serde(default)]
    pub vector: Option<Vec<f32>>,
    #[serde(default)]
    pub element_type: Option<VectorElementType>,
    #[serde(default)]
    pub metric: Option<VectorMetric>,
    #[serde(default)]
    pub rrf_k: Option<u32>,
    #[serde(default)]
    pub fts_weight: Option<f64>,
    #[serde(default)]
    pub vector_weight: Option<f64>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct BatchRequest {
    pub operations: Vec<HttpBatchOperation>,
}

#[derive(Debug, Deserialize)]
pub struct SqlRequest {
    pub query: String,
    #[serde(default)]
    pub params: Vec<Value>,
}

#[allow(clippy::needless_pass_by_value)] // reason: `map_err` hands the error over by value.
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

async fn kv_get_handler(
    State(state): State<HttpState>,
    Json(payload): Json<KvGetRequest>,
) -> HttpResult {
    let encoding = payload.encoding.as_deref();
    let key = decode_bytes(&payload.key, encoding, "key")?;
    let operation = Operation::KvGet {
        namespace: payload.namespace.clone(),
        key,
    };
    let entry = answer!(run(&state, operation).await?, ResponseValue::Kv);
    Ok(Json(json!({
        "ok": true,
        "entry": entry.map(|e| kv_json(&e, encoding))
    })))
}

async fn kv_set_handler(
    State(state): State<HttpState>,
    Json(payload): Json<KvSetRequest>,
) -> HttpResult {
    let encoding = payload.encoding.as_deref();
    let operation = BatchOperation::KvSet {
        key: decode_bytes(&payload.key, encoding, "key")?,
        value: decode_bytes(&payload.value, encoding, "value")?,
        namespace: payload.namespace,
        if_version: payload.if_version,
        expires_at_ms: payload.expires_at_ms,
    };
    Ok(written(&commit(&state, vec![operation]).await?))
}

async fn kv_delete_handler(
    State(state): State<HttpState>,
    Json(payload): Json<KvDeleteRequest>,
) -> HttpResult {
    let operation = BatchOperation::KvDelete {
        key: decode_bytes(&payload.key, payload.encoding.as_deref(), "key")?,
        namespace: payload.namespace,
        if_version: payload.if_version,
    };
    Ok(deleted(&commit(&state, vec![operation]).await?))
}

async fn kv_scan_handler(
    State(state): State<HttpState>,
    Json(payload): Json<KvScanRequest>,
) -> HttpResult {
    let encoding = payload.encoding.as_deref();
    let after = payload
        .after
        .as_deref()
        .map(|after| decode_bytes(after, encoding, "after"))
        .transpose()?;
    let operation = Operation::KvScan {
        namespace: payload.namespace.clone(),
        after,
        limit: payload.limit.unwrap_or(100),
    };
    let entries = answer!(run(&state, operation).await?, ResponseValue::KvPage);
    let formatted: Vec<Value> = entries.iter().map(|e| kv_json(e, encoding)).collect();
    Ok(Json(json!({
        "ok": true,
        "entries": formatted,
        "count": formatted.len()
    })))
}

async fn json_get(
    state: &HttpState,
    space: String,
    id: String,
) -> Result<Option<JsonEntry>, Failure> {
    Ok(answer!(
        run(state, Operation::JsonGet { space, id }).await?,
        ResponseValue::Json
    ))
}

async fn json_get_path_handler(
    State(state): State<HttpState>,
    Path((space, id)): Path<(String, String)>,
) -> HttpResult {
    match json_get(&state, space, id).await? {
        Some(document) => Ok(Json(json!({
            "ok": true,
            "document": document_json(&document)
        }))),
        None => Err(map_error(LiliaError::new(
            ErrorCode::NotFound,
            "document not found",
            false,
        ))),
    }
}

async fn json_get_post_handler(
    State(state): State<HttpState>,
    Json(payload): Json<JsonGetRequest>,
) -> HttpResult {
    let document = json_get(&state, payload.space, payload.id).await?;
    Ok(Json(json!({
        "ok": true,
        "document": document.as_ref().map(document_json)
    })))
}

async fn json_put_handler(
    State(state): State<HttpState>,
    Json(payload): Json<JsonPutRequest>,
) -> HttpResult {
    let operation = BatchOperation::JsonPut {
        space: payload.space,
        id: payload.id,
        value: payload.value,
        if_version: payload.if_version,
    };
    Ok(written(&commit(&state, vec![operation]).await?))
}

async fn json_delete_handler(
    State(state): State<HttpState>,
    Json(payload): Json<JsonDeleteRequest>,
) -> HttpResult {
    let operation = BatchOperation::JsonDelete {
        space: payload.space,
        id: payload.id,
        if_version: payload.if_version,
    };
    Ok(deleted(&commit(&state, vec![operation]).await?))
}

async fn json_scan_handler(
    State(state): State<HttpState>,
    Json(payload): Json<JsonScanRequest>,
) -> HttpResult {
    let operation = Operation::JsonScan {
        space: payload.space,
        after: payload.after,
        limit: payload.limit.unwrap_or(100),
    };
    let documents = answer!(run(&state, operation).await?, ResponseValue::JsonPage);
    Ok(documents_json(&documents))
}

async fn json_create_index_handler(
    State(state): State<HttpState>,
    Json(payload): Json<JsonCreateIndexRequest>,
) -> HttpResult {
    let operation = Operation::JsonCreateIndex {
        space: payload.space,
        name: payload.name,
        path: payload.path,
        filter: payload.filter,
    };
    run(&state, operation).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn json_drop_index_handler(
    State(state): State<HttpState>,
    Json(payload): Json<JsonDropIndexRequest>,
) -> HttpResult {
    let operation = Operation::JsonDropIndex {
        space: payload.space,
        name: payload.name,
    };
    let dropped = answer!(run(&state, operation).await?, ResponseValue::Bool);
    Ok(Json(json!({ "ok": true, "deleted": dropped })))
}

async fn json_list_indexes_handler(
    State(state): State<HttpState>,
    Json(payload): Json<JsonListIndexesRequest>,
) -> HttpResult {
    let operation = Operation::JsonListIndexes {
        space: payload.space,
    };
    let indexes = answer!(run(&state, operation).await?, ResponseValue::JsonIndexes);
    let formatted: Vec<Value> = indexes
        .into_iter()
        .map(|index| {
            json!({
                "space": index.space,
                "index_name": index.index_name,
                "json_path": index.json_path
            })
        })
        .collect();
    Ok(Json(json!({ "ok": true, "indexes": formatted })))
}

async fn json_scan_by_index_handler(
    State(state): State<HttpState>,
    Json(payload): Json<JsonScanByIndexRequest>,
) -> HttpResult {
    let operation = Operation::JsonScanByIndex {
        space: payload.space,
        index_name: payload.index_name,
        value: payload.value,
        after: payload.after,
        limit: payload.limit.unwrap_or(100),
    };
    let documents = answer!(run(&state, operation).await?, ResponseValue::JsonPage);
    Ok(documents_json(&documents))
}

async fn json_find_handler(
    State(state): State<HttpState>,
    Json(payload): Json<JsonFindRequest>,
) -> HttpResult {
    let operation = Operation::JsonFind {
        space: payload.space,
        query: Value::Object(payload.query),
        after: payload.after,
        limit: payload.limit.unwrap_or(100),
    };
    let documents = answer!(run(&state, operation).await?, ResponseValue::JsonPage);
    Ok(documents_json(&documents))
}

/// The protocol form of a batch write.
// reason: one arm per write, each moving its fields into the protocol form.
#[allow(clippy::too_many_lines)]
fn batch_operation(operation: HttpBatchOperation) -> Result<BatchOperation, Failure> {
    Ok(match operation {
        HttpBatchOperation::KvSet {
            namespace,
            key,
            value,
            if_version,
            expires_at_ms,
            encoding,
        } => BatchOperation::KvSet {
            namespace,
            key: decode_bytes(&key, encoding.as_deref(), "key")?,
            value: decode_bytes(&value, encoding.as_deref(), "value")?,
            if_version,
            expires_at_ms,
        },
        HttpBatchOperation::KvDelete {
            namespace,
            key,
            if_version,
            encoding,
        } => BatchOperation::KvDelete {
            namespace,
            key: decode_bytes(&key, encoding.as_deref(), "key")?,
            if_version,
        },
        HttpBatchOperation::JsonPut {
            space,
            id,
            value,
            if_version,
        } => BatchOperation::JsonPut {
            space,
            id,
            value,
            if_version,
        },
        HttpBatchOperation::JsonDelete {
            space,
            id,
            if_version,
        } => BatchOperation::JsonDelete {
            space,
            id,
            if_version,
        },
        HttpBatchOperation::GraphSetNode {
            graph,
            id,
            kind,
            metadata,
            doc_space,
            doc_id,
        } => BatchOperation::GraphSetNode {
            graph,
            id,
            kind,
            metadata: metadata.unwrap_or_else(|| json!({})),
            doc_space,
            doc_id,
        },
        HttpBatchOperation::GraphDeleteNode { graph, id } => {
            BatchOperation::GraphDeleteNode { graph, id }
        }
        HttpBatchOperation::GraphAddEdge {
            graph,
            source,
            target,
            kind,
            weight,
        } => BatchOperation::GraphAddEdge {
            graph,
            source,
            target,
            kind,
            weight: weight.unwrap_or(1.0),
        },
        HttpBatchOperation::GraphRemoveEdge {
            graph,
            source,
            target,
            kind,
        } => BatchOperation::GraphRemoveEdge {
            graph,
            source,
            target,
            kind,
        },
        HttpBatchOperation::FtsIndex {
            collection,
            doc_id,
            content,
        } => BatchOperation::FtsIndex {
            collection,
            doc_id,
            content,
        },
        HttpBatchOperation::FtsDelete { collection, doc_id } => {
            BatchOperation::FtsDelete { collection, doc_id }
        }
        HttpBatchOperation::VectorSet {
            collection,
            id,
            element_type,
            vector,
        } => {
            let (dim, element_type, vector) = vector_bytes(&vector, element_type)?;
            BatchOperation::VectorSet {
                collection,
                id,
                dim,
                element_type,
                vector,
            }
        }
        HttpBatchOperation::VectorDelete { collection, id } => {
            BatchOperation::VectorDelete { collection, id }
        }
        HttpBatchOperation::CrdtPnCounterIncrement {
            space,
            id,
            actor_id,
            amount,
        } => BatchOperation::CrdtPnCounterIncrement {
            space,
            id,
            actor_id,
            amount,
        },
        HttpBatchOperation::CrdtPnCounterDecrement {
            space,
            id,
            actor_id,
            amount,
        } => BatchOperation::CrdtPnCounterDecrement {
            space,
            id,
            actor_id,
            amount,
        },
        HttpBatchOperation::CrdtOrSetAdd {
            space,
            id,
            actor_id,
            element,
        } => BatchOperation::CrdtOrSetAdd {
            space,
            id,
            actor_id,
            element,
        },
        HttpBatchOperation::CrdtOrSetRemove {
            space,
            id,
            element,
            timestamp_ms,
        } => BatchOperation::CrdtOrSetRemove {
            space,
            id,
            element,
            timestamp_ms,
        },
        HttpBatchOperation::CrdtJsonSetField {
            space,
            id,
            actor_id,
            path,
            value,
            timestamp_ms,
        } => BatchOperation::CrdtJsonSetField {
            space,
            id,
            actor_id,
            path,
            value,
            timestamp_ms,
        },
        HttpBatchOperation::CrdtJsonDeleteField {
            space,
            id,
            actor_id,
            path,
            timestamp_ms,
        } => BatchOperation::CrdtJsonDeleteField {
            space,
            id,
            actor_id,
            path,
            timestamp_ms,
        },
        HttpBatchOperation::CrdtJsonMerge { space, id, state } => {
            BatchOperation::CrdtJsonMerge { space, id, state }
        }
    })
}

async fn batch_handler(
    State(state): State<HttpState>,
    Json(payload): Json<BatchRequest>,
) -> HttpResult {
    if payload.operations.len() > MAX_BATCH_OPERATIONS {
        return Err(invalid(format!(
            "a batch holds at most {MAX_BATCH_OPERATIONS} operations, not {}",
            payload.operations.len()
        )));
    }
    let operations = payload
        .operations
        .into_iter()
        .map(batch_operation)
        .collect::<Result<Vec<_>, _>>()?;
    let formatted: Vec<Value> = commit(&state, operations)
        .await?
        .into_iter()
        .map(|r| {
            json!({
                "version": r.version.map(|v| v.to_string()),
                "deleted": r.deleted
            })
        })
        .collect();
    Ok(Json(json!({
        "ok": true,
        "results": formatted
    })))
}

async fn sql_handler(
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

async fn graph_node_handler(
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

async fn graph_edge_handler(
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

async fn graph_neighbors_handler(
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

async fn graph_reachability_handler(
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

async fn fts_index_handler(
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

async fn fts_search_handler(
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

async fn fts_suggest_handler(
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

async fn fts_fuzzy_search_handler(
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

async fn fts_phonetic_handler(
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

async fn vector_set_handler(
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

async fn vector_get_handler(
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

async fn vector_search_handler(
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

async fn hybrid_search_handler(
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
