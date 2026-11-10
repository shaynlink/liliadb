use lilia_sqlite_native::{NativeTransaction, StmtSlot};
use serde_json::json;

use crate::json::decode_json_value;
use crate::storage::{now_ms, parse_version, store_version, validate_name};
use lilia_core::{
    compress_bytes, json_patch, BatchOperation, ErrorCode, LiliaError, MutationResult, Result,
};

fn record_json_revision(
    tx: &mut NativeTransaction<'_>,
    space: &str,
    id: &str,
    version: u64,
    prev_val: Option<&serde_json::Value>,
    new_val: &serde_json::Value,
) -> Result<()> {
    let now = now_ms();
    let (delta, is_keyframe) = match prev_val {
        Some(old) if !version.is_multiple_of(50) => {
            let patch = json_patch::diff(old, new_val);
            let raw = serde_json::to_vec(&patch)
                .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
            (compress_bytes(&raw), 0)
        }
        _ => {
            let raw = serde_json::to_vec(new_val)
                .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
            (compress_bytes(&raw), 1)
        }
    };

    let mut stmt = tx.prepare(
        "INSERT INTO _lilia_json_revisions(space, id, version, timestamp_ms, delta, is_keyframe, is_deletion) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, 0);",
    )?;
    stmt.bind_text_static(1, space)?;
    stmt.bind_text_static(2, id)?;
    stmt.bind_int64(3, store_version(version)?)?;
    stmt.bind_int64(4, now)?;
    stmt.bind_blob_static(5, &delta)?;
    stmt.bind_int64(6, is_keyframe)?;
    stmt.step()?;
    Ok(())
}

