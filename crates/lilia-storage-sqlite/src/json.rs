#![allow(clippy::missing_errors_doc)]

use std::collections::BTreeMap;

use lilia_sqlite_native::StmtSlot;

use crate::storage::{normalize_limit, parse_version, validate_name};
use crate::Database;
use lilia_core::json::BinaryDoc;
pub use lilia_core::DictStats;
use lilia_core::{
    compression::decompress_bytes, json_patch, CollectionKind, ErrorCode, JsonEntry, JsonIndexInfo,
    JsonRevisionInfo, LiliaError, Result, StorageTier, Version,
};

pub(crate) fn json_snapshot_to_entry(
    space: &str,
    id: String,
    value: &serde_json::Value,
    version: u64,
) -> Result<JsonEntry> {
    Ok(JsonEntry::new(
        space,
        id,
        BinaryDoc::from_json_value(value)?,
        version.max(1),
    ))
}

#[allow(dead_code)]
pub const MAGIC_RAW: &[u8; 4] = b"LBJ\x01";
pub const MAGIC_ZSTD: &[u8; 4] = b"LBZ\x02";
pub const MAGIC_ZSTD_V1: &[u8; 4] = b"LBZ\x01";

pub(crate) enum EncodedDoc {
    Text(String),
    Blob(Vec<u8>),
}

pub(crate) fn decode_json_value(
    encoded: &str,
    codec: &dyn lilia_core::ValueCodec,
) -> Result<serde_json::Value> {
    if codec.name() == "identity" {
        return serde_json::from_str(encoded)
            .map_err(|error| LiliaError::new(ErrorCode::Storage, error.to_string(), false));
    }
    if let Ok(envelope) = serde_json::from_str::<serde_json::Value>(encoded) {
        if let (Some(_c_name), Some(b64_data)) = (
            envelope.get("$codec").and_then(serde_json::Value::as_str),
            envelope.get("$data").and_then(serde_json::Value::as_str),
        ) {
            use base64::Engine;
            let cipher_bytes = base64::engine::general_purpose::STANDARD
                .decode(b64_data)
                .map_err(|error| LiliaError::new(ErrorCode::Storage, error.to_string(), false))?;
            let plaintext_bytes = codec.decode(&cipher_bytes)?;
            return serde_json::from_slice(&plaintext_bytes)
                .map_err(|error| LiliaError::new(ErrorCode::Storage, error.to_string(), false));
        }
    }
    serde_json::from_str(encoded)
        .map_err(|error| LiliaError::new(ErrorCode::Storage, error.to_string(), false))
}

#[allow(clippy::too_many_arguments)] // the stored row's columns plus the three decoding registries
pub(crate) fn decode_stored_doc(
    stmt: &lilia_sqlite_native::NativeStatement,
    col_idx: i32,
    format: i64,
    dict_id: i64,
    space: &str,
    codec: &dyn lilia_core::ValueCodec,
    dict_registry: &lilia_core::DictionaryRegistry,
    key_registry: Option<&lilia_core::KeyRegistry>,
) -> Result<lilia_core::json::BinaryDoc> {
    let mut raw_doc = match format {
        1 => {
            let blob = stmt.column_blob(col_idx);
            if codec.name() == "identity" {
                lilia_core::json::BinaryDoc::from_bytes(blob.to_vec())?
            } else {
                let decrypted = codec.decode(blob)?;
                lilia_core::json::BinaryDoc::from_bytes(decrypted)?
            }
        }
        2 => {
            let blob = stmt.column_blob(col_idx);
            let mut resolved_dict_id = u32::try_from(dict_id).unwrap_or_default();
            let payload = if blob.starts_with(MAGIC_ZSTD) && blob.len() >= 16 {
                let embedded_id = u32::from_le_bytes(blob[4..8].try_into().unwrap());
                if resolved_dict_id == 0 {
                    resolved_dict_id = embedded_id;
                }
                &blob[16..]
            } else if blob.starts_with(MAGIC_ZSTD_V1) && blob.len() >= 8 {
                let embedded_id = u32::from_le_bytes(blob[4..8].try_into().unwrap());
                if resolved_dict_id == 0 {
                    resolved_dict_id = embedded_id;
                }
                &blob[8..]
            } else {
                blob
            };

            let dict = dict_registry
                .get_by_id(resolved_dict_id)
                .or_else(|| dict_registry.get(space))
                .ok_or_else(|| {
                    LiliaError::new(
                        ErrorCode::Storage,
                        format!("missing dictionary id {resolved_dict_id} for space '{space}'"),
                        false,
                    )
                })?;

            let decompressed =
                lilia_core::compression::dict::decompress_with_dict(payload, &dict.raw_bytes)?;
            lilia_core::json::BinaryDoc::from_bytes(decompressed)?
        }
        _ => {
            let text = stmt.column_text(col_idx);
            let val = decode_json_value(text, codec)?;
            lilia_core::json::BinaryDoc::from_json_value(&val)?
        }
    };

    if let Some(keys) = key_registry {
        if raw_doc.as_bytes().windows(4).any(|w| w == b"ENC:") {
            let mut val = raw_doc.to_json_value();
            let _ = lilia_core::decrypt_json_fields(
                &mut val,
                keys,
                lilia_core::DecryptionMode::Tolerant,
            );
            raw_doc = lilia_core::json::BinaryDoc::from_json_value(&val)?;
        }
    }

    Ok(raw_doc)
}

pub(crate) fn encode_doc_for_storage(
    doc: &lilia_core::json::BinaryDoc,
    space: &str,
    codec: &dyn lilia_core::ValueCodec,
    dict_registry: &lilia_core::DictionaryRegistry,
    json_options: &crate::engine::JsonOptions,
) -> Result<(EncodedDoc, i64, i64)> {
    let has_fle = doc.as_bytes().windows(4).any(|w| w == b"ENC:");
    if !json_options.binary_storage || (codec.name() != "identity" && !has_fle) {
        let val = doc.to_json_value();
        let encoded_str = if codec.name() == "identity" || has_fle {
            serde_json::to_string(&val)
                .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?
        } else {
            use base64::Engine;
            let raw_bytes = serde_json::to_vec(&val)
                .map_err(|e| LiliaError::new(ErrorCode::InvalidInput, e.to_string(), false))?;
            let cipher_bytes = codec.encode(&raw_bytes)?;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&cipher_bytes);
            serde_json::to_string(&serde_json::json!({
                "$codec": codec.name(),
                "$data": b64,
            }))
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?
        };
        return Ok((EncodedDoc::Text(encoded_str), 0, 0));
    }

    if let Some(dict) = dict_registry.get(space) {
        let compressed =
            lilia_core::compression::dict::compress_with_dict(doc.as_bytes(), &dict.raw_bytes, 3)?;
        let symbol_mask = dict.symbols.compute_doc_signature(doc.as_bytes());
        let mut payload = Vec::with_capacity(16 + compressed.len());
        payload.extend_from_slice(MAGIC_ZSTD);
        payload.extend_from_slice(&dict.dict_id.to_le_bytes());
        payload.extend_from_slice(&symbol_mask.to_le_bytes());
        payload.extend_from_slice(&compressed);
        return Ok((EncodedDoc::Blob(payload), 2, i64::from(dict.dict_id)));
    }

    Ok((EncodedDoc::Blob(doc.as_bytes().to_vec()), 1, 0))
}

fn doc_matches_query(
    doc: &lilia_core::json::BinaryDoc,
    query: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    for (k, expected_v) in query {
        if let Some(target_val) = doc.extract(k) {
            if let Some(arr) = target_val.as_array() {
                if arr.iter().any(|item| &item.to_json_value() == expected_v) {
                    continue;
                }
            }
            if &target_val.to_json_value() == expected_v {
                continue;
            }
            return false;
        } else if let Ok(expr) = lilia_core::parse_expr(k) {
            if let Ok(eval_val) = lilia_core::eval_expr(&expr, doc) {
                if eval_val
                    .to_index_strings()
                    .iter()
                    .any(|s| match expected_v {
                        serde_json::Value::String(exp_s) => s == exp_s,
                        serde_json::Value::Number(n) => s == &n.to_string(),
                        serde_json::Value::Bool(b) => s == &b.to_string(),
                        serde_json::Value::Null => s == "null",
                        _ => false,
                    })
                {
                    continue;
                }
            }
            return false;
        } else if !expected_v.is_null() {
            return false;
        }
    }
    true
}

