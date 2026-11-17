use lilia_core::fle::{FieldCipherAlgorithm, FlePolicy};
use lilia_core::{BatchOperation, ErrorCode};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

#[test]
fn test_fle_compartment_lifecycle_and_authentication() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        dir.path().join("compartments.lilia"),
    ))
    .unwrap();

    let comp_id = "vault:team";
    let passphrase = "correct_horse_battery_staple";

    // 1. Create compartment
    let km = db.create_compartment(comp_id, passphrase).unwrap();
    assert_eq!(km.gcm_key().len(), 32);
    assert_eq!(km.siv_key().len(), 64);
    assert!(db.is_compartment_unlocked(comp_id));

    // 2. Lock compartment
    assert!(db.lock_compartment(comp_id));
    assert!(!db.is_compartment_unlocked(comp_id));

    // 3. Unlock with wrong passphrase should fail with Unauthorized
    let err = db
        .unlock_compartment_with_passphrase(comp_id, "wrong_password")
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Unauthorized);
    assert!(!db.is_compartment_unlocked(comp_id));

    // 4. Unlock with correct passphrase should succeed
    db.unlock_compartment_with_passphrase(comp_id, passphrase)
        .unwrap();
    assert!(db.is_compartment_unlocked(comp_id));

    // 5. List compartments
    let comps = db.list_compartments().unwrap();
    assert!(comps.iter().any(|c| c.id == comp_id));
}

