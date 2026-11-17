use lilia_core::{BatchOperation, ErrorCode, Version};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::{json, Value};

fn open(dir: &tempfile::TempDir) -> Database {
    Database::open(DatabaseOptions::durable(dir.path().join("p.lilia"))).unwrap()
}

fn put(value: Value, if_version: Option<u64>) -> BatchOperation {
    BatchOperation::JsonPut {
        space: "users".into(),
        id: "u1".into(),
        value,
        if_version,
    }
}

fn del(if_version: Option<u64>) -> BatchOperation {
    BatchOperation::JsonDelete {
        space: "users".into(),
        id: "u1".into(),
        if_version,
    }
}

fn at_version(db: &Database, version: u64) -> Option<Value> {
    db.json()
        .get_at_version("users", "u1", Version(version))
        .unwrap()
        .map(|entry| entry.value())
}

#[test]
fn deleted_document_can_be_written_again() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);

    db.batch(&[put(json!({"a": 1}), None)]).unwrap();
    db.batch(&[del(None)]).unwrap();
    db.batch(&[put(json!({"a": 1}), None)]).unwrap();

    let current = db.json().get("users", "u1").unwrap().unwrap();
    assert_eq!(current.value(), json!({"a": 1}));
}

#[test]
fn versions_continue_across_delete_cycles_and_history_stays_readable() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);

    let first = db.batch(&[put(json!({"gen": 1}), None)]).unwrap();
    assert_eq!(first[0].version, Some(1));
    assert!(db.batch(&[del(None)]).unwrap()[0].deleted);

    let second = db.batch(&[put(json!({"gen": 2}), None)]).unwrap();
    assert_eq!(second[0].version, Some(3));
    assert!(db.batch(&[del(None)]).unwrap()[0].deleted);

    let third = db.batch(&[put(json!({"gen": 3}), None)]).unwrap();
    assert_eq!(third[0].version, Some(5));

    let current = db.json().get("users", "u1").unwrap().unwrap();
    assert_eq!(current.version, 5);
    assert_eq!(current.value(), json!({"gen": 3}));

    assert_eq!(at_version(&db, 1), Some(json!({"gen": 1})));
    assert_eq!(at_version(&db, 2), None);
    assert_eq!(at_version(&db, 3), Some(json!({"gen": 2})));
    assert_eq!(at_version(&db, 4), None);
    assert_eq!(at_version(&db, 5), Some(json!({"gen": 3})));

    let history = db.json().history("users", "u1").unwrap();
    let versions: Vec<Version> = history.iter().map(|revision| revision.version).collect();
    assert_eq!(versions, (1..=5).map(Version).collect::<Vec<_>>());
    let deletions: Vec<bool> = history
        .iter()
        .map(|revision| revision.is_deletion)
        .collect();
    assert_eq!(deletions, [false, true, false, true, false]);
    // Each incarnation starts from a full snapshot: nothing is diffed against a tombstone.
    assert!(history[2].is_keyframe);
    assert!(history[4].is_keyframe);

    let as_of = db
        .sql_query(
            "SELECT json_extract(value, '$.gen') FROM users AS OF VERSION 3 WHERE id = 'u1';",
            &[],
        )
        .unwrap();
    assert_eq!(as_of.rows, vec![vec![json!(2)]]);
}

#[test]
fn optimistic_checks_after_a_delete() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);

    db.batch(&[put(json!({"gen": 1}), None)]).unwrap();
    db.batch(&[del(Some(1))]).unwrap();

    // The version seen before the delete no longer matches anything.
    let stale = db.batch(&[put(json!({"gen": 2}), Some(1))]).unwrap_err();
    assert_eq!(stale.code, ErrorCode::Conflict);

    // `if_version: 0` still means "create only if absent".
    let recreated = db.batch(&[put(json!({"gen": 2}), Some(0))]).unwrap();
    assert_eq!(recreated[0].version, Some(3));
    let exists = db.batch(&[put(json!({"gen": 9}), Some(0))]).unwrap_err();
    assert_eq!(exists.code, ErrorCode::Conflict);

    let updated = db.batch(&[put(json!({"gen": 3}), Some(3))]).unwrap();
    assert_eq!(updated[0].version, Some(4));
    assert_eq!(at_version(&db, 3), Some(json!({"gen": 2})));
    assert_eq!(at_version(&db, 4), Some(json!({"gen": 3})));

    db.batch(&[del(Some(4))]).unwrap();
    assert_eq!(at_version(&db, 5), None);
    let last = db.batch(&[put(json!({"gen": 4}), None)]).unwrap();
    assert_eq!(last[0].version, Some(6));
}

#[test]
fn history_survives_reopen_and_keeps_the_sequence() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = open(&dir);
        db.batch(&[put(json!({"gen": 1}), None)]).unwrap();
        db.batch(&[del(None)]).unwrap();
        db.close(std::time::Duration::from_secs(5)).unwrap();
    }
    let db = open(&dir);
    let recreated = db.batch(&[put(json!({"gen": 2}), None)]).unwrap();
    assert_eq!(recreated[0].version, Some(3));
    assert_eq!(at_version(&db, 1), Some(json!({"gen": 1})));
    assert_eq!(at_version(&db, 3), Some(json!({"gen": 2})));
}
