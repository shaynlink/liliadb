//! The ledger: one commit per batch, proofs of commits and records, verification, tampering.

mod common;

use std::sync::Arc;

use common::small_options;
use lilia_db::{Batch, Database, Error, IndexOptions, LedgerVerifyMode, Options, Write};
use lilia_engine::Durability;
use serde_json::json;

fn options(ledger: bool) -> Options {
    small_options(Durability::Performance, ledger)
}

#[test]
fn every_batch_is_a_commit_and_records_have_proofs() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), options(true)).unwrap();
    let s = db.system();
    assert_eq!((db.ledger_len().unwrap(), db.ledger_root().unwrap()), (0, [0; 32]));
    db.create_index("users", "by_city", "$.city", &IndexOptions::default()).unwrap();

    let mut batch = Batch::new();
    batch.kv_put("cfg", "mode", "fast").json_put("users", "u1", &json!({"city": "Lyon"})).unwrap();
    s.commit(&batch).unwrap();
    let after_one = db.ledger_root().unwrap();
    s.json_put("users", "u2", &json!({"city": "Nice"})).unwrap();
    s.json_put("users", "u1", &json!({"city": "Paris"})).unwrap();
    assert!(s.json_delete("users", "u2").unwrap());
    assert!(!s.kv_delete("cfg", b"absent").unwrap());
    assert_eq!(db.ledger_len().unwrap(), 4, "a write that changes nothing is not a commit");
    let root = db.ledger_root().unwrap();
    assert_ne!(root, after_one);

    let first = db.ledger_commit(1).unwrap();
    assert_eq!((first.commit_seq, first.operation_count, first.parent_hash), (1, 2, [0; 32]));
    assert_eq!(first.merkle_root, after_one);
    assert_eq!(db.ledger_commit(2).unwrap().parent_hash, first.commit_hash);
    assert!(matches!(db.ledger_commit(5), Err(Error::NotFound(_))));

    for sequence in 1..=4 {
        let proof = db.prove_commit(sequence).unwrap();
        assert!(proof.verify(&root), "commit {sequence}");
        assert!(!proof.verify(&after_one));
    }
    let doc = db.prove_doc("users", "u1").unwrap();
    assert!(doc.verify(&root));
    assert_eq!((doc.delta.model.as_str(), doc.delta.id.as_str(), doc.commit.commit_seq), ("json", "u1", 3));
    assert_eq!(doc.delta.version, s.json_get("users", "u1").unwrap().unwrap().version);
    let mut forged = doc.clone();
    forged.delta.payload_hash = [9; 32];
    assert!(!forged.verify(&root), "a proof does not hold for another payload");
    let kv = db.prove_kv("cfg", b"mode").unwrap();
    assert!(kv.verify(&root));
    assert_eq!(kv.delta.id, hex::encode(b"mode"));
    assert!(matches!(db.prove_doc("users", "u2"), Err(Error::NotFound(_))));

    let report = db.verify_ledger(LedgerVerifyMode::Strict).unwrap();
    assert!(report.is_valid, "{report:?}");
    assert_eq!((report.commit_count, report.leaves_verified, report.merkle_root), (4, 2, root));
    db.close().unwrap();

    assert!(matches!(Database::open(dir.path(), options(false)), Err(Error::InvalidArgument(_))), "a ledger cannot be turned off");
    let reopened = Database::open(dir.path(), Options { verify_ledger: LedgerVerifyMode::Strict, ..options(true) }).unwrap();
    assert_eq!(reopened.ledger_root().unwrap(), root);
    reopened.system().kv_put("cfg", b"mode", b"safe").unwrap();
    assert!(reopened.prove_kv("cfg", b"mode").unwrap().verify(&reopened.ledger_root().unwrap()));
}

#[test]
fn a_database_without_a_ledger_can_start_one() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = Database::open(dir.path(), options(false)).unwrap();
        db.system().kv_put("ns", b"old", b"1").unwrap();
        assert!(matches!(db.ledger_root(), Err(Error::LedgerDisabled)));
        db.close().unwrap();
    }
    let db = Database::open(dir.path(), options(true)).unwrap();
    db.system().kv_put("ns", b"new", b"2").unwrap();
    assert!(matches!(db.prove_kv("ns", b"old"), Err(Error::NotFound(_))), "older records are not attested");
    assert!(db.prove_kv("ns", b"new").unwrap().verify(&db.ledger_root().unwrap()));
    let report = db.verify_ledger(LedgerVerifyMode::Strict).unwrap();
    assert!(report.is_valid && report.leaves_verified == 1, "{report:?}");
}

#[test]
fn concurrent_writers_share_ledger_groups() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::open(dir.path(), small_options(Durability::Balanced, true)).unwrap());
    let writers: Vec<_> = (0..8)
        .map(|t| {
            let db = Arc::clone(&db);
            std::thread::spawn(move || {
                let s = db.system();
                for i in 0..60 {
                    let key = format!("k{}", i % 10);
                    if i % 7 == 0 {
                        // Contended conditional writes: some fail, and a failed batch must leave
                        // no commit behind.
                        let current = s.kv_get("shared", b"c").unwrap().map(|e| e.version);
                        let write = Write::KvPut {
                            namespace: "shared".into(),
                            key: b"c".to_vec(),
                            value: format!("{t}-{i}").into_bytes(),
                            expires_at_ms: None,
                            if_version: Some(current.unwrap_or(0)),
                        };
                        match s.write(&write) {
                            Ok(_) | Err(Error::Conflict { .. }) => {}
                            Err(error) => panic!("{error}"),
                        }
                    } else {
                        s.json_put(&format!("space{t}"), &key, &json!({"i": i})).unwrap();
                    }
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let report = db.verify_ledger(LedgerVerifyMode::Strict).unwrap();
    assert!(report.is_valid, "{report:?}");
    assert!(report.commit_count >= 8 * 51, "every unconditional write is a commit");
    assert_eq!(report.leaves_verified, 8 * 10 + 1);
    for t in 0..8 {
        for i in 0..10 {
            assert!(db.prove_doc(&format!("space{t}"), &format!("k{i}")).unwrap().verify(&report.merkle_root));
        }
    }
}

#[test]
fn tampering_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = Database::open(dir.path(), options(true)).unwrap();
        for i in 0..20 {
            db.system().kv_put("ns", format!("k{i}").as_bytes(), b"honest").unwrap();
        }
        db.close().unwrap();
    }
    {
        // Rewrite a record behind the database's back, keeping its envelope format.
        let engine = lilia_engine::Db::open(dir.path(), options(true).engine).unwrap();
        engine.put(b"\x01ns\x00k7".to_vec(), b"\x00forged".to_vec()).unwrap();
        engine.close().unwrap();
    }
    let db = Database::open(dir.path(), options(true)).unwrap();
    assert_eq!(db.system().kv_get("ns", b"k7").unwrap().unwrap().value, b"forged");
    let fast = db.verify_ledger(LedgerVerifyMode::Fast).unwrap();
    assert!(fast.is_valid, "the chain itself is intact");
    let strict = db.verify_ledger(LedgerVerifyMode::Strict).unwrap();
    assert!(!strict.is_valid);
    assert!(strict.error.unwrap().contains("kv:ns/k7"));
    assert!(matches!(db.prove_kv("ns", b"k7"), Err(Error::Corruption(_))));
    db.close().unwrap();
    assert!(matches!(
        Database::open(dir.path(), Options { verify_ledger: LedgerVerifyMode::Strict, ..options(true) }),
        Err(Error::Corruption(_))
    ));
}
