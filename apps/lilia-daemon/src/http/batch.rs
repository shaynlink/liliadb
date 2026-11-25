//! The batch route: writes of every model, committed atomically.

use axum::extract::State;
use axum::Json;
use lilia_core::BatchOperation;
use serde_json::{json, Value};

use super::requests::{BatchRequest, HttpBatchOperation};
use super::{
    commit, decode_bytes, invalid, vector_bytes, Failure, HttpResult, HttpState,
    MAX_BATCH_OPERATIONS,
};

/// The protocol form of a batch write.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per write, each moving its fields into the protocol form"
)]
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

pub(super) async fn batch_handler(
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
