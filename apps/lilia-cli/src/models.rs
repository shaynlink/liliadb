use std::fs;
use std::path::Path;

use lilia_core::{BatchOperation, VectorElementType};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::{json, Value};

use base64::Engine;

use crate::args::{
    BlobCommand, Command, CrdtCommand, CrdtCounterCommand, CrdtJsonCommand, CrdtSetCommand,
    Encoding, FtsCommand, GraphCommand, HybridCommand, IntegrityCommand, JsonCommand, KvCommand,
    LedgerCommand, VectorCommand,
};

pub(crate) fn resolve_plugin_path(plugin: &Path) -> anyhow::Result<std::path::PathBuf> {
    if plugin.exists() {
        if plugin.join("package").join("manifest.json").is_file() {
            return Ok(plugin.join("package"));
        }
        return Ok(plugin.to_path_buf());
    }

    let query = plugin.to_str().unwrap_or_default();
    if let Ok(root) = lilia_plugin_api::default_plugin_root() {
        if let Ok(Some(installed)) = lilia_plugin_api::find_installed(&root, query) {
            return Ok(installed.package_path);
        }
    }

    anyhow::bail!(
        "plugin `{}` not found as a file or directory, and not installed in default plugin store",
        plugin.display()
    );
}

#[allow(unsafe_code)]
pub(crate) fn execute(
    command: Command,
    path: &Path,
    plugin: Option<&Path>,
) -> anyhow::Result<Value> {
    let mut options = DatabaseOptions::durable(path);
    let mut sql_plugin = None;
    let _loaded = if let Some(plugin_arg) = plugin {
        let plugin_path = resolve_plugin_path(plugin_arg)?;
        let loaded = unsafe { lilia_plugin_api::load_plugin(&plugin_path)? };
        if let Some(codec) = loaded.codec() {
            options = options.with_codec(std::sync::Arc::new(codec));
        }
        if let Some(sql) = loaded.sql() {
            sql_plugin = Some(sql);
        }
        Some(loaded)
    } else {
        None
    };
    let database = Database::open(options)?;
    Ok(match command {
        Command::Init => json!({"ok": true, "path": database.path()}),
        Command::Doctor { deep } => json!({
            "ok": database.integrity_check()?,
            "path": database.path(),
            "journal": "wal",
            "synchronous": "full",
            "deep": deep,
            "checkpoint": if deep { database.checkpoint().is_ok() } else { true }
        }),
        Command::Backup { destination } => {
            database.backup(&destination)?;
            json!({"ok": true, "destination": destination})
        }
        Command::Kv { command } => kv(&database, command)?,
        Command::Json { command } => json_model(&database, command)?,
        Command::Graph { command } => graph(&database, command)?,
        Command::Fts { command } => fts(&database, command)?,
        Command::Vector { command } => vector(&database, command)?,
        Command::Hybrid { command } => hybrid(&database, command)?,
        Command::Crdt { command } => crdt(&database, command)?,
        Command::Blob { command } => blob(&database, command)?,
        Command::Integrity { command } => integrity_model(&database, command)?,
        Command::Ledger { command } => ledger_model(&database, command)?,
        Command::Batch { file } => {
            let operations: Vec<BatchOperation> = serde_json::from_slice(&fs::read(file)?)?;
            serde_json::to_value(database.batch(&operations)?)?
        }
        Command::Sql { query, params } => {
            if let Some(sql) = &sql_plugin {
                sql.validate_query(&query)?;
            }
            let json_params: Vec<Value> = params
                .iter()
                .map(|p| serde_json::from_str(p).unwrap_or_else(|_| Value::String(p.clone())))
                .collect();
            let result = database.sql_query(&query, &json_params)?;
            serde_json::to_value(result)?
        }
        Command::Plugin { .. } | Command::Daemon { .. } => unreachable!(),
    })
}

fn decode_binary(raw: &str, encoding: Encoding, field: &str) -> lilia_core::Result<Vec<u8>> {
    match encoding {
        Encoding::Text => Ok(raw.as_bytes().to_vec()),
        Encoding::Hex => hex::decode(raw).map_err(|error| {
            lilia_core::LiliaError::new(
                lilia_core::ErrorCode::InvalidInput,
                format!("invalid {field} hex encoding: {error}"),
                false,
            )
        }),
        Encoding::Base64 => base64::engine::general_purpose::STANDARD
            .decode(raw)
            .map_err(|error| {
                lilia_core::LiliaError::new(
                    lilia_core::ErrorCode::InvalidInput,
                    format!("invalid {field} base64 encoding: {error}"),
                    false,
                )
            }),
    }
}

fn kv(database: &Database, command: KvCommand) -> anyhow::Result<Value> {
    let mutate =
        |operation| Ok::<_, anyhow::Error>(serde_json::to_value(database.batch(&[operation])?)?);
    match command {
        KvCommand::Get {
            namespace,
            key,
            encoding,
            key_encoding,
        } => {
            let key = decode_binary(&key, key_encoding.unwrap_or(encoding), "key")?;
            Ok(serde_json::to_value(database.kv_get(&namespace, &key)?)?)
        }
        KvCommand::Set {
            namespace,
            key,
            value,
            if_version,
            expires_at_ms,
            encoding,
            key_encoding,
            value_encoding,
        } => {
            let key = decode_binary(&key, key_encoding.unwrap_or(encoding), "key")?;
            let value = decode_binary(&value, value_encoding.unwrap_or(encoding), "value")?;
            mutate(BatchOperation::KvSet {
                namespace,
                key,
                value,
                if_version,
                expires_at_ms,
            })
        }
        KvCommand::Delete {
            namespace,
            key,
            if_version,
            encoding,
            key_encoding,
        } => {
            let key = decode_binary(&key, key_encoding.unwrap_or(encoding), "key")?;
            mutate(BatchOperation::KvDelete {
                namespace,
                key,
                if_version,
            })
        }
        KvCommand::Scan {
            namespace,
            after,
            limit,
            encoding,
            key_encoding,
        } => {
            let key_enc = key_encoding.unwrap_or(encoding);
            let after = after
                .as_deref()
                .map(|cursor| decode_binary(cursor, key_enc, "after"))
                .transpose()?;
            Ok(serde_json::to_value(database.kv_scan(
                &namespace,
                after.as_deref(),
                limit,
            )?)?)
        }
    }
}

