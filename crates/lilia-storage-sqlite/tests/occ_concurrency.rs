use std::sync::Arc;

use lilia_core::{ErrorCode, Result};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

#[test]
fn test_occ_basic_commit_and_ryow() -> Result<()> {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("occ_basic.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path))?;

    // 1. Open active OCC transaction without database write locks
    let mut tx = db.occ_transaction()?;

    // Write a JSON document
    tx.json().put(
        "users",
        "alice",
        &json!({ "role": "member", "points": 100 }),
    )?;

    // Verify Read-Your-Own-Writes (RYOW) inside transaction
    let doc = tx
        .json()
        .get("users", "alice")?
        .expect("alice in workspace");
    assert_eq!(doc.value()["role"], "member");
    assert_eq!(doc.value()["points"], 100);

    // Overwrite inside the same workspace
    tx.json()
        .put("users", "alice", &json!({ "role": "admin", "points": 500 }))?;
    let doc_updated = tx
        .json()
        .get("users", "alice")?
        .expect("alice updated in workspace");
    assert_eq!(doc_updated.value()["role"], "admin");
    assert_eq!(doc_updated.value()["points"], 500);

    // Write a KV entry
    tx.kv().set("config", b"timeout", b"30s")?;
    let kv_val = tx
        .kv()
        .get("config", b"timeout")?
        .expect("timeout in workspace");
    assert_eq!(kv_val.value, b"30s");

    // Outside database must not see uncommitted changes yet (Isolation)
    assert!(db.json_get("users", "alice")?.is_none());
    assert!(db.kv_get("config", b"timeout")?.is_none());

    // 2. Commit transaction
    tx.commit()?;

    // After commit, mutations must be physically persisted in SQLite
    let persisted_doc = db.json_get("users", "alice")?.expect("persisted alice");
    assert_eq!(persisted_doc.value()["role"], "admin");
    assert_eq!(persisted_doc.value()["points"], 500);

    let persisted_kv = db
        .kv_get("config", b"timeout")?
        .expect("persisted kv timeout");
    assert_eq!(persisted_kv.value, b"30s");

    Ok(())
}

#[test]
fn test_occ_concurrent_non_conflicting_writes() -> Result<()> {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("occ_concurrent.lilia");
    let db = Arc::new(Database::open(DatabaseOptions::durable(&db_path))?);

    const THREAD_COUNT: usize = 50;
    let mut handles = Vec::with_capacity(THREAD_COUNT);

    let barrier = Arc::new(std::sync::Barrier::new(THREAD_COUNT));

    for i in 0..THREAD_COUNT {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || -> Result<()> {
            // Synchronize all threads so they begin and commit simultaneously
            barrier.wait();

            let mut tx = db.occ_transaction()?;
            let doc_id = format!("doc_{i}");
            let key = format!("key_{i}").into_bytes();

            tx.json().put(
                "items",
                &doc_id,
                &json!({ "worker": i, "status": "active" }),
            )?;
            tx.kv()
                .set("worker_keys", &key, format!("val_{i}").as_bytes())?;

            tx.commit()?;
            Ok(())
        }));
    }

    for h in handles {
        h.join().expect("thread join")?;
    }

    // Verify all 50 writes are present in SQLite with zero data loss or contention
    for i in 0..THREAD_COUNT {
        let doc_id = format!("doc_{i}");
        let key = format!("key_{i}").into_bytes();

        let doc = db.json_get("items", &doc_id)?.expect("doc exists");
        assert_eq!(doc.value()["worker"], i);

        let kv = db.kv_get("worker_keys", &key)?.expect("kv exists");
        assert_eq!(kv.value, format!("val_{i}").as_bytes());
    }

    let stats = db.occ_stats();
    assert_eq!(stats.transactions_committed, THREAD_COUNT as u64);
    assert_eq!(stats.conflicts_detected, 0);

    // Group commits must have merged transactions together, saving disk syncs!
    assert!(
        stats.group_commits_executed < THREAD_COUNT as u64,
        "group commits ({}) should be significantly less than transactions ({})",
        stats.group_commits_executed,
        THREAD_COUNT
    );

    Ok(())
}