/// The index keys a document contributes to an index: none when it does not satisfy the
/// index's partial filter or the expression cannot be evaluated.
pub(crate) fn index_keys(
    expression: &lilia_core::Expr,
    filter: Option<&lilia_core::Expr>,
    doc: &BinaryDoc,
) -> Vec<String> {
    if filter.is_some_and(|predicate| !lilia_core::eval_filter_predicate(predicate, doc)) {
        return Vec::new();
    }
    lilia_core::eval_expr(expression, doc).map_or_default(|value| value.to_index_strings())
}

/// How a `json_find` was served, reported to the auto-tuner.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindTrace {
    /// Name of the index that served the lookup; `None` for a full scan.
    pub index_used: Option<String>,
    /// Documents read and tested against the query.
    pub rows_examined: u64,
    /// Documents eliminated in the compressed domain without decompression.
    pub compressed_skipped: u64,
}

fn insert_nested(
    root: &mut serde_json::Map<String, serde_json::Value>,
    path: &str,
    value: serde_json::Value,
) {
    let mut segments = path.split('.').peekable();
    let mut current = root;
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            current.insert(segment.to_owned(), value);
            return;
        }
        let entry = current
            .entry(segment.to_owned())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        let serde_json::Value::Object(next) = entry else {
            return;
        };
        current = next;
    }
}

/// Picks the composite index (`composite($.a, $.b, ...)`) whose fields are all constrained by
/// the query, preferring the one covering the most fields, and computes the lookup key.
///
/// Indexes over fields protected by field-level encryption are never used here, and neither are
/// queries comparing a field with an array or object value.
fn match_composite_index(
    indexes: &[JsonIndexInfo],
    query: &serde_json::Map<String, serde_json::Value>,
    query_doc: &BinaryDoc,
    fle_policy: Option<&lilia_core::FlePolicy>,
) -> Option<(JsonIndexInfo, serde_json::Value)> {
    let constrained: BTreeMap<String, &serde_json::Value> = query
        .iter()
        .map(|(key, value)| (lilia_core::autotune::normalize_field(key), value))
        .collect();

    let mut best: Option<(&JsonIndexInfo, Vec<String>)> = None;
    for index in indexes {
        let Some(fields) = lilia_core::index_fields(&index.json_path) else {
            continue;
        };
        if fields.len() < 2
            || best
                .as_ref()
                .is_some_and(|(_, found)| found.len() >= fields.len())
        {
            continue;
        }
        let constrained_by_scalars = fields.iter().all(|field| {
            constrained
                .get(field)
                .is_some_and(|value| !value.is_array() && !value.is_object())
        });
        if !constrained_by_scalars
            || fle_policy.is_some_and(|policy| fields.iter().any(|f| policy.is_field_encrypted(f)))
        {
            continue;
        }
        if let Some(filter) = index.filter.as_deref() {
            if let Ok(filter_expr) = lilia_core::parse_expr(filter) {
                if !lilia_core::eval_filter_predicate(&filter_expr, query_doc) {
                    continue;
                }
            }
        }
        best = Some((index, fields));
    }

    let (index, fields) = best?;
    let mut shaped = serde_json::Map::new();
    for field in &fields {
        insert_nested(&mut shaped, field, (*constrained[field]).clone());
    }
    let expression = lilia_core::parse_expr(&index.json_path).ok()?;
    let probe = BinaryDoc::from_json_value(&serde_json::Value::Object(shaped)).ok()?;
    match lilia_core::eval_expr(&expression, &probe).ok()? {
        lilia_core::EvaluatedValue::String(key) => {
            Some((index.clone(), serde_json::Value::String(key)))
        }
        _ => None,
    }
}

impl Database {
    pub(crate) fn read_json_entry(&self, space: &str, id: &str) -> Result<Option<JsonEntry>> {
        validate_name("space", space)?;
        validate_name("id", id)?;
        let tier = self.catalog.tier_for(CollectionKind::Json, space);
        if tier == StorageTier::Memory {
            let now = crate::storage::now_ms();
            return self
                .mem_engine
                .json
                .get(space, id.as_bytes(), now, true)
                .map(|snap| {
                    let mut val = snap.value.clone();
                    let _ = lilia_core::decrypt_json_fields(
                        &mut val,
                        &self.key_registry,
                        lilia_core::DecryptionMode::Tolerant,
                    );
                    json_snapshot_to_entry(space, id.to_owned(), &val, snap.version)
                })
                .transpose();
        }
        if tier == StorageTier::Buffered {
            let now = crate::storage::now_ms();
            if let Some(cell) = self
                .ring_core
                .overlay
                .json
                .get(space, id.as_bytes(), now, false)
            {
                return cell
                    .value
                    .state
                    .map(|(val, seq, _)| {
                        let mut v = val.clone();
                        let _ = lilia_core::decrypt_json_fields(
                            &mut v,
                            &self.key_registry,
                            lilia_core::DecryptionMode::Tolerant,
                        );
                        json_snapshot_to_entry(space, id.to_owned(), &v, seq)
                    })
                    .transpose();
            }
        }
        let mut connection = self.read_lock()?;
        let stmt = connection.slot(StmtSlot::JsonGet)?;
        stmt.reset()?;
        stmt.bind_text_static(1, space)?;
        stmt.bind_text_static(2, id)?;
        let entry = if stmt.step()? {
            let format = stmt.column_int64(1);
            let dict_id = stmt.column_int64(2);
            let version = parse_version(stmt.column_int64(3))?;
            let doc = decode_stored_doc(
                stmt,
                0,
                format,
                dict_id,
                space,
                self.codec.as_ref(),
                &self.dict_registry,
                Some(self.key_registry.as_ref()),
            )?;
            Some(JsonEntry::new(space, id, doc, version))
        } else {
            None
        };
        stmt.reset()?;
        Ok(entry)
    }

    pub(crate) fn is_json_doc_authorized(
        &self,
        space: &str,
        doc: &serde_json::Value,
        action: lilia_core::security::PolicyAction,
    ) -> bool {
        let policies = self.security_registry.policies_for_table(space);
        if policies.is_empty() {
            return true;
        }
        let sec_ctx = crate::security::get_active_context();
        for np in policies {
            let applies = match action {
                lilia_core::security::PolicyAction::Select => np.policy.action.applies_to_read(),
                lilia_core::security::PolicyAction::Insert
                | lilia_core::security::PolicyAction::Update
                | lilia_core::security::PolicyAction::Delete => np.policy.action.applies_to_write(),
                _ => true,
            };
            if !applies {
                continue;
            }
            if let Ok(expr) = lilia_core::parse_expr(&np.policy.condition) {
                if !lilia_core::security::eval_security_condition(&expr, doc, sec_ctx.as_ref()) {
                    return false;
                }
            }
        }
        true
    }

    pub(crate) fn mask_json_entry(&self, space: &str, entry: JsonEntry) -> Result<JsonEntry> {
        let rules = self.security_registry.masking_rules_for_table(space);
        if rules.is_empty() {
            return Ok(entry);
        }
        let sec_ctx = crate::security::get_active_context();
        let mut val = entry.value();
        lilia_core::security::mask_document(&mut val, &rules, sec_ctx.as_ref(), |exempt, c| {
            lilia_core::security::eval_exemption_condition(exempt, c)
        });
        Ok(JsonEntry::new(
            space,
            entry.id,
            lilia_core::json::BinaryDoc::from_json_value(&val)?,
            entry.version,
        ))
    }

    pub fn json_get(&self, space: &str, id: &str) -> Result<Option<JsonEntry>> {
        let Some(entry) = self.read_json_entry(space, id)? else {
            return Ok(None);
        };
        if !self.is_json_doc_authorized(
            space,
            &entry.value(),
            lilia_core::security::PolicyAction::Select,
        ) {
            return Ok(None);
        }
        self.mask_json_entry(space, entry).map(Some)
    }

