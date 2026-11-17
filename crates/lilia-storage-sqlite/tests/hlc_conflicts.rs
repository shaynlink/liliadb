use lilia_core::{CausalRelation, ConflictResolutionStrategy, DatabaseEvent, Hlc};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

fn create_test_db(name: &str, node_id: &str) -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::open(DatabaseOptions::durable(dir.path().join(name)).with_node_id(node_id))
        .expect("open db");
    (dir, db)
}

#[test]
#[allow(clippy::too_many_lines)]
fn test_hlc_offline_concurrency_detection_and_three_way_merge() {
    let (_dir1, db1) = create_test_db("node_a.lilia", "device-a");
    let mut rx = db1.subscribe();

    // 1. Initial common base state (t=50, counter=0)
    let base_hlc = Hlc::new(50, 0, "seed-node");
    let initial_doc = json!({
        "user": {
            "name": "Alex",
            "email": "alex@example.com",
            "role": "member"
        }
    });

    let res_init = db1
        .json_put_hlc(
            "users",
            "user-1",
            initial_doc,
            Some(base_hlc.clone()),
            None,
            ConflictResolutionStrategy::LastWriteWins,
        )
        .expect("initial put");
    assert!(!res_init.conflict_detected);
    assert_eq!(res_init.hlc, base_hlc);

    // Verify initial HLC record
    let rec = db1
        .json_get_hlc("users", "user-1")
        .expect("get hlc")
        .expect("record exists");
    assert_eq!(rec.hlc, base_hlc);
    assert_eq!(rec.node_id, "seed-node");

    // 2. Offline Device A: modifies user.name to "Alice" at [HLC: 100.1@device-a], parent=50.0
    let hlc_a = Hlc::new(100, 1, "device-a");
    let doc_a = json!({
        "user": {
            "name": "Alice",
            "email": "alex@example.com",
            "role": "member"
        }
    });
    let res_a = db1
        .json_put_hlc(
            "users",
            "user-1",
            doc_a,
            Some(hlc_a.clone()),
            Some(base_hlc.clone()),
            ConflictResolutionStrategy::LastWriteWins,
        )
        .expect("put device a");
    assert!(!res_a.conflict_detected);
    assert_eq!(res_a.hlc, hlc_a);

    // 3. Offline Device B: independently modifies user.name to "Alicia" and role to "admin"
    // at [HLC: 100.2@device-b], parent=50.0
    let hlc_b = Hlc::new(100, 2, "device-b");
    let doc_b = json!({
        "user": {
            "name": "Alicia",
            "email": "alex@example.com",
            "role": "admin"
        }
    });

    // 4. Sync: Device B's mutation arrives at db1.
    // Both branched from base_hlc=50.0 independently -> Concurrency Detected (100.1 ∥ 100.2)!
    let res_sync = db1
        .json_put_hlc(
            "users",
            "user-1",
            doc_b,
            Some(hlc_b.clone()),
            Some(base_hlc.clone()),
            ConflictResolutionStrategy::ThreeWayMerge,
        )
        .expect("sync put device b");

    // Concurrency detection assertions
    assert!(res_sync.conflict_detected, "conflict must be detected!");
    assert_eq!(res_sync.causal_relation, CausalRelation::Concurrent);
    assert!(res_sync.conflict_id.is_some());

    // Verify 3-Way Merge result:
    // - "name" had a collision ("Alice" vs "Alicia"): resolved to "Alicia" by HLC total order (100.2 > 100.1)
    // - "role" was changed only by B: merged cleanly to "admin"
    // - "email" was untouched: preserved as "alex@example.com"
    let merged_doc = db1
        .json_get("users", "user-1")
        .expect("get")
        .expect("exists")
        .value();
    assert_eq!(merged_doc["user"]["name"], "Alicia");
    assert_eq!(merged_doc["user"]["email"], "alex@example.com");
    assert_eq!(merged_doc["user"]["role"], "admin");

    // 5. Verify the conflict was logged in _lilia_conflicts table
    let conflicts = db1.list_conflicts(Some("users")).expect("list conflicts");
    assert_eq!(conflicts.len(), 1);
    let conflict = &conflicts[0];
    assert_eq!(conflict.doc_id, "user-1");
    assert_eq!(conflict.base_hlc, Some(base_hlc));
    assert_eq!(conflict.ours_hlc, hlc_a);
    assert_eq!(conflict.theirs_hlc, hlc_b);
    assert_eq!(conflict.ours_value["user"]["name"], "Alice");
    assert_eq!(conflict.theirs_value["user"]["name"], "Alicia");
    assert_eq!(
        conflict.resolution_strategy,
        ConflictResolutionStrategy::ThreeWayMerge
    );

    // 6. Verify through SQLite View lilia_conflicts
    let view_res = db1
        .sql_query(
            "SELECT doc_id, ours_hlc, theirs_hlc, status FROM lilia_conflicts WHERE space = ?1;",
            &[json!("users")],
        )
        .expect("query view");
    assert_eq!(view_res.rows.len(), 1);
    assert_eq!(view_res.rows[0][0], json!("user-1"));
    assert_eq!(view_res.rows[0][1], json!(hlc_a.to_string()));
    assert_eq!(view_res.rows[0][2], json!(hlc_b.to_string()));
    assert_eq!(view_res.rows[0][3], json!("resolved"));

    // 7. Verify CDC event stream emitted DatabaseEvent::ConflictDetected
    let mut event_found = false;
    while let Ok(evt) = rx.try_recv() {
        if let DatabaseEvent::ConflictDetected {
            space,
            id,
            ours_hlc,
            theirs_hlc,
            ..
        } = evt
        {
            assert_eq!(space, "users");
            assert_eq!(id, "user-1");
            assert_eq!(ours_hlc, hlc_a.to_string());
            assert_eq!(theirs_hlc, hlc_b.to_string());
            event_found = true;
            break;
        }
    }
    assert!(event_found, "ConflictDetected event must be broadcast");
}

