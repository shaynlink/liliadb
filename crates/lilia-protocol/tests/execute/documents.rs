//! Key-value records and documents: indexes, history and undo.

use lilia_core::{BatchOperation, ErrorCode};
use lilia_protocol::{execute, Operation, ResponseValue, PROTOCOL_MAJOR};
use serde_json::json;

use super::{batch, open, put, run};

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one pass over the operations of each model"
)]
fn documents_round_trip_with_indexes_history_and_undo() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    assert!(matches!(
        run(
            &db,
            Operation::Handshake {
                major: PROTOCOL_MAJOR,
                minor: 0
            }
        ),
        ResponseValue::Handshake { .. }
    ));
    let refused = execute(&db, Operation::Handshake { major: 1, minor: 2 }).unwrap_err();
    assert_eq!(refused.code, ErrorCode::Unsupported);

    run(
        &db,
        Operation::JsonCreateIndex {
            space: "users".into(),
            name: "by_city".into(),
            path: "$.city".into(),
            filter: None,
        },
    );
    let versions = batch(
        &db,
        vec![
            put("users", "u1", json!({"name": "Ada", "city": "Paris"})),
            put("users", "u2", json!({"name": "Bob", "city": "Lyon"})),
            put("users", "x9", json!({"name": "Cy", "city": "Paris"})),
            BatchOperation::KvSet {
                namespace: "ns".into(),
                key: b"k".to_vec(),
                value: b"v".to_vec(),
                if_version: None,
                expires_at_ms: None,
            },
        ],
    );
    assert!(versions.iter().all(|&v| v > 0));

    let ResponseValue::Kv(Some(entry)) = run(
        &db,
        Operation::KvGet {
            namespace: "ns".into(),
            key: b"k".to_vec(),
        },
    ) else {
        panic!("the key was written");
    };
    assert_eq!(entry.value, b"v");

    let ResponseValue::JsonPage(paris) = run(
        &db,
        Operation::JsonScanByIndex {
            space: "users".into(),
            index_name: "by_city".into(),
            value: "Paris".into(),
            after: None,
            limit: 0,
        },
    ) else {
        panic!("an index scan answers with documents");
    };
    let ids: Vec<&str> = paris.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, ["u1", "x9"]);

    let ResponseValue::JsonPage(prefixed) = run(
        &db,
        Operation::JsonScanPrefix {
            space: "users".into(),
            prefix: "u".into(),
            after_id: Some("u1".into()),
            limit: 0,
        },
    ) else {
        panic!("a prefix scan answers with documents");
    };
    assert_eq!(prefixed.len(), 1);
    assert_eq!(prefixed[0].id, "u2");

    let ResponseValue::JsonExtracted(name) = run(
        &db,
        Operation::JsonExtract {
            space: "users".into(),
            id: "u2".into(),
            path: "$.name".into(),
        },
    ) else {
        panic!("an extraction answers with a value");
    };
    assert_eq!(name, Some(json!("Bob")));

    let before = versions[0];
    batch(
        &db,
        vec![put("users", "u1", json!({"name": "Ada", "city": "Nice"}))],
    );
    let ResponseValue::Json(Some(old)) = run(
        &db,
        Operation::JsonGetAtVersion {
            space: "users".into(),
            id: "u1".into(),
            version: before,
        },
    ) else {
        panic!("the old version is in the history");
    };
    assert_eq!(old.value()["city"], "Paris");
    let ResponseValue::JsonRevisions(revisions) = run(
        &db,
        Operation::JsonHistory {
            space: "users".into(),
            id: "u1".into(),
        },
    ) else {
        panic!("a history answers with revisions");
    };
    assert_eq!(revisions.len(), 2);

    let ResponseValue::UndoStep(Some(step)) = run(&db, Operation::Undo) else {
        panic!("the last commit can be undone");
    };
    assert!(step.keys > 0);
    let ResponseValue::Json(Some(restored)) = run(
        &db,
        Operation::JsonGet {
            space: "users".into(),
            id: "u1".into(),
        },
    ) else {
        panic!("the document is back");
    };
    assert_eq!(restored.value()["city"], "Paris");

    let ResponseValue::Sql(result) = run(
        &db,
        Operation::SqlQuery {
            sql: "SELECT count(*) FROM lilia_json WHERE space = $1".into(),
            params: vec![json!("users")],
        },
    ) else {
        panic!("SQL answers with rows");
    };
    assert_eq!(result.scalar_f64(), Some(3.0));

    assert!(matches!(
        run(&db, Operation::IntegrityCheck),
        ResponseValue::Bool(true)
    ));
    assert!(matches!(
        run(&db, Operation::Checkpoint),
        ResponseValue::Ack
    ));
    let ResponseValue::StorageStats(stats) = run(&db, Operation::StorageStats) else {
        panic!("storage statistics");
    };
    assert!(stats.levels.iter().any(|level| level.runs > 0));
}