#[test]
fn test_fle_selective_encryption_and_cleartext_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        dir.path().join("selective_fle.lilia"),
    ))
    .unwrap();

    let comp_id = "vault:health";
    db.create_compartment(comp_id, "top_secret_passphrase")
        .unwrap();

    // Configure FLE policy on "patients"
    let policy = FlePolicy::new()
        .field("medical_record", FieldCipherAlgorithm::RandomizedGcm, comp_id)
        .field("ssn", FieldCipherAlgorithm::RandomizedGcm, comp_id)
        .field("email", FieldCipherAlgorithm::DeterministicSiv, comp_id);

    db.collection("patients")
        .compartment(comp_id)
        .fle_policy(policy)
        .json()
        .unwrap();

    // Insert doc 1, 2, and 3
    db.batch(&[
        BatchOperation::JsonPut {
            space: "patients".into(),
            id: "p1".into(),
            value: json!({
                "id": "p1",
                "status": "active",
                "department": "cardiology",
                "email": "alice@hospital.org",
                "ssn": "123-45-6789",
                "medical_record": "Hypertension stage 2"
            }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "patients".into(),
            id: "p2".into(),
            value: json!({
                "id": "p2",
                "status": "discharged",
                "department": "neurology",
                "email": "bob@hospital.org",
                "ssn": "987-65-4321",
                "medical_record": "Migraine"
            }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "patients".into(),
            id: "p3".into(),
            value: json!({
                "id": "p3",
                "status": "active",
                "department": "cardiology",
                "email": "carol@hospital.org",
                "ssn": "555-55-5555",
                "medical_record": "Hypertension stage 2" // identical to p1
            }),
            if_version: None,
        },
    ])
    .unwrap();

    // 1. Read back with unlocked key_registry: transparently decrypted
    let doc1 = db.json_get("patients", "p1").unwrap().unwrap();
    assert_eq!(doc1.value()["status"], "active");
    assert_eq!(doc1.value()["department"], "cardiology");
    assert_eq!(doc1.value()["email"], "alice@hospital.org");
    assert_eq!(doc1.value()["ssn"], "123-45-6789");
    assert_eq!(doc1.value()["medical_record"], "Hypertension stage 2");

    // 2. Direct inspection of SQLite storage table `_lilia_json` using zero-copy lilia_extract
    let raw_rows = db
        .sql_query(
            "SELECT id, \
                    lilia_extract(value, '$.status') AS status, \
                    lilia_extract(value, '$.department') AS department, \
                    lilia_extract(value, '$.email') AS email, \
                    lilia_extract(value, '$.ssn') AS ssn, \
                    lilia_extract(value, '$.medical_record') AS medical_record \
             FROM _lilia_json WHERE space = 'patients' ORDER BY id ASC;",
            &[],
        )
        .unwrap();
    assert_eq!(raw_rows.rows.len(), 3);

    // Verify cleartext fields remained in plain text
    assert_eq!(raw_rows.rows[0][1], "active");
    assert_eq!(raw_rows.rows[0][2], "cardiology");

    // Verify encrypted fields hold ENC envelopes
    let raw_email = raw_rows.rows[0][3].as_str().unwrap();
    assert!(
        raw_email.starts_with("ENC:siv:vault:health:"),
        "raw_email was: {raw_email}"
    );

    let raw_ssn = raw_rows.rows[0][4].as_str().unwrap();
    assert!(
        raw_ssn.starts_with("ENC:gcm:vault:health:"),
        "raw_ssn was: {raw_ssn}"
    );

    let raw_med1 = raw_rows.rows[0][5].as_str().unwrap();
    assert!(
        raw_med1.starts_with("ENC:gcm:vault:health:"),
        "raw_med1 was: {raw_med1}"
    );

    // 3. Verify GCM Randomized Confidentiality:
    // doc1 and doc3 have identical plaintext "Hypertension stage 2", but their ciphertexts must differ
    let raw_med3 = raw_rows.rows[2][5].as_str().unwrap();
    assert_ne!(
        raw_med1, raw_med3,
        "GCM ciphertexts for identical plaintext must be non-deterministic and distinct"
    );
}

#[test]
fn test_fle_deterministic_siv_equality_search_and_btree_indexing() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        dir.path().join("siv_indexing.lilia"),
    ))
    .unwrap();

    let comp_id = "vault:org";
    db.create_compartment(comp_id, "siv_secret_key").unwrap();

    let policy = FlePolicy::new()
        .field("email", FieldCipherAlgorithm::DeterministicSiv, comp_id)
        .field("salary", FieldCipherAlgorithm::RandomizedGcm, comp_id);

    db.collection("employees")
        .compartment(comp_id)
        .fle_policy(policy)
        .json()
        .unwrap();

    // Create a secondary B-Tree JSON index on the encrypted field "email"
    db.create_json_index("employees", "by_email", "$.email")
        .unwrap();

    // Insert employees
    db.batch(&[
        BatchOperation::JsonPut {
            space: "employees".into(),
            id: "e1".into(),
            value: json!({
                "name": "Alice",
                "email": "alice@company.com",
                "role": "engineer",
                "salary": 120000
            }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "employees".into(),
            id: "e2".into(),
            value: json!({
                "name": "Bob",
                "email": "bob@company.com",
                "role": "manager",
                "salary": 150000
            }),
            if_version: None,
        },
    ])
    .unwrap();

    // Check that SQLite _lilia_json_index_entries contains the SIV-encrypted string
    let idx_rows = db
        .sql_query(
            "SELECT doc_id, idx_value FROM _lilia_json_index_entries WHERE space = 'employees' AND index_name = 'by_email' ORDER BY doc_id ASC;",
            &[],
        )
        .unwrap();
    assert_eq!(idx_rows.rows.len(), 2);
    let e1_idx_val = idx_rows.rows[0][1].as_str().unwrap();
    assert!(e1_idx_val.starts_with("ENC:siv:vault:org:"));

    // Query using json_find: user queries for plaintext email "alice@company.com"
    // LiliaDB transparently detects that $.email has an SIV policy, encrypts "alice@company.com",
    // and performs an O(log N) B-Tree seek on _lilia_json_index_entries!
    let mut query = serde_json::Map::new();
    query.insert("email".into(), json!("alice@company.com"));

    let results = db.json_find("employees", &query, None, 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "e1");
    assert_eq!(results[0].value()["name"], "Alice");
    assert_eq!(results[0].value()["email"], "alice@company.com");
    assert_eq!(results[0].value()["salary"], 120000);

    // Multi-criteria query combining SIV encrypted field + cleartext field
    let mut query2 = serde_json::Map::new();
    query2.insert("email".into(), json!("alice@company.com"));
    query2.insert("role".into(), json!("engineer"));
    let results2 = db.json_find("employees", &query2, None, 10).unwrap();
    assert_eq!(results2.len(), 1);
    assert_eq!(results2[0].id, "e1");

    // Non-matching query
    let mut query_none = serde_json::Map::new();
    query_none.insert("email".into(), json!("nonexistent@company.com"));
    let results_none = db.json_find("employees", &query_none, None, 10).unwrap();
    assert!(results_none.is_empty());
}

