use lilia_core::security::{MaskFunction, MaskingRule, Policy, PolicyAction, SecurityContext};
use lilia_core::ErrorCode;
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

#[test]
fn test_rls_json_engine_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("rls_json.lilia"))).unwrap();

    // 1. Define RLS isolation policy
    db.security()
        .create_policy(
            "tenant_isolation",
            Policy::new(
                "documents",
                "json_extract(data, '$.tenant_id') == current_context('tenant_id')",
            ),
        )
        .unwrap();

    // 2. Insert documents belonging to two distinct tenants
    let ctx_a = SecurityContext::from_pair("tenant_id", "hospital_a");
    let ctx_b = SecurityContext::from_pair("tenant_id", "hospital_b");

    let hosp_a = db.with_context(ctx_a.clone());
    let hosp_b = db.with_context(ctx_b.clone());

    hosp_a
        .json_put(
            "documents",
            "patient_1",
            &json!({
                "tenant_id": "hospital_a",
                "patient": "Alice",
                "condition": "Stable"
            }),
        )
        .unwrap();

    hosp_b
        .json_put(
            "documents",
            "patient_2",
            &json!({
                "tenant_id": "hospital_b",
                "patient": "Bob",
                "condition": "Critical"
            }),
        )
        .unwrap();

    // 3. Verify access without context: fail-closed, cannot read any document
    assert!(db.json_get("documents", "patient_1").unwrap().is_none());
    assert!(db.json_get("documents", "patient_2").unwrap().is_none());
    assert_eq!(db.json_scan("documents", None, 10).unwrap().len(), 0);

    // 4. Verify Hospital A isolation
    let doc1_a = hosp_a.json_get("documents", "patient_1").unwrap();
    assert!(doc1_a.is_some());
    assert_eq!(doc1_a.unwrap().value()["patient"], "Alice");

    // Hospital A cannot access patient 2 from Hospital B
    assert!(hosp_a.json_get("documents", "patient_2").unwrap().is_none());
    assert!(hosp_a
        .json_extract("documents", "patient_2", "$.patient")
        .unwrap()
        .is_none());

    // json_scan under Hospital A only returns Alice
    let scan_a = hosp_a.json_scan("documents", None, 10).unwrap();
    assert_eq!(scan_a.len(), 1);
    assert_eq!(scan_a[0].id, "patient_1");

    // json_find under Hospital A only returns Alice
    let mut query = serde_json::Map::new();
    query.insert("tenant_id".to_string(), json!("hospital_a"));
    let find_a = hosp_a.json_find("documents", &query, None, 10).unwrap();
    assert_eq!(find_a.len(), 1);
    assert_eq!(find_a[0].id, "patient_1");

    // 5. Verify Hospital B isolation
    let doc2_b = hosp_b.json_get("documents", "patient_2").unwrap();
    assert!(doc2_b.is_some());
    assert_eq!(doc2_b.unwrap().value()["patient"], "Bob");
    assert!(hosp_b.json_get("documents", "patient_1").unwrap().is_none());

    let scan_b = hosp_b.json_scan("documents", None, 10).unwrap();
    assert_eq!(scan_b.len(), 1);
    assert_eq!(scan_b[0].id, "patient_2");

    // 6. Write protection: Hospital A cannot insert a document for Hospital B
    let err = hosp_a
        .json_put(
            "documents",
            "patient_3",
            &json!({
                "tenant_id": "hospital_b",
                "patient": "Charlie"
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Unauthorized);

    // Hospital A cannot delete Hospital B's patient
    let deleted = hosp_a.json_delete("documents", "patient_2").unwrap_err();
    assert_eq!(deleted.code, ErrorCode::Unauthorized);
}

#[test]
fn test_rls_sql_engine_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("rls_sql.lilia"))).unwrap();

    db.security()
        .create_policy(
            "tenant_isolation",
            Policy::new(
                "documents",
                "json_extract(data, '$.tenant_id') == current_context('tenant_id')",
            ),
        )
        .unwrap();

    let ctx_a = SecurityContext::from_pair("tenant_id", "pos_terminal_1");
    let ctx_b = SecurityContext::from_pair("tenant_id", "pos_terminal_2");

    let session_a = db.with_context(ctx_a.clone());
    let session_b = db.with_context(ctx_b.clone());

    session_a
        .json_put(
            "documents",
            "sale_101",
            &json!({
                "tenant_id": "pos_terminal_1",
                "amount": 42.50,
                "cashier": "Claire"
            }),
        )
        .unwrap();

    session_b
        .json_put(
            "documents",
            "sale_201",
            &json!({
                "tenant_id": "pos_terminal_2",
                "amount": 99.00,
                "cashier": "David"
            }),
        )
        .unwrap();

    // 1. SQL query via session_a (SELECT from lilia_json view)
    let sql_res_a = session_a
        .sql_query(
            "SELECT space, id, value FROM lilia_json WHERE space = 'documents';",
            &[],
        )
        .unwrap();
    assert_eq!(sql_res_a.rows.len(), 1);
    assert_eq!(sql_res_a.rows[0][1], json!("sale_101"));

    // 2. SQL query even without WHERE clause: automatic RLS injection
    let sql_all_a = session_a
        .sql_query("SELECT space, id FROM lilia_json;", &[])
        .unwrap();
    assert_eq!(sql_all_a.rows.len(), 1);
    assert_eq!(sql_all_a.rows[0][1], json!("sale_101"));

    // 3. SQL query via session_b
    let sql_res_b = session_b
        .sql_query(
            "SELECT space, id, value FROM lilia_json WHERE space = 'documents';",
            &[],
        )
        .unwrap();
    assert_eq!(sql_res_b.rows.len(), 1);
    assert_eq!(sql_res_b.rows[0][1], json!("sale_201"));

    // 4. SQL query without active security context: returns 0 rows
    let sql_unauth = db
        .sql_query(
            "SELECT space, id FROM lilia_json WHERE space = 'documents';",
            &[],
        )
        .unwrap();
    assert_eq!(sql_unauth.rows.len(), 0);

    // 5. Test SQLite scalar function current_context('tenant_id')
    db.security().with_context(&ctx_a, || {
        let res = db
            .sql_query("SELECT current_context('tenant_id') AS tenant;", &[])
            .unwrap();
        assert_eq!(res.rows[0][0], json!("pos_terminal_1"));
    });
}

