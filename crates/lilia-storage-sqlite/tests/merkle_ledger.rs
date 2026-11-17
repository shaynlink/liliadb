use std::fs;
use std::path::PathBuf;

use lilia_core::{BatchOperation, ErrorCode};
use lilia_sqlite_native::NativeConnection;
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

fn temp_db_path() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger_test.lilia");
    (dir, path)
}

#[test]
fn test_merkle_dag_commit_chaining() {
    let (_dir, path) = temp_db_path();
    let db = Database::open(DatabaseOptions::durable(&path)).expect("open db");

    // Initially, no commits
    assert_eq!(db.ledger_root(), [0u8; 32]);
    assert!(db.ledger_latest_commit().unwrap().is_none());

    // Commit 1: Insert document
    db.batch(&[BatchOperation::JsonPut {
        space: "patients".into(),
        id: "p1".into(),
        value: json!({ "name": "Alice", "status": "active" }),
        if_version: None,
    }])
    .expect("batch 1");

    let root1 = db.ledger_root();
    assert_ne!(root1, [0u8; 32]);

    let commit1 = db.ledger_latest_commit().unwrap().expect("commit 1");
    assert_eq!(commit1.commit_seq, 1);
    assert_eq!(commit1.parent_hash, [0u8; 32]);
    assert_eq!(commit1.operation_count, 1);
    assert_eq!(commit1.merkle_root, root1);

    // Commit 2: Insert second document and a KV pair
    db.batch(&[
        BatchOperation::JsonPut {
            space: "patients".into(),
            id: "p2".into(),
            value: json!({ "name": "Bob", "status": "active" }),
            if_version: None,
        },
        BatchOperation::KvSet {
            namespace: "audit".into(),
            key: b"login:p2".to_vec(),
            value: b"ip:127.0.0.1".to_vec(),
            if_version: None,
            expires_at_ms: None,
        },
    ])
    .expect("batch 2");

    let root2 = db.ledger_root();
    assert_ne!(root2, root1);

    let commit2 = db.ledger_latest_commit().unwrap().expect("commit 2");
    assert_eq!(commit2.commit_seq, 2);
    assert_eq!(commit2.parent_hash, commit1.commit_hash);
    assert_eq!(commit2.operation_count, 2);
    assert_eq!(commit2.merkle_root, root2);

    // Commit 3: Update document 1
    db.batch(&[BatchOperation::JsonPut {
        space: "patients".into(),
        id: "p1".into(),
        value: json!({ "name": "Alice Smith", "status": "discharged" }),
        if_version: None,
    }])
    .expect("batch 3");

    let root3 = db.ledger_root();
    assert_ne!(root3, root2);

    let commit3 = db.ledger_latest_commit().unwrap().expect("commit 3");
    assert_eq!(commit3.commit_seq, 3);
    assert_eq!(commit3.parent_hash, commit2.commit_hash);

    // Close and reopen database -> verification must pass and root must be preserved
    drop(db);
    let reopened = Database::open(DatabaseOptions::durable(&path)).expect("reopen db");
    assert_eq!(reopened.ledger_root(), root3);

    let report = reopened.verify_ledger().expect("verify report");
    assert!(report.is_valid);
    assert_eq!(report.commit_count, 3);
    assert!(report.leaves_verified >= 3);
}

#[test]
fn test_inclusion_proof_export_and_verification() {
    let (_dir, path) = temp_db_path();
    let db = Database::open(DatabaseOptions::durable(&path)).expect("open db");

    db.batch(&[
        BatchOperation::JsonPut {
            space: "records".into(),
            id: "rec_100".into(),
            value: json!({ "amount": 1500, "currency": "EUR" }),
            if_version: None,
        },
        BatchOperation::KvSet {
            namespace: "tokens".into(),
            key: b"session_alpha".to_vec(),
            value: b"valid_token_data".to_vec(),
            if_version: None,
            expires_at_ms: None,
        },
    ])
    .expect("batch 1");

    let root1 = db.ledger_root();

    db.batch(&[BatchOperation::JsonPut {
        space: "records".into(),
        id: "rec_200".into(),
        value: json!({ "amount": 3200, "currency": "USD" }),
        if_version: None,
    }])
    .expect("batch 2");

    let current_root = db.ledger_root();

    // 1. Export Inclusion Proof for JSON document 'records/rec_100'
    let doc_proof = db
        .ledger_prove_doc("records", "rec_100", None)
        .expect("prove doc");
    assert_eq!(doc_proof.commit_seq, 1);
    assert_eq!(doc_proof.merkle_root, current_root);

    // Standalone mathematical verification against the current database root
    assert!(doc_proof.verify(&current_root));

    // Verification against obsolete root1 must fail
    assert!(!doc_proof.verify(&root1));

    // Verification with tampered leaf must fail
    let mut bad_doc_proof = doc_proof.clone();
    bad_doc_proof.leaf_hash[0] ^= 0xFF;
    assert!(!bad_doc_proof.verify(&current_root));

    // 2. Export Inclusion Proof for KV entry 'tokens/session_alpha'
    let kv_proof = db
        .ledger_prove_kv("tokens", b"session_alpha", None)
        .expect("prove kv");
    assert_eq!(kv_proof.commit_seq, 1);
    assert!(kv_proof.verify(&current_root));

    // 3. Export Inclusion Proof for Commit 2
    let commit_proof = db.ledger_prove_commit(2).expect("prove commit");
    assert_eq!(commit_proof.commit_seq, 2);
    assert!(commit_proof.verify(&current_root));
}

