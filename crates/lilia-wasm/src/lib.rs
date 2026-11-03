#![allow(
    clippy::missing_errors_doc,
    clippy::needless_pass_by_value,
    clippy::unused_async
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lilia_core::{BatchOperation, Durability};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
use sqlite_wasm_rs::WasmOsCallback;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
use sqlite_wasm_vfs::sahpool::{install as install_sahpool, OpfsSAHPoolCfgBuilder};

trait ToJsError {
    fn to_js_error(self) -> JsValue;
}

impl ToJsError for lilia_core::LiliaError {
    fn to_js_error(self) -> JsValue {
        let json = serde_json::to_string(&self).unwrap_or_else(|_| self.to_string());
        JsValue::from_str(&json)
    }
}

impl ToJsError for serde_wasm_bindgen::Error {
    fn to_js_error(self) -> JsValue {
        JsValue::from_str(&self.to_string())
    }
}

fn to_js_err<E: ToJsError>(error: E) -> JsValue {
    error.to_js_error()
}

#[wasm_bindgen(start)]
pub fn start() {
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        std::panic::set_hook(Box::new(|info| {
            web_sys::console::error_1(&JsValue::from_str(&format!("WASM PANIC: {info}")));
        }));
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WasmKvEntry {
    pub namespace: String,
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub version: String,
    pub expires_at_ms: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WasmFtsHit {
    pub collection: String,
    pub doc_id: String,
    pub snippet: String,
    pub rank: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WasmOpenOptions {
    pub path: String,
    pub vfs: Option<String>,
    pub durability: Option<String>,
    pub busy_timeout_ms: Option<u32>,
    pub writer_queue_capacity: Option<u32>,
    pub encryption_key: Option<String>,
}

#[wasm_bindgen]
pub async fn init_opfs_vfs(directory: Option<String>) -> Result<(), JsValue> {
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        let mut builder = OpfsSAHPoolCfgBuilder::new();
        if let Some(ref dir) = directory {
            builder = builder.directory(dir);
        }
        let cfg = builder.build();
        install_sahpool::<WasmOsCallback>(&cfg, false)
            .await
            .map_err(|e| {
                JsValue::from_str(&format!("failed to install OPFS SAHPool VFS: {e:?}"))
            })?;
        Ok(())
    }
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    {
        let _ = directory;
        Ok(())
    }
}

#[wasm_bindgen]
#[must_use]
pub fn is_opfs_supported() -> bool {
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        if let Some(window) = web_sys::window() {
            return window.navigator().storage().is_truthy();
        }
        // In dedicated worker
        js_sys::Reflect::has(
            &js_sys::global(),
            &JsValue::from_str("FileSystemSyncAccessHandle"),
        )
        .unwrap_or(false)
    }
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    {
        false
    }
}

#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct WasmDatabase {
    inner: Arc<Database>,
}

#[wasm_bindgen]
impl WasmDatabase {
    #[wasm_bindgen]
    pub async fn open(options_val: JsValue) -> Result<WasmDatabase, JsValue> {
        let options: WasmOpenOptions = serde_wasm_bindgen::from_value(options_val)
            .map_err(|e| JsValue::from_str(&format!("invalid open options: {e}")))?;

        let is_mem = options.path == ":memory:"
            || options.vfs.as_deref() == Some("memory")
            || options.path.starts_with("file:lilia_mem_");

        let resolved_path = if is_mem {
            if options.path == ":memory:" || options.path.is_empty() {
                PathBuf::from(":memory:")
            } else if options.path.starts_with("file:") {
                PathBuf::from(options.path)
            } else {
                PathBuf::from(format!("file:{}?mode=memory&cache=shared", options.path))
            }
        } else if options.vfs.as_deref() == Some("opfs")
            || (options.vfs.as_deref() == Some("auto") && is_opfs_supported())
            || options.path.contains("?vfs=opfs-sahpool")
        {
            // Ensure OPFS SAHPool VFS is registered
            init_opfs_vfs(None).await?;
            if options.path.contains("?vfs=") {
                PathBuf::from(options.path)
            } else if options.path.starts_with("file:") {
                PathBuf::from(format!("{}?vfs=opfs-sahpool", options.path))
            } else {
                PathBuf::from(format!("file:{}?vfs=opfs-sahpool", options.path))
            }
        } else {
            PathBuf::from(options.path)
        };

        let mut settings = if is_mem && resolved_path.to_str() == Some(":memory:") {
            DatabaseOptions::in_memory()
        } else {
            DatabaseOptions::durable(resolved_path)
        };

        settings.durability = match options.durability.as_deref() {
            Some("balanced") => Durability::Balanced,
            Some("performance") => Durability::Performance,
            _ => Durability::Durable,
        };

        if let Some(value) = options.busy_timeout_ms {
            settings.busy_timeout_ms = u64::from(value);
        }
        if let Some(value) = options.writer_queue_capacity {
            settings.writer_queue_capacity = value as usize;
        }

        if let Some(key) = options.encryption_key {
            settings =
                settings.with_codec(Arc::new(lilia_core::Aes256GcmCodec::from_passphrase(&key)));
        }

        let database = Database::open(settings).map_err(to_js_err)?;
        Ok(Self {
            inner: Arc::new(database),
        })
    }