fn record_json_deletion_revision(
    tx: &mut NativeTransaction<'_>,
    space: &str,
    id: &str,
    version: u64,
) -> Result<()> {
    let now = now_ms();
    let mut stmt = tx.prepare(
        "INSERT INTO _lilia_json_revisions(space, id, version, timestamp_ms, delta, is_keyframe, is_deletion) \
         VALUES(?1, ?2, ?3, ?4, X'', 0, 1);",
    )?;
    stmt.bind_text_static(1, space)?;
    stmt.bind_text_static(2, id)?;
    stmt.bind_int64(3, store_version(version)?)?;
    stmt.bind_int64(4, now)?;
    stmt.step()?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub(crate) fn apply(
    tx: &mut NativeTransaction<'_>,
    operation: &BatchOperation,
    codec: &dyn lilia_core::ValueCodec,
) -> Result<MutationResult> {
    match operation {
        BatchOperation::KvSet {
            namespace,
            key,
            value,
            if_version,
            expires_at_ms,
        } => {
            validate_name("namespace", namespace)?;
            let stored_value = codec.encode(value)?;
            let version = match if_version {
                None => {
                    let stmt = tx.slot(StmtSlot::KvUpsertReturning)?;
                    stmt.reset()?;
                    stmt.bind_text_static(1, namespace)?;
                    stmt.bind_blob_static(2, key)?;
                    stmt.bind_blob_static(3, stored_value.as_ref())?;
                    match expires_at_ms {
                        Some(exp) => stmt.bind_int64(4, *exp)?,
                        None => stmt.bind_null(4)?,
                    }
                    if !stmt.step()? {
                        return Err(LiliaError::new(
                            ErrorCode::Storage,
                            "no version returned from upsert",
                            false,
                        ));
                    }
                    let v = stmt.column_int64(0);
                    stmt.reset()?;
                    parse_version(v)?
                }
                Some(expected) => {
                    let current = kv_version(tx, namespace, key)?;
                    assert_version(current, Some(*expected))?;
                    let next_version = current.unwrap_or(0) + 1;
                    let stmt = tx.slot(StmtSlot::KvUpsertVersioned)?;
                    stmt.reset()?;
                    stmt.bind_text_static(1, namespace)?;
                    stmt.bind_blob_static(2, key)?;
                    stmt.bind_blob_static(3, stored_value.as_ref())?;
                    stmt.bind_int64(4, store_version(next_version)?)?;
                    match expires_at_ms {
                        Some(exp) => stmt.bind_int64(5, *exp)?,
                        None => stmt.bind_null(5)?,
                    }
                    stmt.step()?;
                    stmt.reset()?;
                    next_version
                }
            };
            Ok(MutationResult {
                version: Some(version),
                deleted: false,
            })
        }
        BatchOperation::KvDelete {
            namespace,
            key,
            if_version,
        } => {
            validate_name("namespace", namespace)?;
            let current = kv_version(tx, namespace, key)?;
            assert_version(current, *if_version)?;
            let stmt = tx.slot(StmtSlot::KvDelete)?;
            stmt.reset()?;
            stmt.bind_text_static(1, namespace)?;
            stmt.bind_blob_static(2, key)?;
            stmt.step()?;
            stmt.reset()?;
            let deleted = tx.conn().changes() > 0;
            Ok(MutationResult {
                version: None,
                deleted,
            })
        }
        BatchOperation::JsonPut {
            space,
            id,
            value,
            if_version,
        } => {
            validate_name("space", space)?;
            validate_name("id", id)?;
            let prev_doc: Option<serde_json::Value> = {
                let stmt = tx.slot(StmtSlot::JsonGet)?;
                stmt.reset()?;
                stmt.bind_text_static(1, space)?;
                stmt.bind_text_static(2, id)?;
                let res = if stmt.step()? {
                    let encoded_prev = stmt.column_text(0);
                    decode_json_value(encoded_prev, codec).ok()
                } else {
                    None
                };
                stmt.reset()?;
                res
            };
            let encoded = if codec.name() == "identity" {
                serde_json::to_string(value).map_err(|error| {
                    LiliaError::new(ErrorCode::InvalidInput, error.to_string(), false)
                })?
            } else {
                use base64::Engine;
                let raw_bytes = serde_json::to_vec(value).map_err(|error| {
                    LiliaError::new(ErrorCode::InvalidInput, error.to_string(), false)
                })?;
                let cipher_bytes = codec.encode(&raw_bytes)?;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&cipher_bytes);
                serde_json::to_string(&json!({
                    "$codec": codec.name(),
                    "$data": b64,
                }))
                .map_err(|error| LiliaError::new(ErrorCode::Storage, error.to_string(), false))?
            };
            let version = match if_version {
                None => {
                    let stmt = tx.slot(StmtSlot::JsonPutReturning)?;
                    stmt.reset()?;
                    stmt.bind_text_static(1, space)?;
                    stmt.bind_text_static(2, id)?;
                    stmt.bind_text_static(3, &encoded)?;
                    if !stmt.step()? {
                        return Err(LiliaError::new(
                            ErrorCode::Storage,
                            "no version returned from json put",
                            false,
                        ));
                    }
                    let v = stmt.column_int64(0);
                    stmt.reset()?;
                    parse_version(v)?
                }
                Some(expected) => {
                    let current = json_version(tx, space, id)?;
                    assert_version(current, Some(*expected))?;
                    let next_version = current.unwrap_or(0) + 1;
                    let stmt = tx.slot(StmtSlot::JsonPutVersioned)?;
                    stmt.reset()?;
                    stmt.bind_text_static(1, space)?;
                    stmt.bind_text_static(2, id)?;
                    stmt.bind_text_static(3, &encoded)?;
                    stmt.bind_int64(4, store_version(next_version)?)?;
                    stmt.step()?;
                    stmt.reset()?;
                    next_version
                }
            };
            record_json_revision(tx, space, id, version, prev_doc.as_ref(), value)?;
            update_json_indexes(tx, space, id, value)?;
            Ok(MutationResult {
                version: Some(version),
                deleted: false,
            })
        }
        BatchOperation::JsonDelete {
            space,
            id,
            if_version,
        } => {
            validate_name("space", space)?;
            validate_name("id", id)?;
            let current = json_version(tx, space, id)?;
            assert_version(current, *if_version)?;
            if let Some(cur_ver) = current {
                record_json_deletion_revision(tx, space, id, cur_ver + 1)?;
            }
            let stmt = tx.slot(StmtSlot::JsonDelete)?;
            stmt.reset()?;
            stmt.bind_text_static(1, space)?;
            stmt.bind_text_static(2, id)?;
            stmt.step()?;
            stmt.reset()?;
            let deleted = tx.conn().changes() > 0;
            if deleted {
                delete_json_indexes(tx, space, id)?;
            }
            Ok(MutationResult {
                version: None,
                deleted,
            })
        }
        BatchOperation::GraphSetNode {
            graph,
            id,
            kind,
            metadata,
            doc_space,
            doc_id,
        } => {
            validate_name("graph", graph)?;
            validate_name("id", id)?;
            let meta_str = serde_json::to_string(metadata).map_err(|error| {
                LiliaError::new(ErrorCode::InvalidInput, error.to_string(), false)
            })?;
            let stmt = tx.slot(StmtSlot::GraphNodeSet)?;
            stmt.reset()?;
            stmt.bind_text_static(1, graph)?;
            stmt.bind_text_static(2, id)?;
            stmt.bind_text_static(3, kind)?;
            stmt.bind_text_static(4, &meta_str)?;
            if let Some(space) = doc_space {
                stmt.bind_text_static(5, space)?;
            } else {
                stmt.bind_null(5)?;
            }
            if let Some(did) = doc_id {
                stmt.bind_text_static(6, did)?;
            } else {
                stmt.bind_null(6)?;
            }
            stmt.step()?;
            stmt.reset()?;
            Ok(MutationResult {
                version: None,
                deleted: false,
            })
        }
        BatchOperation::GraphDeleteNode { graph, id } => {
            validate_name("graph", graph)?;
            validate_name("id", id)?;
            let mut stmt_edges = tx.prepare(
                "DELETE FROM _lilia_graph_edges WHERE graph = ?1 AND (source = ?2 OR target = ?2);",
            )?;
            stmt_edges.bind_text_static(1, graph)?;
            stmt_edges.bind_text_static(2, id)?;
            stmt_edges.step()?;
            drop(stmt_edges);

            let stmt = tx.slot(StmtSlot::GraphNodeDelete)?;
            stmt.reset()?;
            stmt.bind_text_static(1, graph)?;
            stmt.bind_text_static(2, id)?;
            stmt.step()?;
            stmt.reset()?;
            let deleted = tx.conn().changes() > 0;
            Ok(MutationResult {
                version: None,
                deleted,
            })
        }
        BatchOperation::GraphAddEdge {
            graph,
            source,
            target,
            kind,
            weight,
        } => {
            validate_name("graph", graph)?;
            validate_name("source", source)?;
            validate_name("target", target)?;
            let stmt = tx.slot(StmtSlot::GraphEdgeSet)?;
            stmt.reset()?;
            stmt.bind_text_static(1, graph)?;
            stmt.bind_text_static(2, source)?;
            stmt.bind_text_static(3, target)?;
            stmt.bind_text_static(4, kind)?;
            stmt.bind_double(5, *weight)?;
            stmt.step()?;
            stmt.reset()?;
            Ok(MutationResult {
                version: None,
                deleted: false,
            })
        }
        BatchOperation::GraphRemoveEdge {
            graph,
            source,
            target,
            kind,
        } => {
            validate_name("graph", graph)?;
            validate_name("source", source)?;
            validate_name("target", target)?;
            let stmt = tx.slot(StmtSlot::GraphEdgeDelete)?;
            stmt.reset()?;
            stmt.bind_text_static(1, graph)?;
            stmt.bind_text_static(2, source)?;
            stmt.bind_text_static(3, target)?;
            stmt.bind_text_static(4, kind)?;
            stmt.step()?;
            stmt.reset()?;
            let deleted = tx.conn().changes() > 0;
            Ok(MutationResult {
                version: None,
                deleted,
            })
        }
        BatchOperation::FtsIndex {
            collection,
            doc_id,
            content,
        } => {
            validate_name("collection", collection)?;
            validate_name("doc_id", doc_id)?;
            let stmt_del = tx.slot(StmtSlot::FtsIndexDelete)?;
            stmt_del.reset()?;
            stmt_del.bind_text_static(1, collection)?;
            stmt_del.bind_text_static(2, doc_id)?;
            stmt_del.step()?;
            stmt_del.reset()?;

            let stmt_ins = tx.slot(StmtSlot::FtsIndexInsert)?;
            stmt_ins.reset()?;
            stmt_ins.bind_text_static(1, collection)?;
            stmt_ins.bind_text_static(2, doc_id)?;
            stmt_ins.bind_text_static(3, content)?;
            stmt_ins.step()?;
            stmt_ins.reset()?;

            Ok(MutationResult {
                version: None,
                deleted: false,
            })
        }
        BatchOperation::FtsDelete { collection, doc_id } => {
            validate_name("collection", collection)?;
            validate_name("doc_id", doc_id)?;
            let stmt_del = tx.slot(StmtSlot::FtsIndexDelete)?;
            stmt_del.reset()?;
            stmt_del.bind_text_static(1, collection)?;
            stmt_del.bind_text_static(2, doc_id)?;
            stmt_del.step()?;
            stmt_del.reset()?;
            let deleted = tx.conn().changes() > 0;
            Ok(MutationResult {
                version: None,
                deleted,
            })
        }
        BatchOperation::VectorSet {
            collection,
            id,
            dim,
            element_type,
            vector,
        } => {
            validate_name("collection", collection)?;
            validate_name("id", id)?;
            let stored_vector = codec.encode(vector)?;
            let stmt = tx.slot(StmtSlot::VectorUpsert)?;
            stmt.reset()?;
            stmt.bind_text_static(1, collection)?;
            stmt.bind_text_static(2, id)?;
            stmt.bind_int64(3, i64::from(*dim))?;
            stmt.bind_int64(4, i64::from(*element_type as u8))?;
            stmt.bind_blob_static(5, stored_vector.as_ref())?;
            stmt.step()?;
            stmt.reset()?;
            Ok(MutationResult {
                version: None,
                deleted: false,
            })
        }
        BatchOperation::VectorDelete { collection, id } => {
            validate_name("collection", collection)?;
            validate_name("id", id)?;
            let stmt = tx.slot(StmtSlot::VectorDelete)?;
            stmt.reset()?;
            stmt.bind_text_static(1, collection)?;
            stmt.bind_text_static(2, id)?;
            stmt.step()?;
            stmt.reset()?;
            let deleted = tx.conn().changes() > 0;
            Ok(MutationResult {
                version: None,
                deleted,
            })
        }
    }
}

