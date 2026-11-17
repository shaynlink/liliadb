use lilia_core::{BatchOperation, ErrorCode, KeyMapping, LiliaError, RefAction, Reference};
use lilia_storage_sqlite::{
    CollectionOptions, Database, DatabaseOptions, ShardingConfig, StorageTier,
};
use serde_json::{json, Value};

const ACCOUNT_CHECK: &str = "$.balance >= 0.0 AND length($.iban) == 27";

fn iban() -> String {
    "F".repeat(27)
}

fn put(space: &str, id: &str, value: Value) -> BatchOperation {
    BatchOperation::JsonPut {
        space: space.into(),
        id: id.into(),
        value,
        if_version: None,
    }
}

fn delete(space: &str, id: &str) -> BatchOperation {
    BatchOperation::JsonDelete {
        space: space.into(),
        id: id.into(),
        if_version: None,
    }
}

fn node(graph: &str, id: &str, space: &str, doc_id: Option<&str>) -> BatchOperation {
    BatchOperation::GraphSetNode {
        graph: graph.into(),
        id: id.into(),
        kind: "person".into(),
        metadata: json!({}),
        doc_space: Some(space.into()),
        doc_id: doc_id.map(Into::into),
    }
}

fn plain_node(graph: &str, id: &str) -> BatchOperation {
    BatchOperation::GraphSetNode {
        graph: graph.into(),
        id: id.into(),
        kind: "person".into(),
        metadata: json!({}),
        doc_space: None,
        doc_id: None,
    }
}

fn edge(graph: &str, source: &str, target: &str) -> BatchOperation {
    BatchOperation::GraphAddEdge {
        graph: graph.into(),
        source: source.into(),
        target: target.into(),
        kind: "knows".into(),
        weight: 1.0,
    }
}

fn kv_set(namespace: &str, key: &str) -> BatchOperation {
    BatchOperation::KvSet {
        namespace: namespace.into(),
        key: key.as_bytes().to_vec(),
        value: b"v".to_vec(),
        if_version: None,
        expires_at_ms: None,
    }
}

fn open_in(directory: &tempfile::TempDir) -> Database {
    Database::open(DatabaseOptions::durable(directory.path().join("app.lilia"))).unwrap()
}

fn open() -> (tempfile::TempDir, Database) {
    let directory = tempfile::tempdir().unwrap();
    let database = open_in(&directory);
    (directory, database)
}

fn violation(error: &LiliaError, kind: &str) {
    assert_eq!(error.code, ErrorCode::ConstraintViolation, "{error:?}");
    assert!(!error.retryable);
    let details = error.details.as_ref().expect("structured details");
    assert_eq!(details["kind"], kind, "{details}");
    assert!(details["constraint"].is_string());
}

fn node_count(database: &Database, graph: &str) -> usize {
    database.graph_list_nodes(graph, None, 1000).unwrap().len()
}

fn edge_count(database: &Database, graph: &str) -> usize {
    database
        .graph_list_edges(graph, None, None, None, 1000)
        .unwrap()
        .len()
}

fn accounts(database: &Database) {
    database
        .json()
        .create_collection(
            "accounts",
            CollectionOptions {
                check: Some(ACCOUNT_CHECK.into()),
                ..CollectionOptions::default()
            },
        )
        .unwrap();
}