fn json_model(database: &Database, command: JsonCommand) -> anyhow::Result<Value> {
    let mutate =
        |operation| Ok::<_, anyhow::Error>(serde_json::to_value(database.batch(&[operation])?)?);
    match command {
        JsonCommand::Get { space, id } => {
            Ok(serde_json::to_value(database.json_get(&space, &id)?)?)
        }
        JsonCommand::Put {
            space,
            id,
            value,
            if_version,
        } => mutate(BatchOperation::JsonPut {
            space,
            id,
            value: serde_json::from_str(&value)?,
            if_version,
        }),
        JsonCommand::Delete {
            space,
            id,
            if_version,
        } => mutate(BatchOperation::JsonDelete {
            space,
            id,
            if_version,
        }),
        JsonCommand::Scan {
            space,
            after,
            limit,
        } => Ok(serde_json::to_value(database.json_scan(
            &space,
            after.as_deref(),
            limit,
        )?)?),
        JsonCommand::CreateCollection { space, check } => {
            database.json().create_collection(
                &space,
                lilia_core::CollectionOptions {
                    check,
                    auto_embed: None,
                },
            )?;
            Ok(json!({ "ok": true, "space": space }))
        }
        JsonCommand::SetCheck { space, check } => {
            database.json().set_check(&space, check.as_deref())?;
            Ok(json!({ "ok": true, "space": space, "check": check }))
        }
    }
}

fn integrity_model(database: &Database, command: IntegrityCommand) -> anyhow::Result<Value> {
    match command {
        IntegrityCommand::AddGraphRef {
            graph,
            target,
            name,
            on_delete,
            required,
        } => {
            let action = match on_delete.to_lowercase().as_str() {
                "cascade" => lilia_core::RefAction::Cascade,
                "restrict" => lilia_core::RefAction::Restrict,
                "set_null" => lilia_core::RefAction::SetNull,
                other => anyhow::bail!("unknown on-delete action '{other}': expected cascade, restrict, or set_null"),
            };
            let mut reference = lilia_core::Reference::graph_nodes(graph, target)
                .on_delete(action)
                .required(required);
            if let Some(n) = name {
                reference = reference.named(n);
            }
            database.integrity().add_reference(reference)?;
            Ok(json!({ "ok": true }))
        }
        IntegrityCommand::AddKvRef {
            namespace,
            target,
            name,
            on_delete,
            prefix_separator,
        } => {
            let action = match on_delete.to_lowercase().as_str() {
                "cascade" => lilia_core::RefAction::Cascade,
                "restrict" => lilia_core::RefAction::Restrict,
                other => anyhow::bail!("unknown on-delete action '{other}': expected cascade or restrict"),
            };
            let mut reference = lilia_core::Reference::kv(namespace, target)
                .on_delete(action);
            if let Some(sep) = prefix_separator {
                reference = reference.key(lilia_core::KeyMapping::Prefix { separator: sep });
            }
            if let Some(n) = name {
                reference = reference.named(n);
            }
            database.integrity().add_reference(reference)?;
            Ok(json!({ "ok": true }))
        }
        IntegrityCommand::DropRef { name } => {
            let dropped = database.integrity().drop_reference(&name)?;
            Ok(json!({ "ok": true, "dropped": dropped }))
        }
        IntegrityCommand::ListRefs => {
            let refs = database.integrity().references();
            Ok(serde_json::to_value(refs)?)
        }
        IntegrityCommand::ListChecks => {
            let checks = database.integrity().checks();
            Ok(serde_json::to_value(checks)?)
        }
    }
}

fn ledger_model(database: &Database, command: LedgerCommand) -> anyhow::Result<Value> {
    match command {
        LedgerCommand::Root => {
            let root = database.ledger_root();
            Ok(json!({
                "merkle_root": hex::encode(root)
            }))
        }
        LedgerCommand::Latest => {
            let commit = database.ledger_latest_commit()?;
            Ok(serde_json::to_value(commit)?)
        }
        LedgerCommand::ProveDoc { space, id, version } => {
            let proof = database.ledger_prove_doc(&space, &id, version)?;
            Ok(serde_json::to_value(proof)?)
        }
        LedgerCommand::ProveKv {
            namespace,
            key,
            encoding,
            version,
        } => {
            let raw_key = decode_binary(&key, encoding, "key")?;
            let proof = database.ledger_prove_kv(&namespace, &raw_key, version)?;
            Ok(serde_json::to_value(proof)?)
        }
        LedgerCommand::ProveCommit { commit_seq } => {
            let proof = database.ledger_prove_commit(commit_seq)?;
            Ok(serde_json::to_value(proof)?)
        }
        LedgerCommand::VerifyProof {
            proof_file,
            expected_root,
        } => {
            let bytes = fs::read(&proof_file)?;
            let proof: lilia_core::ledger::InclusionProof = serde_json::from_slice(&bytes)?;
            let root = if let Some(r_hex) = expected_root {
                let mut buf = [0u8; 32];
                hex::decode_to_slice(&r_hex, &mut buf)?;
                buf
            } else {
                database.ledger_root()
            };
            let is_valid = proof.verify(&root);
            Ok(json!({
                "valid": is_valid,
                "target": proof.target,
                "merkle_root": hex::encode(root),
                "commit_seq": proof.commit_seq,
                "timestamp_ms": proof.timestamp_ms,
            }))
        }
        LedgerCommand::VerifyDb => {
            let report = database.verify_ledger()?;
            Ok(serde_json::to_value(report)?)
        }
    }
}