    pub fn json_extract(
        &self,
        space: &str,
        id: &str,
        path: &str,
    ) -> Result<Option<serde_json::Value>> {
        let Some(entry) = self.read_json_entry(space, id)? else {
            return Ok(None);
        };
        if !self.is_json_doc_authorized(
            space,
            &entry.value(),
            lilia_core::security::PolicyAction::Select,
        ) {
            return Ok(None);
        }
        let masked = self.mask_json_entry(space, entry)?;
        Ok(masked.doc.extract(path).map(|v| v.to_json_value()))
    }

    pub fn json_extract_str(&self, space: &str, id: &str, path: &str) -> Result<Option<String>> {
        let Some(entry) = self.read_json_entry(space, id)? else {
            return Ok(None);
        };
        if !self.is_json_doc_authorized(
            space,
            &entry.value(),
            lilia_core::security::PolicyAction::Select,
        ) {
            return Ok(None);
        }
        let masked = self.mask_json_entry(space, entry)?;
        Ok(masked.doc.extract_str(path).map(ToOwned::to_owned))
    }

    pub fn json_extract_i64(&self, space: &str, id: &str, path: &str) -> Result<Option<i64>> {
        let Some(entry) = self.read_json_entry(space, id)? else {
            return Ok(None);
        };
        if !self.is_json_doc_authorized(
            space,
            &entry.value(),
            lilia_core::security::PolicyAction::Select,
        ) {
            return Ok(None);
        }
        let masked = self.mask_json_entry(space, entry)?;
        Ok(masked.doc.extract_i64(path))
    }

    pub fn json_extract_f64(&self, space: &str, id: &str, path: &str) -> Result<Option<f64>> {
        let Some(entry) = self.read_json_entry(space, id)? else {
            return Ok(None);
        };
        if !self.is_json_doc_authorized(
            space,
            &entry.value(),
            lilia_core::security::PolicyAction::Select,
        ) {
            return Ok(None);
        }
        let masked = self.mask_json_entry(space, entry)?;
        Ok(masked.doc.extract_f64(path))
    }

    pub fn json_extract_bool(&self, space: &str, id: &str, path: &str) -> Result<Option<bool>> {
        let Some(entry) = self.read_json_entry(space, id)? else {
            return Ok(None);
        };
        if !self.is_json_doc_authorized(
            space,
            &entry.value(),
            lilia_core::security::PolicyAction::Select,
        ) {
            return Ok(None);
        }
        let masked = self.mask_json_entry(space, entry)?;
        Ok(masked.doc.extract_bool(path))
    }

    fn collect_json_scan_rows(
        &self,
        space: &str,
        prefix: Option<&str>,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JsonEntry>> {
        self.collect_json_scan_rows_filtered(space, prefix, after, limit, None, None)
    }

    #[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
    fn collect_json_scan_rows_filtered(
        &self,
        space: &str,
        prefix: Option<&str>,
        after: Option<&str>,
        limit: u32,
        symbol_filter: Option<u64>,
        mut skipped_counter: Option<&mut u64>,
    ) -> Result<Vec<JsonEntry>> {
        validate_name("space", space)?;
        let limit_i64 = normalize_limit(limit);
        let limit_usize = usize::try_from(limit_i64).unwrap_or(usize::MAX);
        if limit_usize == 0 {
            return Ok(Vec::new());
        }
        let after_non_empty = after.filter(|s| !s.is_empty());
        let tier = self.catalog.tier_for(CollectionKind::Json, space);
        if tier == StorageTier::Memory {
            let now = crate::storage::now_ms();
            let scan_limit = if prefix.is_some() {
                usize::MAX
            } else {
                limit_usize
            };
            let raw = self.mem_engine.json.scan(
                Some(space),
                after_non_empty.map(str::as_bytes),
                scan_limit,
                now,
            );
            let mut entries = Vec::new();
            for (_ns, key_bytes, snap) in raw {
                let Ok(id) = String::from_utf8(key_bytes) else {
                    continue;
                };
                if prefix.is_some_and(|p| !id.starts_with(p)) {
                    continue;
                }
                if !self.is_json_doc_authorized(
                    space,
                    &snap.value,
                    lilia_core::security::PolicyAction::Select,
                ) {
                    continue;
                }
                let entry = json_snapshot_to_entry(space, id, &snap.value, snap.version)?;
                entries.push(self.mask_json_entry(space, entry)?);
                if entries.len() >= limit_usize {
                    break;
                }
            }
            return Ok(entries);
        }

        let now = crate::storage::now_ms();
        let overlay_items = if tier == StorageTier::Buffered {
            self.ring_core.overlay.json.scan(
                Some(space),
                after_non_empty.map(str::as_bytes),
                usize::MAX,
                now,
            )
        } else {
            Vec::new()
        };
        let fetch_limit = if overlay_items.is_empty() {
            limit_i64
        } else {
            limit_i64.saturating_add(i64::try_from(overlay_items.len()).unwrap_or(i64::MAX))
        };

        let mut merged: BTreeMap<String, Option<JsonEntry>> = BTreeMap::new();
        {
            let mut connection = self.read_lock()?;
            if let Some(pfx) = prefix {
                let min_id = after.unwrap_or("");
                let mut stmt = connection.prepare(
                    "SELECT id, value, format, dict_id, version FROM _lilia_json \
                     WHERE space = ?1 AND id >= ?2 AND id > ?3 AND id LIKE ?4 \
                     ORDER BY id LIMIT ?5;",
                )?;
                let like_pattern = format!("{pfx}%");
                stmt.bind_text_static(1, space)?;
                stmt.bind_text_static(2, pfx)?;
                stmt.bind_text_static(3, min_id)?;
                stmt.bind_text_static(4, &like_pattern)?;
                stmt.bind_int64(5, fetch_limit)?;
                while stmt.step()? {
                    let format = stmt.column_int64(2);
                    if let Some(query_sig) = symbol_filter {
                        if format == 2 {
                            let blob = stmt.column_blob(1);
                            if let Some(doc_mask) =
                                lilia_core::compression::dict::extract_symbol_signature(blob)
                            {
                                if (doc_mask & query_sig) != query_sig {
                                    if let Some(ref mut c) = skipped_counter {
                                        **c += 1;
                                    }
                                    continue;
                                }
                            }
                        }
                    }

                    let id = stmt.column_text(0).to_string();
                    let dict_id = stmt.column_int64(3);
                    let version = parse_version(stmt.column_int64(4))?;
                    let doc = decode_stored_doc(
                        &stmt,
                        1,
                        format,
                        dict_id,
                        space,
                        self.codec.as_ref(),
                        &self.dict_registry,
                        Some(self.key_registry.as_ref()),
                    )?;
                    merged.insert(id.clone(), Some(JsonEntry::new(space, id, doc, version)));
                }
            } else {
                let stmt = connection.slot(StmtSlot::JsonScan)?;
                stmt.reset()?;
                stmt.bind_text_static(1, space)?;
                stmt.bind_text_static(2, after.unwrap_or(""))?;
                stmt.bind_int64(3, fetch_limit)?;
                while stmt.step()? {
                    let format = stmt.column_int64(2);
                    if let Some(query_sig) = symbol_filter {
                        if format == 2 {
                            let blob = stmt.column_blob(1);
                            if let Some(doc_mask) =
                                lilia_core::compression::dict::extract_symbol_signature(blob)
                            {
                                if (doc_mask & query_sig) != query_sig {
                                    if let Some(ref mut c) = skipped_counter {
                                        **c += 1;
                                    }
                                    continue;
                                }
                            }
                        }
                    }

                    let id = stmt.column_text(0).to_string();
                    let dict_id = stmt.column_int64(3);
                    let version = parse_version(stmt.column_int64(4))?;
                    let doc = decode_stored_doc(
                        stmt,
                        1,
                        format,
                        dict_id,
                        space,
                        self.codec.as_ref(),
                        &self.dict_registry,
                        Some(self.key_registry.as_ref()),
                    )?;
                    merged.insert(id.clone(), Some(JsonEntry::new(space, id, doc, version)));
                }
                stmt.reset()?;
            }
        }

        for (_ns, key_bytes, snap) in overlay_items {
            let Ok(id) = String::from_utf8(key_bytes) else {
                continue;
            };
            if prefix.is_some_and(|p| !id.starts_with(p)) {
                continue;
            }
            match snap.value.state {
                Some((val, seq, _)) => {
                    merged.insert(
                        id.clone(),
                        Some(json_snapshot_to_entry(space, id, &val, seq)?),
                    );
                }
                None => {
                    merged.insert(id, None);
                }
            }
        }

        let mut out = Vec::with_capacity(limit_usize.min(merged.len()));
        for entry in merged.into_values().flatten() {
            if self.is_json_doc_authorized(
                space,
                &entry.value(),
                lilia_core::security::PolicyAction::Select,
            ) {
                out.push(self.mask_json_entry(space, entry)?);
                if out.len() >= limit_usize {
                    break;
                }
            }
        }
        Ok(out)
    }

    pub fn json_scan(
        &self,
        space: &str,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JsonEntry>> {
        self.collect_json_scan_rows(space, None, after, limit)
    }

    pub fn create_json_index(&self, space: &str, index_name: &str, json_path: &str) -> Result<()> {
        self.create_json_index_with_options(
            space,
            index_name,
            json_path,
            lilia_core::IndexOptions::default(),
        )
    }

    pub fn create_json_index_with_options(
        &self,
        space: &str,
        index_name: &str,
        expression: &str,
        options: lilia_core::IndexOptions,
    ) -> Result<()> {
        validate_name("space", space)?;
        validate_name("index_name", index_name)?;
        if expression.is_empty() {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "expression cannot be empty",
                false,
            ));
        }

        // Validate expression and optional filter upfront
        let compiled_expr = lilia_core::parse_expr(expression)?;
        let compiled_filter = options
            .filter
            .as_deref()
            .map(lilia_core::parse_expr)
            .transpose()?;

        let tier = self.catalog.tier_for(CollectionKind::Json, space);
        if tier == StorageTier::Memory {
            self.mem_engine
                .json_indexes
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(
                    (space.to_owned(), index_name.to_owned()),
                    crate::mem_engine::MemIndexInfo {
                        expression: expression.to_owned(),
                        filter: options.filter,
                    },
                );
            return Ok(());
        }
        if tier == StorageTier::Buffered {
            self.ring_core.flush()?;
        }
        let mut writer = self.lock_writer()?;
        let tx = writer.transaction()?;
        let mut stmt_upsert = tx.prepare(
            "INSERT INTO _lilia_json_indexes(space, index_name, json_path, filter_predicate) VALUES(?1, ?2, ?3, ?4) \
             ON CONFLICT(space, index_name) DO UPDATE SET json_path = excluded.json_path, filter_predicate = excluded.filter_predicate;",
        )?;
        stmt_upsert.bind_text_static(1, space)?;
        stmt_upsert.bind_text_static(2, index_name)?;
        stmt_upsert.bind_text_static(3, expression)?;
        if let Some(ref filt) = options.filter {
            stmt_upsert.bind_text_static(4, filt)?;
        } else {
            stmt_upsert.bind_null(4)?;
        }
        stmt_upsert.step()?;
        drop(stmt_upsert);

        // Index entries live in the same schema as the documents they index.
        for schema in self.data_schemas() {
            let docs_table = crate::sharding::qualified(schema.as_deref(), "_lilia_json");
            let entries_table =
                crate::sharding::qualified(schema.as_deref(), "_lilia_json_index_entries");

            let mut stmt_delete = tx.prepare(&format!(
                "DELETE FROM {entries_table} WHERE space = ?1 AND index_name = ?2;"
            ))?;
            stmt_delete.bind_text_static(1, space)?;
            stmt_delete.bind_text_static(2, index_name)?;
            stmt_delete.step()?;
            drop(stmt_delete);

            let mut stmt_select = tx.prepare(&format!(
                "SELECT id, value, format, dict_id FROM {docs_table} WHERE space = ?1;"
            ))?;
            stmt_select.bind_text_static(1, space)?;
            let mut entries = Vec::new();
            while stmt_select.step()? {
                let id = stmt_select.column_text(0).to_string();
                let format = stmt_select.column_int64(2);
                let dict_id = stmt_select.column_int64(3);
                let doc = decode_stored_doc(
                    &stmt_select,
                    1,
                    format,
                    dict_id,
                    space,
                    self.codec.as_ref(),
                    &self.dict_registry,
                    None,
                )?;
                entries.push((id, doc));
            }
            drop(stmt_select);

            let mut ins = tx.prepare(&format!(
                "INSERT OR IGNORE INTO {entries_table}(space, index_name, idx_value, doc_id) VALUES(?1, ?2, ?3, ?4);"
            ))?;
            for (doc_id, doc) in entries {
                for key in index_keys(&compiled_expr, compiled_filter.as_ref(), &doc) {
                    ins.reset()?;
                    ins.bind_text_static(1, space)?;
                    ins.bind_text_static(2, index_name)?;
                    ins.bind_text_static(3, &key)?;
                    ins.bind_text_static(4, &doc_id)?;
                    ins.step()?;
                }
            }
            drop(ins);
        }
        tx.commit()?;
        Ok(())
    }