#[test]
fn check_rejects_violating_documents_on_every_write_path() {
    let (_directory, database) = open();
    accounts(&database);

    database
        .batch(&[put("accounts", "a", json!({ "balance": 10.0, "iban": iban() }))])
        .unwrap();

    let negative = database
        .batch(&[put("accounts", "b", json!({ "balance": -1.0, "iban": iban() }))])
        .unwrap_err();
    violation(&negative, "check");
    let details = negative.details.unwrap();
    assert_eq!(details["model"], "json");
    assert_eq!(details["collection"], "accounts");
    assert_eq!(details["id"], "b");
    assert_eq!(details["expression"], ACCOUNT_CHECK);
    assert!(database.json_get("accounts", "b").unwrap().is_none());

    let raw = BatchOperation::JsonPutRaw {
        space: "accounts".into(),
        id: "raw".into(),
        data: br#"{"balance": 1.0, "iban": "FR76"}"#.to_vec(),
        if_version: None,
    };
    violation(&database.batch(&[raw]).unwrap_err(), "check");

    let hlc = BatchOperation::JsonPutHlc {
        space: "accounts".into(),
        id: "hlc".into(),
        value: json!({ "balance": -5.0, "iban": iban() }),
        hlc: None,
        parent_hlc: None,
        strategy: lilia_core::ConflictResolutionStrategy::LastWriteWins,
    };
    violation(&database.batch(&[hlc]).unwrap_err(), "check");

    let code = serde_json::to_value(negative.code).unwrap();
    assert_eq!(code, "CONSTRAINT_VIOLATION");
}

#[test]
fn rejected_operation_rolls_back_the_whole_batch() {
    let (_directory, database) = open();
    accounts(&database);

    let error = database
        .batch(&[
            kv_set("audit", "entry"),
            put("accounts", "bad", json!({ "balance": -1.0, "iban": iban() })),
        ])
        .unwrap_err();
    violation(&error, "check");
    assert!(database.kv_get("audit", b"entry").unwrap().is_none());
}

#[test]
fn missing_fields_pass_unless_required_explicitly() {
    let (_directory, database) = open();
    accounts(&database);
    database
        .batch(&[put("accounts", "partial", json!({ "owner": "alice" }))])
        .unwrap();

    database
        .json()
        .create_collection(
            "strict",
            CollectionOptions {
                check: Some("$.balance IS NOT NULL AND $.balance >= 0".into()),
                ..CollectionOptions::default()
            },
        )
        .unwrap();
    let error = database
        .batch(&[put("strict", "x", json!({ "owner": "alice" }))])
        .unwrap_err();
    violation(&error, "check");
}

