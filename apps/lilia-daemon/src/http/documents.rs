//! Routes of key-value records and JSON documents.

use axum::extract::{Path, State};
use axum::Json;
use lilia_core::{BatchOperation, ErrorCode, JsonEntry, LiliaError};
use lilia_protocol::{Operation, ResponseValue};
use serde_json::{json, Value};

use super::requests::{
    JsonCreateIndexRequest, JsonDeleteRequest, JsonDropIndexRequest, JsonFindRequest,
    JsonGetRequest, JsonListIndexesRequest, JsonPutRequest, JsonScanByIndexRequest,
    JsonScanRequest, KvDeleteRequest, KvGetRequest, KvScanRequest, KvSetRequest,
};
use super::{
    commit, decode_bytes, deleted, document_json, documents_json, kv_json, map_error, run, written,
    Failure, HttpResult, HttpState,
};

pub(super) async fn kv_get_handler(
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

pub(super) async fn kv_set_handler(
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

pub(super) async fn kv_delete_handler(
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

pub(super) async fn kv_scan_handler(
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

pub(super) async fn json_get_path_handler(
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

pub(super) async fn json_get_post_handler(
    State(state): State<HttpState>,
    Json(payload): Json<JsonGetRequest>,
) -> HttpResult {
    let document = json_get(&state, payload.space, payload.id).await?;
    Ok(Json(json!({
        "ok": true,
        "document": document.as_ref().map(document_json)
    })))
}

pub(super) async fn json_put_handler(
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

pub(super) async fn json_delete_handler(
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

pub(super) async fn json_scan_handler(
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

pub(super) async fn json_create_index_handler(
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

pub(super) async fn json_drop_index_handler(
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

pub(super) async fn json_list_indexes_handler(
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

pub(super) async fn json_scan_by_index_handler(
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

pub(super) async fn json_find_handler(
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
