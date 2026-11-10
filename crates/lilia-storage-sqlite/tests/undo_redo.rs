use lilia_core::BatchOperation;
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

#[test]
fn test_native_transactional_undo_redo() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(
        DatabaseOptions::durable(temp_dir.path().join("undo_redo.lilia"))
            .with_undo_log_capacity(10),
    )
    .unwrap();

    // 1. Initial state: insert user document and settings key in a single transaction
    db.batch(&[
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "user_1".into(),
            value: json!({ "name": "Ada", "role": "admin" }),
            if_version: None,
        },
        BatchOperation::KvSet {
            namespace: "settings".into(),
            key: b"theme".to_vec(),
            value: b"dark".to_vec(),
            if_version: None,
            expires_at_ms: None,
        },
    ])
    .unwrap();

    assert_eq!(
        db.json_get("users", "user_1").unwrap().unwrap().value["name"],
        "Ada"
    );
    assert_eq!(
        db.kv_get("settings", b"theme").unwrap().unwrap().value,
        b"dark"
    );

    // 2. Mutation 2: update user role and change theme
    db.batch(&[
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "user_1".into(),
            value: json!({ "name": "Ada", "role": "superadmin" }),
            if_version: Some(1),
        },
        BatchOperation::KvSet {
            namespace: "settings".into(),
            key: b"theme".to_vec(),
            value: b"light".to_vec(),
            if_version: Some(1),
            expires_at_ms: None,
        },
    ])
    .unwrap();

    assert_eq!(
        db.json_get("users", "user_1").unwrap().unwrap().value["role"],
        "superadmin"
    );
    assert_eq!(
        db.kv_get("settings", b"theme").unwrap().unwrap().value,
        b"light"
    );

    // 3. Roll back last mutation (undo mutation 2)
    let undo_res = db
        .rollback_last_mutation()
        .unwrap()
        .expect("must undo mutation 2");
    assert!(undo_res.summary.contains("batch"));

    // State must be cleanly restored to Mutation 1!
    assert_eq!(
        db.json_get("users", "user_1").unwrap().unwrap().value["role"],
        "admin"
    );
    assert_eq!(
        db.kv_get("settings", b"theme").unwrap().unwrap().value,
        b"dark"
    );

    // 4. Redo mutation 2
    let redo_res = db.redo().unwrap().expect("must redo mutation 2");
    assert!(redo_res.summary.contains("batch"));

    assert_eq!(
        db.json_get("users", "user_1").unwrap().unwrap().value["role"],
        "superadmin"
    );
    assert_eq!(
        db.kv_get("settings", b"theme").unwrap().unwrap().value,
        b"light"
    );

    // 5. Roll back mutation 2 and then mutation 1
    db.undo().unwrap().expect("undo 2");
    assert_eq!(
        db.json_get("users", "user_1").unwrap().unwrap().value["role"],
        "admin"
    );

    db.undo().unwrap().expect("undo 1");
    // After undoing mutation 1, the document and key do not exist!
    assert!(db.json_get("users", "user_1").unwrap().is_none());
    assert!(db.kv_get("settings", b"theme").unwrap().is_none());

    // 6. Undo when history is empty returns None
    assert!(db.undo().unwrap().is_none());

    // 7. Check undo_history list
    let history = db.undo_history().unwrap();
    assert_eq!(history.len(), 2);
    assert!(history[0].undone);
    assert!(history[1].undone);
}
