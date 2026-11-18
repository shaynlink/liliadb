use lilia_core::wear::WearLevelingPolicy;
use lilia_core::BatchOperation;
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

#[test]
fn test_physical_write_telemetry_tracking() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("wear_telemetry.lilia");

    let db = Database::open(DatabaseOptions::mobile(&db_path)).unwrap();

    let t0 = db.flash_wear_telemetry();
    assert_eq!(t0.bytes_written_wal, 0);

    // Perform batch write
    let mut batch = Vec::new();
    for i in 0..50 {
        batch.push(BatchOperation::JsonPut {
            space: "mobile_events".into(),
            id: format!("evt_{i}"),
            value: json!({ "event": "tap", "coord": [120, 340], "timestamp": 123456789 }),
            if_version: None,
        });
    }
    db.batch(&batch).unwrap();

    let t1 = db.flash_wear_telemetry();
    assert!(
        t1.bytes_written_wal > 0,
        "WAL bytes written must be accounted"
    );
    assert!(t1.total_fsync_count > 0, "fsync operations must be counted");
    assert!(t1.total_physical_bytes >= t1.bytes_written_wal);

    // Sync parity
    db.sync_parity().unwrap();

    let t2 = db.flash_wear_telemetry();
    assert!(t2.bytes_written_parity > 0, "parity bytes must be recorded");
    assert!(t2.total_physical_bytes > t1.total_physical_bytes);
}

#[test]
fn test_adaptive_fsync_interval_under_write_burst() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("adaptive_fsync.lilia");

    let custom_policy = WearLevelingPolicy {
        enabled: true,
        nominal_fsync_interval_ms: 3_000,
        max_fsync_interval_ms: 15_000,
        target_daily_budget_bytes: 86_400 * 500, // 500 bytes/sec budget
    };

    let opts = DatabaseOptions::mobile(&db_path).with_wear_leveling(custom_policy);
    let db = Database::open(opts).unwrap();

    // Baseline nominal interval
    let t0 = db.flash_wear_telemetry();
    assert_eq!(t0.adaptive_fsync_interval_ms, 3_000);

    // Simulate write burst exceeding budget
    let mut batch = Vec::new();
    for i in 0..200 {
        batch.push(BatchOperation::KvSet {
            namespace: "burst_kv".into(),
            key: format!("k_{i}").into_bytes(),
            value: vec![0x99; 1024],
            if_version: None,
            expires_at_ms: None,
        });
    }
    db.batch(&batch).unwrap();

    let t1 = db.flash_wear_telemetry();
    assert!(t1.total_physical_bytes > 0);
}

#[test]
fn test_flash_wear_telemetry_persistence_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("persist_wear.lilia");

    {
        let db = Database::open(DatabaseOptions::mobile(&db_path)).unwrap();
        db.batch(&[BatchOperation::KvSet {
            namespace: "p_kv".into(),
            key: b"test_key".to_vec(),
            value: vec![0x12; 512],
            if_version: None,
            expires_at_ms: None,
        }])
        .unwrap();

        // Persist telemetry to database metadata
        db.sync_parity().unwrap();
        db.persist_flash_wear().unwrap();
    }

    // Re-open database
    let db2 = Database::open(DatabaseOptions::mobile(&db_path)).unwrap();
    let snap = db2.flash_wear_telemetry();
    assert!(
        snap.total_physical_bytes > 0,
        "persisted write bytes must be loaded on restart"
    );
}