#[test]
fn test_rls_kv_engine_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("rls_kv.lilia"))).unwrap();

    // KV policy enforcing tenant match on JSON value in "vault_sessions"
    db.security()
        .create_policy(
            "kv_tenant_isolation",
            Policy::new(
                "vault_sessions",
                "json_extract(data, '$.tenant_id') == current_context('tenant_id')",
            ),
        )
        .unwrap();

    let ctx_a = SecurityContext::from_pair("tenant_id", "dept_cardiology");
    let ctx_b = SecurityContext::from_pair("tenant_id", "dept_neurology");

    let sess_a = db.with_context(ctx_a);
    let sess_b = db.with_context(ctx_b);

    sess_a
        .kv_put(
            "vault_sessions",
            b"session_user_1",
            &serde_json::to_vec(
                &json!({ "tenant_id": "dept_cardiology", "token": "token_cardio_123" }),
            )
            .unwrap(),
        )
        .unwrap();

    sess_b
        .kv_put(
            "vault_sessions",
            b"session_user_2",
            &serde_json::to_vec(
                &json!({ "tenant_id": "dept_neurology", "token": "token_neuro_456" }),
            )
            .unwrap(),
        )
        .unwrap();

    // Session A can read session_user_1, cannot read session_user_2
    assert!(sess_a
        .kv_get("vault_sessions", b"session_user_1")
        .unwrap()
        .is_some());
    assert!(sess_a
        .kv_get("vault_sessions", b"session_user_2")
        .unwrap()
        .is_none());

    // kv_scan under Session A only returns 1 entry
    let kv_scan_a = sess_a.kv_scan("vault_sessions", None, 10).unwrap();
    assert_eq!(kv_scan_a.len(), 1);
    assert_eq!(kv_scan_a[0].key, b"session_user_1");

    // Unauthorized without context
    assert!(db
        .kv_get("vault_sessions", b"session_user_1")
        .unwrap()
        .is_none());
}

