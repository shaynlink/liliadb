use lilia_core::BatchOperation;
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};

#[test]
fn test_auto_repair_recovers_corrupted_page_without_data_loss() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("flash_resilience.lilia");

    // 1. Open mobile database with Reed-Solomon parity enabled by default
    let db = Database::open(DatabaseOptions::mobile(&db_path)).unwrap();

    // 2. Populate data spanning multiple pages
    let mut batch = Vec::new();
    for i in 0..100 {
        batch.push(BatchOperation::JsonPut {
            space: "mobile_docs".into(),
            id: format!("doc_{i}"),
            value: json!({
                "id": i,
                "payload": "x".repeat(300),
                "device": "iPhone15,2",
                "battery": 100 - (i % 100),
            }),
            if_version: None,
        });
        batch.push(BatchOperation::KvSet {
            namespace: "mobile_cache".into(),
            key: format!("k_{i}").into_bytes(),
            value: vec![0xBB; 256],
            if_version: None,
            expires_at_ms: None,
        });
    }
    db.batch(&batch).unwrap();

    // 3. Synchronize parity blocks
    db.sync_parity().unwrap();
    let parity_path = dir.path().join("flash_resilience.lilia.rsp");
    assert!(parity_path.exists(), "parity sidecar file must exist");

    // Check all pages pass initial parity verification
    assert!(db.verify_page_parity(1).unwrap());
    assert!(db.verify_page_parity(2).unwrap());

    // 4. Drop DB instance to close connections before raw physical corruption
    drop(db);

    // 5. Simulate NAND flash physical bitflip in page 2
    let page_size = 4096usize;
    let page2_offset = page_size as u64; // page 2 offset (0-indexed page index 1)

    {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&db_path)
            .unwrap();
        file.seek(SeekFrom::Start(page2_offset + 500)).unwrap();
        let mut bad_byte = [0u8; 1];
        file.read_exact(&mut bad_byte).unwrap();
        bad_byte[0] ^= 0b01010101; // flip 4 bits!
        file.seek(SeekFrom::Start(page2_offset + 500)).unwrap();
        file.write_all(&bad_byte).unwrap();
        file.sync_all().unwrap();
    }

    // 6. Verify that page 2 is detected as corrupted by the parity manager
    let parity_mgr =
        lilia_storage_sqlite::ParityManager::new(&db_path, lilia_core::ParityConfig::default())
            .unwrap();
    assert!(
        !parity_mgr.verify_page(2).unwrap(),
        "parity manager must detect the bitflip in page 2"
    );

    // 7. Perform in-flight auto-repair
    let outcome = parity_mgr.reconstruct_and_repair_page(2).unwrap();
    assert_eq!(outcome.page_id, 2);
    assert!(outcome.checksum_matches);
    assert_eq!(outcome.bytes_reconstructed, page_size);

    // 8. Verify page 2 is now physically repaired on disk
    assert!(
        parity_mgr.verify_page(2).unwrap(),
        "page 2 must pass parity verification after repair"
    );

    // 9. Re-open database with SQLite and verify complete data integrity
    let db = Database::open(DatabaseOptions::mobile(&db_path)).unwrap();
    assert!(
        db.integrity_check().unwrap(),
        "database must pass SQLite integrity check after auto-repair"
    );

    // Read back all documents to verify zero data loss
    let json_col = db.collection("mobile_docs").json().unwrap();
    for i in 0..100 {
        let entry = json_col.get(&format!("doc_{i}")).unwrap();
        assert!(entry.is_some(), "doc_{i} must be intact");
    }
}

#[test]
fn test_preflight_auto_repair_on_database_open() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("preflight_repair.lilia");

    // Open and write documents
    let db = Database::open(DatabaseOptions::mobile(&db_path)).unwrap();
    let mut batch = Vec::new();
    for i in 0..50 {
        batch.push(BatchOperation::JsonPut {
            space: "crash_recovery".into(),
            id: format!("doc_{i}"),
            value: json!({ "id": i, "data": "critical sensor telemetry payload" }),
            if_version: None,
        });
    }
    db.batch(&batch).unwrap();
    db.sync_parity().unwrap();
    drop(db);

    // Corrupt page 2 directly on disk (simulating torn write / battery cutoff)
    let page2_offset = 4096u64;
    {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&db_path)
            .unwrap();
        file.seek(SeekFrom::Start(page2_offset + 120)).unwrap();
        file.write_all(&[0xDE, 0xAD, 0xBE, 0xEF]).unwrap();
        file.sync_all().unwrap();
    }

    // Re-opening with auto_repair_on_open (default for mobile) must repair the database in preflight!
    let db = Database::open(DatabaseOptions::mobile(&db_path)).unwrap();
    assert!(
        db.integrity_check().unwrap(),
        "preflight auto-repair must restore SQLite integrity"
    );

    let doc = db
        .collection("crash_recovery")
        .json()
        .unwrap()
        .get("doc_0")
        .unwrap();
    assert!(doc.is_some());
}

#[test]
fn test_multi_page_repair_across_different_groups() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("multigroup_repair.lilia");

    let db = Database::open(DatabaseOptions::mobile(&db_path)).unwrap();

    // Insert enough data to span more than 8 pages (K=8 data shards per group)
    let mut batch = Vec::new();
    for i in 0..150 {
        batch.push(BatchOperation::KvSet {
            namespace: "large_kv".into(),
            key: format!("key_{i:04}").into_bytes(),
            value: vec![0x77; 1024],
            if_version: None,
            expires_at_ms: None,
        });
    }
    db.batch(&batch).unwrap();
    db.sync_parity().unwrap();
    drop(db);

    // Corrupt page 2 (group 0) and page 10 (group 1)
    let page_size = 4096u64;
    {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&db_path)
            .unwrap();

        // Corrupt page 2
        file.seek(SeekFrom::Start(page_size + 200)).unwrap();
        file.write_all(&[0xFF; 8]).unwrap();

        // Corrupt page 10
        file.seek(SeekFrom::Start((10 - 1) * page_size + 200))
            .unwrap();
        file.write_all(&[0xEE; 8]).unwrap();

        file.sync_all().unwrap();
    }

    // Run auto-repair
    let parity_mgr =
        lilia_storage_sqlite::ParityManager::new(&db_path, lilia_core::ParityConfig::default())
            .unwrap();

    let summary = parity_mgr.auto_repair_database().unwrap();
    assert_eq!(
        summary.bitflips_detected, 2,
        "both corrupted pages must be detected"
    );
    assert_eq!(
        summary.pages_repaired, 2,
        "both corrupted pages must be repaired"
    );
    assert_eq!(summary.unrecoverable_pages, 0);

    // Verify database opens cleanly
    let db = Database::open(DatabaseOptions::mobile(&db_path)).unwrap();
    assert!(db.integrity_check().unwrap());
}