#[test]
fn test_occ_conflict_detection() -> Result<()> {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("occ_conflict.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path))?;

    // Seed initial document
    db.with_occ_transaction(|tx| {
        tx.json()
            .put("inventory", "item_1", &json!({ "stock": 100 }))?;
        Ok(())
    })?;

    // Open two concurrent transactions T1 and T2
    let mut t1 = db.occ_transaction()?;
    let mut t2 = db.occ_transaction()?;

    // Both read the same item_1
    let doc1 = t1.json().get("inventory", "item_1")?.expect("t1 read");
    let doc2 = t2.json().get("inventory", "item_1")?.expect("t2 read");
    assert_eq!(doc1.value()["stock"], 100);
    assert_eq!(doc2.value()["stock"], 100);

    // T1 modifies item_1 and commits successfully
    t1.json()
        .put("inventory", "item_1", &json!({ "stock": 90 }))?;
    t1.commit()?;

    // T2 attempts to modify item_1 based on its stale read
    t2.json()
        .put("inventory", "item_1", &json!({ "stock": 80 }))?;
    let err = t2.commit().expect_err("T2 must fail due to OCC conflict");

    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(err.retryable, "OCC conflict error must be retryable");

    // Verify stock remains 90 (T1's commit was preserved)
    let final_doc = db.json_get("inventory", "item_1")?.expect("final doc");
    assert_eq!(final_doc.value()["stock"], 90);

    let stats = db.occ_stats();
    assert_eq!(stats.conflicts_detected, 1);

    Ok(())
}

#[test]
fn test_occ_automatic_retry() -> Result<()> {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("occ_retry.lilia");
    let db = Arc::new(Database::open(DatabaseOptions::durable(&db_path))?);

    // Seed shared counter in KV
    db.with_occ_transaction(|tx| {
        tx.kv()
            .set("stats", b"counter", 0usize.to_string().as_bytes())?;
        Ok(())
    })?;

    const WORKERS: usize = 20;
    const INCREMENTS_PER_WORKER: usize = 5;
    let mut handles = Vec::with_capacity(WORKERS);

    for _ in 0..WORKERS {
        let db = Arc::clone(&db);
        handles.push(std::thread::spawn(move || -> Result<()> {
            for _ in 0..INCREMENTS_PER_WORKER {
                // with_occ_retry automatically retries if another worker committed in between
                db.with_occ_retry(100, |tx| {
                    let entry = tx.kv().get("stats", b"counter")?.expect("counter exists");
                    let val_str = std::str::from_utf8(&entry.value).unwrap();
                    let current: usize = val_str.parse().unwrap();
                    let next = current + 1;
                    tx.kv()
                        .set("stats", b"counter", next.to_string().as_bytes())?;
                    Ok(())
                })?;
            }
            Ok(())
        }));
    }

    for h in handles {
        h.join().expect("thread join")?;
    }

    // Verify the exact expected counter value after concurrent retried increments
    let final_entry = db.kv_get("stats", b"counter")?.expect("counter entry");
    let final_count: usize = std::str::from_utf8(&final_entry.value)
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        final_count,
        WORKERS * INCREMENTS_PER_WORKER,
        "all increments must be accounted for without lost updates"
    );

    Ok(())
}

