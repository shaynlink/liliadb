use std::sync::{Arc, Mutex};

use lilia_core::{BatchOperation, LiveQueryDiff};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

#[test]
#[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
fn test_live_query_differential_events_on_sql_mutations() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("live_query_test.lilia"),
    ))
    .unwrap();

    // 1. Create table
    db.sql_query(
        "CREATE TABLE tasks (id TEXT PRIMARY KEY, title TEXT, status TEXT, priority INT);",
        &[],
    )
    .unwrap();

    // 2. Subscribe to live query for pending tasks
    let received_diffs = Arc::new(Mutex::new(Vec::<LiveQueryDiff>::new()));
    let diffs_clone = Arc::clone(&received_diffs);

    let _sub = db
        .live_query(
            "SELECT * FROM tasks WHERE status = 'pending';",
            &[],
            move |diff| {
                diffs_clone.lock().unwrap().push(diff);
            },
        )
        .unwrap();

    // Initial snapshot should be emitted immediately
    {
        let diffs = received_diffs.lock().unwrap();
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].is_initial);
        assert_eq!(diffs[0].added, [] as [serde_json::Value; 0]);
    }

    // 3. INSERT matching row T3 (status = 'pending')
    db.sql_query(
        "INSERT INTO tasks VALUES ('T3', 'Deploy', 'pending', 1);",
        &[],
    )
    .unwrap();

    // Check diff: T3 added
    {
        let diffs = received_diffs.lock().unwrap();
        assert_eq!(diffs.len(), 2);
        let d = &diffs[1];
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.added[0]["id"], "T3");
        assert_eq!(d.added[0]["status"], "pending");
        assert_eq!(d.updated, [] as [serde_json::Value; 0]);
        assert_eq!(d.removed, [] as [serde_json::Value; 0]);
    }

    // 4. INSERT non-matching row T4 (status = 'completed') -> must NOT emit any diff!
    db.sql_query(
        "INSERT INTO tasks VALUES ('T4', 'Documentation', 'completed', 2);",
        &[],
    )
    .unwrap();

    {
        let diffs = received_diffs.lock().unwrap();
        assert_eq!(
            diffs.len(),
            2,
            "no event should be emitted for non-matching row"
        );
    }

    // 5. UPDATE T3 to status = 'completed' -> transitions OUT of query view -> removed!
    db.sql_query(
        "UPDATE tasks SET status = 'completed' WHERE id = 'T3';",
        &[],
    )
    .unwrap();

    {
        let diffs = received_diffs.lock().unwrap();
        assert_eq!(diffs.len(), 3);
        let d = &diffs[2];
        assert_eq!(d.added, [] as [serde_json::Value; 0]);
        assert_eq!(d.updated, [] as [serde_json::Value; 0]);
        assert_eq!(d.removed.len(), 1);
        assert_eq!(d.removed[0]["id"], "T3");
    }

    // 6. UPDATE T4 to status = 'pending' -> transitions INTO query view -> added!
    db.sql_query("UPDATE tasks SET status = 'pending' WHERE id = 'T4';", &[])
        .unwrap();

    {
        let diffs = received_diffs.lock().unwrap();
        assert_eq!(diffs.len(), 4);
        let d = &diffs[3];
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.added[0]["id"], "T4");
        assert_eq!(d.updated, [] as [serde_json::Value; 0]);
        assert_eq!(d.removed, [] as [serde_json::Value; 0]);
    }

    // 7. UPDATE T4 priority (remains status = 'pending') -> updated!
    db.sql_query(
        "UPDATE tasks SET priority = 10, title = 'Documentation Urgent' WHERE id = 'T4';",
        &[],
    )
    .unwrap();

    {
        let diffs = received_diffs.lock().unwrap();
        assert_eq!(diffs.len(), 5);
        let d = &diffs[4];
        assert_eq!(d.added, [] as [serde_json::Value; 0]);
        assert_eq!(d.updated.len(), 1);
        assert_eq!(d.updated[0]["id"], "T4");
        assert_eq!(d.updated[0]["priority"], 10);
        assert_eq!(d.updated[0]["title"], "Documentation Urgent");
        assert_eq!(d.removed, [] as [serde_json::Value; 0]);
    }

    // 8. DELETE T4 -> removed!
    db.sql_query("DELETE FROM tasks WHERE id = 'T4';", &[])
        .unwrap();

    {
        let diffs = received_diffs.lock().unwrap();
        assert_eq!(diffs.len(), 6);
        let d = &diffs[5];
        assert_eq!(d.added, [] as [serde_json::Value; 0]);
        assert_eq!(d.updated, [] as [serde_json::Value; 0]);
        assert_eq!(d.removed.len(), 1);
        assert_eq!(d.removed[0]["id"], "T4");
    }
}

#[test]
fn test_live_query_on_json_documents() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("live_query_json.lilia"),
    ))
    .unwrap();

    // Subscribe to JSON space 'tickets' with priority >= 3
    let received_diffs = Arc::new(Mutex::new(Vec::<LiveQueryDiff>::new()));
    let diffs_clone = Arc::clone(&received_diffs);

    let _sub = db
        .live_query(
            "SELECT id, value FROM lilia_json WHERE space = 'tickets' AND json_extract(value, '$.priority') >= 3;",
            &[],
            move |diff| {
                diffs_clone.lock().unwrap().push(diff);
            },
        )
        .unwrap();

    // Batch put ticket with priority 1 (non-matching)
    db.batch(&[BatchOperation::JsonPut {
        space: "tickets".into(),
        id: "TICK-1".into(),
        value: json!({ "title": "Typo in footer", "priority": 1 }),
        if_version: None,
    }])
    .unwrap();

    {
        let diffs = received_diffs.lock().unwrap();
        // Only initial snapshot
        assert_eq!(diffs.len(), 1);
    }

    // Batch put ticket with priority 5 (matching)
    db.batch(&[BatchOperation::JsonPut {
        space: "tickets".into(),
        id: "TICK-2".into(),
        value: json!({ "title": "Server crash", "priority": 5 }),
        if_version: None,
    }])
    .unwrap();

    {
        let diffs = received_diffs.lock().unwrap();
        assert_eq!(diffs.len(), 2);
        assert_eq!(diffs[1].added.len(), 1);
        assert_eq!(diffs[1].added[0]["id"], "TICK-2");
    }

    // Batch update TICK-1 to priority 4 (transitions into view)
    db.batch(&[BatchOperation::JsonPut {
        space: "tickets".into(),
        id: "TICK-1".into(),
        value: json!({ "title": "Typo escalated", "priority": 4 }),
        if_version: None,
    }])
    .unwrap();

    {
        let diffs = received_diffs.lock().unwrap();
        assert_eq!(diffs.len(), 3);
        assert_eq!(diffs[2].added.len(), 1);
        assert_eq!(diffs[2].added[0]["id"], "TICK-1");
    }

    // Batch delete TICK-2
    db.batch(&[BatchOperation::JsonDelete {
        space: "tickets".into(),
        id: "TICK-2".into(),
        if_version: None,
    }])
    .unwrap();

    {
        let diffs = received_diffs.lock().unwrap();
        assert_eq!(diffs.len(), 4);
        assert_eq!(diffs[3].removed.len(), 1);
        assert_eq!(diffs[3].removed[0]["id"], "TICK-2");
    }
}