    pub fn drop_json_index(&self, space: &str, index_name: &str) -> Result<bool> {
        validate_name("space", space)?;
        validate_name("index_name", index_name)?;
        let tier = self.catalog.tier_for(CollectionKind::Json, space);
        if tier == StorageTier::Memory {
            return Ok(self
                .mem_engine
                .json_indexes
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&(space.to_owned(), index_name.to_owned()))
                .is_some());
        }
        if tier == StorageTier::Buffered {
            self.ring_core.flush()?;
        }
        let mut writer = self.lock_writer()?;
        let tx = writer.transaction()?;
        let mut stmt1 =
            tx.prepare("DELETE FROM _lilia_json_indexes WHERE space = ?1 AND index_name = ?2;")?;
        stmt1.bind_text_static(1, space)?;
        stmt1.bind_text_static(2, index_name)?;
        stmt1.step()?;
        drop(stmt1);

        let deleted = tx.conn().changes() > 0;
        if deleted {
            for schema in self.data_schemas() {
                let entries_table =
                    crate::sharding::qualified(schema.as_deref(), "_lilia_json_index_entries");
                let mut stmt2 = tx.prepare(&format!(
                    "DELETE FROM {entries_table} WHERE space = ?1 AND index_name = ?2;"
                ))?;
                stmt2.bind_text_static(1, space)?;
                stmt2.bind_text_static(2, index_name)?;
                stmt2.step()?;
                drop(stmt2);
            }
        }
        tx.commit()?;
        Ok(deleted)
    }

    pub fn list_json_indexes(&self, space: &str) -> Result<Vec<JsonIndexInfo>> {
        validate_name("space", space)?;
        if self.catalog.tier_for(CollectionKind::Json, space) == StorageTier::Memory {
            let guard = self
                .mem_engine
                .json_indexes
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut indexes: Vec<JsonIndexInfo> = guard
                .iter()
                .filter(|((sp, _), _)| sp == space)
                .map(|((sp, idx_name), info)| JsonIndexInfo {
                    space: sp.clone(),
                    index_name: idx_name.clone(),
                    json_path: info.expression.clone(),
                    filter: info.filter.clone(),
                })
                .collect();
            indexes.sort_by(|a, b| a.index_name.cmp(&b.index_name));
            return Ok(indexes);
        }
        let connection = self.read_lock()?;
        let mut statement = connection.prepare(
            "SELECT space, index_name, json_path, filter_predicate FROM _lilia_json_indexes WHERE space = ?1 ORDER BY index_name;",
        )?;
        statement.bind_text_static(1, space)?;
        let mut indexes = Vec::new();
        while statement.step()? {
            let filter = if statement.column_type(3) == 5 {
                None
            } else {
                let s = statement.column_text(3);
                if s.is_empty() {
                    None
                } else {
                    Some(s.to_string())
                }
            };
            indexes.push(JsonIndexInfo {
                space: statement.column_text(0).to_string(),
                index_name: statement.column_text(1).to_string(),
                json_path: statement.column_text(2).to_string(),
                filter,
            });
        }
        Ok(indexes)
    }

    pub fn json_scan_by_index(
        &self,
        space: &str,
        index_name: &str,
        value: &str,
        after_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JsonEntry>> {
        let raw = self.scan_by_index_raw(space, index_name, value, after_id, limit)?;
        self.autotune.record_index_use(space, index_name);
        let mut entries = Vec::new();
        for entry in raw {
            if self.is_json_doc_authorized(
                space,
                &entry.value(),
                lilia_core::security::PolicyAction::Select,
            ) {
                entries.push(self.mask_json_entry(space, entry)?);
            }
        }
        Ok(entries)
    }

    /// Index lookup without usage accounting; `json_find` records its own usage once per query.
    pub(crate) fn scan_by_index_raw(
        &self,
        space: &str,
        index_name: &str,
        value: &str,
        after_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JsonEntry>> {
        validate_name("space", space)?;
        validate_name("index_name", index_name)?;
        let limit_i64 = normalize_limit(limit);
        let limit_usize = usize::try_from(limit_i64).unwrap_or(usize::MAX);
        if limit_usize == 0 {
            return Ok(Vec::new());
        }
        let tier = self.catalog.tier_for(CollectionKind::Json, space);
        if tier == StorageTier::Memory {
            let Some(info) = self
                .mem_engine
                .json_indexes
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&(space.to_owned(), index_name.to_owned()))
                .cloned()
            else {
                return Ok(Vec::new());
            };
            let compiled_expr = lilia_core::parse_expr(&info.expression)?;
            let compiled_filter = info
                .filter
                .as_deref()
                .map(lilia_core::parse_expr)
                .transpose()?;
            let now = crate::storage::now_ms();
            let after_bytes = after_id.filter(|s| !s.is_empty()).map(str::as_bytes);
            let raw = self
                .mem_engine
                .json
                .scan(Some(space), after_bytes, usize::MAX, now);
            let mut entries = Vec::new();
            for (_ns, key_bytes, snap) in raw {
                let Ok(id) = String::from_utf8(key_bytes) else {
                    continue;
                };
                let mut val = snap.value.clone();
                let _ = lilia_core::decrypt_json_fields(
                    &mut val,
                    &self.key_registry,
                    lilia_core::DecryptionMode::Tolerant,
                );
                let doc = BinaryDoc::from_json_value(&val)?;
                if let Some(ref filt) = compiled_filter {
                    if !lilia_core::eval_filter_predicate(filt, &doc) {
                        continue;
                    }
                }
                if let Ok(eval_val) = lilia_core::eval_expr(&compiled_expr, &doc) {
                    if eval_val.to_index_strings().iter().any(|s| s == value) {
                        entries.push(JsonEntry::new(space, id, doc, snap.version.max(1)));
                        if entries.len() >= limit_usize {
                            break;
                        }
                    }
                }
            }
            return Ok(entries);
        }
        if tier == StorageTier::Buffered {
            self.ring_core.flush()?;
        }
        let connection = self.read_lock()?;
        let mut statement = connection.prepare(
            "SELECT j.id, j.value, j.format, j.dict_id, j.version \
             FROM _lilia_json_index_entries e \
             JOIN _lilia_json j ON e.space = j.space AND e.doc_id = j.id \
             WHERE e.space = ?1 AND e.index_name = ?2 AND e.idx_value = ?3 AND e.doc_id > ?4 \
             ORDER BY e.doc_id LIMIT ?5;",
        )?;
        statement.bind_text_static(1, space)?;
        statement.bind_text_static(2, index_name)?;
        statement.bind_text_static(3, value)?;
        statement.bind_text_static(4, after_id.unwrap_or(""))?;
        statement.bind_int64(5, limit_i64)?;
        let mut entries = Vec::new();
        while statement.step()? {
            let id = statement.column_text(0).to_string();
            let format = statement.column_int64(2);
            let dict_id = statement.column_int64(3);
            let version = parse_version(statement.column_int64(4))?;
            let doc = decode_stored_doc(
                &statement,
                1,
                format,
                dict_id,
                space,
                self.codec.as_ref(),
                &self.dict_registry,
                Some(self.key_registry.as_ref()),
            )?;
            entries.push(JsonEntry::new(space, id, doc, version));
        }
        Ok(entries)
    }

    pub fn json_scan_prefix(
        &self,
        space: &str,
        prefix: &str,
        after_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JsonEntry>> {
        self.collect_json_scan_rows(space, Some(prefix), after_id, limit)
    }

    pub fn json_find(
        &self,
        space: &str,
        query: &serde_json::Map<String, serde_json::Value>,
        after_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JsonEntry>> {
        self.json_find_with_trace(space, query, after_id, limit)
            .map(|(entries, _)| entries)
    }

    /// Executes a full-collection scan using native Cranelift JIT compilation for
    /// accelerated hardware register predicate evaluation.
    pub fn json_filter_predicate_scan(
        &self,
        space: &str,
        predicate: &lilia_core::Expr,
        after_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JsonEntry>> {
        validate_name("space", space)?;
        let max_results = usize::try_from(normalize_limit(limit)).unwrap_or(usize::MAX);
        let compiled = self.jit_cache.get_or_compile(predicate)?;

        let mut results = Vec::new();
        let mut cursor = after_id.map(ToOwned::to_owned);
        let mut match_buf = Vec::new();

        while results.len() < max_results {
            let batch = self.collect_json_scan_rows_filtered(
                space,
                None,
                cursor.as_deref(),
                1024,
                None,
                None,
            )?;
            if batch.is_empty() {
                break;
            }

            match_buf.clear();
            let doc_refs: Vec<&BinaryDoc> = batch.iter().map(|e| &e.doc).collect();
            compiled.eval_batch(&doc_refs, &mut match_buf);

            let match_set: std::collections::HashSet<usize> = match_buf.drain(..).collect();
            for (i, entry) in batch.into_iter().enumerate() {
                cursor = Some(entry.id.clone());
                if match_set.contains(&i) {
                    results.push(entry);
                    if results.len() == max_results {
                        break;
                    }
                }
            }
        }

        Ok(results)
    }

    /// Executes `json_find` and returns both matched documents and a [`FindTrace`]
    /// reporting index usage, examined rows, and compressed skips.
    pub fn json_find_with_trace(
        &self,
        space: &str,
        query: &serde_json::Map<String, serde_json::Value>,
        after_id: Option<&str>,
        limit: u32,
    ) -> Result<(Vec<JsonEntry>, FindTrace)> {
        validate_name("space", space)?;
        if query.is_empty() {
            let entries = self.json_scan(space, after_id, limit)?;
            let rows = entries.len() as u64;
            let trace = FindTrace {
                index_used: None,
                rows_examined: rows,
                compressed_skipped: 0,
            };
            return Ok((entries, trace));
        }
        if !self.autotune.records(&self.catalog, space) {
            return self.json_find_traced(space, query, after_id, limit);
        }
        let started = std::time::Instant::now();
        let (entries, trace) = self.json_find_traced(space, query, after_id, limit)?;
        self.autotune
            .record_find(space, query, &trace, entries.len(), started.elapsed());
        Ok((entries, trace))
    }

    /// Executes `json_find` and reports how it was served (index or scan) and how many documents
    /// were examined, for the auto-tuner.
    #[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
    fn json_find_traced(
        &self,
        space: &str,
        query: &serde_json::Map<String, serde_json::Value>,
        after_id: Option<&str>,
        limit: u32,
    ) -> Result<(Vec<JsonEntry>, FindTrace)> {
        let mut trace = FindTrace::default();
        let max_results = usize::try_from(normalize_limit(limit)).unwrap_or(usize::MAX);
        let mut indexes = self.list_json_indexes(space)?;
        indexes.retain(|index| !self.autotune.is_building(space, &index.index_name));
        let query_doc = lilia_core::json::BinaryDoc::from_json_value(&serde_json::Value::Object(
            query.clone(),
        ))?;
        let fle_policy = self
            .catalog
            .spec_for(CollectionKind::Json, space)
            .fle_policy;

        let matched_index = match_composite_index(&indexes, query, &query_doc, fle_policy.as_ref())
            .or_else(|| {
                query.iter().find_map(|(k, v)| {
                    for idx in &indexes {
                        // If index is partial, ensure query satisfies the filter predicate
                        if let Some(ref filt_str) = idx.filter {
                            if let Ok(filt_expr) = lilia_core::parse_expr(filt_str) {
                                if !lilia_core::eval_filter_predicate(&filt_expr, &query_doc) {
                                    continue;
                                }
                            }
                        }

                        // 1. Direct name match
                        if &idx.index_name == k {
                            return Some((idx.clone(), v.clone()));
                        }

                        // 2. Direct path or expression match
                        let matches_exact_path = &idx.json_path == k;
                        let matches_prefixed = idx.json_path.strip_prefix("$.") == Some(k.as_str());
                        let matches_slash = idx.json_path.strip_prefix('/') == Some(k.as_str());
                        if matches_exact_path || matches_prefixed || matches_slash {
                            return Some((idx.clone(), v.clone()));
                        }

                        // 3. Double solution: Transparent expression transformation match
                        // If index is e.g. lower($.email) and query is email: "Alice@example.com"
                        if let Ok(lilia_core::json::Expr::FunctionCall { name, args }) =
                            lilia_core::parse_expr(&idx.json_path)
                        {
                            if let [lilia_core::json::Expr::Path(inner_path)] = args.as_slice() {
                                let inner_clean =
                                    inner_path.trim_start_matches("$.").trim_start_matches('/');
                                let k_clean = k.trim_start_matches("$.").trim_start_matches('/');
                                if inner_clean == k_clean {
                                    let transform_expr = lilia_core::json::Expr::FunctionCall {
                                        name,
                                        args: vec![lilia_core::json::Expr::Path(
                                            "$.val".to_string(),
                                        )],
                                    };
                                    let transformed = lilia_core::json::BinaryDoc::from_json_value(
                                        &serde_json::json!({ "val": v }),
                                    )
                                    .and_then(|dummy_doc| {
                                        lilia_core::eval_expr(&transform_expr, &dummy_doc)
                                    });
                                    if let Ok(transformed) = transformed {
                                        let transformed_v = match transformed {
                                            lilia_core::EvaluatedValue::String(s) => {
                                                serde_json::Value::String(s)
                                            }
                                            lilia_core::EvaluatedValue::Int(i) => {
                                                serde_json::Value::Number(i.into())
                                            }
                                            lilia_core::EvaluatedValue::Float(f) => {
                                                serde_json::Number::from_f64(f).map_or(
                                                    serde_json::Value::Null,
                                                    serde_json::Value::Number,
                                                )
                                            }
                                            lilia_core::EvaluatedValue::Bool(b) => {
                                                serde_json::Value::Bool(b)
                                            }
                                            lilia_core::EvaluatedValue::Null => {
                                                serde_json::Value::Null
                                            }
                                            lilia_core::EvaluatedValue::List(_) => v.clone(),
                                        };
                                        return Some((idx.clone(), transformed_v));
                                    }
                                }
                            }
                        }
                    }
                    None
                })
            });

        let mut results = Vec::new();
        let mut cursor = after_id.map(ToOwned::to_owned);

        if let Some((idx, target_val)) = matched_index {
            trace.index_used = Some(idx.index_name.clone());
            let spec = self.catalog.spec_for(CollectionKind::Json, space);
            let siv_rule = spec
                .fle_policy
                .as_ref()
                .and_then(|p| p.deterministic_rule_for(&idx.json_path));

            let val_str = if let Some(rule) = siv_rule {
                if let Some(key) = self.key_registry.get_key(&rule.key_id) {
                    let plain_bytes = serde_json::to_vec(&target_val).map_err(|e| {
                        LiliaError::new(ErrorCode::InvalidInput, e.to_string(), false)
                    })?;
                    let norm_path = rule.path.trim_start_matches("$.").trim_start_matches('/');
                    let ad = if rule.bind_path_as_ad {
                        Some(norm_path.as_bytes())
                    } else {
                        None
                    };
                    let ct = lilia_core::encrypt_siv(&plain_bytes, key.siv_key(), ad)?;
                    lilia_core::EncryptedEnvelope::new(
                        lilia_core::FieldCipherAlgorithm::DeterministicSiv,
                        &rule.key_id,
                        ct,
                    )
                    .encode()
                } else {
                    return Err(LiliaError::new(
                        ErrorCode::Unauthorized,
                        format!("compartment '{}' is locked", rule.key_id),
                        false,
                    ));
                }
            } else {
                match target_val {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Null => "null".to_string(),
                    _ => target_val.to_string(),
                }
            };

            if query.len() == 1 {
                let raw_entries =
                    self.scan_by_index_raw(space, &idx.index_name, &val_str, after_id, limit)?;
                trace.rows_examined = raw_entries.len() as u64;
                let mut entries = Vec::new();
                for entry in raw_entries {
                    if self.is_json_doc_authorized(
                        space,
                        &entry.value(),
                        lilia_core::security::PolicyAction::Select,
                    ) {
                        entries.push(self.mask_json_entry(space, entry)?);
                    }
                }
                return Ok((entries, trace));
            }

            while results.len() < max_results {
                let batch = self.scan_by_index_raw(
                    space,
                    &idx.index_name,
                    &val_str,
                    cursor.as_deref(),
                    100,
                )?;
                if batch.is_empty() {
                    break;
                }
                for entry in batch {
                    trace.rows_examined += 1;
                    cursor = Some(entry.id.clone());
                    if doc_matches_query(&entry.doc, query)
                        && self.is_json_doc_authorized(
                            space,
                            &entry.value(),
                            lilia_core::security::PolicyAction::Select,
                        )
                    {
                        results.push(self.mask_json_entry(space, entry)?);
                        if results.len() == max_results {
                            break;
                        }
                    }
                }
            }
        } else {
            let query_sig = self.dict_registry.get(space).and_then(|dict| {
                let mut tokens = Vec::new();
                for (k, v) in query {
                    tokens.push(k.as_str());
                    match v {
                        serde_json::Value::String(s) => tokens.push(s.as_str()),
                        serde_json::Value::Bool(b) => {
                            tokens.push(if *b { "true" } else { "false" });
                        }
                        _ => {}
                    }
                }
                let sig = dict.symbols.query_signature(&tokens);
                if sig == 0 {
                    None
                } else {
                    Some(sig)
                }
            });

            let maybe_compiled_predicate = query.iter().find_map(|(k, v)| {
                if v == &serde_json::Value::Bool(true) || v == &serde_json::Value::Null {
                    if let Ok(expr) = lilia_core::parse_expr(k) {
                        if lilia_core::json::can_jit_expr(&expr) {
                            return self.jit_cache.get_or_compile(&expr).ok();
                        }
                    }
                }
                None
            });

            let mut skipped = 0u64;
            let mut match_buf = Vec::new();
            while results.len() < max_results {
                let batch = self.collect_json_scan_rows_filtered(
                    space,
                    None,
                    cursor.as_deref(),
                    100,
                    query_sig,
                    Some(&mut skipped),
                )?;
                if batch.is_empty() {
                    break;
                }
                if let Some(ref compiled) = maybe_compiled_predicate {
                    match_buf.clear();
                    let doc_refs: Vec<&BinaryDoc> = batch.iter().map(|e| &e.doc).collect();
                    compiled.eval_batch(&doc_refs, &mut match_buf);
                    let match_set: std::collections::HashSet<usize> = match_buf.drain(..).collect();
                    for (i, entry) in batch.into_iter().enumerate() {
                        trace.rows_examined += 1;
                        cursor = Some(entry.id.clone());
                        if match_set.contains(&i)
                            && (query.len() == 1 || doc_matches_query(&entry.doc, query))
                        {
                            results.push(entry);
                            if results.len() == max_results {
                                break;
                            }
                        }
                    }
                } else {
                    for entry in batch {
                        trace.rows_examined += 1;
                        cursor = Some(entry.id.clone());
                        if doc_matches_query(&entry.doc, query) {
                            results.push(entry);
                            if results.len() == max_results {
                                break;
                            }
                        }
                    }
                }
            }
            trace.compressed_skipped = skipped;
        }

        Ok((results, trace))
    }

    pub fn train_json_dictionary(
        &self,
        space: &str,
        sample_count: usize,
        dict_size: usize,
    ) -> Result<DictStats> {
        validate_name("space", space)?;
        let mut writer = self.lock_writer()?;
        let tx = writer.transaction()?;

        let mut stmt = tx
            .prepare("SELECT value, format, dict_id FROM _lilia_json WHERE space = ?1 LIMIT ?2;")?;
        stmt.bind_text_static(1, space)?;
        stmt.bind_int64(2, i64::try_from(sample_count).unwrap_or(100))?;

        let mut samples_data: Vec<Vec<u8>> = Vec::new();
        while stmt.step()? {
            let format = stmt.column_int64(1);
            let dict_id = stmt.column_int64(2);
            let doc = decode_stored_doc(
                &stmt,
                0,
                format,
                dict_id,
                space,
                self.codec.as_ref(),
                &self.dict_registry,
                None,
            )?;
            samples_data.push(doc.into_bytes());
        }
        drop(stmt);

        if samples_data.is_empty() {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!("no documents found in space '{space}' to train dictionary"),
                false,
            ));
        }

        let sample_slices: Vec<&[u8]> = samples_data.iter().map(Vec::as_slice).collect();
        let target_size = if dict_size == 0 {
            self.json_options.dict_target_size_bytes
        } else {
            dict_size
        };

        let dict_bytes =
            lilia_core::compression::dict::train_dictionary(&sample_slices, target_size)?;

        let mut stmt_id = tx.prepare(
            "SELECT COALESCE(MAX(dict_id), 0) + 1 FROM _lilia_json_dictionaries WHERE space = ?1;",
        )?;
        stmt_id.bind_text_static(1, space)?;
        let next_dict_id = if stmt_id.step()? {
            u32::try_from(stmt_id.column_int64(0)).unwrap_or_default()
        } else {
            1
        };
        drop(stmt_id);

        let now = crate::storage::now_ms();
        let mut stmt_ins = tx.prepare(
            "INSERT INTO _lilia_json_dictionaries(space, dict_id, dict_data, sample_count, dict_size, created_at_ms) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(space, dict_id) DO UPDATE SET \
               dict_data = excluded.dict_data, \
               sample_count = excluded.sample_count, \
               dict_size = excluded.dict_size, \
               created_at_ms = excluded.created_at_ms;",
        )?;
        stmt_ins.bind_text_static(1, space)?;
        stmt_ins.bind_int64(2, i64::from(next_dict_id))?;
        stmt_ins.bind_blob_static(3, &dict_bytes)?;
        stmt_ins.bind_int64(4, i64::try_from(samples_data.len()).unwrap_or(0))?;
        stmt_ins.bind_int64(5, i64::try_from(dict_bytes.len()).unwrap_or(0))?;
        stmt_ins.bind_int64(6, now)?;
        stmt_ins.step()?;
        drop(stmt_ins);

        tx.commit()?;

        let symbols = std::sync::Arc::new(
            lilia_core::compression::dict::DictionarySymbolTable::from_dict_and_samples(
                &dict_bytes,
                &sample_slices,
            ),
        );
        self.dict_registry
            .register_with_symbols(space, next_dict_id, dict_bytes.clone(), symbols);

        Ok(DictStats {
            space: space.to_string(),
            dict_id: next_dict_id,
            sample_count: samples_data.len(),
            dict_size: dict_bytes.len(),
            created_at_ms: now,
        })
    }

    pub fn set_space_auto_train(&self, space: &str, enabled: bool) -> Result<()> {
        validate_name("space", space)?;
        let key = format!("auto_train:{space}");
        let val = if enabled { "1" } else { "0" };
        let mut writer = self.lock_writer()?;
        let tx = writer.transaction()?;
        let mut stmt = tx.prepare(
            "INSERT INTO _lilia_metadata(key, value) VALUES(?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value;",
        )?;
        stmt.bind_text_static(1, &key)?;
        stmt.bind_text_static(2, val)?;
        stmt.step()?;
        drop(stmt);
        tx.commit()?;
        Ok(())
    }

    #[must_use]
    pub fn is_space_auto_train_enabled(&self, space: &str) -> bool {
        if !self.json_options.auto_train_dict {
            return false;
        }
        if self
            .json_options
            .disabled_auto_train_spaces
            .iter()
            .any(|s| s == space)
        {
            return false;
        }
        if let Ok(conn) = self.read_lock() {
            if let Ok(mut stmt) = conn.prepare("SELECT value FROM _lilia_metadata WHERE key = ?1;")
            {
                let key = format!("auto_train:{space}");
                let _ = stmt.bind_text_static(1, &key);
                if let Ok(true) = stmt.step() {
                    return stmt.column_text(0) == "1";
                }
            }
        }
        true
    }

    /// Fluent JSON interface for document operations, time-travel queries, and history.
    #[must_use]
    pub fn json(&self) -> JsonStore<'_> {
        JsonStore { db: self }
    }

    /// Open a streaming aggregation pipeline on the specified JSON collection.
    #[must_use]
    pub fn aggregate_json(&self, space: &str) -> crate::aggregation::AggregationPipeline<'_> {
        crate::aggregation::AggregationPipeline::new(self, space)
    }

    /// Retrieve a JSON document as it existed at `target_version`.
    pub fn json_get_at_version(
        &self,
        space: &str,
        id: &str,
        target_version: u64,
    ) -> Result<Option<JsonEntry>> {
        struct RevRow {
            version: u64,
            delta: Vec<u8>,
            is_keyframe: bool,
            is_deletion: bool,
        }

        validate_name("space", space)?;
        validate_name("id", id)?;

        if target_version == 0 {
            return Ok(None);
        }

        // Fast path: if the requested version equals current version, read directly
        if let Some(current) = self.json_get(space, id)? {
            if current.version == target_version {
                return Ok(Some(current));
            }
        }

        let connection = self.read_lock()?;
        let mut stmt = connection.prepare(
            "SELECT version, delta, is_keyframe, is_deletion \
             FROM _lilia_json_revisions \
             WHERE space = ?1 AND id = ?2 AND version <= ?3 \
             ORDER BY version ASC;",
        )?;
        stmt.bind_text_static(1, space)?;
        stmt.bind_text_static(2, id)?;
        stmt.bind_int64(3, i64::try_from(target_version).unwrap_or(i64::MAX))?;

        let mut rows = Vec::new();
        while stmt.step()? {
            rows.push(RevRow {
                version: parse_version(stmt.column_int64(0))?,
                delta: stmt.column_blob(1).to_vec(),
                is_keyframe: stmt.column_int64(2) != 0,
                is_deletion: stmt.column_int64(3) != 0,
            });
        }
        drop(stmt);

        let Some(latest_applicable) = rows.last() else {
            return Ok(None);
        };
        if latest_applicable.is_deletion {
            return Ok(None);
        }

        let keyframe_idx = rows
            .iter()
            .rposition(|r| r.is_keyframe && r.version <= latest_applicable.version)
            .ok_or_else(|| {
                LiliaError::new(
                    ErrorCode::Corrupt,
                    format!("missing keyframe for revision history of {space}/{id}"),
                    false,
                )
            })?;

        let keyframe = &rows[keyframe_idx];
        let decompressed_kf = decompress_bytes(&keyframe.delta)?;
        let mut current_val: serde_json::Value = serde_json::from_slice(&decompressed_kf)
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;

        for rev in &rows[keyframe_idx + 1..] {
            if rev.is_deletion {
                current_val = serde_json::Value::Null;
            } else if rev.is_keyframe {
                let decompressed = decompress_bytes(&rev.delta)?;
                current_val = serde_json::from_slice(&decompressed)
                    .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
            } else {
                let decompressed_patch = decompress_bytes(&rev.delta)?;
                let patch: serde_json::Value = serde_json::from_slice(&decompressed_patch)
                    .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
                current_val = json_patch::apply(current_val, &patch)?;
            }
        }

        if current_val.is_null() {
            return Ok(None);
        }

        let doc = lilia_core::json::BinaryDoc::from_json_value(&current_val)?;
        Ok(Some(JsonEntry::new(
            space,
            id,
            doc,
            latest_applicable.version,
        )))
    }

    /// Retrieve a JSON document as it existed at or before `timestamp_ms`.
    pub fn json_get_at_timestamp(
        &self,
        space: &str,
        id: &str,
        timestamp_ms: i64,
    ) -> Result<Option<JsonEntry>> {
        validate_name("space", space)?;
        validate_name("id", id)?;

        let connection = self.read_lock()?;
        let mut stmt = connection.prepare(
            "SELECT version FROM _lilia_json_revisions \
             WHERE space = ?1 AND id = ?2 AND timestamp_ms <= ?3 \
             ORDER BY version DESC LIMIT 1;",
        )?;
        stmt.bind_text_static(1, space)?;
        stmt.bind_text_static(2, id)?;
        stmt.bind_int64(3, timestamp_ms)?;

        let target_ver = if stmt.step()? {
            Some(parse_version(stmt.column_int64(0))?)
        } else {
            None
        };
        drop(stmt);

        match target_ver {
            Some(ver) => self.json_get_at_version(space, id, ver),
            None => Ok(None),
        }
    }

    /// Retrieve the full revision history and delta patches for a JSON document.
    pub fn json_history(&self, space: &str, id: &str) -> Result<Vec<JsonRevisionInfo>> {
        validate_name("space", space)?;
        validate_name("id", id)?;

        let connection = self.read_lock()?;
        let mut stmt = connection.prepare(
            "SELECT version, timestamp_ms, delta, is_keyframe, is_deletion \
             FROM _lilia_json_revisions \
             WHERE space = ?1 AND id = ?2 \
             ORDER BY version ASC;",
        )?;
        stmt.bind_text_static(1, space)?;
        stmt.bind_text_static(2, id)?;

        let mut history = Vec::new();
        while stmt.step()? {
            let ver = parse_version(stmt.column_int64(0))?;
            let ts = stmt.column_int64(1);
            let delta = stmt.column_blob(2);
            let is_keyframe = stmt.column_int64(3) != 0;
            let is_deletion = stmt.column_int64(4) != 0;

            let patch = if !is_deletion && !delta.is_empty() {
                if let Ok(decompressed) = decompress_bytes(delta) {
                    serde_json::from_slice(&decompressed).ok()
                } else {
                    None
                }
            } else {
                None
            };

            history.push(JsonRevisionInfo {
                space: space.to_owned(),
                id: id.to_owned(),
                version: Version(ver),
                timestamp_ms: ts,
                is_deletion,
                is_keyframe,
                patch,
            });
        }
        Ok(history)
    }

    /// Compute an RFC 6902 JSON Patch describing the diff between any two historical versions.
    pub fn json_diff(
        &self,
        space: &str,
        id: &str,
        from_version: u64,
        to_version: u64,
    ) -> Result<serde_json::Value> {
        let from_doc = self.json_get_at_version(space, id, from_version)?;
        let to_doc = self.json_get_at_version(space, id, to_version)?;

        let from_val = from_doc.map_or(serde_json::Value::Null, |d| d.to_json_value());
        let to_val = to_doc.map_or(serde_json::Value::Null, |d| d.to_json_value());

        Ok(json_patch::diff(&from_val, &to_val))
    }
}

