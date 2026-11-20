use lilia_core::BatchOperation;
use lilia_shm::ShmOptions;
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

#[test]
fn test_database_shm_mirroring_and_zero_copy_reads() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("test_shm.lilia");
    let shm_path = temp_dir.path().join("test_shm.lilia.shm");

    let options =
        DatabaseOptions::durable(&db_path).with_shared_memory(ShmOptions::file_backed(&shm_path));

    let db = Database::open(options).expect("open database");
    assert!(db.shm_engine().is_some());

    // 1. Write KV through batch
    db.batch(&[BatchOperation::KvSet {
        namespace: "tokens".into(),
        key: b"session:99".to_vec(),
        value: b"secret_token_123".to_vec(),
        if_version: None,
        expires_at_ms: None,
    }])
    .expect("batch kv");

    // Zero-copy read via Database SHM method (< 50 ns)
    let entry = db.shm_get("tokens", b"session:99").expect("shm get");
    assert_eq!(entry.value, b"secret_token_123");

    // Zero-copy read via standalone ShmReader (e.g. Widget / Daemon process)
    let reader = db.shm_reader().expect("shm reader");
    let reader_entry = reader.get("tokens", b"session:99").expect("reader get");
    assert_eq!(reader_entry.value, b"secret_token_123");

    // 2. Write JSON document through batch
    db.batch(&[BatchOperation::JsonPut {
        space: "devices".into(),
        id: "sensor_alpha".into(),
        value: json!({
            "status": "online",
            "temperature": 23.5,
            "location": "lab-1"
        }),
        if_version: None,
    }])
    .expect("batch json");

    // Zero-copy read JSON from SHM
    let json_entry = db
        .shm_json_get("devices", "sensor_alpha")
        .expect("shm json get");
    assert_eq!(json_entry.id, "sensor_alpha");

    let location = reader.json_extract_str("devices", "sensor_alpha", "location");
    assert_eq!(location.as_deref(), Some("lab-1"));

    // Phase 3: Zero-copy direct rkyv archived view & fast-path extractors
    let archived_view = reader
        .get_archived_json("devices", "sensor_alpha")
        .expect("reader get_archived_json");
    assert_eq!(archived_view.extract_str("location"), Some("lab-1"));
    assert_eq!(archived_view.extract_f64("temperature"), Some(23.5));
    assert_eq!(archived_view.extract_str("status"), Some("online"));

    // Direct Database method
    let db_view = db
        .shm_get_archived_json("devices", "sensor_alpha")
        .expect("db shm_get_archived_json");
    assert_eq!(db_view.extract_str("location"), Some("lab-1"));

    // with_archived_json SHM fast-path
    let with_archived_result = db
        .with_archived_json("devices", "sensor_alpha", |doc| {
            doc.extract_str("location").map(ToString::to_string)
        })
        .expect("with_archived_json");
    assert_eq!(with_archived_result, Some(Some("lab-1".to_string())));

    // 3. Delete KV
    db.batch(&[BatchOperation::KvDelete {
        namespace: "tokens".into(),
        key: b"session:99".to_vec(),
        if_version: None,
    }])
    .expect("delete kv");

    assert!(db.shm_get("tokens", b"session:99").is_none());
    assert!(reader.get("tokens", b"session:99").is_none());
}
