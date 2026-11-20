use lilia_storage_sqlite::{Database, DatabaseOptions, ZkStatement};
use serde_json::json;

#[test]
fn test_zk_sqlite_collection_and_sql_verification() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("zk_test.lilia");

    let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

    // 1. Create a users collection and insert a private record
    let users = db.collection("users").json().unwrap();
    users
        .put(
            "usr_alice",
            json!({
                "name": "Alice Dupont",
                "balance": 2500,     // Private balance
                "age": 26,           // Private age
                "iban": "FR76...",   // Private IBAN
                "_salt": "0x1234567890abcdef"
            }),
        )
        .unwrap();

    // 2. Generate the cryptographic commitment anchor
    let commitment = users.zk_commit("usr_alice", None).unwrap();
    assert!(commitment.starts_with("0x"));
    println!("Document commitment: {commitment}");

    // 3. Define the statement: "balance >= 1000 AND age >= 18"
    let statement = ZkStatement::builder()
        .field_gte("balance", 1000)
        .field_gte("age", 18)
        .build();

    // 4. Generate the zero-knowledge proof bundle (~128 bytes)
    let bundle = users.zk_prove("usr_alice", &statement, None, None).unwrap();
    assert_eq!(bundle.proof_size_bytes, 128);
    println!(
        "ZK Proof successfully generated! Size: {} bytes",
        bundle.proof_size_bytes
    );

    // 5. Verify the proof mathematically via Database::zk_verify
    let report = db.zk_verify(&bundle, None).unwrap();
    assert!(report.valid, "Database zk_verify should validate the proof");
    #[cfg(not(debug_assertions))]
    assert!(
        report.verification_time_ms < 2.0,
        "Sub-millisecond verification expected in release"
    );
    println!(
        "Report: valid={}, verification_time={:.3} ms",
        report.valid, report.verification_time_ms
    );

    // 6. Verify the proof via SQL query
    let bundle_json = serde_json::to_string(&bundle).unwrap();
    let escaped_bundle = bundle_json.replace('\'', "''");
    let query = format!(
        "SELECT zk_verify('{escaped_bundle}') AS valid, zk_commit('usr_alice', 2500, 26, '0x1234567890abcdef') AS commit_check;"
    );
    let result = db.sql_query(&query, &[]).unwrap();
    assert_eq!(
        result.rows[0][0],
        json!(1),
        "SQL zk_verify returned 1 (true)"
    );
    assert_eq!(
        result.rows[0][1],
        json!(commitment),
        "SQL zk_commit matches collection.zk_commit"
    );
    println!(
        "SQL verification passed: zk_verify={}, zk_commit={}",
        result.rows[0][0], result.rows[0][1]
    );

    // 7. Verify that false claims fail: e.g. proving balance >= 5000
    let false_statement = ZkStatement::builder()
        .field_gte("balance", 5000)
        .field_gte("age", 18)
        .build();
    let false_proof = users.zk_prove("usr_alice", &false_statement, None, None);
    assert!(
        false_proof.is_err(),
        "Prover must reject balance >= 5000 for actual balance 2500"
    );
}