/// Fluent JSON interface for document operations, time-travel queries, and history.
#[derive(Debug, Clone, Copy)]
pub struct JsonStore<'a> {
    db: &'a Database,
}

impl<'a> JsonStore<'a> {
    /// Declares a JSON collection with `options`.
    ///
    /// A `check` expression (SQL three-valued logic) is enforced on every document write and
    /// rejected writes fail with `CONSTRAINT_VIOLATION`:
    /// `$.balance >= 0.0 AND length($.iban) == 27`.
    pub fn create_collection(
        &self,
        name: &str,
        options: impl Into<lilia_core::CollectionOptions>,
    ) -> Result<()> {
        self.db.declare_json_collection(name, &options.into())
    }

    /// Replaces (`Some`) or removes (`None`) the `CHECK` expression of a collection, after
    /// validating the documents it already holds.
    pub fn set_check(&self, name: &str, check: Option<&str>) -> Result<()> {
        self.db.apply_json_check(name, check)
    }

    /// Enables (`Some`) or disables (`None`) the background embedding of a text field into the
    /// vector collection of the same name.
    ///
    /// Enabling creates the HNSW index when it is missing and queues every document the
    /// collection already holds. Disabling stops the embedding; the stored vectors are kept.
    pub fn set_auto_embed(
        &self,
        name: &str,
        config: Option<&lilia_core::AutoEmbedConfig>,
    ) -> Result<()> {
        self.db.apply_json_auto_embed(name, config)
    }