#[test]
fn test_tamper_detection_sqlite_cli_manipulation() {
    let (_dir, path) = temp_db_path();
    let db = Database::open(DatabaseOptions::durable(&path)).expect("open db");

    db.batch(&[BatchOperation::JsonPut {
        space: "accounts".into(),
        id: "acc_42".into(),
        value: json!({ "balance": 100 }),
        if_version: None,
    }])
    .expect("batch");

    drop(db);

    // Simulate external malicious modification directly via SQLite connection (bypassing LiliaDB)
    let raw_conn = NativeConnection::open_with_config(&path, "FULL", 0, 5000, -8192)
        .expect("open raw sqlite");
    raw_conn
        .exec("UPDATE _lilia_json SET value = '{\"balance\": 999999}' WHERE id = 'acc_42';")
        .expect("update json");
    drop(raw_conn);

    // Reopening LiliaDB in default Strict mode MUST detect the tampering and abort!
    let open_res = Database::open(DatabaseOptions::durable(&path));
    assert!(
        open_res.is_err(),
        "Database::open must fail when data has been tampered with"
    );
    let err = open_res.err().unwrap();
    assert_eq!(err.code, ErrorCode::TamperDetected);
    assert!(
        err.message.contains("Tamper detected"),
        "error message should indicate tamper detection: {err}"
    );
}

#[test]
fn test_tamper_detection_ledger_chain_rewrite() {
    let (_dir, path) = temp_db_path();
    let db = Database::open(DatabaseOptions::durable(&path)).expect("open db");

    for i in 1..=3 {
        db.batch(&[BatchOperation::JsonPut {
            space: "data".into(),
            id: format!("doc_{i}"),
            value: json!({ "val": i }),
            if_version: None,
        }])
        .expect("batch");
    }

    drop(db);

    // Alter the parent_hash of commit 2 in _lilia_ledger
    let raw_conn = NativeConnection::open_with_config(&path, "FULL", 0, 5000, -8192)
        .expect("open raw sqlite");
    raw_conn
        .exec("UPDATE _lilia_ledger SET parent_hash = X'0000000000000000000000000000000000000000000000000000000000000000' WHERE commit_seq = 2;")
        .expect("tamper ledger");
    drop(raw_conn);

    // Reopen MUST fail with TamperDetected!
    let open_res = Database::open(DatabaseOptions::durable(&path));
    assert!(open_res.is_err());
    let err = open_res.err().unwrap();
    assert_eq!(err.code, ErrorCode::TamperDetected);
    assert!(err.message.contains("Tamper detected in Merkle DAG"));
}

#[test]
fn test_tamper_detection_hex_editing() {
    let (_dir, path) = temp_db_path();
    let db = Database::open(DatabaseOptions::durable(&path)).expect("open db");

    db.batch(&[BatchOperation::JsonPut {
        space: "secure".into(),
        id: "doc_sensitive".into(),
        value: json!({ "classification": "TOP_SECRET_CLEARANCE_REQUIRED" }),
        if_version: None,
    }])
    .expect("batch");

    drop(db);

    // Read the database file as binary bytes
    let mut file_bytes = fs::read(&path).expect("read file");

    // Locate the pattern "TOP_SECRET" in the raw database file and modify it (hex-editing simulation)
    let pattern = b"TOP_SECRET";
    let pos = file_bytes
        .windows(pattern.len())
        .position(|window| window == pattern)
        .expect("pattern must be present in raw database file");

    // Invert the first byte of the pattern
    file_bytes[pos] = b'X';

    // Write back the tampered bytes to the file
    fs::write(&path, &file_bytes).expect("write tampered file");

    // Reopening MUST detect the alteration and reject opening
    let open_res = Database::open(DatabaseOptions::durable(&path));
    assert!(
        open_res.is_err(),
        "Database::open must fail when hex-editing altered the file"
    );
    let err = open_res.err().unwrap();
    assert!(
        err.code == ErrorCode::TamperDetected || err.code == ErrorCode::Storage,
        "unexpected error code: {:?}",
        err.code
    );
}
