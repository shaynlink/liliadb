use lilia_core::BatchOperation;
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

#[test]
#[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
fn test_zero_copy_json_nested_extractions() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("zero_copy_extract.lilia");
    let db = Database::open(DatabaseOptions::durable(db_path)).unwrap();

    let doc = json!({
        "user": {
            "name": "Alice",
            "age": 30,
            "balance": 1250.75,
            "active": true,
            "address": {
                "city": "Paris",
                "zip": "75001"
            },
            "tags": ["admin", "dev"],
            "orders": [
                {"id": "ord_1", "amount": 100},
                {"id": "ord_2", "amount": 250}
            ]
        }
    });

    db.batch(&[BatchOperation::JsonPut {
        space: "accounts".into(),
        id: "acc_1".into(),
        value: doc,
        if_version: None,
    }])
    .unwrap();

    // 1. Direct Zero-Copy typed extractions via Database API
    assert_eq!(
        db.json_extract_str("accounts", "acc_1", "user.name")
            .unwrap()
            .as_deref(),
        Some("Alice")
    );
    assert_eq!(
        db.json_extract_str("accounts", "acc_1", "user.address.city")
            .unwrap()
            .as_deref(),
        Some("Paris")
    );
    assert_eq!(
        db.json_extract_i64("accounts", "acc_1", "user.age")
            .unwrap(),
        Some(30)
    );
    assert_eq!(
        db.json_extract_f64("accounts", "acc_1", "user.balance")
            .unwrap(),
        Some(1250.75)
    );
    assert_eq!(
        db.json_extract_bool("accounts", "acc_1", "user.active")
            .unwrap(),
        Some(true)
    );

    // 2. Array index and alternative syntax support:
    // Dot notation on array index:
    assert_eq!(
        db.json_extract_str("accounts", "acc_1", "user.tags.0")
            .unwrap()
            .as_deref(),
        Some("admin")
    );
    // Bracket array syntax:
    assert_eq!(
        db.json_extract_str("accounts", "acc_1", "user.tags[1]")
            .unwrap()
            .as_deref(),
        Some("dev")
    );
    // Nested array object:
    assert_eq!(
        db.json_extract_str("accounts", "acc_1", "user.orders[1].id")
            .unwrap()
            .as_deref(),
        Some("ord_2")
    );
    assert_eq!(
        db.json_extract_i64("accounts", "acc_1", "user.orders[1].amount")
            .unwrap(),
        Some(250)
    );

    // RFC 6901 JSON pointer syntax:
    assert_eq!(
        db.json_extract_str("accounts", "acc_1", "/user/address/city")
            .unwrap()
            .as_deref(),
        Some("Paris")
    );

    // SQLite path syntax ($ prefix):
    assert_eq!(
        db.json_extract_str("accounts", "acc_1", "$.user.address.city")
            .unwrap()
            .as_deref(),
        Some("Paris")
    );

    // 3. Extraction via SQL scalar function lilia_extract
    let sql_res = db
        .sql_query(
            "SELECT lilia_extract(value, 'user.address.city'), \
                    lilia_extract(value, 'user.orders[0].amount') \
             FROM _lilia_json WHERE space = 'accounts' AND id = 'acc_1';",
            &[],
        )
        .unwrap();

    assert_eq!(sql_res.rows.len(), 1);
    assert_eq!(sql_res.rows[0][0], json!("Paris"));
    assert_eq!(sql_res.rows[0][1], json!(100));

    // 4. json_get returns JsonEntry which implements Deref to BinaryDoc
    let entry = db.json_get("accounts", "acc_1").unwrap().unwrap();
    assert_eq!(entry.extract_str("user.name"), Some("Alice"));
    assert_eq!(entry.extract_i64("user.age"), Some(30));
    assert_eq!(entry.extract_bool("user.active"), Some(true));
    assert_eq!(entry.value()["user"]["address"]["zip"], "75001");
}