fn graph(database: &Database, command: GraphCommand) -> anyhow::Result<Value> {
    match command {
        GraphCommand::Node {
            graph,
            id,
            kind,
            metadata,
        } => {
            let meta_val = match metadata {
                Some(s) => serde_json::from_str(&s)?,
                None => json!({}),
            };
            database.graph_set_node(&graph, &id, &kind, &meta_val)?;
            Ok(json!({
                "ok": true,
                "graph": graph,
                "id": id,
                "kind": kind
            }))
        }
        GraphCommand::Edge {
            graph,
            source,
            target,
            kind,
        } => {
            database.graph_add_edge(&graph, &source, &target, &kind)?;
            Ok(json!({
                "ok": true,
                "graph": graph,
                "source": source,
                "target": target,
                "kind": kind
            }))
        }
        GraphCommand::Neighbors {
            graph,
            node,
            direction,
            kind,
        } => {
            let neighbors =
                database.graph_neighbors(&graph, &node, direction.into(), kind.as_deref())?;
            Ok(serde_json::to_value(neighbors)?)
        }
        GraphCommand::Reach {
            graph,
            start_nodes,
            direction,
        } => {
            let reachable = database.graph_reachability(&graph, &start_nodes, direction.into())?;
            Ok(serde_json::to_value(reachable)?)
        }
    }
}

fn fts(database: &Database, command: FtsCommand) -> anyhow::Result<Value> {
    match command {
        FtsCommand::Index {
            collection,
            doc_id,
            content,
        } => {
            database.fts_index(&collection, &doc_id, &content)?;
            Ok(json!({
                "ok": true,
                "collection": collection,
                "doc_id": doc_id
            }))
        }
        FtsCommand::Search {
            collection,
            query,
            limit,
        } => {
            let hits = database.fts_search(&collection, &query, limit)?;
            Ok(serde_json::to_value(hits)?)
        }
        FtsCommand::Suggest {
            collection,
            prefix,
            max_distance,
            limit,
        } => {
            let suggestions = database.fts_suggest(&collection, &prefix, max_distance, limit)?;
            Ok(serde_json::to_value(suggestions)?)
        }
        FtsCommand::Fuzzy {
            collection,
            query,
            max_distance,
            limit,
        } => {
            let hits = database.fts_fuzzy_search(&collection, &query, max_distance, limit)?;
            Ok(serde_json::to_value(hits)?)
        }
    }
}

fn parse_element_type(raw: &str) -> anyhow::Result<VectorElementType> {
    match raw.to_lowercase().as_str() {
        "f32" | "float32" | "float" => Ok(VectorElementType::Float32),
        "i8" | "int8" => Ok(VectorElementType::Int8),
        "1bit" | "bit1" | "bit" | "binary" => Ok(VectorElementType::Bit),
        other => {
            anyhow::bail!("unknown vector element type `{other}`, expected `f32`, `i8`, or `1bit`")
        }
    }
}

fn parse_vector_data(raw: &str, element_type: VectorElementType) -> anyhow::Result<(Vec<u8>, u32)> {
    let trimmed = raw.trim();
    // 1. Try JSON array
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(trimmed) {
            match element_type {
                VectorElementType::Float32 => {
                    let mut bytes = Vec::with_capacity(arr.len() * 4);
                    for item in &arr {
                        let f = item
                            .as_f64()
                            .ok_or_else(|| anyhow::anyhow!("vector element not a number"))?
                            as f32;
                        bytes.extend_from_slice(&f.to_le_bytes());
                    }
                    return Ok((bytes, arr.len() as u32));
                }
                VectorElementType::Int8 => {
                    let mut bytes = Vec::with_capacity(arr.len());
                    for item in &arr {
                        let i = item
                            .as_i64()
                            .ok_or_else(|| anyhow::anyhow!("vector element not an int"))?
                            as i8;
                        bytes.push(i as u8);
                    }
                    return Ok((bytes, arr.len() as u32));
                }
                VectorElementType::Bit => {
                    let mut bytes = Vec::with_capacity(arr.len());
                    for item in &arr {
                        let u = item
                            .as_u64()
                            .ok_or_else(|| anyhow::anyhow!("vector element not a u8"))?
                            as u8;
                        bytes.push(u);
                    }
                    return Ok((bytes, (arr.len() * 8) as u32));
                }
            }
        }
    }

    // 2. Try Base64
    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(trimmed) {
        let dim = match element_type {
            VectorElementType::Float32 => (bytes.len() / 4) as u32,
            VectorElementType::Int8 => bytes.len() as u32,
            VectorElementType::Bit => (bytes.len() * 8) as u32,
        };
        return Ok((bytes, dim));
    }

    // 3. Try comma-separated numbers
    let parts: std::result::Result<Vec<f32>, _> = trimmed
        .split(',')
        .map(|s| s.trim().parse::<f32>())
        .collect();
    if let Ok(floats) = parts {
        if !floats.is_empty() {
            match element_type {
                VectorElementType::Float32 => {
                    let mut bytes = Vec::with_capacity(floats.len() * 4);
                    for f in &floats {
                        bytes.extend_from_slice(&f.to_le_bytes());
                    }
                    return Ok((bytes, floats.len() as u32));
                }
                VectorElementType::Int8 => {
                    let mut bytes = Vec::with_capacity(floats.len());
                    for f in &floats {
                        bytes.push(*f as i8 as u8);
                    }
                    return Ok((bytes, floats.len() as u32));
                }
                VectorElementType::Bit => {
                    let mut bytes = Vec::with_capacity(floats.len());
                    for f in &floats {
                        bytes.push(*f as u8);
                    }
                    return Ok((bytes, (floats.len() * 8) as u32));
                }
            }
        }
    }

    anyhow::bail!("unable to parse vector input: must be JSON array (e.g. `[1.0, 2.0]`), comma-separated numbers, or base64")
}

