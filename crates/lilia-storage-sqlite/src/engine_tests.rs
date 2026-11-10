use super::*;
use std::time::Duration;

fn put(value: u8) -> BatchOperation {
    BatchOperation::KvSet {
        namespace: "queue".into(),
        key: b"key".to_vec(),
        value: vec![value],
        if_version: None,
        expires_at_ms: None,
    }
}

#[test]
fn readers_continue_and_expired_queued_batch_never_mutates() {
    let directory = tempfile::tempdir().unwrap();
    let database =
        Database::open(DatabaseOptions::durable(directory.path().join("db.lilia"))).unwrap();
    database.batch(&[put(1)]).unwrap();
    let mut writer = database.lock_writer().unwrap();
    let mut transaction = writer.transaction().unwrap();
    apply(&mut transaction, &put(2), &lilia_core::IdentityCodec).unwrap();
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| database.kv_get("queue", b"key").unwrap().unwrap());
        assert_eq!(reader.join().unwrap().value, vec![1]);
        let queued = scope.spawn(|| {
            database
                .batch_with_deadline(&[put(3)], Some(Instant::now() + Duration::from_millis(50)))
        });
        let error = queued.join().unwrap().unwrap_err();
        assert_eq!(error.code, ErrorCode::Timeout);
        assert!(error.retryable);
    });
    transaction.commit().unwrap();
    drop(writer);
    assert_eq!(
        database.kv_get("queue", b"key").unwrap().unwrap().value,
        vec![2]
    );
    database.batch(&[put(4)]).unwrap();
    assert!(database.integrity_check().unwrap());
}

#[test]
fn concurrent_batches_are_serialized_and_errors_release_admission() {
    let directory = tempfile::tempdir().unwrap();
    let database =
        Database::open(DatabaseOptions::durable(directory.path().join("db.lilia"))).unwrap();
    let barrier = std::sync::Barrier::new(8);
    std::thread::scope(|scope| {
        for value in 0..8 {
            let database = &database;
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                for _ in 0..20 {
                    database.batch(&[put(value)]).unwrap();
                }
            });
        }
    });
    assert_eq!(
        database.kv_get("queue", b"key").unwrap().unwrap().version,
        160
    );
    let invalid = BatchOperation::KvDelete {
        namespace: "queue".into(),
        key: b"key".to_vec(),
        if_version: Some(999),
    };
    assert_eq!(
        database.batch(&[put(9), invalid]).unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(
        database.kv_get("queue", b"key").unwrap().unwrap().version,
        160
    );
    database.batch(&[put(10)]).unwrap();
    assert_eq!(
        database.kv_get("queue", b"key").unwrap().unwrap().version,
        161
    );
}

#[test]
fn close_releases_connections_is_idempotent_and_preserves_commits() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("db.lilia");
    let database = Database::open(DatabaseOptions::durable(&path)).unwrap();
    database.batch(&[put(1)]).unwrap();
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| database.close(Duration::from_secs(5)).unwrap());
        }
    });
    database.close(Duration::ZERO).unwrap();
    assert!(database.resources.writer.lock().unwrap().is_none());
    assert!(database.resources.checkpointer.lock().unwrap().is_none());
    assert!(database
        .resources
        .readers
        .iter()
        .all(|reader| reader.lock().unwrap().is_none()));
    assert_eq!(
        database.batch(&[put(2)]).unwrap_err().code,
        ErrorCode::Closed
    );
    assert_eq!(
        database.kv_get("queue", b"key").unwrap_err().code,
        ErrorCode::Closed
    );
    let backup = directory.path().join("closed-backup.lilia");
    assert_eq!(
        database.backup(&backup).unwrap_err().code,
        ErrorCode::Closed
    );
    assert!(!backup.exists());
    let reopened = Database::open(DatabaseOptions::durable(&path)).unwrap();
    assert_eq!(
        reopened.kv_get("queue", b"key").unwrap().unwrap().value,
        vec![1]
    );
    assert!(reopened.integrity_check().unwrap());
    reopened.close(Duration::from_secs(5)).unwrap();
    directory.close().unwrap();
}