#[test]
fn test_occ_savepoint_rollback_in_memory() -> Result<()> {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("occ_savepoint.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path))?;

    let mut tx = db.occ_transaction()?;

    // 1. Write document A outside savepoint
    tx.json()
        .put("docs", "doc_a", &json!({ "title": "Base Document" }))?;

    // 2. Open speculative savepoint
    let sp_res: Result<()> = tx.spans_savepoint("speculation", |sub_tx| {
        // Inside savepoint: write document B and modify document A
        sub_tx
            .json()
            .put("docs", "doc_b", &json!({ "title": "Speculative Document" }))?;
        sub_tx.json().put(
            "docs",
            "doc_a",
            &json!({ "title": "Modified in Savepoint" }),
        )?;

        assert_eq!(
            sub_tx.json().get("docs", "doc_a")?.unwrap().value()["title"],
            "Modified in Savepoint"
        );
        assert!(sub_tx.json().get("docs", "doc_b")?.is_some());

        // Fail savepoint to trigger in-memory rollback
        Err(lilia_core::LiliaError::new(
            ErrorCode::InvalidInput,
            "simulated business rule rejection",
            false,
        ))
    });

    assert!(sp_res.is_err());

    // 3. Verify workspace was restored to pre-savepoint state in RAM
    let doc_a = tx.json().get("docs", "doc_a")?.expect("doc_a restored");
    assert_eq!(doc_a.value()["title"], "Base Document");
    assert!(
        tx.json().get("docs", "doc_b")?.is_none(),
        "doc_b should not exist after savepoint rollback"
    );

    // Commit transaction
    tx.commit()?;

    // Verify persistence
    assert_eq!(
        db.json_get("docs", "doc_a")?.unwrap().value()["title"],
        "Base Document"
    );
    assert!(db.json_get("docs", "doc_b")?.is_none());

    Ok(())
}

#[test]
fn test_occ_graph_operations_and_ryow() -> Result<()> {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("occ_graph.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path))?;

    let mut tx = db.occ_transaction()?;

    // Add nodes and edges
    tx.graph()
        .set_node("user_1", "Person", &json!({ "name": "Alice" }))?;
    tx.graph()
        .set_node("user_2", "Person", &json!({ "name": "Bob" }))?;
    tx.graph().add_edge("user_1", "user_2", "FRIEND")?;

    // Verify RYOW for edge existence
    assert!(tx.graph().has_edge("user_1", "user_2", "FRIEND")?);
    assert!(!tx.graph().has_edge("user_2", "user_1", "FRIEND")?);

    tx.commit()?;

    // Verify persistence in SQLite
    assert!(db.graph_has_edge("default", "user_1", "user_2", "FRIEND")?);
    assert!(!db.graph_has_edge("default", "user_2", "user_1", "FRIEND")?);

    Ok(())
}

#[test]
fn test_occ_massive_throughput_and_group_commit_ratio() -> Result<()> {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("occ_throughput.lilia");
    let db = Arc::new(Database::open(DatabaseOptions::durable(&db_path))?);

    const THREADS: usize = 20;
    const TX_PER_THREAD: usize = 10;
    const TOTAL_TX: usize = THREADS * TX_PER_THREAD;

    let barrier = Arc::new(std::sync::Barrier::new(THREADS));
    let mut handles = Vec::with_capacity(THREADS);

    let start = std::time::Instant::now();

    for t in 0..THREADS {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || -> Result<()> {
            barrier.wait();
            for i in 0..TX_PER_THREAD {
                let doc_id = format!("doc_{t}_{i}");
                let mut tx = db.occ_transaction()?;
                tx.json().put(
                    "telemetry",
                    &doc_id,
                    &json!({ "thread": t, "tx": i, "payload": "speed_test" }),
                )?;
                tx.commit()?;
            }
            Ok(())
        }));
    }

    for h in handles {
        h.join().expect("thread join")?;
    }

    let elapsed = start.elapsed();
    let stats = db.occ_stats();

    assert_eq!(stats.transactions_committed, TOTAL_TX as u64);
    assert_eq!(stats.conflicts_detected, 0);

    // Group commits executed should be a fraction of the total transactions
    let ratio = (stats.transactions_committed as f64) / (stats.group_commits_executed as f64);
    println!(
        "Total transactions: {}, Group commits: {}, Batching ratio: {:.2}x, Elapsed: {:?}",
        stats.transactions_committed, stats.group_commits_executed, ratio, elapsed
    );

    assert!(
        stats.group_commits_executed < (TOTAL_TX as u64 / 2),
        "Group commits ({}) should be much smaller than total transactions ({})",
        stats.group_commits_executed,
        TOTAL_TX
    );

    Ok(())
}