fn delete_json_indexes(tx: &mut NativeTransaction<'_>, space: &str, doc_id: &str) -> Result<()> {
    let mut stmt =
        tx.prepare("DELETE FROM _lilia_json_index_entries WHERE space = ?1 AND doc_id = ?2;")?;
    stmt.bind_text_static(1, space)?;
    stmt.bind_text_static(2, doc_id)?;
    stmt.step()?;
    Ok(())
}

fn update_json_indexes(
    tx: &mut NativeTransaction<'_>,
    space: &str,
    doc_id: &str,
    value: &serde_json::Value,
) -> Result<()> {
    delete_json_indexes(tx, space, doc_id)?;
    let mut stmt =
        tx.prepare("SELECT index_name, json_path FROM _lilia_json_indexes WHERE space = ?1;")?;
    stmt.bind_text_static(1, space)?;
    let mut indexes: Vec<(String, String)> = Vec::new();
    while stmt.step()? {
        indexes.push((
            stmt.column_text(0).to_string(),
            stmt.column_text(1).to_string(),
        ));
    }
    drop(stmt);
    if indexes.is_empty() {
        return Ok(());
    }
    let mut insert_stmt = tx.prepare(
        "INSERT OR IGNORE INTO _lilia_json_index_entries(space, index_name, idx_value, doc_id) VALUES(?1, ?2, ?3, ?4);",
    )?;
    for (index_name, json_path) in indexes {
        let path = if let Some(stripped) = json_path.strip_prefix("$.") {
            format!("/{}", stripped.replace('.', "/"))
        } else if !json_path.starts_with('/') {
            format!("/{json_path}")
        } else {
            json_path.clone()
        };
        if let Some(target_val) = value.pointer(&path) {
            if let serde_json::Value::Array(items) = target_val {
                for item in items {
                    let val_str = match item {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::Bool(b) => b.to_string(),
                        serde_json::Value::Null => "null".to_string(),
                        _ => item.to_string(),
                    };
                    insert_stmt.reset()?;
                    insert_stmt.bind_text_static(1, space)?;
                    insert_stmt.bind_text_static(2, &index_name)?;
                    insert_stmt.bind_text_static(3, &val_str)?;
                    insert_stmt.bind_text_static(4, doc_id)?;
                    insert_stmt.step()?;
                }
            } else {
                let val_str = match target_val {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Null => "null".to_string(),
                    _ => target_val.to_string(),
                };
                insert_stmt.reset()?;
                insert_stmt.bind_text_static(1, space)?;
                insert_stmt.bind_text_static(2, &index_name)?;
                insert_stmt.bind_text_static(3, &val_str)?;
                insert_stmt.bind_text_static(4, doc_id)?;
                insert_stmt.step()?;
            }
        }
    }
    insert_stmt.reset()?;
    Ok(())
}

