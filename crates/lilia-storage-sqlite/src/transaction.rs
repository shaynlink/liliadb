use lilia_sqlite_native::{NativeTransaction, StmtSlot};
use serde_json::json;

use crate::engine::{Database, WriterGuard};
use crate::integrity::{IntegrityRegistry, JsonContracts};
use crate::storage::{now_ms, parse_version, store_version, validate_name};
use lilia_core::{
    compress_bytes, json_patch, BatchOperation, ErrorCode, LiliaError, MutationResult, Result,
    StorageTier,
};

#[inline]
fn disk_result(version: Option<u64>, deleted: bool) -> MutationResult {
    MutationResult {
        version,
        deleted,
        hlc: None,
        conflict_detected: None,
        tier: StorageTier::Disk,
        buffered: false,
    }
}

pub(crate) fn tokenize_terms(text: &str) -> std::collections::HashSet<String> {
    let mut tokens = std::collections::HashSet::new();
    let mut current = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            current.extend(c.to_lowercase());
        } else if !current.is_empty() {
            tokens.insert(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.insert(current);
    }
    tokens
}

pub(crate) fn apply_fts_index(
    tx: &mut NativeTransaction<'_>,
    collection: &str,
    doc_id: &str,
    content: &str,
    in_memory_db: bool,
) -> Result<MutationResult> {
    validate_name("collection", collection)?;
    validate_name("doc_id", doc_id)?;
    let stmt_del = tx.slot(StmtSlot::fts_delete_for(in_memory_db))?;
    stmt_del.reset()?;
    stmt_del.bind_text_static(1, collection)?;
    stmt_del.bind_text_static(2, doc_id)?;
    stmt_del.step()?;
    stmt_del.reset()?;

    let clean_content = if content.contains(lilia_core::ENC_PREFIX) {
        content
            .split_whitespace()
            .filter(|w| !w.starts_with(lilia_core::ENC_PREFIX))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        content.to_string()
    };
    if clean_content.is_empty() {
        return Ok(disk_result(None, false));
    }

    let stmt_ins = tx.slot(StmtSlot::fts_insert_for(in_memory_db))?;
    stmt_ins.reset()?;
    stmt_ins.bind_text_static(1, collection)?;
    stmt_ins.bind_text_static(2, doc_id)?;
    stmt_ins.bind_text_static(3, &clean_content)?;
    stmt_ins.step()?;
    stmt_ins.reset()?;

    // Index terms into _lilia_fts_terms for dictionary and Levenshtein automaton lookup
    let terms_sql = if in_memory_db {
        "INSERT INTO _lilia_mem._lilia_mem_fts_terms(collection, term, doc_count) \
         VALUES(?1, ?2, 1) \
         ON CONFLICT(collection, term) DO UPDATE SET doc_count = doc_count + 1;"
    } else {
        "INSERT INTO _lilia_fts_terms(collection, term, doc_count) \
         VALUES(?1, ?2, 1) \
         ON CONFLICT(collection, term) DO UPDATE SET doc_count = doc_count + 1;"
    };
    let mut term_stmt = tx.conn().prepare(terms_sql)?;
    let unique_tokens = tokenize_terms(&clean_content);
    for token in unique_tokens {
        term_stmt.reset()?;
        term_stmt.bind_text_static(1, collection)?;
        term_stmt.bind_text_static(2, &token)?;
        term_stmt.step()?;
    }

    let mut res = disk_result(None, false);
    if in_memory_db {
        res.tier = StorageTier::Memory;
    }
    Ok(res)
}

pub(crate) fn apply_fts_delete(
    tx: &mut NativeTransaction<'_>,
    collection: &str,
    doc_id: &str,
    in_memory_db: bool,
) -> Result<MutationResult> {
    validate_name("collection", collection)?;
    validate_name("doc_id", doc_id)?;
    let stmt_del = tx.slot(StmtSlot::fts_delete_for(in_memory_db))?;
    stmt_del.reset()?;
    stmt_del.bind_text_static(1, collection)?;
    stmt_del.bind_text_static(2, doc_id)?;
    stmt_del.step()?;
    stmt_del.reset()?;
    let deleted = tx.conn().changes() > 0;
    let mut res = disk_result(None, deleted);
    if in_memory_db {
        res.tier = StorageTier::Memory;
    }
    Ok(res)
}

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

