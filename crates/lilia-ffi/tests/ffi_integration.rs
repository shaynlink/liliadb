use lilia_ffi::{FfiDirection, FfiResourceProfile, LiliaDatabase};

#[test]
fn test_ffi_sync_api_lifecycle_and_multimodel() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir
        .path()
        .join("test_sync.lilia")
        .to_string_lossy()
        .to_string();

    // 1. Open with Mobile profile and 32-byte encryption key
    let key = vec![0x42u8; 32];
    let db = LiliaDatabase::open(db_path, FfiResourceProfile::Mobile, Some(key)).unwrap();

    // 2. Mobile lifecycle: prepare for background
    assert!(db.prepare_for_background().is_ok());

    // 3. KV Sync operations
    let v1 = db
        .kv_set_sync(
            "users".into(),
            b"alice".to_vec(),
            b"Alice Data".to_vec(),
            None,
        )
        .unwrap();
    assert!(v1 > 0);

    let get_res = db
        .kv_get_sync("users".into(), b"alice".to_vec())
        .unwrap()
        .unwrap();
    assert_eq!(get_res.namespace, "users");
    assert_eq!(get_res.key, b"alice");
    assert_eq!(get_res.value, b"Alice Data");

    let scan_res = db.kv_scan_sync("users".into(), None, 10).unwrap();
    assert_eq!(scan_res.len(), 1);

    // 4. JSON Sync operations
    let j_ver = db
        .json_put_sync(
            "docs".into(),
            "doc1".into(),
            r#"{"title":"Hello Mobile","priority":1}"#.into(),
        )
        .unwrap();
    assert!(j_ver > 0);

    let j_entry = db
        .json_get_sync("docs".into(), "doc1".into())
        .unwrap()
        .unwrap();
    assert_eq!(j_entry.id, "doc1");
    assert!(j_entry.value_json.contains("Hello Mobile"));

    // 5. Graph Sync operations
    db.graph_set_node_sync(
        "social".into(),
        "u1".into(),
        "user".into(),
        r#"{"name":"User 1"}"#.into(),
    )
    .unwrap();
    db.graph_set_node_sync(
        "social".into(),
        "u2".into(),
        "user".into(),
        r#"{"name":"User 2"}"#.into(),
    )
    .unwrap();
    db.graph_add_edge_sync("social".into(), "u1".into(), "u2".into(), "follows".into())
        .unwrap();

    let neighbors = db
        .graph_neighbors_sync("social".into(), "u1".into(), FfiDirection::Outgoing, None)
        .unwrap();
    assert_eq!(neighbors, vec!["u2"]);

    let reachable = db
        .graph_reachability_sync("social".into(), vec!["u1".into()], FfiDirection::Outgoing)
        .unwrap();
    assert!(reachable.contains(&"u1".into()));
    assert!(reachable.contains(&"u2".into()));

    // 6. FTS5 Sync operations
    db.fts_index_sync(
        "articles".into(),
        "art1".into(),
        "Rust SQLite on Apple Watch and Android TV".into(),
    )
    .unwrap();
    let hits = db
        .fts_search_sync("articles".into(), "Apple Watch".into(), 5)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].doc_id, "art1");

    // 7. SQL Sync operations
    let sql_res = db
        .sql_query_sync(
            "SELECT space, id FROM lilia_json WHERE space = 'docs'".into(),
            None,
        )
        .unwrap();
    assert_eq!(sql_res.columns, vec!["space", "id"]);
    assert_eq!(sql_res.rows_json.len(), 1);

    // 8. Maintenance
    assert!(db.quick_check_sync().unwrap());
    assert!(db.checkpoint_sync().is_ok());
    assert!(db.close_sync(5000).is_ok());
}