    #[wasm_bindgen]
    pub fn kv_get(&self, namespace: String, key: Vec<u8>) -> Result<JsValue, JsValue> {
        let entry = self.inner.kv_get(&namespace, &key).map_err(to_js_err)?;
        match entry {
            Some(e) => {
                let wasm_entry = WasmKvEntry {
                    namespace: e.namespace,
                    key: e.key,
                    value: e.value,
                    version: e.version.to_string(),
                    expires_at_ms: e.expires_at_ms,
                };
                serde_wasm_bindgen::to_value(&wasm_entry).map_err(to_js_err)
            }
            None => Ok(JsValue::UNDEFINED),
        }
    }

    #[wasm_bindgen]
    pub fn kv_scan(
        &self,
        namespace: String,
        after: Option<Vec<u8>>,
        limit: u32,
    ) -> Result<JsValue, JsValue> {
        let entries = self
            .inner
            .kv_scan(&namespace, after.as_deref(), limit)
            .map_err(to_js_err)?;
        let wasm_entries: Vec<WasmKvEntry> = entries
            .into_iter()
            .map(|e| WasmKvEntry {
                namespace: e.namespace,
                key: e.key,
                value: e.value,
                version: e.version.to_string(),
                expires_at_ms: e.expires_at_ms,
            })
            .collect();
        serde_wasm_bindgen::to_value(&wasm_entries).map_err(to_js_err)
    }

    #[wasm_bindgen]
    pub fn json_get(&self, space: String, id: String) -> Result<Option<String>, JsValue> {
        let entry = self.inner.json_get(&space, &id).map_err(to_js_err)?;
        match entry {
            Some(e) => serde_json::to_string(&e)
                .map(Some)
                .map_err(|err| JsValue::from_str(&err.to_string())),
            None => Ok(None),
        }
    }

    #[wasm_bindgen]
    pub fn json_scan(
        &self,
        space: String,
        after: Option<String>,
        limit: u32,
    ) -> Result<String, JsValue> {
        let entries = self
            .inner
            .json_scan(&space, after.as_deref(), limit)
            .map_err(to_js_err)?;
        serde_json::to_string(&entries).map_err(|err| JsValue::from_str(&err.to_string()))
    }

    #[wasm_bindgen]
    pub fn json_create_index(
        &self,
        space: String,
        name: String,
        path: String,
    ) -> Result<(), JsValue> {
        self.inner
            .create_json_index(&space, &name, &path)
            .map_err(to_js_err)
    }

    #[wasm_bindgen]
    pub fn json_drop_index(&self, space: String, name: String) -> Result<bool, JsValue> {
        self.inner.drop_json_index(&space, &name).map_err(to_js_err)
    }

    #[wasm_bindgen]
    pub fn json_list_indexes(&self, space: String) -> Result<String, JsValue> {
        let indexes = self.inner.list_json_indexes(&space).map_err(to_js_err)?;
        serde_json::to_string(&indexes).map_err(|err| JsValue::from_str(&err.to_string()))
    }

    #[wasm_bindgen]
    pub fn json_scan_by_index(
        &self,
        space: String,
        index_name: String,
        value: String,
        after: Option<String>,
        limit: u32,
    ) -> Result<String, JsValue> {
        let entries = self
            .inner
            .json_scan_by_index(&space, &index_name, &value, after.as_deref(), limit)
            .map_err(to_js_err)?;
        serde_json::to_string(&entries).map_err(|err| JsValue::from_str(&err.to_string()))
    }