#[test]
fn close_timeout_keeps_accepted_writer_queued_and_closes_after_commit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("db.lilia");
    let database = Database::open(DatabaseOptions::durable(&path)).unwrap();
    let held = database.lock_writer().unwrap();
    std::thread::scope(|scope| {
        let queued = scope.spawn(|| database.batch(&[put(7)]).unwrap());
        database.resources.writer_queue.wait_for_waiters(1);
        let error = database.close(Duration::from_millis(10)).unwrap_err();
        assert_eq!(error.code, ErrorCode::Timeout);
        assert!(error.retryable);
        assert_eq!(
            database.batch(&[put(8)]).unwrap_err().code,
            ErrorCode::Closed
        );
        drop(held);
        queued.join().unwrap();
        database.close(Duration::from_secs(5)).unwrap();
    });
    let reopened = Database::open(DatabaseOptions::durable(&path)).unwrap();
    assert_eq!(
        reopened.kv_get("queue", b"key").unwrap().unwrap().value,
        vec![7]
    );
}

#[test]
fn close_drains_readers_and_tolerates_external_wal_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("db.lilia");
    let database = Database::open(DatabaseOptions::durable(&path)).unwrap();
    database.batch(&[put(1)]).unwrap();
    let reader = database.read_lock().unwrap();
    assert_eq!(
        database.close(Duration::from_millis(10)).unwrap_err().code,
        ErrorCode::Timeout
    );
    assert!(reader
        .pragma_query_int("SELECT count(*) FROM _lilia_kv")
        .is_ok());
    let external = NativeConnection::open(&path, "NORMAL", 0, 50).unwrap();
    external.exec("BEGIN; SELECT * FROM _lilia_kv;").unwrap();
    drop(reader);
    database.close(Duration::from_secs(5)).unwrap();
    external.exec("ROLLBACK;").unwrap();
    let reopened = Database::open(DatabaseOptions::durable(&path)).unwrap();
    assert!(reopened.integrity_check().unwrap());
}

#[test]
fn in_memory_database_executes_batches_and_queries() {
    let database = Database::open(DatabaseOptions::in_memory()).unwrap();
    database.batch(&[put(42)]).unwrap();
    let entry = database.kv_get("queue", b"key").unwrap().unwrap();
    assert_eq!(entry.value, vec![42]);
    assert!(database.quick_check().unwrap());
    assert!(database.integrity_check().unwrap());
    database.close(Duration::from_secs(1)).unwrap();
}

#[test]
fn defensive_mode_rejects_direct_schema_writes() {
    let database = Database::open(DatabaseOptions::in_memory()).unwrap();
    let writer = database.lock_writer().unwrap();
    let err = writer
        .exec("UPDATE sqlite_schema SET sql = 'corrupt' WHERE name = '_lilia_kv'")
        .unwrap_err();
    assert!(
        err.to_string().contains("may not be modified")
            || err.to_string().contains("not authorized")
    );
}

#[test]
fn deadline_watchdog_interrupts_active_batch_and_rolls_back() {
    let database = Database::open(DatabaseOptions::in_memory()).unwrap();
    database.batch(&[put(1)]).unwrap();

    let ops: Vec<BatchOperation> = (0..1000)
        .map(|i| BatchOperation::KvSet {
            namespace: "slow".into(),
            key: format!("k{i}").into_bytes(),
            value: vec![0u8; 10_000],
            if_version: None,
            expires_at_ms: None,
        })
        .collect();

    let err = database
        .batch_with_deadline(&ops, Some(Instant::now() + Duration::from_nanos(1)))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Timeout);
    assert!(err.retryable);

    assert_eq!(
        database.kv_get("queue", b"key").unwrap().unwrap().value,
        vec![1]
    );
    assert!(database.quick_check().unwrap());
}