#[test]
fn test_rls_fts5_search_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("rls_fts.lilia"))).unwrap();

    db.security()
        .create_policy(
            "fts_tenant_policy",
            Policy::new(
                "clinical_notes",
                "json_extract(data, '$.tenant_id') == current_context('tenant_id')",
            ),
        )
        .unwrap();

    let ctx_a = SecurityContext::from_pair("tenant_id", "clinic_alpha");
    let ctx_b = SecurityContext::from_pair("tenant_id", "clinic_beta");

    let sess_a = db.with_context(ctx_a);
    let sess_b = db.with_context(ctx_b);

    sess_a
        .json_put(
            "clinical_notes",
            "note_alpha_1",
            &json!({
                "tenant_id": "clinic_alpha",
                "summary": "Patient prescribed high dosage antibiotic medication"
            }),
        )
        .unwrap();

    sess_b
        .json_put(
            "clinical_notes",
            "note_beta_1",
            &json!({
                "tenant_id": "clinic_beta",
                "summary": "Patient prescribed mild allergy medication"
            }),
        )
        .unwrap();

    db.fts_index(
        "clinical_notes",
        "note_alpha_1",
        "Patient prescribed high dosage antibiotic medication",
    )
    .unwrap();

    db.fts_index(
        "clinical_notes",
        "note_beta_1",
        "Patient prescribed mild allergy medication",
    )
    .unwrap();

    // 1. Search for common keyword "medication"
    let hits_a = sess_a
        .fts_search("clinical_notes", "medication", 10)
        .unwrap();
    assert_eq!(hits_a.len(), 1);
    assert_eq!(hits_a[0].doc_id, "note_alpha_1");

    let hits_b = sess_b
        .fts_search("clinical_notes", "medication", 10)
        .unwrap();
    assert_eq!(hits_b.len(), 1);
    assert_eq!(hits_b[0].doc_id, "note_beta_1");

    // 2. Search for clinic_beta's keyword "allergy" under clinic_alpha: 0 hits
    let allergy_under_a = sess_a.fts_search("clinical_notes", "allergy", 10).unwrap();
    assert_eq!(allergy_under_a.len(), 0);

    // 3. Fuzzy search isolation
    let fuzzy_hits_a = sess_a
        .fts_fuzzy_search("clinical_notes", "medicatn", 2, 10)
        .unwrap();
    assert_eq!(fuzzy_hits_a.len(), 1);
    assert_eq!(fuzzy_hits_a[0].doc_id, "note_alpha_1");
}

