use lilia_core::{ErrorCode, LiliaError, Result, StorageTier};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct User {
    id: String,
    name: String,
    role: String,
}

fn external_service_validation(should_succeed: bool) -> Result<()> {
    if should_succeed {
        Ok(())
    } else {
        Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "external enrichment validation failed: simulated third-party timeout",
            false,
        ))
    }
}

#[test]
fn test_speculative_savepoint_rollback_user_example() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("savepoint_user_example.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).expect("open database");

    let user = User {
        id: "u1".into(),
        name: "Alice".into(),
        role: "admin".into(),
    };

    // Begin active transaction
    let mut tx = db.transaction().expect("begin transaction");

    // 1. Write user in parent transaction
    tx.json()
        .put("users", "u1", &user)
        .expect("put user u1");

    // Verify user is visible within transaction before savepoint
    let fetched = tx.json().get("users", "u1").expect("get u1");
    assert!(fetched.is_some(), "user u1 must be visible in active transaction");

    // 2. Speculative attempt inside a named savepoint
    let savepoint_result = tx.spans_savepoint("external_enrichment", |sub_tx| {
        // Nested speculation attempts to add an edge
        sub_tx
            .graph()
            .add_edge("u1", "group_a", "MEMBER")
            .expect("add edge");

        // Verify edge is visible inside speculative savepoint
        assert!(
            sub_tx
                .graph()
                .has_edge("u1", "group_a", "MEMBER")
                .expect("has edge"),
            "edge must be visible inside speculative savepoint"
        );

        // Validation fails!
        external_service_validation(false)?;
        Ok(())
    });

    assert!(
        savepoint_result.is_err(),
        "speculative savepoint must return error"
    );

    // 3. Verify only the speculative branch was undone:
    // - Graph edge "u1" -> "group_a" was rolled back in O(1)
    // - The put("users", "u1") is preserved!
    assert!(
        !tx.graph()
            .has_edge("u1", "group_a", "MEMBER")
            .expect("check edge"),
        "speculative graph edge must be rolled back"
    );

    let user_still_there = tx.json().get("users", "u1").expect("get u1 after rollback");
    assert!(
        user_still_there.is_some(),
        "user u1 must still be preserved in outer transaction"
    );

    // 4. Commit parent transaction
    tx.commit().expect("commit transaction");

    // 5. Verify persistent DB state
    let doc_in_db = db.json_get("users", "u1").expect("query db").expect("doc exists");
    assert_eq!(doc_in_db.id, "u1");

    let edges = db
        .graph_list_edges("default", Some("u1"), Some("group_a"), Some("MEMBER"), 10)
        .expect("list edges");
    assert!(
        edges.is_empty(),
        "aborted speculative edge must not exist in database"
    );
}

#[test]
fn test_speculative_savepoint_success_commits_all() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("savepoint_success.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).expect("open database");

    let user = User {
        id: "u2".into(),
        name: "Bob".into(),
        role: "developer".into(),
    };

    let mut tx = db.transaction().expect("begin transaction");
    tx.json().put("users", "u2", &user).expect("put user u2");

    let savepoint_result = tx.spans_savepoint("external_enrichment", |sub_tx| {
        sub_tx
            .graph()
            .add_edge("u2", "group_engineers", "MEMBER")
            .expect("add edge");
        external_service_validation(true)?;
        Ok(())
    });

    assert!(savepoint_result.is_ok(), "savepoint must succeed");
    tx.commit().expect("commit transaction");

    // Both user and edge must be committed
    assert!(db.json_get("users", "u2").expect("query user").is_some());
    let edges = db
        .graph_list_edges(
            "default",
            Some("u2"),
            Some("group_engineers"),
            Some("MEMBER"),
            10,
        )
        .expect("list edges");
    assert_eq!(edges.len(), 1, "committed edge must exist");
}

