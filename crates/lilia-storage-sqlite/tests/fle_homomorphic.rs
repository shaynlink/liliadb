//! Integration tests for Additive Homomorphic Encryption (Paillier cryptosystem) in LiliaDB.
//!
//! Tests:
//! 1. Declarative FLE policy with `FieldCipherAlgorithm::HomomorphicPaillier`.
//! 2. Zero-leakage verification: raw SQLite table `_lilia_json` stores only `ENC:paillier:...` envelopes.
//! 3. Single-pass streaming aggregation: `Aggregate::homomorphic_sum` and transparent `Aggregate::sum`.
//! 4. Zero-Knowledge / Cloud DB mode: server aggregates using ONLY public key $n$, producing sealed result.
//! 5. Client-side final decryption with private key.
//! 6. SQL scalar functions (`lilia_paillier_add`, `lilia_paillier_sub`, `lilia_paillier_mul`) and UDAF `lilia_paillier_sum`.

use lilia_core::aggregation::Aggregate;
use lilia_core::fle::{FieldCipherAlgorithm, FlePolicy};
use lilia_core::BatchOperation;
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

#[test]
fn test_fle_homomorphic_paillier_pipeline_aggregation() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        dir.path().join("homomorphic_payroll.lilia"),
    ))
    .unwrap();

    let comp_id = "vault:payroll";
    let passphrase = "finance_secure_master_password";

    // 1. Create a 512-bit Paillier compartment for fast testing
    let keypair = db
        .create_paillier_compartment(comp_id, passphrase, 512)
        .unwrap();
    assert!(db.is_compartment_unlocked(comp_id));

    // 2. Configure FLE policy: salary and bonus are encrypted homomorphically
    let policy = FlePolicy::new()
        .field("salary", FieldCipherAlgorithm::HomomorphicPaillier, comp_id)
        .field("bonus", FieldCipherAlgorithm::HomomorphicPaillier, comp_id);

    db.collection("employees")
        .compartment(comp_id)
        .fle_policy(policy)
        .json()
        .unwrap();

    // 3. Insert employees with positive and signed amounts
    let employees = vec![
        ("e1", "Engineering", 120_000, 15_000),
        ("e2", "Engineering", 140_000, 20_000),
        ("e3", "Product", 110_000, 10_000),
        ("e4", "Product", 130_000, 12_000),
        ("e5", "Sales", 90_000, 30_000),
    ];

    let mut batch_ops = Vec::new();
    let mut expected_total_salary = 0i64;
    let mut expected_total_bonus = 0i64;

    for (id, dept, salary, bonus) in &employees {
        expected_total_salary += *salary;
        expected_total_bonus += *bonus;
        batch_ops.push(BatchOperation::JsonPut {
            space: "employees".into(),
            id: (*id).into(),
            value: json!({
                "id": id,
                "department": dept,
                "salary": salary,
                "bonus": bonus,
            }),
            if_version: None,
        });
    }

    db.batch(&batch_ops).unwrap();

    // 4. Verify physical storage in SQLite: raw records must contain ENC:paillier envelopes
    let raw_rows = db
        .sql_query(
            "SELECT id, \
                    lilia_extract(value, '$.department') AS dept, \
                    lilia_extract(value, '$.salary') AS raw_sal, \
                    lilia_extract(value, '$.bonus') AS raw_bon \
             FROM _lilia_json WHERE space = 'employees' ORDER BY id ASC;",
            &[],
        )
        .unwrap();

    assert_eq!(raw_rows.rows.len(), 5);
    for row in &raw_rows.rows {
        let sal_str = row[2].as_str().unwrap();
        let bon_str = row[3].as_str().unwrap();
        assert!(
            sal_str.starts_with("ENC:paillier:vault:payroll:"),
            "raw salary was not an ENC:paillier envelope: {sal_str}"
        );
        assert!(
            bon_str.starts_with("ENC:paillier:vault:payroll:"),
            "raw bonus was not an ENC:paillier envelope: {bon_str}"
        );
    }

    // 5. Test AggregationPipeline with explicit Aggregate::homomorphic_sum:
    let agg_result = db
        .json()
        .aggregate("employees")
        .aggregate_all(vec![
            Aggregate::homomorphic_sum("salary", "total_salary"),
            Aggregate::homomorphic_sum("bonus", "total_bonus"),
            Aggregate::count("headcount"),
        ])
        .execute()
        .unwrap();

    assert_eq!(agg_result.len(), 1);
    println!("agg_result: {:?}", agg_result);
    let total_sal_val = agg_result[0]["total_salary"].as_i64().unwrap();
    let total_bon_val = agg_result[0]["total_bonus"].as_i64().unwrap();
    let headcount = agg_result[0]["headcount"].as_u64().unwrap();

    assert_eq!(total_sal_val, expected_total_salary);
    assert_eq!(total_bon_val, expected_total_bonus);
    assert_eq!(headcount, 5);

    // 6. Test Automatic detection via Aggregate::sum on Paillier-encrypted fields:
    let auto_agg_result = db
        .json()
        .aggregate("employees")
        .aggregate_all(vec![
            Aggregate::sum("salary", "total_salary"),
            Aggregate::sum("bonus", "total_bonus"),
        ])
        .execute()
        .unwrap();

    assert_eq!(auto_agg_result.len(), 1);
    assert_eq!(
        auto_agg_result[0]["total_salary"].as_i64().unwrap(),
        expected_total_salary
    );
    assert_eq!(
        auto_agg_result[0]["total_bonus"].as_i64().unwrap(),
        expected_total_bonus
    );

    // 7. Test Group-by aggregation by department:
    let grouped_res = db
        .json()
        .aggregate("employees")
        .group_by(
            "department",
            vec![
                Aggregate::homomorphic_sum("salary", "dept_salary"),
                Aggregate::count("dept_count"),
            ],
        )
        .execute()
        .unwrap();

    assert_eq!(grouped_res.len(), 3);
    for row in grouped_res {
        let dept = row["department"].as_str().unwrap();
        let dept_sal = row["dept_salary"].as_i64().unwrap();
        match dept {
            "Engineering" => assert_eq!(dept_sal, 120_000 + 140_000),
            "Product" => assert_eq!(dept_sal, 110_000 + 130_000),
            "Sales" => assert_eq!(dept_sal, 90_000),
            _ => panic!("unexpected department: {dept}"),
        }
    }

    // 8. Test Zero-Knowledge / Cloud Untrusted Server Mode:
    // Lock the private key so the engine has ZERO decryption ability!
    let _ = db.lock_compartment(comp_id);
    assert!(!db.is_compartment_unlocked(comp_id));

    // Register only the public key on the engine:
    db.register_paillier_public_key(comp_id, (*keypair.public_key).clone())
        .unwrap();

    // Run aggregation without private key:
    let zk_agg = db
        .json()
        .aggregate("employees")
        .aggregate_all(vec![Aggregate::homomorphic_sum("salary", "total_salary")])
        .execute()
        .unwrap();

    assert_eq!(zk_agg.len(), 1);
    let sealed_envelope = zk_agg[0]["total_salary"].as_str().unwrap();
    assert!(
        sealed_envelope.starts_with("ENC:paillier:vault:payroll:"),
        "result must be a sealed envelope in zero-knowledge mode"
    );

    // Decrypt on the client side using the client's private key:
    let client_env = lilia_core::EncryptedEnvelope::parse(sealed_envelope).unwrap();
    let client_ct = lilia_core::PaillierCiphertext::from_bytes(&client_env.ciphertext).unwrap();
    let client_decrypted =
        lilia_core::decrypt_paillier_i64(&keypair.private_key, &keypair.public_key, &client_ct)
            .unwrap();

    assert_eq!(
        client_decrypted, expected_total_salary,
        "client decrypted sum matches expected total with zero server plaintext exposure"
    );
}

