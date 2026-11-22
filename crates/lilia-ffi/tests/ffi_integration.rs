//! The `UniFFI` API and the C ABI, as the native SDKs call them.

#![allow(unsafe_code)]

use std::ffi::{c_char, CStr, CString};

use lilia_ffi::c_abi::{
    lilia_c_close, lilia_c_execute, lilia_c_free_bytes, lilia_c_free_string, lilia_c_open,
};
use lilia_ffi::{
    FfiDirection, FfiDurability, FfiOpenOptions, FfiResourceProfile, FfiTimeSeriesPoint,
    LiliaDatabase, LiliaFfiError,
};
use serde_json::{json, Value};

fn options() -> FfiOpenOptions {
    FfiOpenOptions {
        profile: FfiResourceProfile::Constrained,
        durability: FfiDurability::Performance,
        ledger: false,
        history_retention_ms: None,
        undo_depth: 0,
    }
}

#[test]
fn the_sync_api_covers_the_models_and_closes_once() {
    let dir = tempfile::tempdir().unwrap();
    let db =
        LiliaDatabase::open(dir.path().join("db").to_string_lossy().into(), options()).unwrap();

    let version = db
        .kv_set_sync("users".into(), b"alice".to_vec(), b"Alice".to_vec(), None)
        .unwrap();
    let entry = db
        .kv_get_sync("users".into(), b"alice".to_vec())
        .unwrap()
        .unwrap();
    assert_eq!(
        (entry.value.as_slice(), entry.version),
        (&b"Alice"[..], version)
    );
    assert_eq!(db.kv_scan_sync("users".into(), None, 0).unwrap().len(), 1);
    assert!(db
        .kv_delete_sync("users".into(), b"alice".to_vec())
        .unwrap());

    db.json_put_sync(
        "docs".into(),
        "d1".into(),
        r#"{"title":"Hello","rank":1}"#.into(),
    )
    .unwrap();
    let document = db
        .json_get_sync("docs".into(), "d1".into())
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&document.value_json).unwrap(),
        json!({"title": "Hello", "rank": 1})
    );

    db.json_create_index_sync("docs".into(), "by_rank".into(), "$.rank".into(), None)
        .unwrap();
    let ranked = db
        .json_find_sync("docs".into(), r#"{"by_rank": 1}"#.into(), None, 0)
        .unwrap();
    assert_eq!(ranked.len(), 1);
    assert_eq!(
        db.json_list_indexes_sync("docs".into()).unwrap()[0].index_name,
        "by_rank"
    );
    assert!(db
        .json_drop_index_sync("docs".into(), "by_rank".into())
        .unwrap());

    db.graph_set_node_sync("g".into(), "a".into(), "user".into(), "{}".into())
        .unwrap();
    db.graph_add_edge_sync("g".into(), "a".into(), "b".into(), "follows".into())
        .unwrap();
    assert_eq!(
        db.graph_neighbors_sync("g".into(), "a".into(), FfiDirection::Outgoing, None)
            .unwrap(),
        ["b"]
    );

    db.fts_index_sync("notes".into(), "n1".into(), "local first storage".into())
        .unwrap();
    let hits = db
        .fts_search_sync("notes".into(), "storage".into(), 5)
        .unwrap();
    assert_eq!(hits[0].doc_id, "n1");

    let rows = db
        .sql_query_sync(
            "SELECT id FROM lilia_json WHERE space = $1".into(),
            Some(r#"["docs"]"#.into()),
        )
        .unwrap();
    assert_eq!(rows.rows_json, [r#"["d1"]"#]);

    let points = (0..5_i32)
        .map(|i| FfiTimeSeriesPoint {
            timestamp: i64::from(i) * 1_000,
            value: f64::from(i),
        })
        .collect();
    db.ts_append_points_sync("cpu".into(), points).unwrap();
    let stats = db
        .ts_query_stats_sync("cpu".into(), 0, 10_000)
        .unwrap()
        .unwrap();
    assert_eq!(stats.count, 5);

    assert!(db.integrity_check_sync().unwrap());
    db.close_sync(5_000).unwrap();
    assert!(matches!(
        db.kv_get_sync("users".into(), b"alice".to_vec()),
        Err(LiliaFfiError::Closed(_))
    ));
    db.close_sync(5_000).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_async_api_runs_off_the_caller() {
    let dir = tempfile::tempdir().unwrap();
    let db =
        LiliaDatabase::open(dir.path().join("db").to_string_lossy().into(), options()).unwrap();
    db.json_put("docs".into(), "d1".into(), r#"{"n":1}"#.into())
        .await
        .unwrap();
    db.json_put("docs".into(), "d2".into(), r#"{"n":2}"#.into())
        .await
        .unwrap();
    let document = db
        .json_get("docs".into(), "d2".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(document.value_json, r#"{"n":2}"#);
    assert!(db.json_delete("docs".into(), "d1".into()).await.unwrap());
    db.close(5_000).await.unwrap();
}

/// The JSON of a JS frame and its segments.
fn frame(bytes: &[u8]) -> (Value, Vec<Vec<u8>>) {
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let json = u32_at(0);
    let value = serde_json::from_slice(&bytes[4..4 + json]).unwrap();
    let count = u32_at(4 + json);
    let start = 8 + json + count * 8;
    let segments = (0..count)
        .map(|i| {
            let (offset, len) = (u32_at(8 + json + i * 8), u32_at(12 + json + i * 8));
            bytes[start + offset..start + offset + len].to_vec()
        })
        .collect();
    (value, segments)
}

/// Runs `request` through the C ABI.
fn execute(
    handle: *const lilia_ffi::c_abi::LiliaHandle,
    request: &[u8],
    encoding: i32,
) -> (Value, Vec<Vec<u8>>) {
    let mut len = 0usize;
    let bytes = unsafe {
        lilia_c_execute(
            handle,
            request.as_ptr(),
            request.len(),
            encoding,
            &raw mut len,
        )
    };
    assert!(!bytes.is_null());
    let result = frame(unsafe { std::slice::from_raw_parts(bytes, len) });
    unsafe { lilia_c_free_bytes(bytes, len) };
    result
}

#[test]
fn the_c_abi_takes_json_or_message_pack_and_answers_with_frames() {
    let dir = tempfile::tempdir().unwrap();
    let options = json!({"path": dir.path().join("db"), "durability": "performance"}).to_string();
    let options = CString::new(options).unwrap();
    let mut error: *mut c_char = std::ptr::null_mut();
    let handle = unsafe { lilia_c_open(options.as_ptr(), &raw mut error) };
    assert!(!handle.is_null() && error.is_null());

    let put = json!({"type": "batch", "operations": [{"model": "kv_set", "namespace": "ns", "key": [1, 2], "value": [3, 4, 5]}]});
    let (written, _) = execute(handle, put.to_string().as_bytes(), 1);
    assert_eq!(written["Ok"]["type"], "mutations");

    let get = lilia_protocol::to_msgpack(&lilia_protocol::Operation::KvGet {
        namespace: "ns".into(),
        key: vec![1, 2],
    })
    .unwrap();
    let (read, segments) = execute(handle, &get, 0);
    let value = usize::try_from(read["Ok"]["value"]["value"].as_u64().unwrap()).unwrap();
    assert_eq!(segments[value], [3, 4, 5]);

    let (refused, _) = execute(handle, b"{not json", 1);
    assert_eq!(refused["Err"]["code"], "INVALID_INPUT");
    let (unknown, _) = execute(handle, b"{}", 7);
    assert_eq!(unknown["Err"]["code"], "INVALID_INPUT");

    assert_eq!(unsafe { lilia_c_close(handle, 5_000, &raw mut error) }, 0);

    let bad = CString::new("{\"durability\": \"fast\"}").unwrap();
    let failed = unsafe { lilia_c_open(bad.as_ptr(), &raw mut error) };
    assert!(failed.is_null() && !error.is_null());
    let message = unsafe { CStr::from_ptr(error) }
        .to_string_lossy()
        .into_owned();
    assert!(message.contains("INVALID_INPUT"), "{message}");
    unsafe { lilia_c_free_string(error) };
}