#[test]
fn test_resource_profiles_mobile_and_constrained() {
    let directory = tempfile::tempdir().unwrap();

    // Test Mobile profile
    let mobile_path = directory.path().join("mobile.lilia");
    let mobile_opts = DatabaseOptions::mobile(&mobile_path);
    assert_eq!(mobile_opts.profile, ResourceProfile::Mobile);
    assert_eq!(mobile_opts.cache_size_kib, -8_192);
    assert_eq!(mobile_opts.mmap_size_bytes, 32 * 1024 * 1024);
    assert_eq!(mobile_opts.read_pool_size, 2);

    let mobile_db = Database::open(mobile_opts).unwrap();
    mobile_db
        .batch(&[BatchOperation::KvSet {
            namespace: "ns".into(),
            key: b"k1".to_vec(),
            value: b"v1".to_vec(),
            if_version: None,
            expires_at_ms: None,
        }])
        .unwrap();
    let val = mobile_db.kv_get("ns", b"k1").unwrap().unwrap();
    assert_eq!(val.value, b"v1");
    assert!(mobile_db.quick_check().unwrap());

    // Test Constrained profile (Watch / Wear OS / tvOS)
    let constr_path = directory.path().join("constrained.lilia");
    let constr_opts = DatabaseOptions::constrained(&constr_path);
    assert_eq!(constr_opts.profile, ResourceProfile::Constrained);
    assert_eq!(constr_opts.cache_size_kib, -2_048);
    assert_eq!(constr_opts.mmap_size_bytes, 0);
    assert_eq!(constr_opts.read_pool_size, 1);

    let constr_db = Database::open(constr_opts).unwrap();
    constr_db
        .batch(&[BatchOperation::KvSet {
            namespace: "ns".into(),
            key: b"k2".to_vec(),
            value: b"v2".to_vec(),
            if_version: None,
            expires_at_ms: None,
        }])
        .unwrap();
    let val2 = constr_db.kv_get("ns", b"k2").unwrap().unwrap();
    assert_eq!(val2.value, b"v2");
    assert!(constr_db.quick_check().unwrap());

    // Test with_profile builder
    let custom = DatabaseOptions::durable(&mobile_path).with_profile(ResourceProfile::Constrained);
    assert_eq!(custom.profile, ResourceProfile::Constrained);
    assert_eq!(custom.cache_size_kib, -2_048);
    assert_eq!(custom.mmap_size_bytes, 0);
}

#[test]
fn test_storage_stats_and_sqlite_native_extensions() {
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(DatabaseOptions::durable(
        directory.path().join("ext_test.lilia"),
    ))
    .unwrap();

    // Insert KV and JSON data
    database
        .batch(&[
            BatchOperation::KvSet {
                namespace: "metrics".into(),
                key: b"temp".to_vec(),
                value: b"23.5".to_vec(),
                if_version: None,
                expires_at_ms: None,
            },
            BatchOperation::JsonPut {
                space: "devices".into(),
                id: "dev_1".into(),
                value: r#"{"type":"sensor","reading":42}"#.into(),
                if_version: None,
            },
        ])
        .unwrap();

    // Test storage_stats() via dbstat
    let stats = database.storage_stats().unwrap();
    assert!(!stats.is_empty(), "dbstat should return table stats");
    let kv_stat = stats.iter().find(|s| s.name == "_lilia_kv");
    assert!(kv_stat.is_some(), "_lilia_kv should be tracked in dbstat");
    let kv = kv_stat.unwrap();
    assert!(kv.page_count >= 1);

    // Test native extensions via sql_query: generate_series, decimal, geopoly
    let res_series = database
        .sql_query("SELECT value FROM generate_series(1, 3);", &[])
        .unwrap();
    assert_eq!(res_series.rows.len(), 3);

    let res_decimal = database
        .sql_query("SELECT decimal_add('99.9', '0.1') AS sum_dec;", &[])
        .unwrap();
    assert_eq!(
        &res_decimal.rows[0][0],
        &serde_json::Value::String("100.0".into())
    );

    let res_geopoly = database
        .sql_query(
            "SELECT geopoly_area('[[0,0],[10,0],[10,10],[0,10],[0,0]]') AS area;",
            &[],
        )
        .unwrap();
    let area_val = res_geopoly.rows[0][0].as_f64().unwrap();
    assert!((area_val - 100.0).abs() < 1e-4);
}