#[test]
fn test_zstd_collection_dictionary_compression_and_savings() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("zstd_dict.lilia");

    // Configure automatic training with threshold = 50 documents
    let mut options = DatabaseOptions::durable(&db_path);
    options.json_options.dict_sample_threshold = 50;
    options.json_options.dict_target_size_bytes = 8 * 1024; // 8 KB dictionary

    let db = Database::open(options).unwrap();

    // Generate 60 typical user event documents with repetitive schema keys
    let mut batch_ops = Vec::new();
    for i in 0..60 {
        let doc = json!({
            "event_id": format!("evt_{i:04}"),
            "event_type": "user_session_heartbeat",
            "timestamp": "2026-11-10T12:00:00.000Z",
            "service_name": "auth-service-us-east-1",
            "environment": "production-cluster-aws",
            "payload": {
                "user_id": format!("user_{}", i % 5),
                "ip_address": "192.168.1.100",
                "user_agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
                "action": "page_view",
                "duration_ms": 120 + i
            }
        });

        batch_ops.push(BatchOperation::JsonPut {
            space: "events".into(),
            id: format!("evt_{i:04}"),
            value: doc,
            if_version: None,
        });
    }

    db.batch(&batch_ops).unwrap();

    // Since threshold is 50, batch execution of 60 docs should have automatically trained the dictionary!
    let dict = db.dict_registry().get("events");
    assert!(
        dict.is_some(),
        "Dictionary for 'events' must be automatically trained"
    );
    let dict_info = dict.unwrap();
    assert_eq!(dict_info.space, "events");
    assert!(dict_info.dict_id >= 1);
    assert_ne!(dict_info.raw_bytes, [] as [u8; 0]);

    // Insert 10 more documents to leverage the trained dictionary
    let mut new_ops = Vec::new();
    for i in 100..110 {
        let doc = json!({
            "event_id": format!("evt_{i:04}"),
            "event_type": "user_session_heartbeat",
            "timestamp": "2026-11-10T12:05:00.000Z",
            "service_name": "auth-service-us-east-1",
            "environment": "production-cluster-aws",
            "payload": {
                "user_id": format!("user_{}", i % 5),
                "ip_address": "192.168.1.100",
                "user_agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
                "action": "page_view",
                "duration_ms": 300 + i
            }
        });

        new_ops.push(BatchOperation::JsonPut {
            space: "events".into(),
            id: format!("evt_{i:04}"),
            value: doc,
            if_version: None,
        });
    }
    db.batch(&new_ops).unwrap();

    // Verify format and dictionary ID stored on disk
    let inspect_res = db
        .sql_query(
            "SELECT format, dict_id, length(value) FROM _lilia_json WHERE space = 'events' AND id = 'evt_0105';",
            &[],
        )
        .unwrap();

    assert_eq!(inspect_res.rows.len(), 1);
    let format_val = inspect_res.rows[0][0].as_i64().unwrap();
    let dict_id_val = inspect_res.rows[0][1].as_i64().unwrap();
    let compressed_len = inspect_res.rows[0][2].as_i64().unwrap();

    // Format 2 = Zstd compressed with dictionary
    assert_eq!(format_val, 2);
    assert_eq!(dict_id_val, i64::from(dict_info.dict_id));

    // Notice: uncompressed JSON string is ~350 bytes; compressed with dictionary is ~80-120 bytes!
    assert!(
        compressed_len < 180,
        "Compressed document length ({compressed_len} bytes) must demonstrate substantial space reduction"
    );

    // Read back and verify transparency
    let entry = db.json_get("events", "evt_0105").unwrap().unwrap();
    assert_eq!(
        entry.extract_str("event_type"),
        Some("user_session_heartbeat")
    );
    assert_eq!(entry.extract_str("payload.action"), Some("page_view"));
    assert_eq!(entry.extract_i64("payload.duration_ms"), Some(405));

    // Close and reopen database: dictionaries must be preloaded automatically
    drop(db);
    let db2 = Database::open(DatabaseOptions::durable(&db_path)).unwrap();
    assert!(db2.dict_registry().get("events").is_some());

    let entry2 = db2.json_get("events", "evt_0105").unwrap().unwrap();
    assert_eq!(
        entry2.extract_str("event_type"),
        Some("user_session_heartbeat")
    );
    assert_eq!(entry2.extract_i64("payload.duration_ms"), Some(405));
}

#[test]
fn test_disabled_auto_train_per_space() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("disable_auto_train.lilia");

    let mut options = DatabaseOptions::durable(&db_path);
    options.json_options.dict_sample_threshold = 20;
    // Disable auto-training specifically for the 'raw_logs' space
    options.json_options.disabled_auto_train_spaces = vec!["raw_logs".to_string()];

    let db = Database::open(options).unwrap();

    let mut ops = Vec::new();
    for i in 0..30 {
        ops.push(BatchOperation::JsonPut {
            space: "raw_logs".into(),
            id: format!("log_{i}"),
            value: json!({"message": "test", "level": "info", "idx": i}),
            if_version: None,
        });
    }
    db.batch(&ops).unwrap();

    // Should NOT have trained automatically because 'raw_logs' is disabled
    assert!(db.dict_registry().get("raw_logs").is_none());

    // Explicit manual training can still be called
    let stats = db.train_json_dictionary("raw_logs", 30, 4096).unwrap();
    assert_eq!(stats.space, "raw_logs");
    assert!(stats.dict_size > 0);
    assert!(db.dict_registry().get("raw_logs").is_some());
}

#[test]
fn test_simd_raw_bytes_put() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("simd_raw_put.lilia");
    let db = Database::open(DatabaseOptions::durable(db_path)).unwrap();

    let raw_json = br#"{"title":"High performance SIMD","count":9999,"nested":{"enabled":true}}"#;

    db.batch(&[BatchOperation::JsonPutRaw {
        space: "metrics".into(),
        id: "m_1".into(),
        data: raw_json.to_vec(),
        if_version: None,
    }])
    .unwrap();

    let entry = db.json_get("metrics", "m_1").unwrap().unwrap();
    assert_eq!(entry.extract_str("title"), Some("High performance SIMD"));
    assert_eq!(entry.extract_i64("count"), Some(9999));
    assert_eq!(entry.extract_bool("nested.enabled"), Some(true));
}