fn kv_version(tx: &mut NativeTransaction<'_>, namespace: &str, key: &[u8]) -> Result<Option<u64>> {
    let stmt = tx.slot(StmtSlot::KvVersion)?;
    stmt.reset()?;
    stmt.bind_text_static(1, namespace)?;
    stmt.bind_blob_static(2, key)?;
    let res = if stmt.step()? {
        Some(parse_version(stmt.column_int64(0))?)
    } else {
        None
    };
    stmt.reset()?;
    Ok(res)
}

fn json_version(tx: &mut NativeTransaction<'_>, space: &str, id: &str) -> Result<Option<u64>> {
    let stmt = tx.slot(StmtSlot::JsonVersion)?;
    stmt.reset()?;
    stmt.bind_text_static(1, space)?;
    stmt.bind_text_static(2, id)?;
    let res = if stmt.step()? {
        Some(parse_version(stmt.column_int64(0))?)
    } else {
        None
    };
    stmt.reset()?;
    Ok(res)
}

fn assert_version(current: Option<u64>, expected: Option<u64>) -> Result<()> {
    if expected.is_some_and(|expected| current.unwrap_or(0) != expected) {
        return Err(
            LiliaError::new(ErrorCode::Conflict, "optimistic version conflict", true)
                .details(json!({ "expected": expected.map(|value| value.to_string()), "actual": current.map(|value| value.to_string()) })),
        );
    }
    Ok(())
}