#[test]
fn create_collection_validates_expression_and_existing_documents() {
    let (_directory, database) = open();
    database
        .batch(&[put("ledger", "old", json!({ "balance": -3.0 }))])
        .unwrap();

    let error = database
        .json()
        .create_collection(
            "ledger",
            CollectionOptions {
                check: Some("$.balance >= 0".into()),
                ..CollectionOptions::default()
            },
        )
        .unwrap_err();
    violation(&error, "check");
    assert_eq!(error.details.unwrap()["existing_violations"], json!(["old"]));

    let unknown_function = database
        .json()
        .create_collection(
            "fn",
            CollectionOptions {
                check: Some("composite($.a) == 'x'".into()),
                ..CollectionOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(unknown_function.code, ErrorCode::InvalidInput);

    accounts(&database);
    let duplicate = database
        .json()
        .create_collection("accounts", CollectionOptions::default())
        .unwrap_err();
    assert_eq!(duplicate.code, ErrorCode::Conflict);
}

#[test]
fn set_check_replaces_and_drops_the_expression() {
    let (_directory, database) = open();
    accounts(&database);
    database.json().set_check("accounts", Some("$.balance >= 100")).unwrap();
    assert_eq!(
        database.integrity().checks(),
        vec![("accounts".to_owned(), "$.balance >= 100".to_owned())]
    );
    violation(
        &database
            .batch(&[put("accounts", "a", json!({ "balance": 5.0 }))])
            .unwrap_err(),
        "check",
    );
    database.json().set_check("accounts", None).unwrap();
    database
        .batch(&[put("accounts", "a", json!({ "balance": 5.0 }))])
        .unwrap();
}

#[test]
fn check_survives_reopen() {
    let directory = tempfile::tempdir().unwrap();
    {
        let database = open_in(&directory);
        accounts(&database);
    }
    let database = open_in(&directory);
    violation(
        &database
            .batch(&[put("accounts", "b", json!({ "balance": -1.0, "iban": iban() }))])
            .unwrap_err(),
        "check",
    );
}

#[test]
fn graph_reference_cascades_node_and_edges_atomically() {
    let (_directory, database) = open();
    database
        .integrity()
        .add_reference(Reference::graph_nodes("social", "users"))
        .unwrap();
    database
        .batch(&[
            put("users", "user_42", json!({ "name": "alice" })),
            put("users", "user_7", json!({ "name": "bob" })),
            node("social", "n42", "users", Some("user_42")),
            node("social", "n7", "users", Some("user_7")),
            plain_node("social", "free"),
            edge("social", "n42", "n7"),
            edge("social", "n7", "n42"),
            edge("social", "n7", "free"),
        ])
        .unwrap();

    database.batch(&[delete("users", "user_42")]).unwrap();

    assert!(database.graph_get_node("social", "n42").unwrap().is_none());
    assert!(database.graph_get_node("social", "n7").unwrap().is_some());
    assert!(database.graph_get_node("social", "free").unwrap().is_some());
    assert_eq!(edge_count(&database, "social"), 1);
}

#[test]
fn failed_batch_does_not_apply_cascades() {
    let (_directory, database) = open();
    accounts(&database);
    database
        .integrity()
        .add_reference(Reference::graph_nodes("social", "users"))
        .unwrap();
    database
        .batch(&[
            put("users", "u", json!({})),
            node("social", "n", "users", Some("u")),
        ])
        .unwrap();

    let error = database
        .batch(&[
            delete("users", "u"),
            put("accounts", "bad", json!({ "balance": -1.0, "iban": iban() })),
        ])
        .unwrap_err();
    violation(&error, "check");
    assert!(database.json_get("users", "u").unwrap().is_some());
    assert!(database.graph_get_node("social", "n").unwrap().is_some());
}

#[test]
fn graph_node_defaults_doc_id_and_requires_existing_parent() {
    let (_directory, database) = open();
    database
        .integrity()
        .add_reference(Reference::graph_nodes("social", "users"))
        .unwrap();
    database.batch(&[put("users", "alice", json!({ "age": 30 }))]).unwrap();

    database.batch(&[node("social", "alice", "users", None)]).unwrap();
    let stored = database.graph_get_node("social", "alice").unwrap().unwrap();
    assert_eq!(stored.doc_id.as_deref(), Some("alice"));
    assert_eq!(
        database.graph_get_node_document("social", "alice").unwrap(),
        Some(json!({ "age": 30 }))
    );

    let missing = database
        .batch(&[node("social", "ghost", "users", Some("ghost"))])
        .unwrap_err();
    violation(&missing, "reference");
    assert!(database.graph_get_node("social", "ghost").unwrap().is_none());

    let undeclared = database
        .batch(&[node("social", "x", "orders", Some("o1"))])
        .unwrap_err();
    violation(&undeclared, "reference");

    let direct = database
        .graph_set_node_doc("social", "y", "person", Some("users"), Some("nobody"), &json!({}))
        .unwrap_err();
    violation(&direct, "reference");
    assert!(!database.graph_delete_node("social", "absent").unwrap());
}

#[test]
fn implicit_kind_link_is_gone() {
    let (_directory, database) = open();
    database.batch(&[put("person", "p", json!({ "n": 1 }))]).unwrap();
    database.batch(&[plain_node("g", "p")]).unwrap();
    assert!(database.graph_get_node_document("g", "p").unwrap().is_none());
}

#[test]
fn restrict_blocks_deletion_while_children_exist() {
    let (_directory, database) = open();
    database
        .integrity()
        .add_reference(Reference::graph_nodes("social", "users").on_delete(RefAction::Restrict))
        .unwrap();
    database
        .batch(&[
            put("users", "u", json!({})),
            node("social", "n", "users", Some("u")),
        ])
        .unwrap();

    let error = database.batch(&[delete("users", "u")]).unwrap_err();
    violation(&error, "restrict");
    assert_eq!(error.details.unwrap()["children"], 1);
    assert!(database.json_get("users", "u").unwrap().is_some());

    database
        .batch(&[BatchOperation::GraphDeleteNode {
            graph: "social".into(),
            id: "n".into(),
        }])
        .unwrap();
    database.batch(&[delete("users", "u")]).unwrap();
}

#[test]
fn set_null_detaches_children_without_deleting_them() {
    let (_directory, database) = open();
    database
        .integrity()
        .add_reference(Reference::graph_nodes("social", "users").on_delete(RefAction::SetNull))
        .unwrap();
    database
        .batch(&[
            put("users", "u", json!({})),
            node("social", "n", "users", Some("u")),
            edge("social", "n", "n"),
        ])
        .unwrap();

    database.batch(&[delete("users", "u")]).unwrap();
    let detached = database.graph_get_node("social", "n").unwrap().unwrap();
    assert!(detached.doc_space.is_none() && detached.doc_id.is_none());
    assert_eq!(edge_count(&database, "social"), 1);
}

#[test]
fn required_graph_reference_rejects_unlinked_nodes() {
    let (_directory, database) = open();
    database
        .integrity()
        .add_reference(Reference::graph_nodes("social", "users").required(true))
        .unwrap();
    violation(
        &database.batch(&[plain_node("social", "free")]).unwrap_err(),
        "reference",
    );
}

#[test]
fn kv_reference_validates_parent_and_honours_actions() {
    let (_directory, database) = open();
    database
        .integrity()
        .add_reference(
            Reference::kv("carts", "users")
                .key(KeyMapping::Prefix { separator: ':' })
                .on_delete(RefAction::Cascade),
        )
        .unwrap();
    database
        .integrity()
        .add_reference(Reference::kv("sessions", "users"))
        .unwrap();
    database
        .batch(&[put("users", "user_42", json!({})), put("users", "user_420", json!({}))])
        .unwrap();

    violation(
        &database.batch(&[kv_set("carts", "user_9:items")]).unwrap_err(),
        "reference",
    );
    violation(
        &database.batch(&[kv_set("carts", "no-separator")]).unwrap_err(),
        "reference",
    );
    database
        .batch(&[
            kv_set("carts", "user_42:items"),
            kv_set("carts", "user_42:wishlist"),
            kv_set("carts", "user_420:items"),
            kv_set("sessions", "user_42"),
        ])
        .unwrap();

    let restricted = database.batch(&[delete("users", "user_42")]).unwrap_err();
    violation(&restricted, "restrict");
    assert!(database.kv_get("carts", b"user_42:items").unwrap().is_some());

    database
        .batch(&[BatchOperation::KvDelete {
            namespace: "sessions".into(),
            key: b"user_42".to_vec(),
            if_version: None,
        }])
        .unwrap();
    database.batch(&[delete("users", "user_42")]).unwrap();
    assert!(database.kv_get("carts", b"user_42:items").unwrap().is_none());
    assert!(database.kv_get("carts", b"user_42:wishlist").unwrap().is_none());
    assert!(database.kv_get("carts", b"user_420:items").unwrap().is_some());
}

#[test]
fn declaring_a_reference_validates_existing_children() {
    let (_directory, database) = open();
    database.batch(&[node("social", "dangling", "users", Some("nobody"))]).unwrap();
    let error = database
        .integrity()
        .add_reference(Reference::graph_nodes("social", "users"))
        .unwrap_err();
    violation(&error, "reference");
    assert_eq!(error.details.unwrap()["existing_violations"], json!(["dangling"]));

    database.batch(&[put("users", "nobody", json!({}))]).unwrap();
    database
        .integrity()
        .add_reference(Reference::graph_nodes("social", "users"))
        .unwrap();
    let duplicate = database
        .integrity()
        .add_reference(Reference::graph_nodes("social", "users"))
        .unwrap_err();
    assert_eq!(duplicate.code, ErrorCode::Conflict);
}

#[test]
fn references_survive_reopen_and_can_be_dropped() {
    let directory = tempfile::tempdir().unwrap();
    {
        let database = open_in(&directory);
        database
            .integrity()
            .add_reference(Reference::graph_nodes("social", "users"))
            .unwrap();
    }
    let database = open_in(&directory);
    assert_eq!(database.integrity().references().len(), 1);
    violation(
        &database
            .batch(&[node("social", "n", "users", Some("missing"))])
            .unwrap_err(),
        "reference",
    );
    assert!(database.integrity().drop_reference("fk_graph_social_users").unwrap());
    assert!(!database.integrity().drop_reference("fk_graph_social_users").unwrap());
    database
        .batch(&[node("social", "n", "users", Some("missing"))])
        .unwrap();
}

#[test]
fn contracts_require_the_disk_tier() {
    let (_directory, database) = open();
    database.collection("volatile").memory().json().unwrap();
    let error = database
        .json()
        .set_check("volatile", Some("$.a > 0"))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Unsupported);

    accounts(&database);
    let demote = database
        .demote_collection(lilia_core::CollectionKind::Json, "accounts", StorageTier::Memory)
        .unwrap_err();
    assert_eq!(demote.code, ErrorCode::Unsupported);
    let retier = database.collection("accounts").memory().json();
    assert_eq!(retier.err().unwrap().code, ErrorCode::Unsupported);
}

#[test]
fn check_cannot_cover_encrypted_fields() {
    let (_directory, database) = open();
    database
        .collection("people")
        .field_encryption(
            "$.ssn",
            lilia_core::FieldCipherAlgorithm::RandomizedGcm,
            "vault:team",
        )
        .json()
        .unwrap();
    let error = database
        .json()
        .set_check("people", Some("length($.ssn) == 9"))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidInput);
}