#[test]
fn test_fle_homomorphic_sql_functions() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        dir.path().join("homomorphic_sql.lilia"),
    ))
    .unwrap();

    let comp_id = "vault:accounting";
    let _keypair = db
        .create_paillier_compartment(comp_id, "accounting_secret", 512)
        .unwrap();

    // 1. Test lilia_paillier_encrypt and lilia_paillier_decrypt
    let enc_rows = db
        .sql_query(
            "SELECT lilia_paillier_encrypt(50000, 'vault:accounting') AS enc_val;",
            &[],
        )
        .unwrap();
    let enc_str = enc_rows.rows[0][0].as_str().unwrap();
    assert!(enc_str.starts_with("ENC:paillier:vault:accounting:"));

    let dec_rows = db
        .sql_query(
            &format!("SELECT lilia_paillier_decrypt('{enc_str}', 'vault:accounting');"),
            &[],
        )
        .unwrap();
    assert_eq!(dec_rows.rows[0][0], 50000i64);

    // 2. Test lilia_paillier_add and lilia_paillier_sub in SQL
    let add_rows = db
        .sql_query(
            "SELECT lilia_paillier_decrypt( \
                lilia_paillier_add( \
                    lilia_paillier_encrypt(3000, 'vault:accounting'), \
                    lilia_paillier_encrypt(4500, 'vault:accounting') \
                ), \
                'vault:accounting' \
            );",
            &[],
        )
        .unwrap();
    assert_eq!(add_rows.rows[0][0], 7500i64);

    let sub_rows = db
        .sql_query(
            "SELECT lilia_paillier_decrypt( \
                lilia_paillier_sub( \
                    lilia_paillier_encrypt(10000, 'vault:accounting'), \
                    lilia_paillier_encrypt(3500, 'vault:accounting') \
                ), \
                'vault:accounting' \
            );",
            &[],
        )
        .unwrap();
    assert_eq!(sub_rows.rows[0][0], 6500i64);

    // 3. Test lilia_paillier_mul (scalar multiplication) in SQL
    let mul_rows = db
        .sql_query(
            "SELECT lilia_paillier_decrypt( \
                lilia_paillier_mul( \
                    lilia_paillier_encrypt(1500, 'vault:accounting'), \
                    4 \
                ), \
                'vault:accounting' \
            );",
            &[],
        )
        .unwrap();
    assert_eq!(mul_rows.rows[0][0], 6000i64);

    // 4. Test UDAF lilia_paillier_sum in SQL
    let policy =
        FlePolicy::new().field("amount", FieldCipherAlgorithm::HomomorphicPaillier, comp_id);
    db.collection("invoices")
        .compartment(comp_id)
        .fle_policy(policy)
        .json()
        .unwrap();

    db.batch(&[
        BatchOperation::JsonPut {
            space: "invoices".into(),
            id: "inv1".into(),
            value: json!({ "id": "inv1", "amount": 1200 }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "invoices".into(),
            id: "inv2".into(),
            value: json!({ "id": "inv2", "amount": 3400 }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "invoices".into(),
            id: "inv3".into(),
            value: json!({ "id": "inv3", "amount": 5400 }),
            if_version: None,
        },
    ])
    .unwrap();

    let sql_sum = db
        .sql_query(
            "SELECT lilia_paillier_decrypt( \
                lilia_paillier_sum(lilia_extract(value, '$.amount')), \
                'vault:accounting' \
            ) FROM _lilia_json WHERE space = 'invoices';",
            &[],
        )
        .unwrap();
    assert_eq!(sql_sum.rows[0][0], 1200 + 3400 + 5400);

    // Also verify transparent lilia_fle_decrypt
    let fle_dec = db
        .sql_query(
            "SELECT lilia_fle_decrypt( \
                lilia_paillier_sum(lilia_extract(value, '$.amount')) \
            ) FROM _lilia_json WHERE space = 'invoices';",
            &[],
        )
        .unwrap();
    assert_eq!(fle_dec.rows[0][0], 10000i64);
}