fn vector(database: &Database, command: VectorCommand) -> anyhow::Result<Value> {
    match command {
        VectorCommand::CreateIndex {
            collection,
            m,
            ef_construction,
            ef_search,
            metric,
            element_type,
            dim,
        } => {
            let elem_type = parse_element_type(&element_type)?;
            let metric: lilia_core::VectorMetric = metric.into();
            let config = lilia_core::HnswConfig {
                m,
                ef_construction,
                ef_search,
                metric,
                element_type: elem_type,
                dim,
            };
            database.vector_create_index(&collection, config)?;
            Ok(json!({
                "ok": true,
                "collection": collection,
                "index_created": true,
            }))
        }
        VectorCommand::IndexInfo { collection } => {
            let info = database.vector_get_index_info(&collection)?;
            match info {
                Some(i) => Ok(json!({
                    "ok": true,
                    "index": i,
                })),
                None => Ok(json!({ "ok": false, "found": false })),
            }
        }
        VectorCommand::DropIndex { collection } => {
            let dropped = database.vector_drop_index(&collection)?;
            Ok(json!({ "ok": true, "dropped": dropped }))
        }
        VectorCommand::Set {
            collection,
            id,
            vector,
            element_type,
        } => {
            let elem_type = parse_element_type(&element_type)?;
            let (vec_bytes, dim) = parse_vector_data(&vector, elem_type)?;
            database.vector_set(&collection, &id, dim, elem_type, &vec_bytes)?;
            Ok(json!({
                "ok": true,
                "collection": collection,
                "id": id,
                "dim": dim,
                "element_type": element_type,
            }))
        }
        VectorCommand::Get { collection, id } => {
            let item = database.vector_get(&collection, &id)?;
            match item {
                Some((dim, elem_type, bytes)) => {
                    let values: Value = match elem_type {
                        VectorElementType::Float32 => {
                            let floats: Vec<f32> = bytes
                                .chunks_exact(4)
                                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                                .collect();
                            json!(floats)
                        }
                        VectorElementType::Int8 => {
                            let ints: Vec<i8> = bytes.into_iter().map(|b| b as i8).collect();
                            json!(ints)
                        }
                        VectorElementType::Bit => {
                            let u8s: Vec<u8> = bytes;
                            json!(u8s)
                        }
                    };
                    Ok(json!({
                        "ok": true,
                        "collection": collection,
                        "id": id,
                        "dim": dim,
                        "element_type": elem_type.as_str(),
                        "vector": values,
                    }))
                }
                None => Ok(json!({ "ok": false, "found": false })),
            }
        }
        VectorCommand::Delete { collection, id } => {
            let deleted = database.vector_delete(&collection, &id)?;
            Ok(json!({ "ok": true, "deleted": deleted }))
        }
        VectorCommand::Search {
            collection,
            vector,
            limit,
            metric,
            element_type,
            in_graph_neighbors,
            by_json_index,
        } => {
            let elem_type = parse_element_type(&element_type)?;
            let (vec_bytes, dim) = parse_vector_data(&vector, elem_type)?;
            let metric: lilia_core::VectorMetric = metric.into();

            let hits = if let Some(filter) = in_graph_neighbors {
                let (graph, start_node) = filter
                    .split_once(':')
                    .or_else(|| filter.split_once(','))
                    .ok_or_else(|| {
                        anyhow::anyhow!("in_graph_neighbors format should be `graph:start_node`")
                    })?;
                database.vector_search_in_graph_neighbors(
                    &collection,
                    graph,
                    start_node,
                    lilia_core::Direction::Outgoing,
                    &vec_bytes,
                    dim,
                    elem_type,
                    metric,
                    limit,
                )?
            } else if let Some(filter) = by_json_index {
                let (space_index, value) = filter.split_once('=').ok_or_else(|| {
                    anyhow::anyhow!("by_json_index format should be `space:index_name=value`")
                })?;
                let (space, index_name) = space_index.split_once(':').ok_or_else(|| {
                    anyhow::anyhow!("by_json_index format should be `space:index_name=value`")
                })?;
                database.vector_search_by_json_index(
                    &collection,
                    space,
                    index_name,
                    value,
                    &vec_bytes,
                    dim,
                    elem_type,
                    metric,
                    limit,
                )?
            } else {
                database.vector_search(&collection, &vec_bytes, dim, elem_type, metric, limit)?
            };
            Ok(serde_json::to_value(hits)?)
        }
    }
}

fn hybrid(database: &Database, command: HybridCommand) -> anyhow::Result<Value> {    match command {
        HybridCommand::Search {
            collection,
            query,
            vector,
            limit,
            rrf_k,
            fts_weight,
            vector_weight,
            metric,
            element_type,
        } => {
            let elem_type = parse_element_type(&element_type)?;
            let vector_query = if let Some(vec_str) = vector {
                let (vec_bytes, dim) = parse_vector_data(&vec_str, elem_type)?;
                Some((vec_bytes, dim, elem_type, metric.into()))
            } else {
                None
            };
            let hits = database.hybrid_search(
                &collection,
                query.as_deref(),
                vector_query
                    .as_ref()
                    .map(|(b, d, e, m)| (b.as_slice(), *d, *e, *m)),
                rrf_k,
                fts_weight,
                vector_weight,
                limit,
            )?;
            Ok(serde_json::to_value(hits)?)
        }
    }
}

