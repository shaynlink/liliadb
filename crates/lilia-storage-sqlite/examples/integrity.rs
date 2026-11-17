use lilia_core::{
    BatchOperation, CollectionOptions, ErrorCode, KeyMapping, RefAction, Reference,
};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let db = Database::open(DatabaseOptions::durable(temp_dir.path().join("integrity.lilia")))?;

    println!("[1] Declarative CHECK on a JSON collection");
    db.json().create_collection(
        "accounts",
        CollectionOptions {
            check: Some("$.balance >= 0.0 AND length($.iban) == 27".into()),
            ..CollectionOptions::default()
        },
    )?;
    let overdrawn = db.batch(&[BatchOperation::JsonPut {
        space: "accounts".into(),
        id: "acc_1".into(),
        value: json!({ "balance": -10.0, "iban": "F".repeat(27) }),
        if_version: None,
    }]);
    let error = overdrawn.expect_err("a negative balance must be rejected");
    assert_eq!(error.code, ErrorCode::ConstraintViolation);
    println!("  rejected: {} ({:?})", error.message, error.details);

    println!("\n[2] Graph -> JSON reference with ON DELETE CASCADE");
    db.integrity()
        .add_reference(Reference::graph_nodes("social", "users"))?;
    db.batch(&[
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "user_42".into(),
            value: json!({ "name": "Alice" }),
            if_version: None,
        },
        BatchOperation::GraphSetNode {
            graph: "social".into(),
            id: "alice".into(),
            kind: "person".into(),
            metadata: json!({}),
            doc_space: Some("users".into()),
            doc_id: Some("user_42".into()),
        },
    ])?;
    db.batch(&[BatchOperation::JsonDelete {
        space: "users".into(),
        id: "user_42".into(),
        if_version: None,
    }])?;
    assert!(db.graph_get_node("social", "alice")?.is_none());
    println!("  deleting users/user_42 removed the node 'alice' and its edges");

    println!("\n[3] KV -> JSON reference with RESTRICT");
    db.integrity().add_reference(
        Reference::kv("sessions", "users")
            .key(KeyMapping::Prefix { separator: ':' })
            .on_delete(RefAction::Restrict),
    )?;
    let orphan = db.batch(&[BatchOperation::KvSet {
        namespace: "sessions".into(),
        key: b"user_99:token".to_vec(),
        value: b"secret".to_vec(),
        if_version: None,
        expires_at_ms: None,
    }]);
    println!("  orphan session rejected: {}", orphan.expect_err("parent is missing").message);
    Ok(())
}