#[test]
fn test_nested_savepoint_tree_arbitrary_depth() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("nested_tree.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).expect("open database");

    let mut tx = db.transaction().expect("begin transaction");
    tx.json()
        .put("docs", "root_doc", &json!({"level": "root"}))
        .expect("put root");

    // Level 1: Savepoint A
    tx.spans_savepoint("savepoint_a", |tx_a| {
        tx_a.json()
            .put("docs", "doc_a", &json!({"level": "a"}))
            .expect("put a");

        // Level 2: Savepoint B (which will fail and roll back)
        let res_b: Result<()> = tx_a.spans_savepoint("savepoint_b", |tx_b| {
            tx_b.json()
                .put("docs", "doc_b", &json!({"level": "b"}))
                .expect("put b");

            // Level 3: Nested Savepoint C inside B (which succeeds locally into B)
            tx_b.spans_savepoint("savepoint_c", |tx_c| {
                tx_c.json()
                    .put("docs", "doc_c", &json!({"level": "c"}))
                    .expect("put c");
                Ok(())
            })?;

            // B validates that both doc_b and doc_c are present before failing
            assert!(tx_b.json().get("docs", "doc_b")?.is_some());
            assert!(tx_b.json().get("docs", "doc_c")?.is_some());

            // Simulated failure of branch B
            Err(LiliaError::new(
                ErrorCode::Conflict,
                "branch B business invariant violated",
                false,
            ))
        });

        assert!(res_b.is_err(), "branch B must fail");

        // After B failed: doc_b and doc_c are rolled back, but doc_a and root_doc remain!
        assert!(tx_a.json().get("docs", "doc_b")?.is_none());
        assert!(tx_a.json().get("docs", "doc_c")?.is_none());
        assert!(tx_a.json().get("docs", "doc_a")?.is_some());
        assert!(tx_a.json().get("docs", "root_doc")?.is_some());

        // Level 2 alternate branch: Savepoint D (which succeeds)
        tx_a.spans_savepoint("savepoint_d", |tx_d| {
            tx_d.json()
                .put("docs", "doc_d", &json!({"level": "d"}))
                .expect("put d");
            Ok(())
        })?;

        Ok(())
    })
    .expect("savepoint A must succeed");

    tx.commit().expect("commit root transaction");

    // Check final DB state: root_doc, doc_a, and doc_d must exist. doc_b and doc_c must NOT exist.
    assert!(db.json_get("docs", "root_doc").unwrap().is_some());
    assert!(db.json_get("docs", "doc_a").unwrap().is_some());
    assert!(db.json_get("docs", "doc_d").unwrap().is_some());
    assert!(db.json_get("docs", "doc_b").unwrap().is_none());
    assert!(db.json_get("docs", "doc_c").unwrap().is_none());
}

#[test]
fn test_multi_model_subsystems_in_savepoint() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("multi_model_savepoints.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).expect("open database");

    let mut tx = db.transaction().expect("begin tx");

    // Root writes KV and JSON
    tx.kv().set("config", b"app_mode", b"production").unwrap();
    tx.json()
        .put("users", "admin", &json!({"name": "Root Admin"}))
        .unwrap();

    // Speculative block mutating KV, JSON, Graph, FTS
    let _: Result<()> = tx.spans_savepoint("multi_speculation", |sub_tx| {
        sub_tx.kv().set("config", b"temp_flag", b"1").unwrap();
        sub_tx
            .json()
            .put("users", "guest", &json!({"name": "Temporary Guest"}))
            .unwrap();
        sub_tx.graph().add_edge("admin", "guest", "INVITED").unwrap();
        sub_tx.fts().index("users", "guest", "Temporary Guest Account").unwrap();

        // Verify all visible in savepoint
        assert!(sub_tx.kv().get("config", b"temp_flag")?.is_some());
        assert!(sub_tx.json().get("users", "guest")?.is_some());
        assert!(sub_tx.graph().has_edge("admin", "guest", "INVITED")?);

        Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "cancel speculation",
            false,
        ))
    });

    // After rollback: guest and temp_flag must be gone, admin and app_mode preserved
    assert!(tx.kv().get("config", b"temp_flag").unwrap().is_none());
    assert!(tx.json().get("users", "guest").unwrap().is_none());
    assert!(!tx.graph().has_edge("admin", "guest", "INVITED").unwrap());

    assert!(tx.kv().get("config", b"app_mode").unwrap().is_some());
    assert!(tx.json().get("users", "admin").unwrap().is_some());

    tx.commit().expect("commit");

    assert!(db.kv_get("config", b"app_mode").unwrap().is_some());
    assert!(db.kv_get("config", b"temp_flag").unwrap().is_none());
    assert!(db.json_get("users", "admin").unwrap().is_some());
    assert!(db.json_get("users", "guest").unwrap().is_none());
}