#[test]
fn test_timeseries_chunking_and_compression_on_constrained_profile() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("watch_health.db");

    // Open database with Constrained resource profile (watchOS / Wear OS)
    let opts = DatabaseOptions::constrained(&db_path);
    let db = Database::open(opts).unwrap();

    // 1. Simulate 2,500 heart rate observations (1 Hz, ~41 minutes of running)
    let mut hr_points = Vec::with_capacity(2500);
    let start_ts = 1_700_000_000_000i64;
    let mut current_hr = 145.0;

    for i in 0..2500 {
        if i % 20 == 0 {
            current_hr += 0.5;
        } else if i % 35 == 0 {
            current_hr -= 0.3;
        }
        hr_points.push(lilia_core::TimeSeriesPoint::new(
            start_ts + (i * 1000),
            current_hr,
        ));
    }

    // Append to "running_hr" series (auto-chunked into chunks of 1000 pts)
    db.ts_append_points("running_hr", &hr_points).unwrap();

    // Verify chunk count in database
    let chunks_res = db
        .sql_query(
            "SELECT count(*), sum(point_count), sum(compressed_bytes) FROM lilia_timeseries_chunks WHERE series_id = 'running_hr';",
            &[],
        )
        .unwrap();
    assert_eq!(chunks_res.rows[0][0], serde_json::json!(3)); // 1000 + 1000 + 500 = 3 chunks
    assert_eq!(chunks_res.rows[0][1], serde_json::json!(2500));

    // Verify high compression ratio: 2500 points * 16 bytes = 40,000 bytes uncompressed
    let compressed_bytes = chunks_res.rows[0][2].as_i64().unwrap();
    assert!(
        compressed_bytes < 5000,
        "2500 points should compress into < 5KB (got {compressed_bytes}B)"
    );

    // 2. Query range with precise filtering
    let sub_range = db
        .ts_query_range("running_hr", start_ts + 500_000, start_ts + 600_000)
        .unwrap();
    assert_eq!(sub_range.len(), 101); // 100s + 1 inclusive
    assert_eq!(sub_range[0].timestamp, start_ts + 500_000);
    assert_eq!(sub_range.last().unwrap().timestamp, start_ts + 600_000);

    // 3. Fast O(chunks) stats aggregation
    let stats = db
        .ts_query_stats("running_hr", start_ts, start_ts + 2_500_000)
        .unwrap()
        .unwrap();
    assert_eq!(stats.count, 2500);
    assert!(stats.min >= 140.0 && stats.max <= 200.0);
    assert!(stats.avg > 140.0);
}

#[test]
fn test_gorilla_90_percent_disk_space_reduction() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("ts_compression_benchmark.db");

    let opts = DatabaseOptions::mobile(&db_path);
    let db = Database::open(opts).unwrap();

    // 1. Create a classic SQLite time-series table with B-tree index
    db.sql_query(
        "CREATE TABLE classic_sensor(
            series_id TEXT NOT NULL,
            timestamp INTEGER NOT NULL,
            value REAL NOT NULL,
            PRIMARY KEY(series_id, timestamp)
         );",
        &[],
    )
    .unwrap();

    // 2. Generate 10,000 points of typical 1 Hz IoT/health sensor telemetry (temperature/heart rate)
    let start_ts = 1_700_000_000_000i64;
    let mut points = Vec::with_capacity(10_000);
    let mut current_val = 72.0;

    for i in 0..10_000i64 {
        if i % 15 == 0 {
            current_val += 0.2;
        } else if i % 25 == 0 {
            current_val -= 0.1;
        }
        points.push(lilia_core::TimeSeriesPoint::new(
            start_ts + (i * 1000),
            current_val,
        ));
    }

    // Insert into classic SQLite table
    {
        let writer = db.lock_writer().unwrap();
        writer.exec("BEGIN IMMEDIATE;").unwrap();
        let mut stmt = writer
            .prepare("INSERT INTO classic_sensor(series_id, timestamp, value) VALUES (?1, ?2, ?3);")
            .unwrap();
        for p in &points {
            stmt.reset().unwrap();
            stmt.bind_text_static(1, "sensor_temp").unwrap();
            stmt.bind_int64(2, p.timestamp).unwrap();
            stmt.bind_double(3, p.value).unwrap();
            stmt.step().unwrap();
        }
        stmt.reset().unwrap();
        writer.exec("COMMIT;").unwrap();
    }

    // Insert same 10,000 points into Lilia Gorilla time-series chunks
    db.ts_append_points("sensor_temp", &points).unwrap();

    // Measure space using SQLite dbstat
    let classic_stats = db
        .sql_query(
            "SELECT count(*), sum(payload) FROM dbstat WHERE name = 'classic_sensor';",
            &[],
        )
        .unwrap();
    let classic_payload = classic_stats.rows[0][1].as_i64().unwrap();

    // Gorilla chunk storage compressed payload bytes
    let gorilla_stats = db
        .sql_query(
            "SELECT sum(length(data)) FROM _lilia_timeseries_chunks WHERE series_id = 'sensor_temp';",
            &[],
        )
        .unwrap();
    let gorilla_payload = gorilla_stats.rows[0][0].as_i64().unwrap();

    // Calculate reduction ratio
    let space_reduction_pct = (1.0 - (gorilla_payload as f64 / classic_payload as f64)) * 100.0;

    println!(
        "Benchmark: Classic SQLite payload: {} bytes, Gorilla compressed payload: {} bytes (Reduction: {:.2}%)",
        classic_payload, gorilla_payload, space_reduction_pct
    );

    // Verify > 90% space reduction
    assert!(
        space_reduction_pct >= 90.0,
        "Gorilla compression must achieve at least 90% disk space reduction (got {space_reduction_pct:.2}%)"
    );
}

