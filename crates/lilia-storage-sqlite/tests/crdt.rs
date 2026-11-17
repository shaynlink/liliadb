use std::collections::BTreeSet;

use lilia_core::BatchOperation;
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

fn database() -> (tempfile::TempDir, Database) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let database = Database::open(DatabaseOptions::durable(
        directory.path().join("crdt_test.lilia"),
    ))
    .expect("open database");
    (directory, database)
}

#[test]
fn test_storage_pn_counter_lifecycle_and_sql_view() {
    let (_dir, db) = database();
    let counter = db.crdt().counter("metrics", "hits");

    assert_eq!(counter.get().expect("get counter"), 0);

    // Actor 1: +10, -2
    counter.increment("device-1", 10).expect("inc");
    counter.decrement("device-1", 2).expect("dec");
    assert_eq!(counter.get().expect("get"), 8);

    // Actor 2: +15, -5
    counter.increment("device-2", 15).expect("inc");
    counter.decrement("device-2", 5).expect("dec");
    assert_eq!(counter.get().expect("get"), 18);

    // Verify through the SQLite View lilia_crdt_counters
    let res = db
        .sql_query(
            "SELECT value FROM lilia_crdt_counters WHERE space = ?1 AND id = ?2;",
            &[json!("metrics"), json!("hits")],
        )
        .expect("sql query");
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0][0], json!(18));

    // Verify state export
    let state = counter.state().expect("state");
    assert_eq!(state.value(), 18);
    assert_eq!(state.p.get("device-1"), Some(&10));
    assert_eq!(state.n.get("device-1"), Some(&2));
    assert_eq!(state.p.get("device-2"), Some(&15));
    assert_eq!(state.n.get("device-2"), Some(&5));

    // Remote merge
    let mut remote = state.clone();
    remote.increment("device-3", 100);
    remote.decrement("device-1", 5); // higher decrement than 2

    let merged_val = counter.merge(&remote).expect("merge");
    assert_eq!(merged_val, 113); // device-1: 10 - (2+5) = 3; device-2: 15 - 5 = 10; device-3: 100 => 113
    assert_eq!(counter.get().expect("get"), 113);
}

#[test]
fn test_storage_or_set_lifecycle_add_wins_and_sql_view() {
    let (_dir, db1) = database();
    let (_dir2, db2) = database();

    let set1 = db1.crdt().set("inventory", "items");
    let set2 = db2.crdt().set("inventory", "items");

    // Add elements on db1
    set1.add("scanner-1", "widget-A").expect("add");
    set1.add("scanner-1", "widget-B").expect("add");

    assert!(set1.contains("widget-A").expect("contains"));
    assert!(set1.contains("widget-B").expect("contains"));
    assert!(!set1.contains("widget-C").expect("not contains"));
    assert_eq!(
        set1.read().expect("read"),
        BTreeSet::from(["widget-A".to_string(), "widget-B".to_string()])
    );

    // Sync state from db1 to db2
    let state1 = set1.state().expect("state1");
    set2.merge(&state1).expect("merge into db2");
    assert_eq!(
        set2.read().expect("read db2"),
        set1.read().expect("read db1")
    );

    // Concurrent offline operations:
    // db1 removes "widget-A"
    set1.remove("widget-A").expect("remove widget-A");
    assert!(!set1.contains("widget-A").expect("not contains"));

    // db2 concurrently re-adds "widget-A" (with a new tag)
    set2.add("scanner-2", "widget-A").expect("re-add on db2");

    // Merge db2 into db1 -> Add-Wins semantics ensures "widget-A" is kept!
    let state2 = set2.state().expect("state2");
    set1.merge(&state2).expect("merge db2 into db1");
    assert!(set1.contains("widget-A").expect("Add wins on merge"));
    assert!(set1.contains("widget-B").expect("contains B"));

    // Verify through SQLite View lilia_crdt_orsets
    let res = db1
        .sql_query(
            "SELECT element FROM lilia_crdt_orsets WHERE space = ?1 AND id = ?2 ORDER BY element;",
            &[json!("inventory"), json!("items")],
        )
        .expect("sql view query");
    assert_eq!(res.rows.len(), 2);
    assert_eq!(res.rows[0][0], json!("widget-A"));
    assert_eq!(res.rows[1][0], json!("widget-B"));

    // Prune tombstones older than future timestamp
    let pruned = set1.prune(i64::MAX).expect("prune");
    assert!(pruned >= 1);
}

