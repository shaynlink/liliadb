//! Row-level security and masking apply to every read path and every write of a session.

mod common;

use common::small_options;
use lilia_db::{
    Database, Error, IndexOptions, MaskFunction, MaskingRule, Policy, PolicyAction, Scan,
    SecurityContext,
};
use lilia_engine::Durability;
use serde_json::{json, Map, Value};

fn tenant(id: &str) -> SecurityContext {
    SecurityContext::from_pair("tenant", id)
}

fn query(value: &Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn setup(dir: &std::path::Path) -> Database {
    let db = Database::open(dir, small_options(Durability::Performance, false)).unwrap();
    let s = db.system();
    for (id, owner, email) in [
        ("a1", "acme", "ann@acme.io"),
        ("a2", "acme", "al@acme.io"),
        ("g1", "globex", "gil@globex.io"),
    ] {
        s.json_put(
            "accounts",
            id,
            &json!({"tenant": owner, "email": email, "plan": "pro"}),
        )
        .unwrap();
    }
    db.create_index("accounts", "by_plan", "$.plan", &IndexOptions::default())
        .unwrap();
    db.create_policy(
        "tenant_isolation",
        Policy::new("accounts", "$.tenant == current_context('tenant')"),
    )
    .unwrap();
    db
}

#[test]
fn read_policies_filter_every_read_path() {
    let dir = tempfile::tempdir().unwrap();
    let db = setup(dir.path());
    let acme = db.session(tenant("acme"));
    assert!(acme.json_get("accounts", "a1").unwrap().is_some());
    assert!(
        acme.json_get("accounts", "g1").unwrap().is_none(),
        "another tenant's document is invisible"
    );
    let scanned: Vec<String> = acme
        .json_scan("accounts", &Scan::all())
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(scanned, ["a1", "a2"]);
    let found = acme
        .json_find(
            "accounts",
            &query(&json!({"plan": "pro"})),
            &Scan::all().limit(5),
        )
        .unwrap();
    assert_eq!(found.len(), 2, "an index lookup is filtered too");
    let tx = acme.transaction();
    assert!(tx.json_get("accounts", "g1").unwrap().is_none());
    assert_eq!(tx.json_scan("accounts", &Scan::all()).unwrap().len(), 2);
    assert_eq!(
        db.system()
            .json_scan("accounts", &Scan::all())
            .unwrap()
            .len(),
        3,
        "system access bypasses policies"
    );
    assert!(db
        .session(SecurityContext::new())
        .json_scan("accounts", &Scan::all())
        .unwrap()
        .is_empty());

    assert!(db.drop_policy("tenant_isolation").unwrap());
    assert_eq!(acme.json_scan("accounts", &Scan::all()).unwrap().len(), 3);
    assert!(!db.drop_policy("tenant_isolation").unwrap());
}

#[test]
fn write_policies_check_inserts_updates_and_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let db = setup(dir.path());
    let acme = db.session(tenant("acme"));
    acme.json_put("accounts", "a3", &json!({"tenant": "acme", "plan": "free"}))
        .unwrap();
    assert!(
        matches!(
            acme.json_put("accounts", "a4", &json!({"tenant": "globex"})),
            Err(Error::Denied(_))
        ),
        "insert for another tenant"
    );
    assert!(
        matches!(
            acme.json_put("accounts", "g1", &json!({"tenant": "acme"})),
            Err(Error::Denied(_))
        ),
        "taking over another tenant's row"
    );
    assert!(
        matches!(
            acme.json_put("accounts", "a1", &json!({"tenant": "globex"})),
            Err(Error::Denied(_))
        ),
        "handing a row to another tenant"
    );
    assert!(matches!(
        acme.json_delete("accounts", "g1"),
        Err(Error::Denied(_))
    ));
    assert!(acme.json_delete("accounts", "a3").unwrap());
    assert_eq!(
        db.system()
            .json_get("accounts", "g1")
            .unwrap()
            .unwrap()
            .extract_str("tenant"),
        Some("globex")
    );

    let tx = acme.transaction();
    tx.json_put("accounts", "a5", &json!({"tenant": "acme"}))
        .unwrap();
    tx.json_put("accounts", "g2", &json!({"tenant": "globex"}))
        .unwrap();
    assert!(matches!(tx.commit(), Err(Error::Denied(_))));
    assert!(
        db.system().json_get("accounts", "a5").unwrap().is_none(),
        "a denied transaction writes nothing"
    );

    db.create_policy(
        "reads_only",
        Policy::new("kv_secrets", "$.owner == current_context('tenant')")
            .with_action(PolicyAction::Select),
    )
    .unwrap();
    db.create_policy(
        "owned_writes",
        Policy::new("kv_secrets", "$.owner == current_context('tenant')")
            .with_action(PolicyAction::Insert),
    )
    .unwrap();
    assert!(matches!(
        db.create_policy("owned_writes", Policy::new("x", "true")),
        Err(Error::InvalidArgument(_))
    ));
    acme.kv_put("kv_secrets", b"k1", br#"{"owner":"acme","v":1}"#)
        .unwrap();
    assert!(matches!(
        acme.kv_put("kv_secrets", b"k2", br#"{"owner":"globex"}"#),
        Err(Error::Denied(_))
    ));
    db.system()
        .kv_put("kv_secrets", b"k3", br#"{"owner":"globex"}"#)
        .unwrap();
    let visible: Vec<Vec<u8>> = acme
        .kv_scan("kv_secrets", &Scan::all())
        .unwrap()
        .into_iter()
        .map(|e| e.key)
        .collect();
    assert_eq!(visible, [b"k1".to_vec()]);
    assert!(acme.kv_get("kv_secrets", b"k3").unwrap().is_none());
    assert_eq!(
        db.policies()
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["owned_writes", "reads_only", "tenant_isolation"]
    );
}

#[test]
fn masking_rewrites_fields_unless_the_caller_is_exempt_and_queries_see_masked_values() {
    let dir = tempfile::tempdir().unwrap();
    let db = setup(dir.path());
    db.create_masking_rule(MaskingRule::new(
        "accounts",
        "$.email",
        MaskFunction::Email,
        Some("has_role('auditor')".into()),
    ))
    .unwrap();
    let acme = db.session(tenant("acme"));
    let masked = acme.json_get("accounts", "a1").unwrap().unwrap();
    assert_eq!(masked.extract_str("email"), Some("a***n@acme.io"));
    let auditor = db.session(tenant("acme").with_role("auditor"));
    assert_eq!(
        auditor
            .json_get("accounts", "a1")
            .unwrap()
            .unwrap()
            .extract_str("email"),
        Some("ann@acme.io")
    );
    assert!(
        acme.json_find(
            "accounts",
            &query(&json!({"email": "ann@acme.io"})),
            &Scan::all()
        )
        .unwrap()
        .is_empty(),
        "a masked field cannot be probed"
    );
    assert_eq!(
        auditor
            .json_find(
                "accounts",
                &query(&json!({"email": "ann@acme.io"})),
                &Scan::all()
            )
            .unwrap()
            .len(),
        1
    );
    assert!(acme
        .json_scan("accounts", &Scan::all())
        .unwrap()
        .iter()
        .all(|e| e.extract_str("email").unwrap().contains("***")));

    db.create_masking_rule(MaskingRule::new(
        "accounts",
        "email",
        MaskFunction::Redact,
        None,
    ))
    .unwrap();
    assert_eq!(
        db.masking_rules().len(),
        1,
        "a rule on the same path replaces the previous one"
    );
    assert_eq!(
        auditor
            .json_get("accounts", "a1")
            .unwrap()
            .unwrap()
            .extract_str("email"),
        Some("[REDACTED]")
    );
    assert!(db.drop_masking_rule("accounts", "$.email").unwrap());
    assert_eq!(
        acme.json_get("accounts", "a1")
            .unwrap()
            .unwrap()
            .extract_str("email"),
        Some("ann@acme.io")
    );
    db.close().unwrap();

    let db = Database::open(dir.path(), small_options(Durability::Performance, false)).unwrap();
    assert!(
        db.session(tenant("acme"))
            .json_get("accounts", "g1")
            .unwrap()
            .is_none(),
        "policies persist"
    );
}
