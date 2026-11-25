//! The `lilia` binary, as a user runs it: each model, the ledger, zero-knowledge proofs, and
//! the errors it reports.

use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

/// Runs `lilia --database <database> --output json <arguments>` and returns its JSON output,
/// or the error it printed (as `Err`).
fn lilia(database: &Path, arguments: &[&str]) -> Result<Value, Value> {
    let output = Command::new(env!("CARGO_BIN_EXE_lilia"))
        .arg("--database")
        .arg(database)
        .args(["--output", "json", "--durability", "performance"])
        .args(arguments)
        .output()
        .unwrap();
    if output.status.success() {
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(serde_json::from_slice(&output.stdout).unwrap())
    } else {
        assert_eq!(String::from_utf8_lossy(&output.stdout), "");
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        Err(error["error"].clone())
    }
}

fn ok(database: &Path, arguments: &[&str]) -> Value {
    lilia(database, arguments).unwrap_or_else(|error| panic!("{arguments:?}: {error}"))
}

#[test]
fn keys_documents_indexes_and_sql() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");

    assert_eq!(ok(&db, &["init"])["ok"], true);
    let written = ok(&db, &["kv", "set", "settings", "theme", "dark"]);
    let entry = ok(&db, &["kv", "get", "settings", "theme"]);
    assert_eq!(entry["value"], json!(b"dark".to_vec()));
    assert_eq!(entry["version"], written["version"]);
    assert_eq!(
        ok(
            &db,
            &["kv", "set", "settings", "6b", "00ff", "--encoding", "hex"]
        )["deleted"],
        false
    );
    assert_eq!(
        ok(&db, &["kv", "scan", "settings"])
            .as_array()
            .unwrap()
            .len(),
        2
    );

    ok(
        &db,
        &[
            "json",
            "put",
            "users",
            "u1",
            r#"{"name": "Ada", "age": 36}"#,
        ],
    );
    ok(
        &db,
        &[
            "json",
            "put",
            "users",
            "u2",
            r#"{"name": "Grace", "age": 85}"#,
        ],
    );
    ok(&db, &["json", "create-index", "users", "by_age", "$.age"]);
    assert_eq!(
        ok(&db, &["json", "indexes", "users"])[0]["index_name"],
        "by_age"
    );
    let found = ok(&db, &["json", "find", "users", r#"{"by_age": 85}"#]);
    assert_eq!(found[0]["id"], "u2");

    let rows = ok(
        &db,
        &[
            "sql",
            "SELECT id FROM users WHERE json_get_int(value, 'age') > $1",
            "--param",
            "50",
        ],
    );
    assert_eq!(rows["rows"], json!([["u2"]]));
}

#[test]
fn batches_graphs_searches_and_vectors() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    let batch = dir.path().join("batch.json");
    std::fs::write(
        &batch,
        json!([
            {"model": "graph_add_edge", "graph": "g", "source": "a", "target": "b", "kind": "follows"},
            {"model": "fts_index", "collection": "notes", "doc_id": "n1", "content": "local first storage"},
        ])
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        ok(&db, &["batch", batch.to_str().unwrap()])
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(ok(&db, &["graph", "neighbors", "g", "a"]), json!(["b"]));
    assert_eq!(ok(&db, &["graph", "reach", "g", "a"]), json!(["a", "b"]));
    assert_eq!(
        ok(&db, &["fts", "search", "notes", "storage"])[0]["doc_id"],
        "n1"
    );

    ok(&db, &["vector", "create-index", "emb", "--dim", "2"]);
    ok(&db, &["vector", "set", "emb", "v1", "[1, 0]"]);
    ok(&db, &["vector", "set", "emb", "v2", "0, 1"]);
    assert_eq!(
        ok(
            &db,
            &["vector", "search", "emb", "[0.9, 0.1]", "--limit", "1"]
        )[0]["id"],
        "v1"
    );
    let hybrid = ok(&db, &["hybrid", "search", "emb", "--vector", "[0, 1]"]);
    assert_eq!(hybrid[0]["id"], "v2");
}

#[test]
fn crdts_bitemporal_entities_and_the_doctor() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    ok(&db, &["crdt", "counter", "inc", "c", "visits", "me", "3"]);
    ok(&db, &["crdt", "counter", "dec", "c", "visits", "you"]);
    assert_eq!(
        ok(&db, &["crdt", "counter", "get", "c", "visits"]),
        json!(2)
    );
    ok(&db, &["crdt", "set", "add", "c", "tags", "me", "red"]);
    assert_eq!(
        ok(&db, &["crdt", "set", "contains", "c", "tags", "red"]),
        json!(true)
    );

    ok(
        &db,
        &[
            "bitemp",
            "put",
            "prices",
            "p1",
            r#"{"eur": 10}"#,
            "--from",
            "2026-01-01",
            "--to",
            "infinity",
        ],
    );
    let price = ok(
        &db,
        &["bitemp", "get", "prices", "p1", "--for-vt", "2026-06-01"],
    );
    assert_eq!(price["value"]["eur"], 10);
    assert_eq!(
        ok(&db, &["bitemp", "history", "prices", "p1"])
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let doctor = ok(&db, &["doctor", "--deep"]);
    assert_eq!(doctor["deep"], true);
    assert!(doctor["storage"]["levels"].is_array());
}

#[test]
fn the_ledger_proves_records_and_commits() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    let ledger = |arguments: &[&str]| ok(&db, &[&["--ledger"], arguments].concat());

    ledger(&["json", "put", "contracts", "c1", r#"{"title": "NDA"}"#]);
    ledger(&["kv", "set", "ns", "k", "v"]);
    let root = ledger(&["ledger", "root"]);
    assert_eq!(root["commits"], 2);
    assert_eq!(ledger(&["ledger", "latest"])["commit_seq"], 2);

    for (proof, name) in [
        (
            ledger(&["ledger", "prove-doc", "contracts", "c1"]),
            "doc.json",
        ),
        (ledger(&["ledger", "prove-kv", "ns", "k"]), "kv.json"),
        (ledger(&["ledger", "prove-commit", "1"]), "commit.json"),
    ] {
        let file = dir.path().join(name);
        std::fs::write(&file, proof.to_string()).unwrap();
        let trusted = ledger(&[
            "ledger",
            "verify-proof",
            file.to_str().unwrap(),
            "--expected-root",
            root["merkle_root"].as_str().unwrap(),
        ]);
        assert_eq!(trusted["valid"], true, "{name}");
        let wrong = ledger(&[
            "ledger",
            "verify-proof",
            file.to_str().unwrap(),
            "--expected-root",
            &"00".repeat(32),
        ]);
        assert_eq!(wrong["valid"], false, "{name}");
    }
    assert_eq!(ledger(&["ledger", "verify-db"])["is_valid"], true);
}

#[test]
fn a_zero_knowledge_proof_answers_for_the_published_commitment() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    ok(
        &db,
        &[
            "json",
            "put",
            "accounts",
            "a1",
            r#"{"balance": 5000, "age": 34}"#,
        ],
    );
    let commitment = ok(&db, &["zk", "commit", "accounts", "a1"])["commitment"].clone();

    let bundle = ok(
        &db,
        &[
            "zk",
            "prove",
            "accounts",
            "a1",
            "--condition",
            "balance >= 1000 AND age >= 18",
        ],
    );
    assert_eq!(bundle["commitment_hex"], commitment);
    let file = dir.path().join("proof.json");
    std::fs::write(&file, bundle.to_string()).unwrap();
    assert_eq!(
        ok(&db, &["zk", "verify", "--proof", file.to_str().unwrap()])["valid"],
        true
    );

    let error = lilia(
        &db,
        &[
            "zk",
            "prove",
            "accounts",
            "a1",
            "--condition",
            "balance < 10",
        ],
    )
    .unwrap_err();
    assert_eq!(error["code"], "INVALID_INPUT");
}

#[test]
fn errors_are_structured_json() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    let malformed = lilia(&db, &["json", "put", "docs", "d1", "{"]).unwrap_err();
    assert_eq!(malformed["code"], "INVALID_INPUT");
    assert!(malformed["request_id"].is_string());

    let written = ok(&db, &["kv", "set", "ns", "k", "v"]);
    let stale = written["version"]
        .as_str()
        .map_or_else(|| written["version"].to_string(), str::to_owned);
    ok(&db, &["kv", "set", "ns", "k", "w"]);
    let conflict = lilia(&db, &["kv", "set", "ns", "k", "x", "--if-version", &stale]).unwrap_err();
    assert_eq!(conflict["code"], "CONFLICT");

    let unknown = lilia(&db, &["--profile", "desktop", "init"]).unwrap_err();
    assert_eq!(unknown["code"], "INVALID_INPUT");
}