#[test]
fn test_storage_lww_json_concurrent_fields_and_sql_extract() {
    let (_dir, db1) = database();
    let (_dir2, db2) = database();

    let doc1 = db1.crdt().doc("users", "user-42");
    let doc2 = db2.crdt().doc("users", "user-42");

    // Seed initial document on db1
    let v1 = doc1
        .set_field("system", "/username", json!("johndoe"), Some(100))
        .expect("set username");
    assert_eq!(v1, 1);
    doc1.set_field("system", "/profile/bio", json!("Initial bio"), Some(100))
        .expect("set bio");
    doc1.set_field("system", "/profile/theme", json!("dark"), Some(100))
        .expect("set theme");

    let initial = doc1.get().expect("get").expect("exists");
    assert_eq!(initial["username"], "johndoe");
    assert_eq!(initial["profile"]["bio"], "Initial bio");
    assert_eq!(initial["profile"]["theme"], "dark");

    // Sync to db2
    let state1 = doc1.state().expect("state").expect("exists");
    doc2.merge(&state1).expect("merge to db2");

    // Offline concurrent edits:
    // db1 modifies bio at t=150
    doc1.set_field(
        "device-a",
        "/profile/bio",
        json!("Updated on mobile"),
        Some(150),
    )
    .expect("update bio");

    // db2 modifies theme at t=160 and adds avatar
    doc2.set_field("device-b", "/profile/theme", json!("light"), Some(160))
        .expect("update theme");
    doc2.set_field(
        "device-b",
        "/profile/avatar",
        json!("avatar.png"),
        Some(160),
    )
    .expect("add avatar");

    // Merge db2 into db1
    let state2 = doc2.state().expect("state2").expect("exists");
    doc1.merge(&state2).expect("merge db2 into db1");

    let merged_val = doc1.get().expect("get merged").expect("exists");
    // All independent concurrent field updates merged cleanly!
    assert_eq!(merged_val["username"], "johndoe");
    assert_eq!(merged_val["profile"]["bio"], "Updated on mobile");
    assert_eq!(merged_val["profile"]["theme"], "light");
    assert_eq!(merged_val["profile"]["avatar"], "avatar.png");

    // Verify querying through the SQLite View lilia_crdt_json and json_extract
    let res = db1
        .sql_query(
            "SELECT json_extract(value, '$.profile.bio'), json_extract(value, '$.profile.theme') \
             FROM lilia_crdt_json WHERE space = ?1 AND id = ?2;",
            &[json!("users"), json!("user-42")],
        )
        .expect("sql query");
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0][0], json!("Updated on mobile"));
    assert_eq!(res.rows[0][1], json!("light"));

    // Verify custom scalar function lilia_crdt_json_extract on compressed binary state
    let res_scalar = db1
        .sql_query(
            "SELECT lilia_crdt_json_extract(state, '/profile/theme') \
             FROM _lilia_crdt_json WHERE space = ?1 AND id = ?2;",
            &[json!("users"), json!("user-42")],
        )
        .expect("scalar extract");
    assert_eq!(res_scalar.rows.len(), 1);
    assert_eq!(res_scalar.rows[0][0], json!("\"light\""));

    // Test deleting a field
    doc1.delete_field("device-a", "/profile/avatar", Some(200))
        .expect("delete avatar");
    let after_delete = doc1.get().expect("get").expect("exists");
    assert!(after_delete["profile"].get("avatar").is_none());
}

#[test]
fn test_crdt_batch_atomicity_and_cdc_events() {
    let (_dir, db) = database();
    let mut rx = db.subscribe();

    let ops = vec![
        BatchOperation::CrdtPnCounterIncrement {
            space: "analytics".into(),
            id: "pageviews".into(),
            actor_id: "agent-1".into(),
            amount: 50,
        },
        BatchOperation::CrdtOrSetAdd {
            space: "tags".into(),
            id: "article-1".into(),
            actor_id: "agent-1".into(),
            element: "database".into(),
        },
        BatchOperation::CrdtJsonSetField {
            space: "articles".into(),
            id: "art-1".into(),
            actor_id: "agent-1".into(),
            path: "/title".into(),
            value: json!("CRDTs in Storage"),
            timestamp_ms: None,
        },
    ];

    let results = db.batch(&ops).expect("batch execution");
    assert_eq!(results.len(), 3);

    // Verify counter
    assert_eq!(
        db.crdt()
            .counter("analytics", "pageviews")
            .get()
            .expect("counter"),
        50
    );
    // Verify set
    assert!(db
        .crdt()
        .set("tags", "article-1")
        .contains("database")
        .expect("set"));
    // Verify doc
    let doc = db
        .crdt()
        .doc("articles", "art-1")
        .get()
        .expect("doc")
        .expect("exists");
    assert_eq!(doc["title"], "CRDTs in Storage");

    // Verify CDC events were dispatched
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    assert_eq!(events.len(), 3);

    match &events[0] {
        lilia_core::DatabaseEvent::CrdtCounterMutated { space, id, value } => {
            assert_eq!(space, "analytics");
            assert_eq!(id, "pageviews");
            assert_eq!(*value, 50);
        }
        other => panic!("expected CrdtCounterMutated, got {other:?}"),
    }

    match &events[1] {
        lilia_core::DatabaseEvent::CrdtSetMutated {
            space,
            id,
            element,
            deleted,
        } => {
            assert_eq!(space, "tags");
            assert_eq!(id, "article-1");
            assert_eq!(element, "database");
            assert!(!deleted);
        }
        other => panic!("expected CrdtSetMutated, got {other:?}"),
    }

    match &events[2] {
        lilia_core::DatabaseEvent::CrdtJsonMutated { space, id, version } => {
            assert_eq!(space, "articles");
            assert_eq!(id, "art-1");
            assert_eq!(*version, Some(1));
        }
        other => panic!("expected CrdtJsonMutated, got {other:?}"),
    }
}