#[test]
fn test_timeseries_declarative_lifecycle_and_downsampling() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("ts_lifecycle.db");

    let opts = DatabaseOptions::mobile(&db_path);
    let db = Database::open(opts).unwrap();

    // Declare 3-tier lifecycle policy:
    // Tier 1: raw 1s resolution, retained for 24h
    // Tier 2: 1m (60,000 ms) resolution, retained for 30 days
    // Tier 3: 1h (3,600,000 ms) resolution, retained indefinitely
    let policy = lilia_core::TimeSeriesLifecyclePolicy::new(
        "patient:*",
        vec![
            lilia_core::TimeSeriesRetentionRule::new(
                1_000,
                Some(24 * 3600 * 1000), // 24 hours
                lilia_core::DownsampleAggregation::Avg,
            ),
            lilia_core::TimeSeriesRetentionRule::new(
                60_000,
                Some(30 * 24 * 3600 * 1000), // 30 days
                lilia_core::DownsampleAggregation::Avg,
            ),
            lilia_core::TimeSeriesRetentionRule::new(
                3_600_000,
                None, // indefinite
                lilia_core::DownsampleAggregation::Avg,
            ),
        ],
    );

    db.ts_set_policy(&policy).unwrap();

    // Verify policy persisted and retrieved
    let retrieved = db.ts_get_policy("patient:*").unwrap().unwrap();
    assert_eq!(retrieved.series_pattern, "patient:*");
    assert_eq!(retrieved.rules.len(), 3);

    // Insert 2 hours of 1 Hz raw points (7,200 points) starting at base_time
    let base_time = 1_700_000_000_000i64;
    let mut points = Vec::with_capacity(7200);
    for i in 0..7200i64 {
        points.push(lilia_core::TimeSeriesPoint::new(
            base_time + (i * 1000),
            60.0 + (i % 20) as f64,
        ));
    }
    db.ts_append_points("patient:hr", &points).unwrap();

    // Initially, raw points are present (7,200 points in resolution 0)
    let raw = db
        .ts_query_range("patient:hr", base_time, base_time + 7_200_000)
        .unwrap();
    assert_eq!(raw.len(), 7200);

    // 1. Fast forward to +27 hours:
    // 27h - 2h data duration = 25h > 24h retention, so all raw data should be downsampled to 1m and 1h, then raw pruned!
    let time_after_27h = base_time + (27 * 3600 * 1000);
    let stats_27h = db.ts_apply_retention_at(time_after_27h).unwrap();

    assert!(stats_27h.downsampled_points_created > 0);
    assert!(stats_27h.chunks_pruned > 0);

    // Raw chunks (res 0) must now be pruned
    let raw_after_prune = db
        .ts_query_range("patient:hr", base_time, base_time + 7_200_000)
        .unwrap();
    assert_eq!(
        raw_after_prune.len(),
        0,
        "raw points older than 24h must be pruned"
    );

    // Tier 1 (1-minute resolution = 60,000 ms) must have 120 points (2 hours * 60 min)
    let pts_1m = db
        .ts_query_range_with_resolution("patient:hr", 60_000, base_time, base_time + 7_200_000)
        .unwrap();
    assert_eq!(
        pts_1m.len(),
        120,
        "2 hours of data should produce exactly 120 1-minute buckets"
    );

    // Tier 2 (1-hour resolution = 3,600,000 ms) must have 2 points (2 hours)
    let pts_1h = db
        .ts_query_range_with_resolution("patient:hr", 3_600_000, base_time, base_time + 7_200_000)
        .unwrap();
    assert_eq!(
        pts_1h.len(),
        2,
        "2 hours of data should produce exactly 2 1-hour buckets"
    );

    // 2. Fast forward to +32 days:
    // 32 days > 30 days retention, so 1-minute points must be pruned, but 1-hour points remain!
    let time_after_32d = base_time + (32 * 24 * 3600 * 1000);
    let stats_32d = db.ts_apply_retention_at(time_after_32d).unwrap();
    assert!(stats_32d.chunks_pruned > 0);

    // 1-minute points are now pruned
    let pts_1m_after_30d = db
        .ts_query_range_with_resolution("patient:hr", 60_000, base_time, base_time + 7_200_000)
        .unwrap();
    assert_eq!(
        pts_1m_after_30d.len(),
        0,
        "1-minute points older than 30 days must be pruned"
    );

    // 1-hour points are retained indefinitely!
    let pts_1h_after_30d = db
        .ts_query_range_with_resolution("patient:hr", 3_600_000, base_time, base_time + 7_200_000)
        .unwrap();
    assert_eq!(
        pts_1h_after_30d.len(),
        2,
        "1-hour points must be retained permanently"
    );
}