#[test]
fn contracts_hold_with_temporal_sharding() {
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(
        DatabaseOptions::durable(directory.path().join("app.lilia"))
            .with_sharding(ShardingConfig::new()),
    )
    .unwrap();
    accounts(&database);
    database
        .integrity()
        .add_reference(Reference::graph_nodes("social", "users"))
        .unwrap();
    database
        .batch(&[
            put("users", "u", json!({})),
            node("social", "n", "users", Some("u")),
            put("accounts", "ok", json!({ "balance": 1.0, "iban": iban() })),
        ])
        .unwrap();
    violation(
        &database
            .batch(&[put("accounts", "bad", json!({ "balance": -1.0, "iban": iban() }))])
            .unwrap_err(),
        "check",
    );
    violation(
        &database
            .batch(&[node("social", "m", "users", Some("missing"))])
            .unwrap_err(),
        "reference",
    );
    database.batch(&[delete("users", "u")]).unwrap();
    assert_eq!(node_count(&database, "social"), 0);
}

#[test]
fn raw_sql_cannot_write_governed_tables_once_contracts_exist() {
    let (_directory, database) = open();
    database.batch(&[put("docs", "a", json!({ "n": 1 }))]).unwrap();
    accounts(&database);

    for statement in [
        "DELETE FROM _lilia_json WHERE space = 'docs';",
        "UPDATE _lilia_graph_nodes SET doc_space = NULL;",
        "INSERT INTO _lilia_kv(namespace, key, value, version) VALUES('n', x'01', x'02', 1);",
    ] {
        let error = database.sql_query(statement, &[]).unwrap_err();
        assert_eq!(error.code, ErrorCode::Unsupported, "{statement}: {error:?}");
    }
    assert!(database.json_get("docs", "a").unwrap().is_some());

    let rows = database
        .sql_query("SELECT count(*) FROM _lilia_json;", &[])
        .unwrap();
    assert_eq!(rows.rows[0][0], json!(1));
    database.sql_query("CREATE TABLE scratch(x INTEGER);", &[]).unwrap();
    database.sql_query("INSERT INTO scratch(x) VALUES (1);", &[]).unwrap();
    database.batch(&[delete("docs", "a")]).unwrap();
}