fn crdt(database: &Database, command: CrdtCommand) -> anyhow::Result<Value> {
    let mutate =
        |operation| Ok::<_, anyhow::Error>(serde_json::to_value(database.batch(&[operation])?)?);
    match command {
        // ── PN-Counter ──────────────────────────────────────────────────────────
        CrdtCommand::Counter {
            command: CrdtCounterCommand::Get { space, id },
        } => {
            let value = database.crdt().counter(&space, &id).get()?;
            Ok(json!({ "ok": true, "space": space, "id": id, "value": value }))
        }
        CrdtCommand::Counter {
            command: CrdtCounterCommand::Inc { space, id, actor_id, amount },
        } => mutate(lilia_core::BatchOperation::CrdtPnCounterIncrement {
            space,
            id,
            actor_id,
            amount,
        }),
        CrdtCommand::Counter {
            command: CrdtCounterCommand::Dec { space, id, actor_id, amount },
        } => mutate(lilia_core::BatchOperation::CrdtPnCounterDecrement {
            space,
            id,
            actor_id,
            amount,
        }),

        // ── OR-Set ──────────────────────────────────────────────────────────────
        CrdtCommand::Set {
            command: CrdtSetCommand::Get { space, id },
        } => {
            let elements = database.crdt().set(&space, &id).read()?;
            Ok(json!({ "ok": true, "space": space, "id": id, "elements": elements }))
        }
        CrdtCommand::Set {
            command: CrdtSetCommand::Contains { space, id, element },
        } => {
            let contains = database.crdt().set(&space, &id).contains(&element)?;
            Ok(json!({ "ok": true, "space": space, "id": id, "element": element, "contains": contains }))
        }
        CrdtCommand::Set {
            command: CrdtSetCommand::Add { space, id, actor_id, element },
        } => mutate(lilia_core::BatchOperation::CrdtOrSetAdd {
            space,
            id,
            actor_id,
            element,
        }),
        CrdtCommand::Set {
            command: CrdtSetCommand::Remove { space, id, element },
        } => mutate(lilia_core::BatchOperation::CrdtOrSetRemove {
            space,
            id,
            element,
            timestamp_ms: None,
        }),
        CrdtCommand::Set {
            command: CrdtSetCommand::Prune { space, id, cutoff_ms },
        } => {
            database.crdt().set(&space, &id).prune(cutoff_ms)?;
            Ok(json!({ "ok": true, "space": space, "id": id, "pruned_before_ms": cutoff_ms }))
        }

        // ── LWW-JSON Document ───────────────────────────────────────────────────
        CrdtCommand::Json {
            command: CrdtJsonCommand::Get { space, id },
        } => {
            let doc = database.crdt().doc(&space, &id).get()?;
            Ok(json!({ "ok": true, "space": space, "id": id, "document": doc }))
        }
        CrdtCommand::Json {
            command: CrdtJsonCommand::SetField { space, id, actor_id, path, value },
        } => {
            let json_value: serde_json::Value = serde_json::from_str(&value)
                .map_err(|e| anyhow::anyhow!("invalid JSON value: {e}"))?;
            mutate(lilia_core::BatchOperation::CrdtJsonSetField {
                space,
                id,
                actor_id,
                path,
                value: json_value,
                timestamp_ms: None,
            })
        }
        CrdtCommand::Json {
            command: CrdtJsonCommand::DeleteField { space, id, actor_id, path },
        } => mutate(lilia_core::BatchOperation::CrdtJsonDeleteField {
            space,
            id,
            actor_id,
            path,
            timestamp_ms: None,
        }),
    }
}