#[test]
fn test_fle_multi_tenant_compartments_and_partial_decryption() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        dir.path().join("multitenant.lilia"),
    ))
    .unwrap();

    // Create 2 independent compartments in the same .lilia file
    db.create_compartment("vault:private", "biometric_local_seed")
        .unwrap();
    db.create_compartment("vault:team", "shared_org_key")
        .unwrap();

    // Define a collection where different fields belong to different compartments
    let policy = FlePolicy::new()
        .field("private_notes", FieldCipherAlgorithm::RandomizedGcm, "vault:private")
        .field("team_discussion", FieldCipherAlgorithm::RandomizedGcm, "vault:team");

    db.collection("workspaces")
        .fle_policy(policy)
        .json()
        .unwrap();

    db.batch(&[BatchOperation::JsonPut {
        space: "workspaces".into(),
        id: "w1".into(),
        value: json!({
            "title": "Project Pegasus",
            "private_notes": "My personal evaluation",
            "team_discussion": "Sprint goals agreed"
        }),
        if_version: None,
    }])
    .unwrap();

    // Lock "vault:private" (e.g. biometric locked / session timed out)
    db.lock_compartment("vault:private");
    assert!(!db.is_compartment_unlocked("vault:private"));
    assert!(db.is_compartment_unlocked("vault:team"));

    // In Tolerant mode (default for reads), fields whose key is missing remain ENC envelopes
    // while unlocked fields and cleartext fields are decrypted
    let doc = db.json_get("workspaces", "w1").unwrap().unwrap();
    let val = doc.value();
    assert_eq!(val["title"], "Project Pegasus");
    assert_eq!(val["team_discussion"], "Sprint goals agreed");
    let priv_val = val["private_notes"].as_str().unwrap();
    assert!(
        priv_val.starts_with("ENC:gcm:vault:private:"),
        "Locked field must remain sealed: {priv_val}"
    );

    // Now unlock "vault:private"
    db.unlock_compartment_with_passphrase("vault:private", "biometric_local_seed")
        .unwrap();
    let doc_unlocked = db.json_get("workspaces", "w1").unwrap().unwrap();
    assert_eq!(doc_unlocked.value()["private_notes"], "My personal evaluation");
    assert_eq!(doc_unlocked.value()["team_discussion"], "Sprint goals agreed");
}