#[test]
fn test_timeseries_adaptive_query_auto() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("ts_auto.db");
    let db = Database::open(DatabaseOptions::mobile(&db_path)).unwrap();

    let start_ts = 1_700_000_000_000i64;
    // Insert raw points (resolution 0)
    let mut raw_points = Vec::new();
    for i in 0..1200 {
        raw_points.push(lilia_core::TimeSeriesPoint::new(
            start_ts + (i * 1000),
            50.0,
        ));
    }
    db.ts_append_points("device_net", &raw_points).unwrap();

    // Insert 1-minute points (resolution 60_000)
    let downsampled_1m =
        lilia_core::downsample_points(&raw_points, 60_000, lilia_core::DownsampleAggregation::Avg);
    db.ts_append_chunk("device_net", 60_000, &downsampled_1m)
        .unwrap();

    // Query with max_points = 50 -> should automatically pick 1m resolution (20 points <= 50)
    let auto_small = db
        .ts_query_auto("device_net", start_ts, start_ts + 1_200_000, 50)
        .unwrap();
    assert_eq!(auto_small.len(), 20);

    // Query with max_points = 2000 -> should pick raw resolution (1200 points <= 2000)
    let auto_large = db
        .ts_query_auto("device_net", start_ts, start_ts + 1_200_000, 2000)
        .unwrap();
    assert_eq!(auto_large.len(), 1200);
}

#[test]
fn test_sql_downsample_function() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("ts_sql.db");
    let db = Database::open(DatabaseOptions::mobile(&db_path)).unwrap();

    let res = db.sql_query(
        "SELECT lilia_ts_downsample('[{\"timestamp\": 1000, \"value\": 10.0}, {\"timestamp\": 2000, \"value\": 20.0}]', 5000, 'avg');",
        &[],
    ).unwrap();
    assert_eq!(res.rows.len(), 1);
    let json_str = res.rows[0][0].as_str().unwrap();
    assert!(json_str.contains("\"value\":15.0") || json_str.contains("\"value\": 15.0"));
}