fn blob(database: &Database, command: BlobCommand) -> anyhow::Result<Value> {
    match command {
        BlobCommand::Put { file, mime_type } => {
            let mut f = fs::File::open(&file)?;
            let descriptor = database.blob_store_stream(&mut f, mime_type)?;
            Ok(serde_json::to_value(descriptor)?)
        }
        BlobCommand::Get { hash, output } => {
            let bytes = database
                .blob_get(&hash)?
                .ok_or_else(|| anyhow::anyhow!("blob with hash '{}' not found", hash))?;
            if let Some(dest) = output {
                fs::write(&dest, &bytes)?;
                Ok(json!({ "hash": hash, "written_to": dest, "bytes": bytes.len() }))
            } else {
                let text = match String::from_utf8(bytes.clone()) {
                    Ok(s) => s,
                    Err(_) => base64::engine::general_purpose::STANDARD.encode(&bytes),
                };
                Ok(json!({ "hash": hash, "size_bytes": bytes.len(), "data": text }))
            }
        }
        BlobCommand::Path { hash } => {
            let path = database.blob_path(&hash);
            let exists = path.is_file();
            Ok(json!({ "hash": hash, "path": path, "exists": exists }))
        }
        BlobCommand::Exists { hash } => {
            let exists = database.blob_exists(&hash);
            Ok(json!({ "hash": hash, "exists": exists }))
        }
        BlobCommand::Gc => {
            let stats = database.blob_gc()?;
            Ok(serde_json::to_value(stats)?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn decode_binary_formats() {
        assert_eq!(
            decode_binary("hello", Encoding::Text, "key").unwrap(),
            b"hello"
        );
        assert_eq!(
            decode_binary("0080ff", Encoding::Hex, "key").unwrap(),
            vec![0, 128, 255]
        );
        assert_eq!(
            decode_binary("AID/", Encoding::Base64, "key").unwrap(),
            vec![0, 128, 255]
        );

        let err_hex = decode_binary("zz", Encoding::Hex, "key").unwrap_err();
        assert_eq!(err_hex.code, lilia_core::ErrorCode::InvalidInput);

        let err_b64 = decode_binary("!!!", Encoding::Base64, "key").unwrap_err();
        assert_eq!(err_b64.code, lilia_core::ErrorCode::InvalidInput);
    }

    #[test]
    fn kv_hex_and_base64_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.lilia");
        let database = Database::open(DatabaseOptions::durable(&path)).unwrap();

        let res = kv(
            &database,
            KvCommand::Set {
                namespace: "test_ns".into(),
                key: "0080ff".into(),
                value: "gICAgA==".into(),
                if_version: None,
                expires_at_ms: None,
                encoding: Encoding::Text,
                key_encoding: Some(Encoding::Hex),
                value_encoding: Some(Encoding::Base64),
            },
        )
        .unwrap();
        assert_eq!(res[0]["deleted"], false);

        let entry = kv(
            &database,
            KvCommand::Get {
                namespace: "test_ns".into(),
                key: "0080ff".into(),
                encoding: Encoding::Hex,
                key_encoding: None,
            },
        )
        .unwrap();
        assert_eq!(entry["key"], serde_json::json!([0, 128, 255]));
        assert_eq!(entry["value"], serde_json::json!([128, 128, 128, 128]));

        let scan = kv(
            &database,
            KvCommand::Scan {
                namespace: "test_ns".into(),
                after: Some("00".into()),
                limit: 10,
                encoding: Encoding::Hex,
                key_encoding: None,
            },
        )
        .unwrap();
        let items: Vec<Value> = serde_json::from_value(scan).unwrap();
        assert_eq!(items.len(), 1);

        let del = kv(
            &database,
            KvCommand::Delete {
                namespace: "test_ns".into(),
                key: "0080ff".into(),
                if_version: None,
                encoding: Encoding::Hex,
                key_encoding: None,
            },
        )
        .unwrap();
        assert_eq!(del[0]["deleted"], true);
    }

    #[test]
    fn sql_command_execution() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("cli_sql.lilia");

        let init = execute(Command::Init, &db_path, None).unwrap();
        assert_eq!(init["ok"], true);

        // Put a document
        let put = execute(
            Command::Json {
                command: JsonCommand::Put {
                    space: "users".into(),
                    id: "bob".into(),
                    value: "{\"name\":\"Bob\",\"score\":99}".into(),
                    if_version: None,
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(put[0]["version"], "1");

        // Query via SQL
        let sql_res = execute(
            Command::Sql {
                query: "SELECT id, json_extract(value, '$.score') as score FROM lilia_json WHERE space = 'users'".into(),
                params: vec![],
            },
            &db_path,
            None,
        )
        .unwrap();

        assert_eq!(sql_res["columns"], json!(["id", "score"]));
        assert_eq!(sql_res["rows"], json!([["bob", 99]]));
    }

    #[test]
    fn graph_and_fts_command_execution() {
        use crate::args::DirectionArg;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("cli_graph_fts.lilia");

        // Graph node & edge
        execute(
            Command::Graph {
                command: GraphCommand::Node {
                    graph: "deps".into(),
                    id: "app".into(),
                    kind: "binary".into(),
                    metadata: Some(r#"{"version":"1.0"}"#.into()),
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        execute(
            Command::Graph {
                command: GraphCommand::Node {
                    graph: "deps".into(),
                    id: "lib".into(),
                    kind: "library".into(),
                    metadata: None,
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        execute(
            Command::Graph {
                command: GraphCommand::Edge {
                    graph: "deps".into(),
                    source: "app".into(),
                    target: "lib".into(),
                    kind: "uses".into(),
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        let neighbors = execute(
            Command::Graph {
                command: GraphCommand::Neighbors {
                    graph: "deps".into(),
                    node: "app".into(),
                    direction: DirectionArg::Outgoing,
                    kind: None,
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(neighbors, json!(["lib"]));

        let reach = execute(
            Command::Graph {
                command: GraphCommand::Reach {
                    graph: "deps".into(),
                    start_nodes: vec!["app".into()],
                    direction: DirectionArg::Outgoing,
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(reach, json!(["app", "lib"]));

        // FTS index & search
        execute(
            Command::Fts {
                command: FtsCommand::Index {
                    collection: "articles".into(),
                    doc_id: "doc1".into(),
                    content: "liliadb high performance engine".into(),
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        let search = execute(
            Command::Fts {
                command: FtsCommand::Search {
                    collection: "articles".into(),
                    query: "engine".into(),
                    limit: 5,
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        let hits = search.as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["doc_id"], "doc1");

        // Vector set & get & search
        execute(
            Command::Vector {
                command: VectorCommand::Set {
                    collection: "docs".into(),
                    id: "doc1".into(),
                    vector: "[1.0, 0.0]".into(),
                    element_type: "f32".into(),
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        execute(
            Command::Vector {
                command: VectorCommand::Set {
                    collection: "docs".into(),
                    id: "doc2".into(),
                    vector: "[0.0, 1.0]".into(),
                    element_type: "f32".into(),
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        let vget = execute(
            Command::Vector {
                command: VectorCommand::Get {
                    collection: "docs".into(),
                    id: "doc1".into(),
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(vget["ok"], true);
        assert_eq!(vget["dim"], 2);

        let vsearch = execute(
            Command::Vector {
                command: VectorCommand::Search {
                    collection: "docs".into(),
                    vector: "[1.0, 0.0]".into(),
                    limit: 1,
                    metric: crate::args::VectorMetricArg::Cosine,
                    element_type: "f32".into(),
                    in_graph_neighbors: None,
                    by_json_index: None,
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        let vhits = vsearch.as_array().unwrap();
        assert_eq!(vhits.len(), 1);
        assert_eq!(vhits[0]["id"], "doc1");

        // Hybrid search
        let hsearch = execute(
            Command::Hybrid {
                command: HybridCommand::Search {
                    collection: "docs".into(),
                    query: None,
                    vector: Some("[1.0, 0.0]".into()),
                    limit: 2,
                    rrf_k: 60,
                    fts_weight: 1.0,
                    vector_weight: 1.0,
                    metric: crate::args::VectorMetricArg::Cosine,
                    element_type: "f32".into(),
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        let hhits = hsearch.as_array().unwrap();
        assert_eq!(hhits.len(), 2);
    }

    #[test]
    fn resolve_plugin_path_priority() {
        let dir = tempfile::tempdir().unwrap();
        let direct_file = dir.path().join("libplugin.dylib");
        std::fs::write(&direct_file, b"content").unwrap();

        // 1. Existing file resolves directly
        let resolved = resolve_plugin_path(&direct_file).unwrap();
        assert_eq!(resolved, direct_file);

        // 2. Directory with package/manifest.json resolves to package subdir
        let pkg_dir = dir.path().join("my-plugin");
        let inner_pkg = pkg_dir.join("package");
        std::fs::create_dir_all(&inner_pkg).unwrap();
        std::fs::write(inner_pkg.join("manifest.json"), b"{}").unwrap();
        let resolved_pkg = resolve_plugin_path(&pkg_dir).unwrap();
        assert_eq!(resolved_pkg, inner_pkg);

        // 3. Non-existent path returns descriptive error
        let err =
            resolve_plugin_path(Path::new("definitely-nonexistent-plugin-12345")).unwrap_err();
        assert!(err.to_string().contains("not found as a file or directory"));
    }

    #[test]
    fn crdt_cli_command_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("crdt_cli.lilia");

        // PN-Counter Inc & Dec & Get
        execute(
            Command::Crdt {
                command: CrdtCommand::Counter {
                    command: CrdtCounterCommand::Inc {
                        space: "stats".into(),
                        id: "visits".into(),
                        actor_id: "node_1".into(),
                        amount: 10,
                    },
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        execute(
            Command::Crdt {
                command: CrdtCommand::Counter {
                    command: CrdtCounterCommand::Dec {
                        space: "stats".into(),
                        id: "visits".into(),
                        actor_id: "node_2".into(),
                        amount: 3,
                    },
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        let counter_val = execute(
            Command::Crdt {
                command: CrdtCommand::Counter {
                    command: CrdtCounterCommand::Get {
                        space: "stats".into(),
                        id: "visits".into(),
                    },
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(counter_val["value"], 7);

        // OR-Set Add & Contains & Remove & Get
        execute(
            Command::Crdt {
                command: CrdtCommand::Set {
                    command: CrdtSetCommand::Add {
                        space: "tags".into(),
                        id: "post1".into(),
                        actor_id: "phone".into(),
                        element: "rust".into(),
                    },
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        execute(
            Command::Crdt {
                command: CrdtCommand::Set {
                    command: CrdtSetCommand::Add {
                        space: "tags".into(),
                        id: "post1".into(),
                        actor_id: "laptop".into(),
                        element: "database".into(),
                    },
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        let contains = execute(
            Command::Crdt {
                command: CrdtCommand::Set {
                    command: CrdtSetCommand::Contains {
                        space: "tags".into(),
                        id: "post1".into(),
                        element: "rust".into(),
                    },
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(contains["contains"], true);

        execute(
            Command::Crdt {
                command: CrdtCommand::Set {
                    command: CrdtSetCommand::Remove {
                        space: "tags".into(),
                        id: "post1".into(),
                        element: "rust".into(),
                    },
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        let set_items = execute(
            Command::Crdt {
                command: CrdtCommand::Set {
                    command: CrdtSetCommand::Get {
                        space: "tags".into(),
                        id: "post1".into(),
                    },
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(set_items["elements"], json!(["database"]));

        // LWW-JSON SetField & Get & DeleteField
        execute(
            Command::Crdt {
                command: CrdtCommand::Json {
                    command: CrdtJsonCommand::SetField {
                        space: "settings".into(),
                        id: "user_42".into(),
                        actor_id: "client_a".into(),
                        path: "theme/mode".into(),
                        value: "\"dark\"".into(),
                    },
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        execute(
            Command::Crdt {
                command: CrdtCommand::Json {
                    command: CrdtJsonCommand::SetField {
                        space: "settings".into(),
                        id: "user_42".into(),
                        actor_id: "client_b".into(),
                        path: "notifications/email".into(),
                        value: "true".into(),
                    },
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        let doc = execute(
            Command::Crdt {
                command: CrdtCommand::Json {
                    command: CrdtJsonCommand::Get {
                        space: "settings".into(),
                        id: "user_42".into(),
                    },
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(doc["document"]["theme"]["mode"], "dark");
        assert_eq!(doc["document"]["notifications"]["email"], true);

        execute(
            Command::Crdt {
                command: CrdtCommand::Json {
                    command: CrdtJsonCommand::DeleteField {
                        space: "settings".into(),
                        id: "user_42".into(),
                        actor_id: "client_a".into(),
                        path: "theme/mode".into(),
                    },
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        let doc_after = execute(
            Command::Crdt {
                command: CrdtCommand::Json {
                    command: CrdtJsonCommand::Get {
                        space: "settings".into(),
                        id: "user_42".into(),
                    },
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(doc_after["document"]["theme"].get("mode"), None);
        assert_eq!(doc_after["document"]["notifications"]["email"], true);
    }

    #[test]
    fn blob_put_get_path_exists_and_gc() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test_blob.lilia");
        let src_file = dir.path().join("sample.bin");
        let payload = vec![0x42u8; 100_000];
        fs::write(&src_file, &payload).unwrap();

        // 1. Put
        let put_res = execute(
            Command::Blob {
                command: BlobCommand::Put {
                    file: src_file.clone(),
                    mime_type: Some("application/octet-stream".into()),
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        let hash = put_res["hash"].as_str().unwrap().to_string();
        assert_eq!(put_res["size_bytes"], 100_000);
        assert_eq!(put_res["mime_type"], "application/octet-stream");

        // 2. Exists
        let exists_res = execute(
            Command::Blob {
                command: BlobCommand::Exists {
                    hash: hash.clone(),
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(exists_res["exists"], true);

        // 3. Path
        let path_res = execute(
            Command::Blob {
                command: BlobCommand::Path {
                    hash: hash.clone(),
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(path_res["exists"], true);

        // 4. Get to file
        let out_file = dir.path().join("recovered.bin");
        let get_res = execute(
            Command::Blob {
                command: BlobCommand::Get {
                    hash: hash.clone(),
                    output: Some(out_file.clone()),
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(get_res["bytes"], 100_000);
        assert_eq!(fs::read(&out_file).unwrap(), payload);

        // 5. GC (blob has ref_count = 0 because it was stored directly without KV/JSON ref)
        let gc_res = execute(
            Command::Blob {
                command: BlobCommand::Gc,
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(gc_res["orphans_purged"], 1);
        assert_eq!(gc_res["bytes_reclaimed"], 100_000);

        // 6. Exists after GC
        let exists_after = execute(
            Command::Blob {
                command: BlobCommand::Exists {
                    hash,
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(exists_after["exists"], false);
    }

    #[test]
    fn integrity_cli_command_execution() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("cli_integrity.lilia");

        execute(Command::Init, &db_path, None).unwrap();

        // 1. Create collection with CHECK constraint
        let create_res = execute(
            Command::Json {
                command: JsonCommand::CreateCollection {
                    space: "accounts".into(),
                    check: Some("$.balance >= 0.0 AND length($.iban) == 27".into()),
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(create_res["ok"], true);

        // 2. Put valid document
        let valid_iban = "F".repeat(27);
        let valid_put = execute(
            Command::Json {
                command: JsonCommand::Put {
                    space: "accounts".into(),
                    id: "acc_1".into(),
                    value: format!(r#"{{"balance": 100.0, "iban": "{valid_iban}"}}"#),
                    if_version: None,
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        assert!(valid_put.is_array());

        // 3. Put invalid document (negative balance) -> fails with CONSTRAINT_VIOLATION
        let invalid_put = execute(
            Command::Json {
                command: JsonCommand::Put {
                    space: "accounts".into(),
                    id: "acc_2".into(),
                    value: format!(r#"{{"balance": -10.0, "iban": "{valid_iban}"}}"#),
                    if_version: None,
                },
            },
            &db_path,
            None,
        );
        let err = invalid_put.expect_err("violating CHECK must be rejected");
        let lilia_err = err.downcast_ref::<lilia_core::LiliaError>().expect("LiliaError");
        assert_eq!(lilia_err.code, lilia_core::ErrorCode::ConstraintViolation);

        // 4. Add graph reference (CASCADE) and KV reference (RESTRICT)
        execute(
            Command::Integrity {
                command: IntegrityCommand::AddGraphRef {
                    graph: "social".into(),
                    target: "users".into(),
                    name: None,
                    on_delete: "cascade".into(),
                    required: false,
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        execute(
            Command::Integrity {
                command: IntegrityCommand::AddKvRef {
                    namespace: "sessions".into(),
                    target: "users".into(),
                    name: None,
                    on_delete: "restrict".into(),
                    prefix_separator: Some(':'),
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        // 5. List checks and references
        let checks = execute(
            Command::Integrity {
                command: IntegrityCommand::ListChecks,
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(checks[0][0], "accounts");

        let refs = execute(
            Command::Integrity {
                command: IntegrityCommand::ListRefs,
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(refs.as_array().unwrap().len(), 2);

        // 6. Put parent JSON document
        execute(
            Command::Json {
                command: JsonCommand::Put {
                    space: "users".into(),
                    id: "user_42".into(),
                    value: r#"{"name": "Alice"}"#.into(),
                    if_version: None,
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        // 7. Orphan KV key fails with ConstraintViolation
        let orphan_kv = execute(
            Command::Kv {
                command: KvCommand::Set {
                    namespace: "sessions".into(),
                    key: "user_99:tok".into(),
                    value: "secret".into(),
                    if_version: None,
                    expires_at_ms: None,
                    encoding: Encoding::Text,
                    key_encoding: None,
                    value_encoding: None,
                },
            },
            &db_path,
            None,
        );
        let err = orphan_kv.expect_err("orphan KV must be rejected");
        let lilia_err = err.downcast_ref::<lilia_core::LiliaError>().expect("LiliaError");
        assert_eq!(lilia_err.code, lilia_core::ErrorCode::ConstraintViolation);

        // 8. Valid child KV key
        execute(
            Command::Kv {
                command: KvCommand::Set {
                    namespace: "sessions".into(),
                    key: "user_42:tok".into(),
                    value: "secret".into(),
                    if_version: None,
                    expires_at_ms: None,
                    encoding: Encoding::Text,
                    key_encoding: None,
                    value_encoding: None,
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        // 9. Deleting parent JSON while child KV exists is rejected by RESTRICT
        let delete_parent_blocked = execute(
            Command::Json {
                command: JsonCommand::Delete {
                    space: "users".into(),
                    id: "user_42".into(),
                    if_version: None,
                },
            },
            &db_path,
            None,
        );
        let err = delete_parent_blocked.expect_err("deletion blocked by RESTRICT");
        let lilia_err = err.downcast_ref::<lilia_core::LiliaError>().expect("LiliaError");
        assert_eq!(lilia_err.code, lilia_core::ErrorCode::ConstraintViolation);

        // 10. Delete child KV, then delete parent -> succeeds
        execute(
            Command::Kv {
                command: KvCommand::Delete {
                    namespace: "sessions".into(),
                    key: "user_42:tok".into(),
                    if_version: None,
                    encoding: Encoding::Text,
                    key_encoding: None,
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        let delete_parent = execute(
            Command::Json {
                command: JsonCommand::Delete {
                    space: "users".into(),
                    id: "user_42".into(),
                    if_version: None,
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        assert!(delete_parent.is_array());
    }

    #[test]
    fn test_ledger_cli_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("cli_ledger.lilia");

        // 1. Initial root before any writes
        let root_res = execute(
            Command::Ledger {
                command: LedgerCommand::Root,
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(root_res["merkle_root"], "0".repeat(64));

        // 2. Insert document
        execute(
            Command::Json {
                command: JsonCommand::Put {
                    space: "patients".into(),
                    id: "p42".into(),
                    value: r#"{"name": "Alice", "status": "active"}"#.into(),
                    if_version: None,
                },
            },
            &db_path,
            None,
        )
        .unwrap();

        // 3. Root after write
        let root_res2 = execute(
            Command::Ledger {
                command: LedgerCommand::Root,
            },
            &db_path,
            None,
        )
        .unwrap();
        let root2_hex = root_res2["merkle_root"].as_str().unwrap().to_string();
        assert_ne!(root2_hex, "0".repeat(64));

        // 4. Prove document
        let proof_res = execute(
            Command::Ledger {
                command: LedgerCommand::ProveDoc {
                    space: "patients".into(),
                    id: "p42".into(),
                    version: None,
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(proof_res["commit_seq"], 1);

        let proof_file = dir.path().join("proof.json");
        fs::write(&proof_file, serde_json::to_vec(&proof_res).unwrap()).unwrap();

        // 5. Verify proof via CLI
        let verify_res = execute(
            Command::Ledger {
                command: LedgerCommand::VerifyProof {
                    proof_file: proof_file.clone(),
                    expected_root: Some(root2_hex),
                },
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(verify_res["valid"], true);

        // 6. Verify full DB
        let verify_db_res = execute(
            Command::Ledger {
                command: LedgerCommand::VerifyDb,
            },
            &db_path,
            None,
        )
        .unwrap();
        assert_eq!(verify_db_res["is_valid"], true);
        assert_eq!(verify_db_res["commit_count"], 1);
    }
}