/// Applies one batch operation. With sharding enabled, KV and JSON operations
/// are first routed to the shard that owns (or will own) the record.
///
/// When integrity contracts are declared, references are verified before the operation and the
/// cascades they imply are applied right after it, all inside the caller's transaction.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    tx: &mut NativeTransaction<'_>,
    operation: &BatchOperation,
    codec: &dyn lilia_core::ValueCodec,
    dict_registry: &lilia_core::DictionaryRegistry,
    json_options: &crate::engine::JsonOptions,
    hlc_clock: &lilia_core::HybridClock,
    cas: &crate::cas::CasEngine,
    shards: Option<&crate::sharding::ShardManager>,
    integrity: Option<&IntegrityRegistry>,
) -> Result<MutationResult> {
    let Some(registry) = integrity.filter(|registry| !registry.is_empty()) else {
        return apply_routed(tx, operation, codec, dict_registry, json_options, hlc_clock, cas, shards, None);
    };
    let follow_ups = crate::integrity::guard(tx, operation, registry, shards)?;
    let result = apply_routed(
        tx,
        operation,
        codec,
        dict_registry,
        json_options,
        hlc_clock,
        cas,
        shards,
        Some(registry),
    )?;
    for follow_up in &follow_ups {
        apply(
            tx,
            follow_up,
            codec,
            dict_registry,
            json_options,
            hlc_clock,
            cas,
            shards,
            integrity,
        )?;
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn apply_routed(
    tx: &mut NativeTransaction<'_>,
    operation: &BatchOperation,
    codec: &dyn lilia_core::ValueCodec,
    dict_registry: &lilia_core::DictionaryRegistry,
    json_options: &crate::engine::JsonOptions,
    hlc_clock: &lilia_core::HybridClock,
    cas: &crate::cas::CasEngine,
    shards: Option<&crate::sharding::ShardManager>,
    integrity: Option<&IntegrityRegistry>,
) -> Result<MutationResult> {
    let Some(shards) = shards else {
        return apply_in_schema(tx, operation, codec, dict_registry, json_options, hlc_clock, cas, integrity);
    };
    let target = shards.route_operation(tx, operation)?;
    tx.conn_mut().set_write_schema(target.as_deref());
    let result = apply_in_schema(tx, operation, codec, dict_registry, json_options, hlc_clock, cas, integrity);
    tx.conn_mut().set_write_schema(None);
    result
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn apply_in_schema(
    tx: &mut NativeTransaction<'_>,
    operation: &BatchOperation,
    codec: &dyn lilia_core::ValueCodec,
    dict_registry: &lilia_core::DictionaryRegistry,
    json_options: &crate::engine::JsonOptions,
    hlc_clock: &lilia_core::HybridClock,
    cas: &crate::cas::CasEngine,
    integrity: Option<&IntegrityRegistry>,
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
            let now = now_ms();
            let old_raw: Option<Vec<u8>> = {
                let stmt = tx.slot(StmtSlot::KvGet)?;
                stmt.reset()?;
                stmt.bind_text_static(1, namespace)?;
                stmt.bind_blob_static(2, key)?;
                stmt.bind_int64(3, i64::MAX)?;
                let res = if stmt.step()? {
                    Some(stmt.column_blob(0).to_vec())
                } else {
                    None
                };
                stmt.reset()?;
                res
            };

            let stored_value = if cas.options().enabled && value.len() >= cas.options().threshold_bytes {
                let desc = cas.store(value, None)?;
                crate::cas::register_blob_in_db(tx, &desc, now)?;
                let (_, raw_hash) = lilia_core::cas::compute_hash(cas.options().hash_algorithm, value);
                let ptr = lilia_core::cas::encode_kv_pointer(cas.options().hash_algorithm, &raw_hash, value.len() as u64);
                std::borrow::Cow::Owned(ptr)
            } else {
                codec.encode(value)?
            };
            crate::cas::sync_kv_blob_ref(tx, namespace, key, old_raw.as_deref(), stored_value.as_ref(), now)?;

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
            Ok(disk_result(Some(version), false))
        }
        BatchOperation::KvDelete {
            namespace,
            key,
            if_version,
        } => {
            validate_name("namespace", namespace)?;
            let current = kv_version(tx, namespace, key)?;
            assert_version(current, *if_version)?;
            crate::cas::delete_kv_blob_ref(tx, namespace, key)?;
            let stmt = tx.slot(StmtSlot::KvDelete)?;
            stmt.reset()?;
            stmt.bind_text_static(1, namespace)?;
            stmt.bind_blob_static(2, key)?;
            stmt.step()?;
            stmt.reset()?;
            let deleted = tx.conn().changes() > 0;
            Ok(disk_result(None, deleted))
        }
        BatchOperation::JsonPut {
            space,
            id,
            value,
            if_version,
        } => {
            validate_name("space", space)?;
            validate_name("id", id)?;
            let bin_doc = lilia_core::json::BinaryDoc::from_json_value(value);
            let contracts = integrity
                .map(|registry| registry.json_contracts(space))
                .unwrap_or_default();
            put_json_doc(
                tx,
                space,
                id,
                &bin_doc,
                *if_version,
                codec,
                dict_registry,
                json_options,
                &contracts,
            )
        }
        BatchOperation::JsonPutRaw {
            space,
            id,
            data,
            if_version,
        } => {
            validate_name("space", space)?;
            validate_name("id", id)?;
            let mut buf = data.clone();
            let bin_doc = lilia_core::json::parse_json_bytes_to_binary(&mut buf)?;
            let contracts = integrity
                .map(|registry| registry.json_contracts(space))
                .unwrap_or_default();
            put_json_doc(
                tx,
                space,
                id,
                &bin_doc,
                *if_version,
                codec,
                dict_registry,
                json_options,
                &contracts,
            )
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
            crate::cas::delete_json_blob_refs(tx, space, id)?;
            let stmt = tx.slot(StmtSlot::JsonDelete)?;
            stmt.reset()?;
            stmt.bind_text_static(1, space)?;
            stmt.bind_text_static(2, id)?;
            stmt.step()?;
            stmt.reset()?;
            let deleted = tx.conn().changes() > 0;
            if deleted {
                delete_json_indexes(tx, space, id)?;
                if integrity.is_some_and(|registry| registry.auto_embed_for(space).is_some()) {
                    enqueue_embedding(tx, space, id)?;
                }
            }
            Ok(disk_result(None, deleted))
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
            match (doc_id.as_deref(), doc_space) {
                (Some(did), _) => stmt.bind_text_static(6, did)?,
                (None, Some(_)) => stmt.bind_text_static(6, id)?,
                (None, None) => stmt.bind_null(6)?,
            }
            stmt.step()?;
            stmt.reset()?;
            Ok(disk_result(None, false))
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
            Ok(disk_result(None, deleted))
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
            Ok(disk_result(None, false))
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
            Ok(disk_result(None, deleted))
        }
        BatchOperation::FtsIndex {
            collection,
            doc_id,
            content,
        } => apply_fts_index(tx, collection, doc_id, content, false),
        BatchOperation::FtsDelete { collection, doc_id } => {
            apply_fts_delete(tx, collection, doc_id, false)
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
            Ok(disk_result(None, false))
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
            Ok(disk_result(None, deleted))
        }
        BatchOperation::CrdtPnCounterIncrement {
            space,
            id,
            actor_id,
            amount,
        } => {
            validate_name("space", space)?;
            validate_name("id", id)?;
            let mut stmt = tx.prepare(
                "INSERT INTO _lilia_crdt_counters(space, id, actor_id, p_count, n_count) \
                 VALUES(?1, ?2, ?3, ?4, 0) \
                 ON CONFLICT(space, id, actor_id) DO UPDATE SET p_count = p_count + excluded.p_count;",
            )?;
            stmt.bind_text_static(1, space)?;
            stmt.bind_text_static(2, id)?;
            stmt.bind_text_static(3, actor_id)?;
            stmt.bind_int64(4, i64::try_from(*amount).unwrap_or(i64::MAX))?;
            stmt.step()?;
            Ok(disk_result(None, false))
        }
        BatchOperation::CrdtPnCounterDecrement {
            space,
            id,
            actor_id,
            amount,
        } => {
            validate_name("space", space)?;
            validate_name("id", id)?;
            let mut stmt = tx.prepare(
                "INSERT INTO _lilia_crdt_counters(space, id, actor_id, p_count, n_count) \
                 VALUES(?1, ?2, ?3, 0, ?4) \
                 ON CONFLICT(space, id, actor_id) DO UPDATE SET n_count = n_count + excluded.n_count;",
            )?;
            stmt.bind_text_static(1, space)?;
            stmt.bind_text_static(2, id)?;
            stmt.bind_text_static(3, actor_id)?;
            stmt.bind_int64(4, i64::try_from(*amount).unwrap_or(i64::MAX))?;
            stmt.step()?;
            Ok(disk_result(None, false))
        }
        BatchOperation::CrdtOrSetAdd {
            space,
            id,
            actor_id,
            element,
        } => {
            validate_name("space", space)?;
            validate_name("id", id)?;
            let tag = format!("{actor_id}:{}", uuid::Uuid::new_v4());
            let mut stmt = tx.prepare(
                "INSERT INTO _lilia_crdt_orsets(space, id, element, tag, tombstone_ms) \
                 VALUES(?1, ?2, ?3, ?4, NULL);",
            )?;
            stmt.bind_text_static(1, space)?;
            stmt.bind_text_static(2, id)?;
            stmt.bind_text_static(3, element)?;
            stmt.bind_text_static(4, &tag)?;
            stmt.step()?;
            Ok(disk_result(None, false))
        }
        BatchOperation::CrdtOrSetRemove {
            space,
            id,
            element,
            timestamp_ms,
        } => {
            validate_name("space", space)?;
            validate_name("id", id)?;
            let now = timestamp_ms.unwrap_or_else(crate::storage::now_ms);
            let mut stmt = tx.prepare(
                "UPDATE _lilia_crdt_orsets SET tombstone_ms = ?1 \
                 WHERE space = ?2 AND id = ?3 AND element = ?4 AND tombstone_ms IS NULL;",
            )?;
            stmt.bind_int64(1, now)?;
            stmt.bind_text_static(2, space)?;
            stmt.bind_text_static(3, id)?;
            stmt.bind_text_static(4, element)?;
            stmt.step()?;
            let deleted = tx.conn().changes() > 0;
            Ok(disk_result(None, deleted))
        }
        BatchOperation::CrdtJsonSetField {
            space,
            id,
            actor_id,
            path,
            value,
            timestamp_ms,
        } => {
            validate_name("space", space)?;
            validate_name("id", id)?;
            let (mut doc, cur_ver) = load_crdt_doc(tx, space, id)?;
            let now = timestamp_ms.unwrap_or_else(crate::storage::now_ms);
            let clock = lilia_core::crdt::LwwClock::at(now, actor_id);
            doc.set_field(path, value.clone(), clock);
            let next_version = cur_ver + 1;
            save_crdt_doc(tx, space, id, &doc, next_version)?;
            Ok(disk_result(Some(next_version), false))
        }
        BatchOperation::CrdtJsonDeleteField {
            space,
            id,
            actor_id,
            path,
            timestamp_ms,
        } => {
            validate_name("space", space)?;
            validate_name("id", id)?;
            let (mut doc, cur_ver) = load_crdt_doc(tx, space, id)?;
            let now = timestamp_ms.unwrap_or_else(crate::storage::now_ms);
            let clock = lilia_core::crdt::LwwClock::at(now, actor_id);
            doc.delete_field(path, &clock);
            let next_version = cur_ver + 1;
            save_crdt_doc(tx, space, id, &doc, next_version)?;
            Ok(disk_result(Some(next_version), false))
        }
        BatchOperation::CrdtJsonMerge { space, id, state } => {
            validate_name("space", space)?;
            validate_name("id", id)?;
            let raw = lilia_core::compression::decompress_bytes(state)?;
            let remote_doc: lilia_core::crdt::LwwJsonDocument = serde_json::from_slice(&raw)
                .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
            let (mut doc, cur_ver) = load_crdt_doc(tx, space, id)?;
            lilia_core::crdt::CvRdt::merge(&mut doc, &remote_doc);
            let next_version = cur_ver + 1;
            save_crdt_doc(tx, space, id, &doc, next_version)?;
            Ok(disk_result(Some(next_version), false))
        }
        BatchOperation::JsonPutHlc {
            space,
            id,
            value,
            hlc,
            parent_hlc,
            strategy,
        } => {
            validate_name("space", space)?;
            validate_name("id", id)?;
            let contracts = integrity
                .map(|registry| registry.json_contracts(space))
                .unwrap_or_default();
            apply_json_put_hlc(
                tx,
                space,
                id,
                value,
                hlc.as_ref(),
                parent_hlc.as_ref(),
                *strategy,
                codec,
                dict_registry,
                json_options,
                hlc_clock,
                &contracts,
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn put_json_doc(
    tx: &mut NativeTransaction<'_>,
    space: &str,
    id: &str,
    bin_doc: &lilia_core::json::BinaryDoc,
    if_version: Option<u64>,
    codec: &dyn lilia_core::ValueCodec,
    dict_registry: &lilia_core::DictionaryRegistry,
    json_options: &crate::engine::JsonOptions,
    contracts: &JsonContracts,
) -> Result<MutationResult> {
    if let Some(check) = contracts.check.as_deref() {
        check.enforce(space, id, bin_doc)?;
    }
    let auto_embed = contracts.auto_embed.as_deref();
    let mut prev_source_text: Option<String> = None;
    let prev_doc: Option<serde_json::Value> = {
        let stmt = tx.slot(StmtSlot::JsonGet)?;
        stmt.reset()?;
        stmt.bind_text_static(1, space)?;
        stmt.bind_text_static(2, id)?;
        let res = if stmt.step()? {
            let format = stmt.column_int64(1);
            let dict_id = stmt.column_int64(2);
            crate::json::decode_stored_doc(
                stmt,
                0,
                format,
                dict_id,
                space,
                codec,
                dict_registry,
                None,
            )
                .ok()
                .map(|d| {
                    prev_source_text = auto_embed
                        .and_then(|config| config.source_text(&d))
                        .map(str::to_owned);
                    d.to_json_value()
                })
        } else {
            None
        };
        stmt.reset()?;
        res
    };

    let (encoded, format, dict_id) = crate::json::encode_doc_for_storage(
        bin_doc,
        space,
        codec,
        dict_registry,
        json_options,
    )?;

    let version = match if_version {
        None => {
            let stmt = tx.slot(StmtSlot::JsonPutReturning)?;
            stmt.reset()?;
            stmt.bind_text_static(1, space)?;
            stmt.bind_text_static(2, id)?;
            match &encoded {
                crate::json::EncodedDoc::Text(s) => stmt.bind_text_static(3, s)?,
                crate::json::EncodedDoc::Blob(b) => stmt.bind_blob_static(3, b)?,
            }
            stmt.bind_int64(4, format)?;
            stmt.bind_int64(5, dict_id)?;
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
            assert_version(current, Some(expected))?;
            let next_version = current.unwrap_or(0) + 1;
            let stmt = tx.slot(StmtSlot::JsonPutVersioned)?;
            stmt.reset()?;
            stmt.bind_text_static(1, space)?;
            stmt.bind_text_static(2, id)?;
            match &encoded {
                crate::json::EncodedDoc::Text(s) => stmt.bind_text_static(3, s)?,
                crate::json::EncodedDoc::Blob(b) => stmt.bind_blob_static(3, b)?,
            }
            stmt.bind_int64(4, format)?;
            stmt.bind_int64(5, dict_id)?;
            stmt.bind_int64(6, store_version(next_version)?)?;
            stmt.step()?;
            stmt.reset()?;
            next_version
        }
    };

    let new_val = bin_doc.to_json_value();
    let now = now_ms();
    crate::cas::sync_json_blob_refs(tx, space, id, prev_doc.as_ref(), &new_val, now)?;
    record_json_revision(tx, space, id, version, prev_doc.as_ref(), &new_val)?;
    update_json_indexes(tx, space, id, bin_doc)?;
    if let Some(config) = auto_embed {
        if prev_source_text.as_deref() != config.source_text(bin_doc) {
            enqueue_embedding(tx, space, id)?;
        }
    }

    Ok(disk_result(Some(version), false))
}

/// Queues `space/id` for the embedding worker, re-arming a document that had failed.
///
/// The queue only names the document: the worker reconciles the vector with whatever the
/// document holds when it runs, so queuing twice or queuing a deleted document is harmless.
pub(crate) fn enqueue_embedding(
    tx: &mut NativeTransaction<'_>,
    space: &str,
    id: &str,
) -> Result<()> {
    let stmt = tx.slot(StmtSlot::EmbedEnqueue)?;
    stmt.reset()?;
    stmt.bind_text_static(1, space)?;
    stmt.bind_text_static(2, id)?;
    stmt.step()?;
    stmt.reset()?;
    Ok(())
}

fn delete_json_indexes(tx: &mut NativeTransaction<'_>, space: &str, doc_id: &str) -> Result<()> {
    let entries = crate::sharding::qualified(tx.conn().write_schema(), "_lilia_json_index_entries");
    let mut stmt = tx.prepare(&format!(
        "DELETE FROM {entries} WHERE space = ?1 AND doc_id = ?2;"
    ))?;
    stmt.bind_text_static(1, space)?;
    stmt.bind_text_static(2, doc_id)?;
    stmt.step()?;
    Ok(())
}

fn update_json_indexes(
    tx: &mut NativeTransaction<'_>,
    space: &str,
    doc_id: &str,
    doc: &lilia_core::json::BinaryDoc,
) -> Result<()> {
    delete_json_indexes(tx, space, doc_id)?;
    let mut stmt =
        tx.prepare("SELECT index_name, json_path, filter_predicate FROM _lilia_json_indexes WHERE space = ?1;")?;
    stmt.bind_text_static(1, space)?;
    let mut indexes: Vec<(String, String, Option<String>)> = Vec::new();
    while stmt.step()? {
        let filter = if stmt.column_type(2) == 5 {
            None
        } else {
            let s = stmt.column_text(2);
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        };
        indexes.push((
            stmt.column_text(0).to_string(),
            stmt.column_text(1).to_string(),
            filter,
        ));
    }
    drop(stmt);
    if indexes.is_empty() {
        return Ok(());
    }
    let entries = crate::sharding::qualified(tx.conn().write_schema(), "_lilia_json_index_entries");
    let mut insert_stmt = tx.prepare(&format!(
        "INSERT OR IGNORE INTO {entries}(space, index_name, idx_value, doc_id) VALUES(?1, ?2, ?3, ?4);"
    ))?;
    for (index_name, expr_str, filter_str) in indexes {
        if let Some(ref filt_text) = filter_str {
            if let Ok(filt_expr) = lilia_core::parse_expr(filt_text) {
                if !lilia_core::eval_filter_predicate(&filt_expr, doc) {
                    continue;
                }
            }
        }
        if let Ok(expr) = lilia_core::parse_expr(&expr_str) {
            if let Ok(eval_val) = lilia_core::eval_expr(&expr, doc) {
                for val_str in eval_val.to_index_strings() {
                    insert_stmt.reset()?;
                    insert_stmt.bind_text_static(1, space)?;
                    insert_stmt.bind_text_static(2, &index_name)?;
                    insert_stmt.bind_text_static(3, &val_str)?;
                    insert_stmt.bind_text_static(4, doc_id)?;
                    insert_stmt.step()?;
                }
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

fn load_crdt_doc(
    tx: &mut NativeTransaction<'_>,
    space: &str,
    id: &str,
) -> Result<(lilia_core::crdt::LwwJsonDocument, u64)> {
    let mut stmt =
        tx.prepare("SELECT state, version FROM _lilia_crdt_json WHERE space = ?1 AND id = ?2;")?;
    stmt.bind_text_static(1, space)?;
    stmt.bind_text_static(2, id)?;
    if stmt.step()? {
        let blob = stmt.column_blob(0);
        let version = parse_version(stmt.column_int64(1))?;
        let raw = lilia_core::compression::decompress_bytes(blob)?;
        let doc: lilia_core::crdt::LwwJsonDocument = serde_json::from_slice(&raw)
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
        Ok((doc, version))
    } else {
        Ok((lilia_core::crdt::LwwJsonDocument::new(), 0))
    }
}

fn save_crdt_doc(
    tx: &mut NativeTransaction<'_>,
    space: &str,
    id: &str,
    doc: &lilia_core::crdt::LwwJsonDocument,
    version: u64,
) -> Result<()> {
    let mat_val = doc.to_value();
    let mat_str = serde_json::to_string(&mat_val)
        .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
    let raw = serde_json::to_vec(doc)
        .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
    let compressed = lilia_core::compression::compress_bytes(&raw);

    let mut stmt = tx.prepare(
        "INSERT INTO _lilia_crdt_json(space, id, state, materialized, version) \
         VALUES(?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(space, id) DO UPDATE SET \
         state = excluded.state, \
         materialized = excluded.materialized, \
         version = excluded.version;",
    )?;
    stmt.bind_text_static(1, space)?;
    stmt.bind_text_static(2, id)?;
    stmt.bind_blob_static(3, &compressed)?;
    stmt.bind_text_static(4, &mat_str)?;
    stmt.bind_int64(5, store_version(version)?)?;
    stmt.step()?;
    Ok(())
}


fn load_record_hlc(
    tx: &mut NativeTransaction<'_>,
    model: &str,
    space: &str,
    id: &str,
) -> Result<Option<lilia_core::HlcRecord>> {
    let mut stmt = tx.prepare(
        "SELECT hlc, parent_hlc, node_id, updated_at_ms FROM _lilia_record_hlc WHERE model = ?1 AND space = ?2 AND id = ?3;",
    )?;
    stmt.bind_text_static(1, model)?;
    stmt.bind_text_static(2, space)?;
    stmt.bind_text_static(3, id)?;
    if stmt.step()? {
        let hlc_str = stmt.column_text(0);
        let parent_str = if stmt.column_is_null(1) {
            None
        } else {
            Some(stmt.column_text(1))
        };
        let node_id = stmt.column_text(2).to_string();
        let updated_at_ms = stmt.column_int64(3);
        let hlc = hlc_str
            .parse::<lilia_core::Hlc>()
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
        let parent_hlc = parent_str
            .map(str::parse::<lilia_core::Hlc>)
            .transpose()
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
        Ok(Some(lilia_core::HlcRecord {
            model: model.to_string(),
            space: space.to_string(),
            id: id.to_string(),
            hlc,
            parent_hlc,
            node_id,
            updated_at_ms,
        }))
    } else {
        Ok(None)
    }
}

fn save_record_hlc(
    tx: &mut NativeTransaction<'_>,
    model: &str,
    space: &str,
    id: &str,
    hlc: &lilia_core::Hlc,
    parent_hlc: Option<&lilia_core::Hlc>,
    now_ms: i64,
) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO _lilia_record_hlc(model, space, id, hlc, parent_hlc, node_id, updated_at_ms)          VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)          ON CONFLICT(model, space, id) DO UPDATE SET          hlc = excluded.hlc, parent_hlc = excluded.parent_hlc, node_id = excluded.node_id, updated_at_ms = excluded.updated_at_ms;",
    )?;
    stmt.bind_text_static(1, model)?;
    stmt.bind_text_static(2, space)?;
    stmt.bind_text_static(3, id)?;
    let hlc_str = hlc.to_string();
    stmt.bind_text_static(4, &hlc_str)?;
    let parent_str = parent_hlc.map(ToString::to_string);
    match &parent_str {
        Some(p) => stmt.bind_text_static(5, p)?,
        None => stmt.bind_null(5)?,
    }
    stmt.bind_text_static(6, &hlc.node_id)?;
    stmt.bind_int64(7, now_ms)?;
    stmt.step()?;
    Ok(())
}

fn load_base_doc(
    tx: &mut NativeTransaction<'_>,
    space: &str,
    id: &str,
    base_version: u64,
) -> Option<serde_json::Value> {
    if base_version == 0 {
        return None;
    }
    let mut stmt = tx.prepare(
        "SELECT delta, is_keyframe FROM _lilia_json_revisions WHERE space = ?1 AND id = ?2 AND version = ?3;",
    ).ok()?;
    stmt.bind_text_static(1, space).ok()?;
    stmt.bind_text_static(2, id).ok()?;
    stmt.bind_int64(3, i64::try_from(base_version).ok()?).ok()?;
    if stmt.step().ok()? {
        let is_keyframe = stmt.column_int64(1);
        let blob = stmt.column_blob(0);
        if is_keyframe == 1 {
            let decomp = lilia_core::decompress_bytes(blob).ok()?;
            serde_json::from_slice(&decomp).ok()
        } else {
            None
        }
    } else {
        None
    }
}

#[allow(clippy::too_many_arguments)]
fn record_conflict_in_db(
    tx: &mut NativeTransaction<'_>,
    space: &str,
    id: &str,
    base_hlc: Option<&lilia_core::Hlc>,
    ours_hlc: &lilia_core::Hlc,
    theirs_hlc: &lilia_core::Hlc,
    ours_val: &serde_json::Value,
    theirs_val: &serde_json::Value,
    resolved_val: &serde_json::Value,
    strategy: lilia_core::ConflictResolutionStrategy,
    now: i64,
) -> Result<()> {
    let base_s = base_hlc.map(ToString::to_string);
    let ours_hlc_s = ours_hlc.to_string();
    let theirs_hlc_s = theirs_hlc.to_string();
    let ours_s = serde_json::to_string(ours_val).unwrap_or_default();
    let theirs_s = serde_json::to_string(theirs_val).unwrap_or_default();
    let resolved_s = if matches!(strategy, lilia_core::ConflictResolutionStrategy::Manual) {
        None
    } else {
        Some(serde_json::to_string(resolved_val).unwrap_or_default())
    };
    let strat_str = format!("{strategy:?}");
    let status_str = if matches!(strategy, lilia_core::ConflictResolutionStrategy::Manual) {
        "unresolved"
    } else {
        "resolved"
    };

    let mut stmt_conf = tx.prepare(
        "INSERT INTO _lilia_conflicts(model, space, doc_id, base_hlc, ours_hlc, theirs_hlc, ours_value, theirs_value, resolved_value, resolution_strategy, status, detected_at_ms) \
         VALUES('json', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11);",
    )?;
    stmt_conf.bind_text_static(1, space)?;
    stmt_conf.bind_text_static(2, id)?;
    match &base_s {
        Some(p) => stmt_conf.bind_text_static(3, p)?,
        None => stmt_conf.bind_null(3)?,
    }
    stmt_conf.bind_text_static(4, &ours_hlc_s)?;
    stmt_conf.bind_text_static(5, &theirs_hlc_s)?;
    stmt_conf.bind_text_static(6, &ours_s)?;
    stmt_conf.bind_text_static(7, &theirs_s)?;
    match &resolved_s {
        Some(r) => stmt_conf.bind_text_static(8, r)?,
        None => stmt_conf.bind_null(8)?,
    }
    stmt_conf.bind_text_static(9, &strat_str)?;
    stmt_conf.bind_text_static(10, status_str)?;
    stmt_conf.bind_int64(11, now)?;
    stmt_conf.step()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_json_put_hlc(
    tx: &mut NativeTransaction<'_>,
    space: &str,
    id: &str,
    value: &serde_json::Value,
    hlc: Option<&lilia_core::Hlc>,
    parent_hlc: Option<&lilia_core::Hlc>,
    strategy: lilia_core::ConflictResolutionStrategy,
    codec: &dyn lilia_core::ValueCodec,
    dict_registry: &lilia_core::DictionaryRegistry,
    json_options: &crate::engine::JsonOptions,
    hlc_clock: &lilia_core::HybridClock,
    contracts: &JsonContracts,
) -> Result<MutationResult> {
    let incoming_hlc = match hlc {
        Some(h) => {
            let _ = hlc_clock.update(h);
            h.clone()
        }
        None => hlc_clock.now(),
    };

    let current_rec = load_record_hlc(tx, "json", space, id)?;
    let prev_doc: Option<serde_json::Value> = {
        let stmt = tx.slot(StmtSlot::JsonGet)?;
        stmt.reset()?;
        stmt.bind_text_static(1, space)?;
        stmt.bind_text_static(2, id)?;
        let res = if stmt.step()? {
            let format = stmt.column_int64(1);
            let dict_id = stmt.column_int64(2);
            crate::json::decode_stored_doc(
                stmt,
                0,
                format,
                dict_id,
                space,
                codec,
                dict_registry,
                None,
            )
                .ok()
                .map(|d| d.to_json_value())
        } else {
            None
        };
        stmt.reset()?;
        res
    };

    let incoming_parent = match parent_hlc {
        Some(p) => Some(p.clone()),
        None if hlc.is_none() => current_rec.as_ref().map(|r| r.hlc.clone()),
        None => None,
    };

    let now = crate::storage::now_ms();

    let (final_val, final_hlc, final_parent, conflict_detected) = match (current_rec, prev_doc) {
        (None, _) | (Some(_), None) => {
            (value.clone(), incoming_hlc, incoming_parent, false)
        }
        (Some(curr), Some(curr_val)) => {
            let relation = lilia_core::detect_causality(
                &curr.hlc,
                curr.parent_hlc.as_ref(),
                &incoming_hlc,
                incoming_parent.as_ref(),
            );

            match relation {
                lilia_core::CausalRelation::Ancestor => {
                    (value.clone(), incoming_hlc, Some(curr.hlc), false)
                }
                lilia_core::CausalRelation::Descendant | lilia_core::CausalRelation::Identical => {
                    (curr_val, curr.hlc, curr.parent_hlc, false)
                }
                lilia_core::CausalRelation::Concurrent => {
                    let base_doc = load_base_doc(tx, space, id, 1);

                    let (resolved_val, winning_hlc) = match strategy {
                        lilia_core::ConflictResolutionStrategy::LastWriteWins => {
                            if incoming_hlc > curr.hlc {
                                (value.clone(), incoming_hlc.clone())
                            } else {
                                (curr_val.clone(), curr.hlc.clone())
                            }
                        }
                        lilia_core::ConflictResolutionStrategy::ThreeWayMerge => {
                            let merge_res = lilia_core::three_way_merge(
                                base_doc.as_ref(),
                                &curr_val,
                                value,
                                &curr.hlc,
                                &incoming_hlc,
                            );
                            let merged_h = hlc_clock.tick_at(now.max(curr.hlc.millis).max(incoming_hlc.millis));
                            (merge_res.merged_value, merged_h)
                        }
                        lilia_core::ConflictResolutionStrategy::Manual => {
                            (curr_val.clone(), curr.hlc.clone())
                        }
                    };

                    record_conflict_in_db(
                        tx,
                        space,
                        id,
                        incoming_parent.as_ref(),
                        &curr.hlc,
                        &incoming_hlc,
                        &curr_val,
                        value,
                        &resolved_val,
                        strategy,
                        now,
                    )?;

                    (resolved_val, winning_hlc, Some(curr.hlc), true)
                }
            }
        }
    };

    let bin_doc = lilia_core::json::BinaryDoc::from_json_value(&final_val);
    let mut mut_res = put_json_doc(
        tx,
        space,
        id,
        &bin_doc,
        None,
        codec,
        dict_registry,
        json_options,
        contracts,
    )?;

    save_record_hlc(tx, "json", space, id, &final_hlc, final_parent.as_ref(), now)?;

    mut_res.hlc = Some(final_hlc);
    mut_res.conflict_detected = Some(conflict_detected);

    Ok(mut_res)
}

fn sanitize_savepoint_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if s.is_empty() {
        "unnamed".to_string()
    } else {
        s
    }
}

#[derive(Debug)]
pub(crate) enum MemUndoOp {
    KvRestore {
        namespace: String,
        key: Vec<u8>,
        previous: Option<crate::mem_engine::MemSnapshot<Vec<u8>>>,
    },
    JsonRestore {
        space: String,
        id: Vec<u8>,
        previous: Option<crate::mem_engine::MemSnapshot<serde_json::Value>>,
    },
}

#[derive(Debug)]
pub(crate) struct SavepointFrame {
    pub(crate) name: String,
    pub(crate) sp_id: String,
    pub(crate) mem_undo: Vec<MemUndoOp>,
}

/// Active multi-model database transaction supporting nested declarative savepoint trees.
#[derive(Debug)]
pub struct Transaction<'a> {
    db: &'a Database,
    writer: WriterGuard<'a>,
    savepoint_stack: Vec<SavepointFrame>,
    savepoint_seq: u64,
    committed: bool,
}

impl<'a> Transaction<'a> {
    pub(crate) fn new(db: &'a Database, writer: WriterGuard<'a>) -> Self {
        Self {
            db,
            writer,
            savepoint_stack: Vec::new(),
            savepoint_seq: 0,
            committed: false,
        }
    }

    #[must_use]
    pub fn db(&self) -> &'a Database {
        self.db
    }

    /// Returns the stack of active nested savepoint names from root to tip.
    #[must_use]
    pub fn active_savepoints(&self) -> Vec<&str> {
        self.savepoint_stack.iter().map(|f| f.name.as_str()).collect()
    }

    pub fn commit(mut self) -> Result<()> {
        if self.committed {
            return Ok(());
        }
        while let Some(frame) = self.savepoint_stack.pop() {
            self.writer.release_savepoint(&frame.sp_id)?;
        }
        self.writer.commit()?;
        self.committed = true;
        Ok(())
    }

    pub fn rollback(mut self) -> Result<()> {
        if self.committed {
            return Ok(());
        }
        while let Some(frame) = self.savepoint_stack.pop() {
            self.revert_mem_undo(frame.mem_undo);
        }
        self.writer.rollback()?;
        self.committed = true;
        Ok(())
    }

    pub fn spans_savepoint<F, R>(&mut self, name: &str, f: F) -> Result<R>
    where
        F: FnOnce(&mut Transaction<'_>) -> Result<R>,
    {
        if self.committed {
            return Err(LiliaError::new(
                ErrorCode::Conflict,
                "cannot open savepoint on committed transaction",
                false,
            ));
        }
        self.savepoint_seq += 1;
        let clean_name = sanitize_savepoint_name(name);
        let sp_id = format!("sp_{}_{}", self.savepoint_seq, clean_name);
        self.writer.savepoint(&sp_id)?;
        self.savepoint_stack.push(SavepointFrame {
            name: name.to_string(),
            sp_id: sp_id.clone(),
            mem_undo: Vec::new(),
        });

        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            f(self)
        }));

        match res {
            Ok(Ok(val)) => {
                self.writer.release_savepoint(&sp_id)?;
                if let Some(frame) = self.savepoint_stack.pop() {
                    if let Some(parent) = self.savepoint_stack.last_mut() {
                        parent.mem_undo.extend(frame.mem_undo);
                    }
                }
                Ok(val)
            }
            Ok(Err(err)) => {
                self.writer.rollback_to_savepoint(&sp_id)?;
                self.writer.release_savepoint(&sp_id)?;
                if let Some(frame) = self.savepoint_stack.pop() {
                    self.revert_mem_undo(frame.mem_undo);
                }
                Err(err)
            }
            Err(panic_payload) => {
                let _ = self.writer.rollback_to_savepoint(&sp_id);
                let _ = self.writer.release_savepoint(&sp_id);
                if let Some(frame) = self.savepoint_stack.pop() {
                    self.revert_mem_undo(frame.mem_undo);
                }
                let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                    (*s).to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic/trap in savepoint".to_string()
                };
                Err(LiliaError::new(
                    ErrorCode::Storage,
                    format!("savepoint '{name}' aborted due to panic/trap: {msg}"),
                    false,
                ))
            }
        }
    }

    pub fn execute_sandboxed_plugin<F, R>(&mut self, plugin_name: &str, f: F) -> Result<R>
    where
        F: FnOnce(&mut Transaction<'_>) -> Result<R>,
    {
        let sp_name = format!("wasm_plugin_{plugin_name}");
        self.spans_savepoint(&sp_name, f)
    }

    fn revert_mem_undo(&self, undo_ops: Vec<MemUndoOp>) {
        let now = crate::storage::now_ms();
        for op in undo_ops.into_iter().rev() {
            match op {
                MemUndoOp::KvRestore { namespace, key, previous } => {
                    match previous {
                        Some(snap) => {
                            let _ = self.db.mem_engine.kv_put_direct(
                                &namespace,
                                &key,
                                snap.value,
                                None,
                                snap.expires_at_ms,
                            );
                        }
                        None => {
                            let _ = self.db.mem_engine.kv_delete_direct(&namespace, &key, None);
                        }
                    }
                }
                MemUndoOp::JsonRestore { space, id, previous } => {
                    let id_str = String::from_utf8_lossy(&id);
                    match previous {
                        Some(snap) => {
                            let _ = self.db.mem_engine.json_put_direct(
                                &space,
                                &id_str,
                                snap.value,
                                None,
                            );
                        }
                        None => {
                            let _ = self.db.mem_engine.json.delete(&space, &id, None, now);
                        }
                    }
                }
            }
        }
    }

    pub fn json(&mut self) -> TxJson<'_, 'a> {
        TxJson { tx: self }
    }

    pub fn graph(&mut self) -> TxGraph<'_, 'a> {
        self.graph_named("default")
    }

    pub fn graph_named(&mut self, graph: &str) -> TxGraph<'_, 'a> {
        TxGraph {
            tx: self,
            graph_name: graph.to_string(),
        }
    }

    pub fn kv(&mut self) -> TxKv<'_, 'a> {
        TxKv { tx: self }
    }

    pub fn fts(&mut self) -> TxFts<'_, 'a> {
        TxFts { tx: self }
    }

    pub fn vector(&mut self) -> TxVector<'_, 'a> {
        TxVector { tx: self }
    }

    pub fn apply_op(&mut self, operation: BatchOperation) -> Result<MutationResult> {
        if self.committed {
            return Err(LiliaError::new(
                ErrorCode::Conflict,
                "cannot apply operation on committed transaction",
                false,
            ));
        }
        use lilia_core::{CollectionKind, StorageTier};
        let (kind, name) = crate::engine::op_collection(&operation);
        let tier = self.db.catalog.tier_for(kind, name);

        // Apply FLE if JsonPut
        let effective_op = match &operation {
            BatchOperation::JsonPut { space, id, value, if_version } => {
                let spec = self.db.catalog.spec_for(CollectionKind::Json, space);
                if let Some(policy) = &spec.fle_policy {
                    let mut encrypted_val = value.clone();
                    lilia_core::encrypt_json_fields(&mut encrypted_val, policy, &self.db.key_registry)?;
                    BatchOperation::JsonPut {
                        space: space.clone(),
                        id: id.clone(),
                        value: encrypted_val,
                        if_version: *if_version,
                    }
                } else {
                    operation
                }
            }
            _ => operation,
        };

        let now = crate::storage::now_ms();

        if tier == StorageTier::Memory {
            if kind == CollectionKind::Fts {
                let mut native_tx = NativeTransaction::wrap_active(&mut self.writer);
                let mut res = match &effective_op {
                    BatchOperation::FtsIndex { collection, doc_id, content } => {
                        apply_fts_index(&mut native_tx, collection, doc_id, content, true)?
                    }
                    BatchOperation::FtsDelete { collection, doc_id } => {
                        apply_fts_delete(&mut native_tx, collection, doc_id, true)?
                    }
                    _ => unreachable!("non-FTS memory op"),
                };
                res.tier = StorageTier::Memory;
                return Ok(res);
            }

            if let Some(frame) = self.savepoint_stack.last_mut() {
                match &effective_op {
                    BatchOperation::KvSet { namespace, key, .. }
                    | BatchOperation::KvDelete { namespace, key, .. } => {
                        let prev = self.db.mem_engine.kv.get(namespace, key, now, true);
                        frame.mem_undo.push(MemUndoOp::KvRestore {
                            namespace: namespace.clone(),
                            key: key.clone(),
                            previous: prev,
                        });
                    }
                    BatchOperation::JsonPut { space, id, .. }
                    | BatchOperation::JsonDelete { space, id, .. } => {
                        let prev = self.db.mem_engine.json.get(space, id.as_bytes(), now, true);
                        frame.mem_undo.push(MemUndoOp::JsonRestore {
                            space: space.clone(),
                            id: id.as_bytes().to_vec(),
                            previous: prev,
                        });
                    }
                    _ => {}
                }
            }

            let op_refs = [&effective_op];
            let mut guards = self.db.mem_engine.lock_kv_json_shards(&op_refs);
            self.db.mem_engine.preflight_with_guards(&op_refs, &guards, now)?;
            let res = self.db.mem_engine.apply_op_with_guards(
                &effective_op,
                &mut guards,
                self.db.codec.as_ref(),
                self.db.hlc.as_ref(),
                now,
            )?;
            self.db.mem_engine.finish_guards(guards, now);
            return Ok(res);
        }

        let mut native_tx = NativeTransaction::wrap_active(&mut self.writer);
        let mut res = apply(
            &mut native_tx,
            &effective_op,
            self.db.codec.as_ref(),
            &self.db.dict_registry,
            &self.db.json_options,
            self.db.hlc.as_ref(),
            &self.db.cas,
            self.db.shard_manager.as_deref(),
            Some(self.db.integrity.as_ref()),
        )?;
        if tier == StorageTier::Buffered {
            res.tier = StorageTier::Buffered;
        }
        match &effective_op {
            BatchOperation::VectorSet { collection, id, dim, element_type, vector } => {
                self.db.sync_hnsw_insert(collection, id, *dim, *element_type, vector, &mut native_tx)?;
            }
            BatchOperation::VectorDelete { collection, id } => {
                self.db.sync_hnsw_delete(collection, id, &mut native_tx)?;
            }
            _ => {}
        }
        Ok(res)
    }

    pub fn exec(&mut self, sql: &str) -> Result<()> {
        self.writer.exec(sql)
    }

    pub fn query_int(&mut self, sql: &str) -> Result<i64> {
        self.writer.query_int(sql)
    }

    pub fn json_get(&mut self, space: &str, id: &str) -> Result<Option<lilia_core::JsonEntry>> {
        validate_name("space", space)?;
        validate_name("id", id)?;
        let tier = self.db.catalog.tier_for(lilia_core::CollectionKind::Json, space);
        if tier == lilia_core::StorageTier::Memory {
            let now = crate::storage::now_ms();
            return Ok(self.db.mem_engine.json.get(space, id.as_bytes(), now, true).map(|snap| {
                let mut val = snap.value.clone();
                let _ = lilia_core::decrypt_json_fields(
                    &mut val,
                    &self.db.key_registry,
                    lilia_core::DecryptionMode::Tolerant,
                );
                crate::json::json_snapshot_to_entry(space, id.to_owned(), &val, snap.version)
            }));
        }

        let stmt = self.writer.slot(StmtSlot::JsonGet)?;
        stmt.reset()?;
        stmt.bind_text_static(1, space)?;
        stmt.bind_text_static(2, id)?;
        if stmt.step()? {
            let format = stmt.column_int64(1);
            let dict_id = stmt.column_int64(2);
            let version = parse_version(stmt.column_int64(3))?;
            let doc = crate::json::decode_stored_doc(
                stmt,
                0,
                format,
                dict_id,
                space,
                self.db.codec.as_ref(),
                &self.db.dict_registry,
                Some(self.db.key_registry.as_ref()),
            )?;
            Ok(Some(lilia_core::JsonEntry::new(space, id, doc, version)))
        } else {
            Ok(None)
        }
    }

    pub fn kv_get(&mut self, namespace: &str, key: &[u8]) -> Result<Option<lilia_core::KvEntry>> {
        validate_name("namespace", namespace)?;
        let tier = self.db.catalog.tier_for(lilia_core::CollectionKind::Kv, namespace);
        let now = now_ms();
        if tier == lilia_core::StorageTier::Memory {
            let Some(snap) = self.db.mem_engine.kv.get(namespace, key, now, true) else {
                return Ok(None);
            };
            let value = self.db.codec.decode(&snap.value)?;
            return Ok(Some(lilia_core::KvEntry {
                namespace: namespace.to_owned(),
                key: key.to_vec(),
                value,
                version: snap.version,
                expires_at_ms: snap.expires_at_ms,
            }));
        }

        let stmt = self.writer.slot(StmtSlot::KvGet)?;
        stmt.reset()?;
        stmt.bind_text_static(1, namespace)?;
        stmt.bind_blob_static(2, key)?;
        stmt.bind_int64(3, now)?;
        if stmt.step()? {
            let raw_val = stmt.column_blob(0);
            let version = parse_version(stmt.column_int64(1))?;
            let expires_at_ms = if stmt.column_is_null(2) {
                None
            } else {
                Some(stmt.column_int64(2))
            };
            let value = self.db.codec.decode(raw_val)?;
            Ok(Some(lilia_core::KvEntry {
                namespace: namespace.to_owned(),
                key: key.to_vec(),
                value,
                version,
                expires_at_ms,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn graph_has_edge(
        &mut self,
        graph: &str,
        source: &str,
        target: &str,
        kind: &str,
    ) -> Result<bool> {
        validate_name("graph", graph)?;
        validate_name("source", source)?;
        validate_name("target", target)?;
        validate_name("kind", kind)?;
        let tier = self.db.catalog.tier_for(lilia_core::CollectionKind::Graph, graph);
        if tier == lilia_core::StorageTier::Memory {
            return Ok(self
                .db
                .mem_engine
                .graph_edges
                .scan(Some(graph), None, usize::MAX, now_ms())
                .into_iter()
                .any(|(_, _, snap)| {
                    snap.value.source == source && snap.value.target == target && snap.value.kind == kind
                }));
        }
        let sql = "SELECT 1 FROM _lilia_graph_edges WHERE graph = ?1 AND source = ?2 AND target = ?3 AND kind = ?4 LIMIT 1;";
        let mut stmt = self.writer.prepare(sql)?;
        stmt.bind_text_static(1, graph)?;
        stmt.bind_text_static(2, source)?;
        stmt.bind_text_static(3, target)?;
        stmt.bind_text_static(4, kind)?;
        stmt.step()
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.committed {
            while let Some(frame) = self.savepoint_stack.pop() {
                self.revert_mem_undo(frame.mem_undo);
            }
            let _ = self.writer.rollback();
        }
    }
}

/// JSON document operations within a transaction.
#[derive(Debug)]
pub struct TxJson<'t, 'a> {
    tx: &'t mut Transaction<'a>,
}

impl<'t, 'a> TxJson<'t, 'a> {
    pub fn put<T: serde::Serialize>(
        &mut self,
        space: &str,
        id: &str,
        value: &T,
    ) -> Result<MutationResult> {
        let val = serde_json::to_value(value).map_err(|e| {
            LiliaError::new(ErrorCode::InvalidInput, e.to_string(), false)
        })?;
        self.tx.apply_op(BatchOperation::JsonPut {
            space: space.to_owned(),
            id: id.to_owned(),
            value: val,
            if_version: None,
        })
    }

    pub fn put_value(
        &mut self,
        space: &str,
        id: &str,
        value: serde_json::Value,
    ) -> Result<MutationResult> {
        self.tx.apply_op(BatchOperation::JsonPut {
            space: space.to_owned(),
            id: id.to_owned(),
            value,
            if_version: None,
        })
    }

    pub fn delete(&mut self, space: &str, id: &str) -> Result<MutationResult> {
        self.tx.apply_op(BatchOperation::JsonDelete {
            space: space.to_owned(),
            id: id.to_owned(),
            if_version: None,
        })
    }

    pub fn get(&mut self, space: &str, id: &str) -> Result<Option<lilia_core::JsonEntry>> {
        self.tx.json_get(space, id)
    }
}

/// Graph operations within a transaction.
#[derive(Debug)]
pub struct TxGraph<'t, 'a> {
    tx: &'t mut Transaction<'a>,
    graph_name: String,
}

impl<'t, 'a> TxGraph<'t, 'a> {
    pub fn add_edge(&mut self, source: &str, target: &str, kind: &str) -> Result<()> {
        self.add_edge_weighted(source, target, kind, 1.0)
    }

    pub fn add_edge_weighted(
        &mut self,
        source: &str,
        target: &str,
        kind: &str,
        weight: f64,
    ) -> Result<()> {
        self.tx.apply_op(BatchOperation::GraphAddEdge {
            graph: self.graph_name.clone(),
            source: source.to_owned(),
            target: target.to_owned(),
            kind: kind.to_owned(),
            weight,
        })?;
        Ok(())
    }

    pub fn remove_edge(&mut self, source: &str, target: &str, kind: &str) -> Result<bool> {
        let res = self.tx.apply_op(BatchOperation::GraphRemoveEdge {
            graph: self.graph_name.clone(),
            source: source.to_owned(),
            target: target.to_owned(),
            kind: kind.to_owned(),
        })?;
        Ok(res.deleted)
    }

    pub fn set_node(
        &mut self,
        id: &str,
        kind: &str,
        metadata: &serde_json::Value,
    ) -> Result<()> {
        self.tx.apply_op(BatchOperation::GraphSetNode {
            graph: self.graph_name.clone(),
            id: id.to_owned(),
            kind: kind.to_owned(),
            metadata: metadata.clone(),
            doc_space: None,
            doc_id: None,
        })?;
        Ok(())
    }

    pub fn delete_node(&mut self, id: &str) -> Result<bool> {
        let res = self.tx.apply_op(BatchOperation::GraphDeleteNode {
            graph: self.graph_name.clone(),
            id: id.to_owned(),
        })?;
        Ok(res.deleted)
    }

    pub fn has_edge(&mut self, source: &str, target: &str, kind: &str) -> Result<bool> {
        self.tx.graph_has_edge(&self.graph_name, source, target, kind)
    }
}

/// Key-Value operations within a transaction.
#[derive(Debug)]
pub struct TxKv<'t, 'a> {
    tx: &'t mut Transaction<'a>,
}

impl<'t, 'a> TxKv<'t, 'a> {
    pub fn set(&mut self, namespace: &str, key: &[u8], value: &[u8]) -> Result<MutationResult> {
        self.tx.apply_op(BatchOperation::KvSet {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
            value: value.to_vec(),
            if_version: None,
            expires_at_ms: None,
        })
    }

    pub fn set_with_ttl(
        &mut self,
        namespace: &str,
        key: &[u8],
        value: &[u8],
        expires_at_ms: i64,
    ) -> Result<MutationResult> {
        self.tx.apply_op(BatchOperation::KvSet {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
            value: value.to_vec(),
            if_version: None,
            expires_at_ms: Some(expires_at_ms),
        })
    }

    pub fn delete(&mut self, namespace: &str, key: &[u8]) -> Result<MutationResult> {
        self.tx.apply_op(BatchOperation::KvDelete {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
            if_version: None,
        })
    }

    pub fn get(&mut self, namespace: &str, key: &[u8]) -> Result<Option<lilia_core::KvEntry>> {
        self.tx.kv_get(namespace, key)
    }
}

/// Full-text search operations within a transaction.
#[derive(Debug)]
pub struct TxFts<'t, 'a> {
    tx: &'t mut Transaction<'a>,
}

impl<'t, 'a> TxFts<'t, 'a> {
    pub fn index(&mut self, collection: &str, doc_id: &str, content: &str) -> Result<()> {
        self.tx.apply_op(BatchOperation::FtsIndex {
            collection: collection.to_owned(),
            doc_id: doc_id.to_owned(),
            content: content.to_owned(),
        })?;
        Ok(())
    }

    pub fn delete(&mut self, collection: &str, doc_id: &str) -> Result<bool> {
        let res = self.tx.apply_op(BatchOperation::FtsDelete {
            collection: collection.to_owned(),
            doc_id: doc_id.to_owned(),
        })?;
        Ok(res.deleted)
    }
}

/// Vector index operations within a transaction.
#[derive(Debug)]
pub struct TxVector<'t, 'a> {
    tx: &'t mut Transaction<'a>,
}

impl<'t, 'a> TxVector<'t, 'a> {
    pub fn set(
        &mut self,
        collection: &str,
        id: &str,
        dim: u32,
        element_type: lilia_core::VectorElementType,
        vector: &[u8],
    ) -> Result<()> {
        self.tx.apply_op(BatchOperation::VectorSet {
            collection: collection.to_owned(),
            id: id.to_owned(),
            dim,
            element_type,
            vector: vector.to_vec(),
        })?;
        Ok(())
    }

    pub fn delete(&mut self, collection: &str, id: &str) -> Result<bool> {
        let res = self.tx.apply_op(BatchOperation::VectorDelete {
            collection: collection.to_owned(),
            id: id.to_owned(),
        })?;
        Ok(res.deleted)
    }
}
