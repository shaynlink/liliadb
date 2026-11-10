#![allow(unsafe_code)]
#![allow(clippy::missing_safety_doc)]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_uchar};
use std::sync::Arc;

use crate::{FfiDirection, FfiResourceProfile, LiliaDatabase};

#[derive(Debug)]
pub struct LiliaHandle {
    pub db: Arc<LiliaDatabase>,
    pub path: String,
}

fn str_from_ptr<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(ptr).to_str().ok() }
    }
}

fn to_c_string(s: String) -> *mut c_char {
    CString::new(s).unwrap_or_default().into_raw()
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_free_string(ptr: *mut c_char) {
    if !ptr.is_null() {
        drop(CString::from_raw(ptr));
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_free_bytes(ptr: *mut c_uchar, len: usize) {
    if !ptr.is_null() && len > 0 {
        drop(Vec::from_raw_parts(ptr, len, len));
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_free_handle(handle: *mut LiliaHandle) {
    if !handle.is_null() {
        drop(Box::from_raw(handle));
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_open(
    path: *const c_char,
    profile_code: c_int,
    key_bytes: *const c_uchar,
    key_len: usize,
    passphrase: *const c_char,
    out_err: *mut *mut c_char,
) -> *mut LiliaHandle {
    let path_str = match str_from_ptr(path) {
        Some(p) => p.to_string(),
        None => {
            if !out_err.is_null() {
                *out_err = to_c_string("Path is required".into());
            }
            return std::ptr::null_mut();
        }
    };

    let profile = match profile_code {
        1 => FfiResourceProfile::Constrained,
        2 => FfiResourceProfile::Server,
        _ => FfiResourceProfile::Mobile,
    };

    let result = if !passphrase.is_null() {
        if let Some(pass) = str_from_ptr(passphrase) {
            LiliaDatabase::open_with_passphrase(path_str.clone(), profile, pass.to_string())
        } else {
            LiliaDatabase::open(path_str.clone(), profile, None)
        }
    } else if !key_bytes.is_null() && key_len == 32 {
        let slice = std::slice::from_raw_parts(key_bytes, 32);
        LiliaDatabase::open(path_str.clone(), profile, Some(slice.to_vec()))
    } else {
        LiliaDatabase::open(path_str.clone(), profile, None)
    };

    match result {
        Ok(db) => Box::into_raw(Box::new(LiliaHandle { db, path: path_str })),
        Err(err) => {
            if !out_err.is_null() {
                *out_err = to_c_string(err.to_string());
            }
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_close(handle: *mut LiliaHandle, timeout_ms: u64) -> c_int {
    if handle.is_null() {
        return -1;
    }
    let h = &*handle;
    match h.db.close_sync(timeout_ms) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_prepare_for_background(handle: *mut LiliaHandle) -> c_int {
    if handle.is_null() {
        return -1;
    }
    let h = &*handle;
    match h.db.prepare_for_background() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_checkpoint(handle: *mut LiliaHandle) -> c_int {
    if handle.is_null() {
        return -1;
    }
    let h = &*handle;
    match h.db.checkpoint_sync() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_vacuum(handle: *mut LiliaHandle) -> c_int {
    if handle.is_null() {
        return -1;
    }
    let h = &*handle;
    match h.db.sql_query_sync("VACUUM;".into(), None) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_get_file_size(
    handle: *mut LiliaHandle,
    out_main: *mut u64,
    out_wal: *mut u64,
    out_shm: *mut u64,
) -> c_int {
    if handle.is_null() {
        return -1;
    }
    let h = &*handle;
    let main_len = std::fs::metadata(&h.path).map(|m| m.len()).unwrap_or(0);
    let wal_len = std::fs::metadata(format!("{}-wal", h.path))
        .map(|m| m.len())
        .unwrap_or(0);
    let shm_len = std::fs::metadata(format!("{}-shm", h.path))
        .map(|m| m.len())
        .unwrap_or(0);

    if !out_main.is_null() {
        *out_main = main_len;
    }
    if !out_wal.is_null() {
        *out_wal = wal_len;
    }
    if !out_shm.is_null() {
        *out_shm = shm_len;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_backup(handle: *mut LiliaHandle, dest: *const c_char) -> c_int {
    if handle.is_null() {
        return -1;
    }
    let dest_str = match str_from_ptr(dest) {
        Some(s) => s,
        None => return -1,
    };
    let h = &*handle;
    match h.db.backup_sync(dest_str.to_string()) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

// ==========================================
// KEY / VALUE ENGINE
// ==========================================

#[no_mangle]
pub unsafe extern "C" fn lilia_c_kv_get(
    handle: *mut LiliaHandle,
    ns: *const c_char,
    key: *const c_uchar,
    key_len: usize,
    out_val_len: *mut usize,
    out_version: *mut u64,
    out_expires: *mut i64,
) -> *mut c_uchar {
    if handle.is_null() || ns.is_null() || key.is_null() {
        return std::ptr::null_mut();
    }
    let ns_str = match str_from_ptr(ns) {
        Some(s) => s.to_string(),
        None => return std::ptr::null_mut(),
    };
    let key_vec = std::slice::from_raw_parts(key, key_len).to_vec();
    let h = &*handle;

    match h.db.kv_get_sync(ns_str, key_vec) {
        Ok(Some(entry)) => {
            if !out_version.is_null() {
                *out_version = entry.version;
            }
            if !out_expires.is_null() {
                *out_expires = entry.expires_at_ms.unwrap_or(0);
            }
            if !out_val_len.is_null() {
                *out_val_len = entry.value.len();
            }
            let mut val_bytes = entry.value.into_boxed_slice();
            let ptr = val_bytes.as_mut_ptr();
            std::mem::forget(val_bytes);
            ptr
        }
        _ => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_kv_set(
    handle: *mut LiliaHandle,
    ns: *const c_char,
    key: *const c_uchar,
    key_len: usize,
    val: *const c_uchar,
    val_len: usize,
    expires_at_ms: i64,
    out_version: *mut u64,
) -> c_int {
    if handle.is_null() || ns.is_null() || key.is_null() || val.is_null() {
        return -1;
    }
    let ns_str = match str_from_ptr(ns) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let key_vec = std::slice::from_raw_parts(key, key_len).to_vec();
    let val_vec = std::slice::from_raw_parts(val, val_len).to_vec();
    let exp_opt = if expires_at_ms > 0 {
        Some(expires_at_ms)
    } else {
        None
    };
    let h = &*handle;

    match h.db.kv_set_sync(ns_str, key_vec, val_vec, exp_opt) {
        Ok(v) => {
            if !out_version.is_null() {
                *out_version = v;
            }
            0
        }
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_kv_delete(
    handle: *mut LiliaHandle,
    ns: *const c_char,
    key: *const c_uchar,
    key_len: usize,
    out_deleted: *mut c_int,
) -> c_int {
    if handle.is_null() || ns.is_null() || key.is_null() {
        return -1;
    }
    let ns_str = match str_from_ptr(ns) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let key_vec = std::slice::from_raw_parts(key, key_len).to_vec();
    let h = &*handle;

    match h.db.kv_delete_sync(ns_str, key_vec) {
        Ok(deleted) => {
            if !out_deleted.is_null() {
                *out_deleted = if deleted { 1 } else { 0 };
            }
            0
        }
        Err(_) => -1,
    }
}

// ==========================================
// JSON DOCUMENT ENGINE
// ==========================================

#[no_mangle]
pub unsafe extern "C" fn lilia_c_json_get(
    handle: *mut LiliaHandle,
    space: *const c_char,
    id: *const c_char,
    out_version: *mut u64,
) -> *mut c_char {
    if handle.is_null() || space.is_null() || id.is_null() {
        return std::ptr::null_mut();
    }
    let space_str = match str_from_ptr(space) {
        Some(s) => s.to_string(),
        None => return std::ptr::null_mut(),
    };
    let id_str = match str_from_ptr(id) {
        Some(s) => s.to_string(),
        None => return std::ptr::null_mut(),
    };
    let h = &*handle;

    match h.db.json_get_sync(space_str, id_str) {
        Ok(Some(entry)) => {
            if !out_version.is_null() {
                *out_version = entry.version;
            }
            to_c_string(entry.value_json)
        }
        _ => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_json_put(
    handle: *mut LiliaHandle,
    space: *const c_char,
    id: *const c_char,
    val_json: *const c_char,
    out_version: *mut u64,
) -> c_int {
    if handle.is_null() || space.is_null() || id.is_null() || val_json.is_null() {
        return -1;
    }
    let space_str = match str_from_ptr(space) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let id_str = match str_from_ptr(id) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let json_str = match str_from_ptr(val_json) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let h = &*handle;

    match h.db.json_put_sync(space_str, id_str, json_str) {
        Ok(v) => {
            if !out_version.is_null() {
                *out_version = v;
            }
            0
        }
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_json_delete(
    handle: *mut LiliaHandle,
    space: *const c_char,
    id: *const c_char,
    out_deleted: *mut c_int,
) -> c_int {
    if handle.is_null() || space.is_null() || id.is_null() {
        return -1;
    }
    let space_str = match str_from_ptr(space) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let id_str = match str_from_ptr(id) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let h = &*handle;

    match h.db.json_delete_sync(space_str, id_str) {
        Ok(deleted) => {
            if !out_deleted.is_null() {
                *out_deleted = if deleted { 1 } else { 0 };
            }
            0
        }
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_json_find(
    handle: *mut LiliaHandle,
    space: *const c_char,
    query_json: *const c_char,
    after_id: *const c_char,
    limit: u32,
    out_json: *mut *mut c_char,
) -> c_int {
    if handle.is_null() || space.is_null() || query_json.is_null() || out_json.is_null() {
        return -1;
    }
    let space_str = match str_from_ptr(space) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let query_str = match str_from_ptr(query_json) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let after_opt = str_from_ptr(after_id).map(|s| s.to_string());
    let h = &*handle;

    match h.db.json_find_sync(space_str, query_str, after_opt, limit) {
        Ok(entries) => {
            let json = serde_json::to_string(&entries).unwrap_or_else(|_| "[]".into());
            *out_json = to_c_string(json);
            0
        }
        Err(_) => -1,
    }
}

// ==========================================
// GRAPH ENGINE
// ==========================================

#[no_mangle]
pub unsafe extern "C" fn lilia_c_graph_set_node(
    handle: *mut LiliaHandle,
    graph: *const c_char,
    id: *const c_char,
    kind: *const c_char,
    metadata_json: *const c_char,
) -> c_int {
    if handle.is_null()
        || graph.is_null()
        || id.is_null()
        || kind.is_null()
        || metadata_json.is_null()
    {
        return -1;
    }
    let g = match str_from_ptr(graph) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let i = match str_from_ptr(id) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let k = match str_from_ptr(kind) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let m = match str_from_ptr(metadata_json) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let h = &*handle;

    match h.db.graph_set_node_sync(g, i, k, m) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_graph_get_node(
    handle: *mut LiliaHandle,
    graph: *const c_char,
    id: *const c_char,
    out_json: *mut *mut c_char,
) -> c_int {
    if handle.is_null() || graph.is_null() || id.is_null() || out_json.is_null() {
        return -1;
    }
    let g = match str_from_ptr(graph) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let i = match str_from_ptr(id) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let h = &*handle;

    match h.db.graph_get_node_sync(g, i) {
        Ok(Some(node)) => {
            let json = serde_json::to_string(&node).unwrap_or_else(|_| "null".into());
            *out_json = to_c_string(json);
            0
        }
        Ok(None) => {
            *out_json = to_c_string("null".into());
            0
        }
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_graph_delete_node(
    handle: *mut LiliaHandle,
    graph: *const c_char,
    id: *const c_char,
    out_deleted: *mut c_int,
) -> c_int {
    if handle.is_null() || graph.is_null() || id.is_null() {
        return -1;
    }
    let g = match str_from_ptr(graph) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let i = match str_from_ptr(id) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let h = &*handle;

    match h.db.graph_delete_node_sync(g, i) {
        Ok(deleted) => {
            if !out_deleted.is_null() {
                *out_deleted = if deleted { 1 } else { 0 };
            }
            0
        }
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_graph_add_edge(
    handle: *mut LiliaHandle,
    graph: *const c_char,
    source: *const c_char,
    target: *const c_char,
    kind: *const c_char,
) -> c_int {
    if handle.is_null() || graph.is_null() || source.is_null() || target.is_null() || kind.is_null()
    {
        return -1;
    }
    let g = match str_from_ptr(graph) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let s = match str_from_ptr(source) {
        Some(str_) => str_.to_string(),
        None => return -1,
    };
    let t = match str_from_ptr(target) {
        Some(str_) => str_.to_string(),
        None => return -1,
    };
    let k = match str_from_ptr(kind) {
        Some(str_) => str_.to_string(),
        None => return -1,
    };
    let h = &*handle;

    match h.db.graph_add_edge_sync(g, s, t, k) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_graph_remove_edge(
    handle: *mut LiliaHandle,
    graph: *const c_char,
    source: *const c_char,
    target: *const c_char,
    kind: *const c_char,
    out_deleted: *mut c_int,
) -> c_int {
    if handle.is_null() || graph.is_null() || source.is_null() || target.is_null() || kind.is_null()
    {
        return -1;
    }
    let g = match str_from_ptr(graph) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let s = match str_from_ptr(source) {
        Some(str_) => str_.to_string(),
        None => return -1,
    };
    let t = match str_from_ptr(target) {
        Some(str_) => str_.to_string(),
        None => return -1,
    };
    let k = match str_from_ptr(kind) {
        Some(str_) => str_.to_string(),
        None => return -1,
    };
    let h = &*handle;

    match h.db.graph_remove_edge_sync(g, s, t, k) {
        Ok(deleted) => {
            if !out_deleted.is_null() {
                *out_deleted = if deleted { 1 } else { 0 };
            }
            0
        }
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_graph_neighbors(
    handle: *mut LiliaHandle,
    graph: *const c_char,
    node: *const c_char,
    direction: c_int,
    kind: *const c_char,
    out_json: *mut *mut c_char,
) -> c_int {
    if handle.is_null() || graph.is_null() || node.is_null() || out_json.is_null() {
        return -1;
    }
    let g = match str_from_ptr(graph) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let n = match str_from_ptr(node) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let k_opt = str_from_ptr(kind).map(|s| s.to_string());
    let dir = match direction {
        0 => FfiDirection::Incoming,
        1 => FfiDirection::Outgoing,
        _ => FfiDirection::Both,
    };
    let h = &*handle;

    match h.db.graph_neighbors_sync(g, n, dir, k_opt) {
        Ok(neighbors) => {
            let json = serde_json::to_string(&neighbors).unwrap_or_else(|_| "[]".into());
            *out_json = to_c_string(json);
            0
        }
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_graph_reachability(
    handle: *mut LiliaHandle,
    graph: *const c_char,
    start_nodes_json: *const c_char,
    direction: c_int,
    out_json: *mut *mut c_char,
) -> c_int {
    if handle.is_null() || graph.is_null() || start_nodes_json.is_null() || out_json.is_null() {
        return -1;
    }
    let g = match str_from_ptr(graph) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let json_str = match str_from_ptr(start_nodes_json) {
        Some(s) => s,
        None => return -1,
    };
    let start_nodes: Vec<String> = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return -1,
    };
    let dir = match direction {
        0 => FfiDirection::Incoming,
        1 => FfiDirection::Outgoing,
        _ => FfiDirection::Both,
    };
    let h = &*handle;

    match h.db.graph_reachability_sync(g, start_nodes, dir) {
        Ok(reachable) => {
            let json = serde_json::to_string(&reachable).unwrap_or_else(|_| "[]".into());
            *out_json = to_c_string(json);
            0
        }
        Err(_) => -1,
    }
}

// ==========================================
// FTS5 SEARCH ENGINE
// ==========================================

#[no_mangle]
pub unsafe extern "C" fn lilia_c_fts_index(
    handle: *mut LiliaHandle,
    collection: *const c_char,
    doc_id: *const c_char,
    content: *const c_char,
) -> c_int {
    if handle.is_null() || collection.is_null() || doc_id.is_null() || content.is_null() {
        return -1;
    }
    let col = match str_from_ptr(collection) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let id = match str_from_ptr(doc_id) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let text = match str_from_ptr(content) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let h = &*handle;

    match h.db.fts_index_sync(col, id, text) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_fts_delete(
    handle: *mut LiliaHandle,
    collection: *const c_char,
    doc_id: *const c_char,
    out_deleted: *mut c_int,
) -> c_int {
    if handle.is_null() || collection.is_null() || doc_id.is_null() {
        return -1;
    }
    let col = match str_from_ptr(collection) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let id = match str_from_ptr(doc_id) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let h = &*handle;

    match h.db.fts_delete_sync(col, id) {
        Ok(deleted) => {
            if !out_deleted.is_null() {
                *out_deleted = if deleted { 1 } else { 0 };
            }
            0
        }
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_fts_search(
    handle: *mut LiliaHandle,
    collection: *const c_char,
    query: *const c_char,
    limit: u32,
    out_json: *mut *mut c_char,
) -> c_int {
    if handle.is_null() || collection.is_null() || query.is_null() || out_json.is_null() {
        return -1;
    }
    let col = match str_from_ptr(collection) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let q = match str_from_ptr(query) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let h = &*handle;

    match h.db.fts_search_sync(col, q, limit) {
        Ok(hits) => {
            let json = serde_json::to_string(&hits).unwrap_or_else(|_| "[]".into());
            *out_json = to_c_string(json);
            0
        }
        Err(_) => -1,
    }
}

// ==========================================
// SQL ANALYTICS ENGINE
// ==========================================

#[no_mangle]
pub unsafe extern "C" fn lilia_c_sql_query(
    handle: *mut LiliaHandle,
    sql: *const c_char,
    params_json: *const c_char,
    out_cols_json: *mut *mut c_char,
    out_rows_json: *mut *mut c_char,
    out_affected: *mut u64,
) -> c_int {
    if handle.is_null() || sql.is_null() || out_cols_json.is_null() || out_rows_json.is_null() {
        return -1;
    }
    let sql_str = match str_from_ptr(sql) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let params_opt = str_from_ptr(params_json).map(|s| s.to_string());
    let h = &*handle;

    match h.db.sql_query_sync(sql_str, params_opt) {
        Ok(res) => {
            let cols = serde_json::to_string(&res.columns).unwrap_or_else(|_| "[]".into());
            let rows = format!("[{}]", res.rows_json.join(","));
            *out_cols_json = to_c_string(cols);
            *out_rows_json = to_c_string(rows);
            if !out_affected.is_null() {
                *out_affected = res.rows_affected;
            }
            0
        }
        Err(_) => -1,
    }
}

// ==========================================
// TIME-SERIES & GEOFENCING C-ABI
// ==========================================

#[no_mangle]
pub unsafe extern "C" fn lilia_c_ts_append_json(
    handle: *mut LiliaHandle,
    series_id: *const c_char,
    points_json: *const c_char,
) -> c_int {
    if handle.is_null() || series_id.is_null() || points_json.is_null() {
        return -1;
    }
    let sid = match str_from_ptr(series_id) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let json_str = match str_from_ptr(points_json) {
        Some(s) => s,
        None => return -1,
    };
    let points: Vec<crate::FfiTimeSeriesPoint> = match serde_json::from_str(json_str) {
        Ok(p) => p,
        Err(_) => return -1,
    };

    let h = &*handle;
    match h.db.ts_append_points_sync(sid, points) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_ts_query_range_json(
    handle: *mut LiliaHandle,
    series_id: *const c_char,
    start_time: i64,
    end_time: i64,
    out_json: *mut *mut c_char,
) -> c_int {
    if handle.is_null() || series_id.is_null() || out_json.is_null() {
        return -1;
    }
    let sid = match str_from_ptr(series_id) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let h = &*handle;
    match h.db.ts_query_range_sync(sid, start_time, end_time) {
        Ok(pts) => {
            let json = serde_json::to_string(&pts).unwrap_or_else(|_| "[]".into());
            *out_json = to_c_string(json);
            0
        }
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_ts_query_stats_json(
    handle: *mut LiliaHandle,
    series_id: *const c_char,
    start_time: i64,
    end_time: i64,
    out_json: *mut *mut c_char,
) -> c_int {
    if handle.is_null() || series_id.is_null() || out_json.is_null() {
        return -1;
    }
    let sid = match str_from_ptr(series_id) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let h = &*handle;
    match h.db.ts_query_stats_sync(sid, start_time, end_time) {
        Ok(stats_opt) => {
            let json = serde_json::to_string(&stats_opt).unwrap_or_else(|_| "null".into());
            *out_json = to_c_string(json);
            0
        }
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_ts_query_auto_json(
    handle: *mut LiliaHandle,
    series_id: *const c_char,
    start_time: i64,
    end_time: i64,
    max_points: u32,
    out_json: *mut *mut c_char,
) -> c_int {
    if handle.is_null() || series_id.is_null() || out_json.is_null() {
        return -1;
    }
    let sid = match str_from_ptr(series_id) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let h = &*handle;
    match h
        .db
        .ts_query_auto_sync(sid, start_time, end_time, max_points)
    {
        Ok(pts) => {
            let json = serde_json::to_string(&pts).unwrap_or_else(|_| "[]".into());
            *out_json = to_c_string(json);
            0
        }
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_ts_set_policy_json(
    handle: *mut LiliaHandle,
    policy_json: *const c_char,
) -> c_int {
    if handle.is_null() || policy_json.is_null() {
        return -1;
    }
    let json_str = match str_from_ptr(policy_json) {
        Some(s) => s,
        None => return -1,
    };
    let policy: crate::FfiTimeSeriesLifecyclePolicy = match serde_json::from_str(json_str) {
        Ok(p) => p,
        Err(_) => return -1,
    };
    let h = &*handle;
    match h.db.ts_set_policy_sync(policy) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_ts_get_policy_json(
    handle: *mut LiliaHandle,
    series_pattern: *const c_char,
    out_json: *mut *mut c_char,
) -> c_int {
    if handle.is_null() || series_pattern.is_null() || out_json.is_null() {
        return -1;
    }
    let pattern = match str_from_ptr(series_pattern) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let h = &*handle;
    match h.db.ts_get_policy_sync(pattern) {
        Ok(policy_opt) => {
            let json = serde_json::to_string(&policy_opt).unwrap_or_else(|_| "null".into());
            *out_json = to_c_string(json);
            0
        }
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_ts_list_policies_json(
    handle: *mut LiliaHandle,
    out_json: *mut *mut c_char,
) -> c_int {
    if handle.is_null() || out_json.is_null() {
        return -1;
    }
    let h = &*handle;
    match h.db.ts_list_policies_sync() {
        Ok(policies) => {
            let json = serde_json::to_string(&policies).unwrap_or_else(|_| "[]".into());
            *out_json = to_c_string(json);
            0
        }
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_ts_delete_policy(
    handle: *mut LiliaHandle,
    series_pattern: *const c_char,
) -> c_int {
    if handle.is_null() || series_pattern.is_null() {
        return -1;
    }
    let pattern = match str_from_ptr(series_pattern) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let h = &*handle;
    match h.db.ts_delete_policy_sync(pattern) {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_ts_apply_retention_json(
    handle: *mut LiliaHandle,
    out_json: *mut *mut c_char,
) -> c_int {
    if handle.is_null() || out_json.is_null() {
        return -1;
    }
    let h = &*handle;
    match h.db.ts_apply_retention_sync() {
        Ok(stats) => {
            let json = serde_json::to_string(&stats).unwrap_or_else(|_| "null".into());
            *out_json = to_c_string(json);
            0
        }
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_geo_add_fence(
    handle: *mut LiliaHandle,
    id: *const c_char,
    name: *const c_char,
    category: *const c_char,
    polygon_json: *const c_char,
    metadata: *const c_char,
) -> c_int {
    if handle.is_null()
        || id.is_null()
        || name.is_null()
        || category.is_null()
        || polygon_json.is_null()
    {
        return -1;
    }
    let fid = match str_from_ptr(id) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let fname = match str_from_ptr(name) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let fcat = match str_from_ptr(category) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let fpoly = match str_from_ptr(polygon_json) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let fmeta = str_from_ptr(metadata).map(|s| s.to_string());

    let h = &*handle;
    match h.db.geo_add_fence_sync(fid, fname, fcat, fpoly, fmeta) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_geo_contains_point(
    handle: *mut LiliaHandle,
    polygon_json: *const c_char,
    lon: f64,
    lat: f64,
    out_inside: *mut c_int,
) -> c_int {
    if handle.is_null() || polygon_json.is_null() || out_inside.is_null() {
        return -1;
    }
    let fpoly = match str_from_ptr(polygon_json) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let h = &*handle;
    match h.db.geo_contains_point_sync(fpoly, lon, lat) {
        Ok(inside) => {
            *out_inside = if inside { 1 } else { 0 };
            0
        }
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_geo_find_fences_for_point(
    handle: *mut LiliaHandle,
    lon: f64,
    lat: f64,
    out_matches_json: *mut *mut c_char,
) -> c_int {
    if handle.is_null() || out_matches_json.is_null() {
        return -1;
    }
    let h = &*handle;
    match h.db.geo_find_fences_for_point_sync(lon, lat) {
        Ok(matches) => {
            let json = serde_json::to_string(&matches).unwrap_or_else(|_| "[]".into());
            *out_matches_json = to_c_string(json);
            0
        }
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_geo_track_distance(
    handle: *mut LiliaHandle,
    track_id: *const c_char,
    out_distance: *mut f64,
) -> c_int {
    if handle.is_null() || track_id.is_null() || out_distance.is_null() {
        return -1;
    }
    let tid = match str_from_ptr(track_id) {
        Some(s) => s.to_string(),
        None => return -1,
    };
    let h = &*handle;
    match h.db.geo_track_distance_sync(tid) {
        Ok(dist) => {
            *out_distance = dist;
            0
        }
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lilia_c_geo_create_circle(
    center_lon: f64,
    center_lat: f64,
    radius_meters: f64,
    n_vertices: u32,
    out_json: *mut *mut c_char,
) -> c_int {
    if out_json.is_null() {
        return -1;
    }
    let circle =
        crate::geo_create_circle_polygon(center_lon, center_lat, radius_meters, n_vertices);
    *out_json = to_c_string(circle);
    0
}
