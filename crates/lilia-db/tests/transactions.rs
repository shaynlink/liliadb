//! Optimistic transactions: snapshot reads, own writes, validation of reads at commit.

mod common;

use std::sync::Arc;

use common::small_options;
use lilia_db::{Database, Error, IndexOptions, Scan};
use lilia_engine::Durability;
use serde_json::{json, Map, Value};

fn open(dir: &std::path::Path, durability: Durability) -> Database {
    Database::open(dir, small_options(durability, false)).unwrap()
}

fn query(value: &Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

#[test]
fn transactions_read_a_snapshot_under_their_own_writes() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), Durability::Performance);
    let s = db.system();
    db.create_index("tasks", "by_state", "$.state", &IndexOptions::default()).unwrap();
    for (id, state) in [("t1", "todo"), ("t2", "done"), ("t3", "todo")] {
        s.json_put("tasks", id, &json!({"state": state})).unwrap();
    }
    s.kv_put("ns", b"b", b"committed").unwrap();

    let tx = s.transaction();
    let reader = s.transaction();
    s.json_put("tasks", "t4", &json!({"state": "todo"})).unwrap();
    s.kv_put("ns", b"c", b"later").unwrap();
    assert!(reader.json_get("tasks", "t4").unwrap().is_none(), "writes after the snapshot are invisible");
    assert!(matches!(reader.commit(), Err(Error::Conflict { .. })), "and a record read as absent then inserted conflicts");

    tx.json_put("tasks", "t2", &json!({"state": "todo"})).unwrap();
    tx.json_delete("tasks", "t1").unwrap();
    tx.json_put("tasks", "t0", &json!({"state": "todo"})).unwrap();
    tx.kv_put("ns", b"a", b"own").unwrap();
    tx.kv_delete("ns", b"b").unwrap();

    let todo: Vec<String> = tx.json_find("tasks", &query(&json!({"state": "todo"})), &Scan::all()).unwrap().into_iter().map(|e| e.id).collect();
    assert_eq!(todo, ["t0", "t2", "t3"], "the index lookup merges the transaction's own writes in id order");
    let after: Vec<String> = tx.json_find("tasks", &query(&json!({"state": "todo"})), &Scan::all().after("t0").limit(1)).unwrap().into_iter().map(|e| e.id).collect();
    assert_eq!(after, ["t2"]);
    let scanned: Vec<String> = tx.json_scan("tasks", &Scan::all()).unwrap().into_iter().map(|e| e.id).collect();
    assert_eq!(scanned, ["t0", "t2", "t3"]);
    let own = tx.kv_get("ns", b"a").unwrap().unwrap();
    assert_eq!((own.value.as_slice(), own.version), (&b"own"[..], 0));
    let keys: Vec<Vec<u8>> = tx.kv_scan("ns", &Scan::all()).unwrap().into_iter().map(|e| e.key).collect();
    assert_eq!(keys, [b"a".to_vec()], "own delete hides b; c is after the snapshot");

    let outcomes = tx.commit().unwrap();
    assert_eq!(outcomes.len(), 5);
    assert!(outcomes[1].deleted);
    let todo: Vec<String> = s.json_find("tasks", &query(&json!({"state": "todo"})), &Scan::all()).unwrap().into_iter().map(|e| e.id).collect();
    assert_eq!(todo, ["t0", "t2", "t3", "t4"]);
    assert_eq!(s.kv_get("ns", b"a").unwrap().unwrap().version, outcomes[3].version.unwrap());
}

#[test]
fn a_changed_read_fails_the_commit_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), Durability::Performance);
    let s = db.system();
    s.kv_put("acct", b"x", b"100").unwrap();

    let tx = s.transaction();
    assert_eq!(tx.kv_get("acct", b"x").unwrap().unwrap().value, b"100");
    tx.kv_put("acct", b"y", b"moved").unwrap();
    s.kv_put("acct", b"x", b"50").unwrap();
    assert!(matches!(tx.commit(), Err(Error::Conflict { .. })));
    assert!(s.kv_get("acct", b"y").unwrap().is_none());

    let absent = s.transaction();
    assert!(absent.json_get("users", "u1").unwrap().is_none());
    absent.kv_put("log", b"saw-no-u1", b"1").unwrap();
    s.json_put("users", "u1", &json!({})).unwrap();
    assert!(matches!(absent.commit(), Err(Error::Conflict { expected: None, .. })), "an insert of a record read as absent conflicts");

    let scanned = s.transaction();
    assert_eq!(scanned.kv_scan("acct", &Scan::all()).unwrap().len(), 1);
    scanned.kv_put("summary", b"n", b"1").unwrap();
    s.kv_put("acct", b"x", b"10").unwrap();
    assert!(matches!(scanned.commit(), Err(Error::Conflict { .. })), "a change to a scanned record conflicts");

    let blind = {
        let tx = s.transaction();
        tx.kv_put("acct", b"x", b"blind").unwrap();
        tx
    };
    s.kv_put("acct", b"x", b"other").unwrap();
    blind.commit().unwrap();
    assert_eq!(s.kv_get("acct", b"x").unwrap().unwrap().value, b"blind", "a write the transaction did not read does not conflict");
}

/// Concurrent read-modify-write transactions with retries never lose an update.
#[test]
fn concurrent_increments_are_serializable() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(open(dir.path(), Durability::Balanced));
    let threads = 8;
    let per_thread = 50;
    let workers: Vec<_> = (0..threads)
        .map(|t| {
            let db = Arc::clone(&db);
            std::thread::spawn(move || {
                let s = db.system();
                let mut conflicts = 0;
                for _ in 0..per_thread {
                    loop {
                        let tx = s.transaction();
                        let current = tx.json_get("counters", "c").unwrap().and_then(|e| e.extract_i64("n")).unwrap_or(0);
                        tx.json_put("counters", "c", &json!({"n": current + 1, "by": t})).unwrap();
                        match tx.commit() {
                            Ok(_) => break,
                            Err(Error::Conflict { .. }) => conflicts += 1,
                            Err(error) => panic!("{error}"),
                        }
                    }
                }
                conflicts
            })
        })
        .collect();
    let conflicts: u32 = workers.into_iter().map(|w| w.join().unwrap()).sum();
    let n = db.system().json_get("counters", "c").unwrap().unwrap().extract_i64("n").unwrap();
    assert_eq!(n, threads * per_thread);
    assert!(conflicts > 0, "the writers contended");
}