#[test]
fn test_transaction_drop_auto_rollback() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("auto_rollback.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).expect("open database");

    {
        let mut tx = db.transaction().expect("begin tx");
        tx.json()
            .put("users", "uncommitted", &json!({"val": 1}))
            .unwrap();
        // Dropped here without tx.commit()
    }

    // Must be rolled back
    assert!(db.json_get("users", "uncommitted").unwrap().is_none());

    // Next transaction can acquire writer cleanly without deadlocks
    let mut tx2 = db.transaction().expect("begin tx2");
    tx2.json()
        .put("users", "committed", &json!({"val": 2}))
        .unwrap();
    tx2.commit().expect("commit tx2");

    assert!(db.json_get("users", "committed").unwrap().is_some());
}

#[test]
fn test_savepoint_panic_safe_catch_and_rollback() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("panic_safety.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).expect("open database");

    let mut tx = db.transaction().expect("begin tx");
    tx.json()
        .put("users", "safe_user", &json!({"status": "active"}))
        .unwrap();

    let panic_res: Result<()> = tx.spans_savepoint("panicking_savepoint", |sub_tx| {
        sub_tx
            .json()
            .put("users", "bad_user", &json!({"status": "crashed"}))
            .unwrap();
        panic!("simulated panic/trap inside guest execution!");
    });

    assert!(panic_res.is_err(), "panic must be intercepted as Err");
    let err_str = panic_res.unwrap_err().to_string();
    assert!(
        err_str.contains("panic/trap"),
        "error must report panic: {err_str}"
    );

    // Savepoint was rolled back, outer transaction is safe
    assert!(tx.json().get("users", "bad_user").unwrap().is_none());
    assert!(tx.json().get("users", "safe_user").unwrap().is_some());

    tx.commit().expect("commit outer transaction");

    assert!(db.json_get("users", "safe_user").unwrap().is_some());
    assert!(db.json_get("users", "bad_user").unwrap().is_none());
}

#[test]
fn test_memory_tier_savepoint_rollback() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("mem_savepoint.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).expect("open database");

    // Configure an explicit StorageTier::Memory collection
    db.collection("mem_cache")
        .tier(StorageTier::Memory)
        .json()
        .expect("configure memory collection");

    let mut tx = db.transaction().expect("begin tx");
    tx.json()
        .put("mem_cache", "k1", &json!({"val": "initial"}))
        .unwrap();

    let _: Result<()> = tx.spans_savepoint("mem_speculation", |sub_tx| {
        sub_tx
            .json()
            .put("mem_cache", "k1", &json!({"val": "overwritten"}))
            .unwrap();
        sub_tx
            .json()
            .put("mem_cache", "k2", &json!({"val": "temp"}))
            .unwrap();
        Err(LiliaError::new(ErrorCode::Storage, "abort", false))
    });

    // After rollback: k2 is gone, k1 has reverted to initial
    assert!(tx.json().get("mem_cache", "k2").unwrap().is_none());
    let k1 = tx.json().get("mem_cache", "k1").unwrap().expect("k1 exists");
    assert_eq!(
        k1.doc.to_json_value(),
        json!({"val": "initial"}),
        "in-memory collection must revert to initial state on savepoint rollback"
    );

    tx.commit().expect("commit");
}