    /// Queues every document of an auto-embedded collection again, e.g. after vectors were
    /// removed by hand.
    pub fn reembed(&self, name: &str) -> Result<()> {
        self.db.requeue_auto_embed(name)
    }

    /// Reports how many documents of the collection still await their vector or failed.
    pub fn embed_status(&self, name: &str) -> Result<lilia_core::EmbedStatus> {
        self.db.auto_embed_status(name)
    }

    /// Searches the auto-embedded collection for documents closest to `text`.
    ///
    /// The query is embedded with the model configured for the collection; hits contain document ids and distances.
    pub fn search_text(
        &self,
        space: &str,
        text: &str,
        limit: u32,
    ) -> Result<Vec<lilia_core::VectorHit>> {
        self.db.vector_search_text(space, text, limit)
    }

    #[must_use]
    pub fn aggregate(&self, space: &str) -> crate::aggregation::AggregationPipeline<'a> {
        crate::aggregation::AggregationPipeline::new(self.db, space)
    }

    pub fn create_index(
        &self,
        index_name: &str,
        space: &str,
        expression: &str,
        options: impl Into<lilia_core::IndexOptions>,
    ) -> Result<()> {
        self.db
            .create_json_index_with_options(space, index_name, expression, options.into())
    }

    pub fn create_index_simple(
        &self,
        index_name: &str,
        space: &str,
        expression: &str,
    ) -> Result<()> {
        self.db.create_json_index_with_options(
            space,
            index_name,
            expression,
            lilia_core::IndexOptions::default(),
        )
    }

    pub fn get(&self, space: &str, id: &str) -> Result<Option<JsonEntry>> {
        self.db.json_get(space, id)
    }

    pub fn extract(&self, space: &str, id: &str, path: &str) -> Result<Option<serde_json::Value>> {
        self.db.json_extract(space, id, path)
    }

    pub fn extract_str(&self, space: &str, id: &str, path: &str) -> Result<Option<String>> {
        self.db.json_extract_str(space, id, path)
    }

    pub fn extract_i64(&self, space: &str, id: &str, path: &str) -> Result<Option<i64>> {
        self.db.json_extract_i64(space, id, path)
    }

    pub fn extract_f64(&self, space: &str, id: &str, path: &str) -> Result<Option<f64>> {
        self.db.json_extract_f64(space, id, path)
    }

    pub fn extract_bool(&self, space: &str, id: &str, path: &str) -> Result<Option<bool>> {
        self.db.json_extract_bool(space, id, path)
    }

    pub fn train_dictionary(&self, space: &str) -> Result<DictStats> {
        self.db.train_json_dictionary(space, 100, 16 * 1024)
    }

    pub fn set_auto_train(&self, space: &str, enabled: bool) -> Result<()> {
        self.db.set_space_auto_train(space, enabled)
    }

    #[must_use]
    pub fn is_auto_train_enabled(&self, space: &str) -> bool {
        self.db.is_space_auto_train_enabled(space)
    }

    pub fn get_at_version(
        &self,
        space: &str,
        id: &str,
        version: impl Into<Version>,
    ) -> Result<Option<JsonEntry>> {
        self.db.json_get_at_version(space, id, version.into().0)
    }

    pub fn get_at_timestamp(
        &self,
        space: &str,
        id: &str,
        timestamp_ms: i64,
    ) -> Result<Option<JsonEntry>> {
        self.db.json_get_at_timestamp(space, id, timestamp_ms)
    }

    pub fn history(&self, space: &str, id: &str) -> Result<Vec<JsonRevisionInfo>> {
        self.db.json_history(space, id)
    }

    pub fn diff(
        &self,
        space: &str,
        id: &str,
        from_version: impl Into<Version>,
        to_version: impl Into<Version>,
    ) -> Result<serde_json::Value> {
        self.db
            .json_diff(space, id, from_version.into().0, to_version.into().0)
    }
}