#[test]
fn test_geofencing_and_gps_tracks_on_watch() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("watch_gps.db");

    let opts = DatabaseOptions::constrained(&db_path);
    let db = Database::open(opts).unwrap();

    // 1. Add a polygonal geofence around Parc Monceau in Paris
    // Coordinates: [longitude, latitude]
    let parc_monceau = "[[2.3060, 48.8785], [2.3120, 48.8785], [2.3120, 48.8815], [2.3060, 48.8815], [2.3060, 48.8785]]";
    db.geo_add_fence(
        "fence_parc_monceau",
        "Parc Monceau Running Track",
        "running",
        parc_monceau,
        Some("{\"target_pace_min_km\": 4.8}"),
    )
    .unwrap();

    // Add a circular geofence around Eiffel Tower with 300m radius using helper
    let eiffel_circle = lilia_core::circle_to_geopoly_json(2.2945, 48.8584, 300.0, 16);
    db.geo_add_fence(
        "fence_eiffel",
        "Tour Eiffel Perimeter (300m)",
        "tourist",
        &eiffel_circle,
        None,
    )
    .unwrap();

    // 2. Test ad-hoc point containment
    // Point inside Parc Monceau: (lon=2.3080, lat=48.8800)
    assert!(db
        .geo_contains_point(parc_monceau, 2.3080, 48.8800)
        .unwrap());
    // Point outside Parc Monceau: (lon=2.3500, lat=48.8500)
    assert!(!db
        .geo_contains_point(parc_monceau, 2.3500, 48.8500)
        .unwrap());

    // 3. Test R*Tree spatial query for a runner inside Parc Monceau
    let matches = db.geo_find_fences_for_point(2.3080, 48.8800).unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].id, "fence_parc_monceau");
    assert_eq!(matches[0].category, "running");

    // 4. Record GPS points for a runner
    let track_id = "workout_run_1";
    let pts = vec![
        lilia_core::GpsPoint::with_details(
            1_700_000_000_000,
            48.8780,
            2.3050,
            Some(50.0),
            Some(3.2),
            Some(4.0),
        ), // outside
        lilia_core::GpsPoint::with_details(
            1_700_000_005_000,
            48.8790,
            2.3070,
            Some(51.0),
            Some(3.3),
            Some(3.5),
        ), // entered Parc Monceau!
    ];
    let fence_events = db.geo_record_gps_points(track_id, &pts).unwrap();
    assert_eq!(fence_events.len(), 1);
    assert_eq!(fence_events[0].id, "fence_parc_monceau");

    // 5. Track points retrieval & distance calculation
    let saved_pts = db.geo_get_track_points(track_id, None, None).unwrap();
    assert_eq!(saved_pts.len(), 2);

    let distance_m = db.geo_track_distance_meters(track_id).unwrap();
    // Distance between (48.8780, 2.3050) and (48.8790, 2.3070) is ~183m
    assert!(
        (distance_m - 183.0).abs() < 25.0,
        "track distance should be ~183m, got {distance_m:.1}m"
    );

    // 6. Test native SQL functions: lilia_geo_distance, lilia_geo_circle
    let sql_res = db
        .sql_query(
            "SELECT lilia_geo_distance(48.8584, 2.2945, 48.8738, 2.2950) AS dist;",
            &[],
        )
        .unwrap();
    let sql_dist = sql_res.rows[0][0].as_f64().unwrap();
    assert!((sql_dist - 1712.0).abs() < 50.0);

    let circle_res = db
        .sql_query("SELECT lilia_geo_circle(2.2945, 48.8584, 100.0, 8);", &[])
        .unwrap();
    let circle_str = circle_res.rows[0][0].as_str().unwrap();
    assert!(circle_str.starts_with('['));

    // 7. Remove fence
    assert!(db.geo_remove_fence("fence_parc_monceau").unwrap());
    assert!(!db.geo_remove_fence("non_existent").unwrap());
    let empty_matches = db.geo_find_fences_for_point(2.3080, 48.8800).unwrap();
    assert!(empty_matches.is_empty());
}