    #[wasm_bindgen]
    pub fn json_find(
        &self,
        space: String,
        query_json: String,
        after: Option<String>,
        limit: u32,
    ) -> Result<String, JsValue> {
        let query_val: serde_json::Value = serde_json::from_str(&query_json)
            .map_err(|e| JsValue::from_str(&format!("invalid JSON query: {e}")))?;
        let query_map = query_val
            .as_object()
            .ok_or_else(|| JsValue::from_str("query must be a JSON object"))?;
        let entries = self
            .inner
            .json_find(&space, query_map, after.as_deref(), limit)
            .map_err(to_js_err)?;
        serde_json::to_string(&entries).map_err(|err| JsValue::from_str(&err.to_string()))
    }

    #[wasm_bindgen]
    pub fn batch(&self, operations_json: String) -> Result<String, JsValue> {
        let operations: Vec<BatchOperation> = serde_json::from_str(&operations_json)
            .map_err(|e| JsValue::from_str(&format!("invalid batch operations: {e}")))?;
        let results = self.inner.batch(&operations).map_err(to_js_err)?;
        serde_json::to_string(&results).map_err(|err| JsValue::from_str(&err.to_string()))
    }

    #[wasm_bindgen]
    pub fn graph_neighbors(
        &self,
        graph: String,
        node: String,
        direction: Option<String>,
        kind: Option<String>,
    ) -> Result<JsValue, JsValue> {
        let dir = match direction.as_deref() {
            Some("incoming") => lilia_core::Direction::Incoming,
            Some("both") => lilia_core::Direction::Both,
            _ => lilia_core::Direction::Outgoing,
        };
        let neighbors = self
            .inner
            .graph_neighbors(&graph, &node, dir, kind.as_deref())
            .map_err(to_js_err)?;
        serde_wasm_bindgen::to_value(&neighbors).map_err(to_js_err)
    }

    #[wasm_bindgen]
    pub fn graph_reachability(
        &self,
        graph: String,
        start_nodes_val: JsValue,
        direction: Option<String>,
    ) -> Result<JsValue, JsValue> {
        let start_nodes: Vec<String> = serde_wasm_bindgen::from_value(start_nodes_val)
            .map_err(|e| JsValue::from_str(&format!("invalid start_nodes array: {e}")))?;
        let dir = match direction.as_deref() {
            Some("incoming") => lilia_core::Direction::Incoming,
            Some("both") => lilia_core::Direction::Both,
            _ => lilia_core::Direction::Outgoing,
        };
        let reachable = self
            .inner
            .graph_reachability(&graph, &start_nodes, dir)
            .map_err(to_js_err)?;
        serde_wasm_bindgen::to_value(&reachable).map_err(to_js_err)
    }

    #[wasm_bindgen]
    pub fn fts_search(
        &self,
        collection: String,
        query: String,
        limit: Option<u32>,
    ) -> Result<JsValue, JsValue> {
        let hits = self
            .inner
            .fts_search(&collection, &query, limit.unwrap_or(10))
            .map_err(to_js_err)?;
        let wasm_hits: Vec<WasmFtsHit> = hits
            .into_iter()
            .map(|h| WasmFtsHit {
                collection: h.collection,
                doc_id: h.doc_id,
                snippet: h.snippet,
                rank: h.rank,
            })
            .collect();
        serde_wasm_bindgen::to_value(&wasm_hits).map_err(to_js_err)
    }

    #[wasm_bindgen]
    pub fn sql_query(&self, sql: String, params_json: Option<String>) -> Result<String, JsValue> {
        let params: Vec<serde_json::Value> = match params_json {
            Some(ref s) if !s.trim().is_empty() => serde_json::from_str(s)
                .map_err(|e| JsValue::from_str(&format!("invalid sql params: {e}")))?,
            _ => Vec::new(),
        };
        let result = self.inner.sql_query(&sql, &params).map_err(to_js_err)?;
        serde_json::to_string(&result).map_err(|err| JsValue::from_str(&err.to_string()))
    }

    #[wasm_bindgen]
    pub fn integrity_check(&self) -> Result<bool, JsValue> {
        self.inner.integrity_check().map_err(to_js_err)
    }

    #[wasm_bindgen]
    pub fn close(&self, timeout_ms: Option<u32>) -> Result<(), JsValue> {
        let timeout = Duration::from_millis(u64::from(timeout_ms.unwrap_or(5_000)));
        self.inner.close(timeout).map_err(to_js_err)
    }
}
