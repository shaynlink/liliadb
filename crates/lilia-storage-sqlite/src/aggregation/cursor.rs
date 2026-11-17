use std::sync::Arc;

use lilia_core::json::BinaryDoc;
use lilia_core::{CollectionKind, Result, StorageTier};
use lilia_sqlite_native::NativeStatement;
use serde_json::Value;

use crate::engine::WriterGuard;
use crate::json::decode_stored_doc;
use crate::Database;

/// A single document yielded by the streaming storage scanner.
#[derive(Debug, Clone)]
pub struct StreamDocument {
    pub id: String,
    pub doc: BinaryDoc,
    pub unwound: Option<(String, Value)>,
}

impl StreamDocument {
    #[must_use]
    pub fn new(id: String, doc: BinaryDoc) -> Self {
        Self {
            id,
            doc,
            unwound: None,
        }
    }

    #[must_use]
    pub fn with_unwound(id: String, doc: BinaryDoc, field: String, value: Value) -> Self {
        Self {
            id,
            doc,
            unwound: Some((field, value)),
        }
    }

    #[must_use]
    pub fn extract_f64(&self, path: &str) -> Option<f64> {
        if let Some((ref field, ref val)) = self.unwound {
            if let Some(f) = extract_from_unwound(field, val, path).and_then(value_to_f64) {
                return Some(f);
            }
        }
        self.doc.extract_f64(path)
    }

    #[must_use]
    pub fn extract_str(&self, path: &str) -> Option<String> {
        if let Some((ref field, ref val)) = self.unwound {
            if let Some(v) = extract_from_unwound(field, val, path) {
                match v {
                    Value::String(s) => return Some(s),
                    Value::Number(n) => return Some(n.to_string()),
                    Value::Bool(b) => return Some(b.to_string()),
                    _ => return Some(v.to_string()),
                }
            }
        }
        self.doc
            .extract_str(path)
            .map(ToOwned::to_owned)
            .or_else(|| self.doc.extract_i64(path).map(|i| i.to_string()))
    }

    #[must_use]
    pub fn extract_value(&self, path: &str) -> Option<Value> {
        if let Some((ref field, ref val)) = self.unwound {
            if let Some(v) = extract_from_unwound(field, val, path) {
                return Some(v);
            }
        }
        self.doc.extract(path).map(|v| v.to_json_value())
    }

    #[must_use]
    pub fn to_json_value(&self) -> Value {
        let mut val = self.doc.to_json_value();
        if let Some((ref field, ref unwound_val)) = self.unwound {
            if let Value::Object(ref mut map) = val {
                map.insert(field.clone(), unwound_val.clone());
            }
        }
        val
    }
}

fn extract_from_unwound(field_name: &str, unwound_val: &Value, query_path: &str) -> Option<Value> {
    let clean_path = query_path.trim_start_matches("$.").trim_start_matches('/');

    // 1. Direct match on key if unwound_val is an Object
    if let Value::Object(map) = unwound_val {
        if let Some(v) = map.get(clean_path) {
            return Some(v.clone());
        }
    }

    // 2. Prefixed match (e.g. query is "items.category" and field is "items")
    let prefix = format!("{field_name}.");
    if let Some(subpath) = clean_path.strip_prefix(&prefix) {
        if let Value::Object(map) = unwound_val {
            if let Some(v) = map.get(subpath) {
                return Some(v.clone());
            }
        }
    }

    // 3. Exact match with field_name itself
    if clean_path == field_name {
        return Some(unwound_val.clone());
    }

    None
}

fn value_to_f64(v: Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// A zero-allocation pull-based SQLite document cursor stream.
pub struct SqliteCursorStream<'a> {
    stmt: NativeStatement,
    _guard: WriterGuard<'a>,
    space: String,
    codec: Arc<dyn lilia_core::ValueCodec>,
    dict_registry: Arc<lilia_core::DictionaryRegistry>,
    key_registry: Arc<lilia_core::KeyRegistry>,
    finished: bool,
}

impl std::fmt::Debug for SqliteCursorStream<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteCursorStream")
            .field("space", &self.space)
            .field("finished", &self.finished)
            .finish()
    }
}

impl Iterator for SqliteCursorStream<'_> {
    type Item = Result<StreamDocument>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        match self.stmt.step() {
            Ok(true) => {
                let id = self.stmt.column_text(0).to_string();
                let format = self.stmt.column_int64(2);
                let dict_id = self.stmt.column_int64(3);
                match decode_stored_doc(
                    &self.stmt,
                    1,
                    format,
                    dict_id,
                    &self.space,
                    self.codec.as_ref(),
                    &self.dict_registry,
                    Some(self.key_registry.as_ref()),
                ) {
                    Ok(doc) => Some(Ok(StreamDocument::new(id, doc))),
                    Err(e) => {
                        self.finished = true;
                        Some(Err(e))
                    }
                }
            }
            Ok(false) => {
                self.finished = true;
                None
            }
            Err(e) => {
                self.finished = true;
                Some(Err(e))
            }
        }
    }
}

/// Open a streaming iterator over documents in the specified space with O(1) RAM.
pub fn open_document_stream<'a>(
    db: &'a Database,
    space: &str,
) -> Result<Box<dyn Iterator<Item = Result<StreamDocument>> + 'a>> {
    crate::storage::validate_name("space", space)?;
    let tier = db.catalog.tier_for(CollectionKind::Json, space);

    if tier == StorageTier::Memory {
        let now = crate::storage::now_ms();
        let raw = db.mem_engine.json.scan(Some(space), None, usize::MAX, now);
        let iter = raw.into_iter().filter_map(|(_ns, key_bytes, snap)| {
            let id = String::from_utf8(key_bytes).ok()?;
            let doc = BinaryDoc::from_json_value(&snap.value);
            Some(Ok(StreamDocument::new(id, doc)))
        });
        return Ok(Box::new(iter));
    }

    if tier == StorageTier::Buffered {
        db.ring_core.flush()?;
    }

    let guard = db.read_lock()?;
    let mut stmt = guard.prepare(
        "SELECT id, value, format, dict_id, version FROM _lilia_json WHERE space = ?1 ORDER BY id;",
    )?;
    stmt.bind_text_transient(1, space)?;

    Ok(Box::new(SqliteCursorStream {
        stmt,
        _guard: guard,
        space: space.to_string(),
        codec: Arc::clone(&db.codec),
        dict_registry: Arc::clone(&db.dict_registry),
        key_registry: Arc::clone(&db.key_registry),
        finished: false,
    }))
}