#[test]
fn test_fle_sqlite_functions_and_unified_sql() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("sql_fle.lilia"))).unwrap();

    let comp_id = "vault:sql";
    db.create_compartment(comp_id, "sql_passphrase").unwrap();

    // 1. Test scalar functions: lilia_encrypt_siv and lilia_decrypt_siv
    let enc_siv_rows = db
        .sql_query(
            "SELECT lilia_encrypt_siv('user@example.com', 'vault:sql') AS enc;",
            &[],
        )
        .unwrap();
    let enc_siv_str = enc_siv_rows.rows[0][0].as_str().unwrap();
    assert!(enc_siv_str.starts_with("ENC:siv:vault:sql:"));

    let dec_siv_rows = db
        .sql_query(
            "SELECT lilia_decrypt_siv(?1, 'vault:sql') AS dec;",
            &[enc_siv_str.into()],
        )
        .unwrap();
    assert_eq!(
        dec_siv_rows.rows[0][0].as_str().unwrap(),
        "user@example.com"
    );

    // 2. Test scalar functions: lilia_encrypt_gcm and lilia_decrypt_gcm
    let enc_gcm_rows = db
        .sql_query(
            "SELECT lilia_encrypt_gcm('confidential note', 'vault:sql') AS enc;",
            &[],
        )
        .unwrap();
    let enc_gcm_str = enc_gcm_rows.rows[0][0].as_str().unwrap();
    assert!(enc_gcm_str.starts_with("ENC:gcm:vault:sql:"));

    let dec_gcm_rows = db
        .sql_query(
            "SELECT lilia_decrypt_gcm(?1, 'vault:sql') AS dec;",
            &[enc_gcm_str.into()],
        )
        .unwrap();
    assert_eq!(
        dec_gcm_rows.rows[0][0].as_str().unwrap(),
        "confidential note"
    );

    // 3. Test lilia_fle_decrypt (auto-detects algorithm and compartment from envelope header)
    let auto_dec_rows = db
        .sql_query(
            "SELECT lilia_fle_decrypt(?1) AS dec;",
            &[enc_gcm_str.into()],
        )
        .unwrap();
    assert_eq!(
        auto_dec_rows.rows[0][0].as_str().unwrap(),
        "confidential note"
    );

    // 4. Test transparent decryption through unified view lilia_json
    let policy = FlePolicy::new().field("secret", FieldCipherAlgorithm::RandomizedGcm, comp_id);
    db.collection("secrets")
        .compartment(comp_id)
        .fle_policy(policy)
        .json()
        .unwrap();

    db.batch(&[BatchOperation::JsonPut {
        space: "secrets".into(),
        id: "s1".into(),
        value: json!({ "category": "finance", "secret": "PIN 9999" }),
        if_version: None,
    }])
    .unwrap();

    let view_rows = db
        .sql_query(
            "SELECT id, json_extract(value, '$.secret') AS secret FROM lilia_json WHERE space = 'secrets';",
            &[],
        )
        .unwrap();
    assert_eq!(view_rows.rows.len(), 1);
    assert_eq!(
        view_rows.rows[0][1].as_str().unwrap(),
        "PIN 9999"
    );
}

#[test]
fn test_fle_fts5_exclusion() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        dir.path().join("fts_exclusion.lilia"),
    ))
    .unwrap();

    let comp_id = "vault:legal";
    db.create_compartment(comp_id, "legal_passphrase").unwrap();

    let policy = FlePolicy::new().field(
        "classified_clause",
        FieldCipherAlgorithm::RandomizedGcm,
        comp_id,
    );

    db.collection("contracts")
        .compartment(comp_id)
        .fle_policy(policy)
        .json()
        .unwrap();

    db.batch(&[BatchOperation::JsonPut {
        space: "contracts".into(),
        id: "c1".into(),
        value: json!({
            "title": "Master Services Agreement",
            "classified_clause": "Strictly confidential penalty clause of 1000000 dollars"
        }),
        if_version: None,
    }])
    .unwrap();

    // Index document text containing both cleartext and encrypted ENC envelope tokens
    db.fts_index(
        "contracts",
        "c1",
        "Master Services Agreement ENC:gcm:vault:legal:b64SecretPayload==",
    )
    .unwrap();

    // FTS index on "Master" works
    let hits_title = db.fts_search("contracts", "Master", 10).unwrap();
    assert_eq!(hits_title.len(), 1);
    assert_eq!(hits_title[0].doc_id, "c1");

    // Search for "ENC" or base64 fragments should return 0 hits because ENC tokens were excluded
    let hits_enc = db.fts_search("contracts", "ENC", 10).unwrap();
    assert!(
        hits_enc.is_empty(),
        "Encrypted tokens must not pollute FTS index"
    );

    let hits_payload = db.fts_search("contracts", "b64SecretPayload", 10).unwrap();
    assert!(
        hits_payload.is_empty(),
        "Ciphertext tokens must not pollute FTS index"
    );
}