#[test]
fn test_hlc_guarantees_causal_order_with_lagging_physical_clock() {
    let (_dir1, db1) = create_test_db("db1.lilia", "device-1");
    let (_dir2, db2) = create_test_db("db2.lilia", "device-2");

    // Device 1 creates an event with physical timestamp 500_000 ms
    let hlc_1 = Hlc::new(500_000, 0, "device-1");
    db1.json_put_hlc(
        "notes",
        "note-1",
        json!({"title": "Original Note"}),
        Some(hlc_1.clone()),
        None,
        ConflictResolutionStrategy::LastWriteWins,
    )
    .expect("put db1");

    // Device 2's physical clock is 10 minutes (600_000 ms) BEHIND!
    // But Device 2 observes HLC from Device 1
    let local_observed = db2.observe_hlc(&hlc_1).expect("observe hlc");
    assert!(
        local_observed > hlc_1,
        "Observing HLC must advance local clock beyond remote HLC"
    );

    // Device 2 performs a subsequent edit observing Device 1's write
    let next_hlc_2 = db2.next_hlc();
    assert!(
        next_hlc_2 > hlc_1,
        "Mathematical guarantee: HLC(B) > HLC(A) even if clock is 10 min behind"
    );

    // When applied, it is sequential (parent=hlc_1), NO conflict detected!
    let res = db1
        .json_put_hlc(
            "notes",
            "note-1",
            json!({"title": "Updated by Device 2"}),
            Some(next_hlc_2.clone()),
            Some(hlc_1.clone()),
            ConflictResolutionStrategy::LastWriteWins,
        )
        .expect("sequential put");

    assert!(!res.conflict_detected, "Sequential write must not conflict");
    assert_eq!(res.causal_relation, CausalRelation::Descendant);

    let doc = db1.json_get("notes", "note-1").unwrap().unwrap().value();
    assert_eq!(doc["title"], "Updated by Device 2");
}

#[test]
fn test_hlc_last_write_wins_resolution_and_conflict_audit() {
    let (_dir, db) = create_test_db("lww.lilia", "local-node");

    let base_hlc = Hlc::new(100, 0, "origin");
    db.json_put_hlc(
        "items",
        "item-1",
        json!({"name": "Base"}),
        Some(base_hlc.clone()),
        None,
        ConflictResolutionStrategy::LastWriteWins,
    )
    .unwrap();

    // Local mutation
    let hlc_local = Hlc::new(200, 1, "local-node");
    db.json_put_hlc(
        "items",
        "item-1",
        json!({"name": "Local Version"}),
        Some(hlc_local.clone()),
        Some(base_hlc.clone()),
        ConflictResolutionStrategy::LastWriteWins,
    )
    .unwrap();

    // Remote concurrent mutation with higher HLC
    let hlc_remote = Hlc::new(200, 2, "remote-node");
    let res = db
        .json_put_hlc(
            "items",
            "item-1",
            json!({"name": "Remote Winner"}),
            Some(hlc_remote.clone()),
            Some(base_hlc.clone()),
            ConflictResolutionStrategy::LastWriteWins,
        )
        .unwrap();

    assert!(res.conflict_detected);
    // Remote had higher HLC, so remote won
    let doc = db.json_get("items", "item-1").unwrap().unwrap().value();
    assert_eq!(doc["name"], "Remote Winner");

    // But the conflict is fully recorded!
    let conflicts = db.list_conflicts(Some("items")).unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(
        conflicts[0].resolved_value,
        Some(json!({"name": "Remote Winner"}))
    );
}

#[test]
fn test_hlc_manual_conflict_resolution() {
    let (_dir, db) = create_test_db("manual.lilia", "local-node");

    let base_hlc = Hlc::new(10, 0, "root");
    db.json_put_hlc(
        "profile",
        "p-1",
        json!({"status": "initial"}),
        Some(base_hlc.clone()),
        None,
        ConflictResolutionStrategy::LastWriteWins,
    )
    .unwrap();

    let hlc_a = Hlc::new(20, 1, "node-a");
    db.json_put_hlc(
        "profile",
        "p-1",
        json!({"status": "active"}),
        Some(hlc_a.clone()),
        Some(base_hlc.clone()),
        ConflictResolutionStrategy::LastWriteWins,
    )
    .unwrap();

    // Concurrent mutation with Manual strategy
    let hlc_b = Hlc::new(20, 2, "node-b");
    let res = db
        .json_put_hlc(
            "profile",
            "p-1",
            json!({"status": "suspended"}),
            Some(hlc_b.clone()),
            Some(base_hlc.clone()),
            ConflictResolutionStrategy::Manual,
        )
        .unwrap();

    assert!(res.conflict_detected);
    let conflict_id = res.conflict_id.expect("conflict id");

    // Manually resolve conflict
    db.resolve_conflict(conflict_id, json!({"status": "manually_resolved"}))
        .expect("manual resolve");

    let doc = db.json_get("profile", "p-1").unwrap().unwrap().value();
    assert_eq!(doc["status"], "manually_resolved");
}
