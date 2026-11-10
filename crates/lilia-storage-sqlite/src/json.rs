#![allow(clippy::missing_errors_doc)]

use lilia_sqlite_native::StmtSlot;

use crate::storage::{normalize_limit, parse_version, validate_name};
use crate::Database;
use lilia_core::{
    compression::decompress_bytes, json_patch, ErrorCode, JsonEntry, JsonIndexInfo,
    JsonRevisionInfo, LiliaError, Result, Version,
};

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

fn doc_matches_query(
    doc: &serde_json::Value,
    query: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    for (k, expected_v) in query {
        let actual_v = if let Some(direct) = doc.get(k) {
            Some(direct)
        } else {
            let ptr = if let Some(stripped) = k.strip_prefix("$.") {
                format!("/{}", stripped.replace('.', "/"))
            } else if k.contains('.') {
                format!("/{}", k.replace('.', "/"))
            } else if k.starts_with('/') {
                k.clone()
            } else {
                format!("/{k}")
            };
            doc.pointer(&ptr)
        };

        if let Some(actual) = actual_v {
            if actual == expected_v {
                continue;
            }
            if let serde_json::Value::Array(items) = actual {
                if items.iter().any(|item| item == expected_v) {
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

impl Database {
    pub fn json_get(&self, space: &str, id: &str) -> Result<Option<JsonEntry>> {
        validate_name("space", space)?;
        validate_name("id", id)?;
        let mut connection = self.read_lock()?;
        let stmt = connection.slot(StmtSlot::JsonGet)?;
        stmt.reset()?;
        stmt.bind_text_static(1, space)?;
        stmt.bind_text_static(2, id)?;
        let entry = if stmt.step()? {
            let encoded = stmt.column_text(0);
            let version = parse_version(stmt.column_int64(1))?;
            let value = decode_json_value(encoded, self.codec.as_ref())?;
            Some(JsonEntry {
                space: space.to_owned(),
                id: id.to_owned(),
                value,
                version,
            })
        } else {
            None
        };
        stmt.reset()?;
        Ok(entry)
    }

    pub fn json_scan(
        &self,
        space: &str,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JsonEntry>> {
        validate_name("space", space)?;
        let mut connection = self.read_lock()?;
        let stmt = connection.slot(StmtSlot::JsonScan)?;
        stmt.reset()?;
        stmt.bind_text_static(1, space)?;
        stmt.bind_text_static(2, after.unwrap_or(""))?;
        stmt.bind_int64(3, normalize_limit(limit))?;
        let mut entries = Vec::new();
        while stmt.step()? {
            let id = stmt.column_text(0).to_string();
            let encoded = stmt.column_text(1);
            let version = parse_version(stmt.column_int64(2))?;
            let value = decode_json_value(encoded, self.codec.as_ref())?;
            entries.push(JsonEntry {
                space: space.to_owned(),
                id,
                value,
                version,
            });
        }
        stmt.reset()?;
        Ok(entries)
    }

    pub fn create_json_index(&self, space: &str, index_name: &str, json_path: &str) -> Result<()> {
        validate_name("space", space)?;
        validate_name("index_name", index_name)?;
        if json_path.is_empty() {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "json_path cannot be empty",
                false,
            ));
        }
        let mut writer = self.lock_writer()?;
        let tx = writer.transaction()?;
        let mut stmt_upsert = tx.prepare(
            "INSERT INTO _lilia_json_indexes(space, index_name, json_path) VALUES(?1, ?2, ?3) \
             ON CONFLICT(space, index_name) DO UPDATE SET json_path = excluded.json_path;",
        )?;
        stmt_upsert.bind_text_static(1, space)?;
        stmt_upsert.bind_text_static(2, index_name)?;
        stmt_upsert.bind_text_static(3, json_path)?;
        stmt_upsert.step()?;
        drop(stmt_upsert);

        let mut stmt_delete = tx.prepare(
            "DELETE FROM _lilia_json_index_entries WHERE space = ?1 AND index_name = ?2;",
        )?;
        stmt_delete.bind_text_static(1, space)?;
        stmt_delete.bind_text_static(2, index_name)?;
        stmt_delete.step()?;
        drop(stmt_delete);

        let mut stmt_select = tx.prepare("SELECT id, value FROM _lilia_json WHERE space = ?1;")?;
        stmt_select.bind_text_static(1, space)?;
        let mut entries = Vec::new();
        while stmt_select.step()? {
            entries.push((
                stmt_select.column_text(0).to_string(),
                stmt_select.column_text(1).to_string(),
            ));
        }
        drop(stmt_select);

        let path = if let Some(stripped) = json_path.strip_prefix("$.") {
            format!("/{}", stripped.replace('.', "/"))
        } else if !json_path.starts_with('/') {
            format!("/{json_path}")
        } else {
            json_path.to_owned()
        };

        let mut ins = tx.prepare(
            "INSERT OR IGNORE INTO _lilia_json_index_entries(space, index_name, idx_value, doc_id) VALUES(?1, ?2, ?3, ?4);",
        )?;
        for (doc_id, encoded) in entries {
            let decoded = decode_json_value(&encoded, self.codec.as_ref())?;
            if let Some(target_val) = decoded.pointer(&path) {
                if let serde_json::Value::Array(items) = target_val {
                    for item in items {
                        let val_str = match item {
                            serde_json::Value::String(s) => s.clone(),
                            serde_json::Value::Number(n) => n.to_string(),
                            serde_json::Value::Bool(b) => b.to_string(),
                            serde_json::Value::Null => "null".to_string(),
                            _ => item.to_string(),
                        };
                        ins.reset()?;
                        ins.bind_text_static(1, space)?;
                        ins.bind_text_static(2, index_name)?;
                        ins.bind_text_static(3, &val_str)?;
                        ins.bind_text_static(4, &doc_id)?;
                        ins.step()?;
                    }
                } else {
                    let val_str = match target_val {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::Bool(b) => b.to_string(),
                        serde_json::Value::Null => "null".to_string(),
                        _ => target_val.to_string(),
                    };
                    ins.reset()?;
                    ins.bind_text_static(1, space)?;
                    ins.bind_text_static(2, index_name)?;
                    ins.bind_text_static(3, &val_str)?;
                    ins.bind_text_static(4, &doc_id)?;
                    ins.step()?;
                }
            }
        }
        drop(ins);
        tx.commit()?;
        Ok(())
    }

    pub fn drop_json_index(&self, space: &str, index_name: &str) -> Result<bool> {
        validate_name("space", space)?;
        validate_name("index_name", index_name)?;
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
            let mut stmt2 = tx.prepare(
                "DELETE FROM _lilia_json_index_entries WHERE space = ?1 AND index_name = ?2;",
            )?;
            stmt2.bind_text_static(1, space)?;
            stmt2.bind_text_static(2, index_name)?;
            stmt2.step()?;
            drop(stmt2);
        }
        tx.commit()?;
        Ok(deleted)
    }

    pub fn list_json_indexes(&self, space: &str) -> Result<Vec<JsonIndexInfo>> {
        validate_name("space", space)?;
        let connection = self.read_lock()?;
        let mut statement = connection.prepare(
            "SELECT space, index_name, json_path FROM _lilia_json_indexes WHERE space = ?1 ORDER BY index_name;",
        )?;
        statement.bind_text_static(1, space)?;
        let mut indexes = Vec::new();
        while statement.step()? {
            indexes.push(JsonIndexInfo {
                space: statement.column_text(0).to_string(),
                index_name: statement.column_text(1).to_string(),
                json_path: statement.column_text(2).to_string(),
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
        validate_name("space", space)?;
        validate_name("index_name", index_name)?;
        let connection = self.read_lock()?;
        let mut statement = connection.prepare(
            "SELECT j.id, j.value, j.version \
             FROM _lilia_json_index_entries e \
             JOIN _lilia_json j ON e.space = j.space AND e.doc_id = j.id \
             WHERE e.space = ?1 AND e.index_name = ?2 AND e.idx_value = ?3 AND e.doc_id > ?4 \
             ORDER BY e.doc_id LIMIT ?5;",
        )?;
        statement.bind_text_static(1, space)?;
        statement.bind_text_static(2, index_name)?;
        statement.bind_text_static(3, value)?;
        statement.bind_text_static(4, after_id.unwrap_or(""))?;
        statement.bind_int64(5, normalize_limit(limit))?;
        let mut entries = Vec::new();
        while statement.step()? {
            let id = statement.column_text(0).to_string();
            let encoded = statement.column_text(1);
            let version = parse_version(statement.column_int64(2))?;
            let val = decode_json_value(encoded, self.codec.as_ref())?;
            entries.push(JsonEntry {
                space: space.to_owned(),
                id,
                value: val,
                version,
            });
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
        validate_name("space", space)?;
        let connection = self.read_lock()?;
        let min_id = after_id.unwrap_or("");
        let mut statement = connection.prepare(
            "SELECT id, value, version FROM _lilia_json \
             WHERE space = ?1 AND id >= ?2 AND id > ?3 AND id LIKE ?4 \
             ORDER BY id LIMIT ?5;",
        )?;
        let like_pattern = format!("{prefix}%");
        statement.bind_text_static(1, space)?;
        statement.bind_text_static(2, prefix)?;
        statement.bind_text_static(3, min_id)?;
        statement.bind_text_static(4, &like_pattern)?;
        statement.bind_int64(5, normalize_limit(limit))?;
        let mut entries = Vec::new();
        while statement.step()? {
            let id = statement.column_text(0).to_string();
            let encoded = statement.column_text(1);
            let version = parse_version(statement.column_int64(2))?;
            let val = decode_json_value(encoded, self.codec.as_ref())?;
            entries.push(JsonEntry {
                space: space.to_owned(),
                id,
                value: val,
                version,
            });
        }
        Ok(entries)
    }

    pub fn json_find(
        &self,
        space: &str,
        query: &serde_json::Map<String, serde_json::Value>,
        after_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JsonEntry>> {
        validate_name("space", space)?;
        if query.is_empty() {
            return self.json_scan(space, after_id, limit);
        }

        let max_results = usize::try_from(normalize_limit(limit)).unwrap_or(usize::MAX);
        let indexes = self.list_json_indexes(space)?;

        let matched_index = query.iter().find_map(|(k, v)| {
            for idx in &indexes {
                let matches_name = &idx.index_name == k;
                let matches_exact_path = &idx.json_path == k;
                let matches_prefixed = idx.json_path.strip_prefix("$.") == Some(k.as_str());
                let matches_slash = idx.json_path.strip_prefix('/') == Some(k.as_str());
                if matches_name || matches_exact_path || matches_prefixed || matches_slash {
                    return Some((idx.clone(), v));
                }
            }
            None
        });

        let mut results = Vec::new();
        let mut cursor = after_id.map(ToOwned::to_owned);

        if let Some((idx, target_val)) = matched_index {
            let val_str = match target_val {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Null => "null".to_string(),
                _ => target_val.to_string(),
            };

            if query.len() == 1 {
                return self.json_scan_by_index(space, &idx.index_name, &val_str, after_id, limit);
            }

            while results.len() < max_results {
                let batch = self.json_scan_by_index(
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
                    cursor = Some(entry.id.clone());
                    if doc_matches_query(&entry.value, query) {
                        results.push(entry);
                        if results.len() == max_results {
                            break;
                        }
                    }
                }
            }
        } else {
            while results.len() < max_results {
                let batch = self.json_scan(space, cursor.as_deref(), 100)?;
                if batch.is_empty() {
                    break;
                }
                for entry in batch {
                    cursor = Some(entry.id.clone());
                    if doc_matches_query(&entry.value, query) {
                        results.push(entry);
                        if results.len() == max_results {
                            break;
                        }
                    }
                }
            }
        }

        Ok(results)
    }

    /// Fluent JSON interface for document operations, time-travel queries, and history.
    #[must_use]
    pub fn json(&self) -> JsonStore<'_> {
        JsonStore { db: self }
    }

    /// Retrieve a JSON document as it existed at `target_version`.
    pub fn json_get_at_version(
        &self,
        space: &str,
        id: &str,
        target_version: u64,
    ) -> Result<Option<JsonEntry>> {
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

        struct RevRow {
            version: u64,
            delta: Vec<u8>,
            is_keyframe: bool,
            is_deletion: bool,
        }

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

        if rows.is_empty() {
            return Ok(None);
        }

        // The state at target_version is determined by the last revision <= target_version
        let latest_applicable = rows.last().unwrap();
        if latest_applicable.is_deletion {
            return Ok(None);
        }

        // Find the index of the nearest keyframe <= latest_applicable
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

        // Start with the keyframe document
        let keyframe = &rows[keyframe_idx];
        let decompressed_kf = decompress_bytes(&keyframe.delta)?;
        let mut current_val: serde_json::Value = serde_json::from_slice(&decompressed_kf)
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;

        // Replay patches sequentially up to latest_applicable
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

        Ok(Some(JsonEntry {
            space: space.to_owned(),
            id: id.to_owned(),
            value: current_val,
            version: latest_applicable.version,
        }))
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

        let from_val = from_doc.map(|d| d.value).unwrap_or(serde_json::Value::Null);
        let to_val = to_doc.map(|d| d.value).unwrap_or(serde_json::Value::Null);

        Ok(json_patch::diff(&from_val, &to_val))
    }
}

/// Fluent JSON interface for document operations, time-travel queries, and history.
#[derive(Debug, Clone, Copy)]
pub struct JsonStore<'a> {
    db: &'a Database,
}

impl<'a> JsonStore<'a> {
    pub fn get(&self, space: &str, id: &str) -> Result<Option<JsonEntry>> {
        self.db.json_get(space, id)
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