#[test]
fn test_dynamic_data_masking() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("masking.lilia"))).unwrap();

    // Configure masking rules with admin exemption
    db.security()
        .create_masking_rule(MaskingRule::new(
            "customers",
            "$.credit_card",
            MaskFunction::CreditCard,
            Some("current_context('role') == 'admin'".to_string()),
        ))
        .unwrap();

    db.security()
        .create_masking_rule(MaskingRule::new(
            "customers",
            "$.email",
            MaskFunction::Email,
            Some("current_context('role') == 'admin'".to_string()),
        ))
        .unwrap();

    db.security()
        .create_masking_rule(MaskingRule::new(
            "customers",
            "$.ssn",
            MaskFunction::Redact,
            Some("current_context('role') == 'admin'".to_string()),
        ))
        .unwrap();

    // Insert sensitive customer document
    let test_card = "9876-5432-1098-1234";
    let test_email = "claire.dupont@hospital.org";
    let test_ssn = "123-45-6789";

    db.batch(&[lilia_core::BatchOperation::JsonPut {
        space: "customers".to_string(),
        id: "cust_42".to_string(),
        value: json!({
            "name": "Claire Dupont",
            "credit_card": test_card,
            "email": test_email,
            "ssn": test_ssn,
            "tier": "gold"
        }),
        if_version: None,
    }])
    .unwrap();

    // 1. Regular user access (role = "user" or default): data is masked
    let user_ctx = SecurityContext::new().with_role("user");
    let user_db = db.with_context(user_ctx);

    let masked_doc = user_db.json_get("customers", "cust_42").unwrap().unwrap();
    let masked_val = masked_doc.value();

    assert_eq!(masked_val["name"], "Claire Dupont");
    assert_eq!(masked_val["credit_card"], "****-****-****-1234");
    assert_eq!(masked_val["email"], "c***t@hospital.org");
    assert_eq!(masked_val["ssn"], "[REDACTED]");

    // json_extract is also masked
    let extracted_card = user_db
        .json_extract_str("customers", "cust_42", "$.credit_card")
        .unwrap()
        .unwrap();
    assert_eq!(extracted_card, "****-****-****-1234");

    // 2. Administrator access (role = "admin"): unmasked original data
    let admin_ctx = SecurityContext::new().with_role("admin");
    let admin_db = db.with_context(admin_ctx);

    let clear_doc = admin_db.json_get("customers", "cust_42").unwrap().unwrap();
    let clear_val = clear_doc.value();

    assert_eq!(clear_val["credit_card"], test_card);
    assert_eq!(clear_val["email"], test_email);
    assert_eq!(clear_val["ssn"], test_ssn);

    // 3. SQL query masking
    let user_sql = user_db
        .sql_query(
            "SELECT space, id, value FROM lilia_json WHERE space = 'customers';",
            &[],
        )
        .unwrap();
    assert_eq!(user_sql.rows.len(), 1);
    let sql_val: serde_json::Value =
        serde_json::from_str(user_sql.rows[0][2].as_str().unwrap()).unwrap();
    assert_eq!(sql_val["credit_card"], "****-****-****-1234");
    assert_eq!(sql_val["email"], "c***t@hospital.org");
    assert_eq!(sql_val["ssn"], "[REDACTED]");
}

#[test]
fn test_rls_persistence_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("rls_persist.lilia");

    {
        let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

        db.security()
            .create_policy(
                "tenant_isolation",
                Policy::new(
                    "documents",
                    "json_extract(data, '$.tenant_id') == current_context('tenant_id')",
                )
                .with_action(PolicyAction::All),
            )
            .unwrap();

        db.security()
            .create_masking_rule(MaskingRule::new(
                "documents",
                "$.credit_card",
                MaskFunction::CreditCard,
                Some("current_context('role') == 'admin'".to_string()),
            ))
            .unwrap();

        let policies = db.security().list_policies();
        assert_eq!(policies.len(), 1);
        assert_eq!(policies[0].name, "tenant_isolation");

        let masking = db.security().list_masking_rules();
        assert_eq!(masking.len(), 1);
        assert_eq!(masking[0].path, "$.credit_card");
    }

    // Reopen database from disk and verify persistent policies and masking rules
    {
        let db2 = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

        let policies = db2.security().list_policies();
        assert_eq!(policies.len(), 1);
        assert_eq!(policies[0].name, "tenant_isolation");
        assert_eq!(policies[0].policy.table, "documents");

        let masking = db2.security().list_masking_rules();
        assert_eq!(masking.len(), 1);
        assert_eq!(masking[0].table, "documents");
        assert_eq!(masking[0].path, "$.credit_card");

        // Verify that dropping policy persists
        assert!(db2.security().drop_policy("tenant_isolation").unwrap());
        assert_eq!(db2.security().list_policies().len(), 0);
    }
}