#[tokio::test]
async fn test_ffi_async_api_and_constrained_profile() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir
        .path()
        .join("test_watch_async.lilia")
        .to_string_lossy()
        .to_string();

    // Open with Constrained profile (for watchOS / Wear OS) with passphrase
    let db = LiliaDatabase::open_with_passphrase(
        db_path,
        FfiResourceProfile::Constrained,
        "watch-secure-passphrase".into(),
    )
    .unwrap();

    // Async KV
    let ver = db
        .kv_set(
            "health".into(),
            b"heart_rate".to_vec(),
            b"72bpm".to_vec(),
            None,
        )
        .await
        .unwrap();
    assert!(ver > 0);

    let entry = db
        .kv_get("health".into(), b"heart_rate".to_vec())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(entry.value, b"72bpm");

    // Async JSON
    db.json_put(
        "metrics".into(),
        "today".into(),
        r#"{"steps":10500,"calories":450}"#.into(),
    )
    .await
    .unwrap();
    let json_doc = db
        .json_get("metrics".into(), "today".into())
        .await
        .unwrap()
        .unwrap();
    assert!(json_doc.value_json.contains("10500"));

    // Async SQL
    let res = db
        .sql_query("SELECT id, value FROM lilia_json".into(), None)
        .await
        .unwrap();
    assert_eq!(res.rows_json.len(), 1);

    // Async Time-Series (Gorilla compressed health data)
    let hr_points = vec![
        lilia_ffi::FfiTimeSeriesPoint {
            timestamp: 1_700_000_000_000,
            value: 72.0,
        },
        lilia_ffi::FfiTimeSeriesPoint {
            timestamp: 1_700_000_001_000,
            value: 72.5,
        },
        lilia_ffi::FfiTimeSeriesPoint {
            timestamp: 1_700_000_002_000,
            value: 73.0,
        },
    ];
    db.ts_append_points("watch_hr".into(), hr_points)
        .await
        .unwrap();

    let queried_ts = db
        .ts_query_range("watch_hr".into(), 1_700_000_000_000, 1_700_000_002_000)
        .await
        .unwrap();
    assert_eq!(queried_ts.len(), 3);

    let ts_stats = db
        .ts_query_stats("watch_hr".into(), 1_700_000_000_000, 1_700_000_002_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ts_stats.count, 3);
    assert_eq!(ts_stats.min, 72.0);
    assert_eq!(ts_stats.max, 73.0);

    // Standalone compression functions
    let comp_bytes = lilia_ffi::ts_compress_points(queried_ts).unwrap();
    assert!(!comp_bytes.is_empty());
    let decomp_pts = lilia_ffi::ts_decompress_points(comp_bytes).unwrap();
    assert_eq!(decomp_pts.len(), 3);

    // FFI Lifecycle Policy & Adaptive Query
    let ffi_policy = lilia_ffi::FfiTimeSeriesLifecyclePolicy {
        series_pattern: "watch_*".into(),
        rules: vec![
            lilia_ffi::FfiTimeSeriesRetentionRule {
                resolution_ms: 1_000,
                retention_ms: Some(24 * 3600 * 1000),
                aggregation: lilia_ffi::FfiDownsampleAggregation::Avg,
            },
            lilia_ffi::FfiTimeSeriesRetentionRule {
                resolution_ms: 60_000,
                retention_ms: None,
                aggregation: lilia_ffi::FfiDownsampleAggregation::Avg,
            },
        ],
    };
    db.ts_set_policy(ffi_policy).await.unwrap();
    let read_policy = db.ts_get_policy("watch_*".into()).await.unwrap().unwrap();
    assert_eq!(read_policy.rules.len(), 2);

    let auto_pts = db
        .ts_query_auto("watch_hr".into(), 1_700_000_000_000, 1_700_000_002_000, 100)
        .await
        .unwrap();
    assert_eq!(auto_pts.len(), 3);

    let ret_stats = db.ts_apply_retention().await.unwrap();
    assert_eq!(ret_stats.series_processed, 1);

    // Async Geofencing & GPS Tracing
    let park_poly = lilia_ffi::geo_create_circle_polygon(2.2945, 48.8584, 250.0, 16);
    db.geo_add_fence(
        "workout_zone".into(),
        "Park Perimeter".into(),
        "running".into(),
        park_poly.clone(),
        None,
    )
    .await
    .unwrap();

    let inside = db
        .geo_contains_point(park_poly, 2.2945, 48.8584)
        .await
        .unwrap();
    assert!(inside);

    let matches = db.geo_find_fences_for_point(2.2945, 48.8584).await.unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].id, "workout_zone");

    let gps_track = vec![
        lilia_ffi::FfiGpsPoint {
            timestamp: 1_700_000_000_000,
            latitude: 48.8580,
            longitude: 2.2940,
            altitude: Some(35.0),
            speed: Some(3.0),
            accuracy: Some(5.0),
        },
        lilia_ffi::FfiGpsPoint {
            timestamp: 1_700_000_005_000,
            latitude: 48.8584,
            longitude: 2.2945,
            altitude: Some(35.5),
            speed: Some(3.1),
            accuracy: Some(4.0),
        },
    ];
    let hits = db
        .geo_record_gps_points("run_1".into(), gps_track)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);

    let dist = db.geo_track_distance("run_1".into()).await.unwrap();
    assert!(dist > 10.0 && dist < 100.0);

    // Async Quick Check & Close
    assert!(db.quick_check().await.unwrap());
    assert!(db.close(5000).await.is_ok());
}
