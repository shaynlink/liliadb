//! SQL over the data models: every table, JSON functions, parameters, policies and masking,
//! queries in the past, refused writes.

mod common;

use std::time::Duration;

use common::small_options;
use lilia_core::TimeSeriesPoint;
use lilia_db::{Database, Error, MaskFunction, MaskingRule, Options, Policy, SecurityContext};
use lilia_engine::Durability;
use serde_json::{json, Value};

fn open(dir: &std::path::Path) -> Database {
    let options = Options {
        history_retention: Duration::from_secs(3600),
        ..small_options(Durability::Performance, false)
    };
    Database::open(dir, options).unwrap()
}

fn rows(db: &lilia_db::Session<'_>, sql: &str, params: &[Value]) -> Vec<Vec<Value>> {
    db.sql(sql, params).unwrap().rows
}

#[test]
fn every_model_table_answers_queries() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let s = db.system();
    s.json_put("docs", "a", &json!({"title": "NDA", "cost": 500}))
        .unwrap();
    s.json_put("docs", "b", &json!({"title": "Lease", "cost": 750}))
        .unwrap();
    s.json_put("other", "c", &json!({"title": "Other"}))
        .unwrap();
    s.kv_put("settings", b"theme", b"dark").unwrap();
    s.kv_put("cache", b"k", b"v").unwrap();
    let graph = s.graph("g");
    graph
        .set_node("ann", "person", &json!({"age": 31}))
        .unwrap();
    graph
        .set_node("bob", "person", &json!({"age": 40}))
        .unwrap();
    graph.add_edge("ann", "bob", "knows", 0.5).unwrap();
    let points: Vec<TimeSeriesPoint> = (0..10)
        .map(|i| TimeSeriesPoint {
            timestamp: i,
            value: f64::from(u32::try_from(i).unwrap()),
        })
        .collect();
    s.series("cpu").append(&points).unwrap();

    let result = s
        .sql(
            "SELECT id, json_get_int(value, 'cost') AS cost FROM docs ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(result.columns, ["id", "cost"]);
    assert_eq!(
        result.rows,
        [vec![json!("a"), json!(500)], vec![json!("b"), json!(750)]]
    );
    assert_eq!(
        rows(
            &s,
            "SELECT value->>'title' FROM docs WHERE json_get_int(value, 'cost') > $1",
            &[json!(600)]
        ),
        [vec![json!("Lease")]]
    );
    assert_eq!(
        rows(
            &s,
            "SELECT space, count(*) FROM lilia_json GROUP BY space ORDER BY space",
            &[]
        ),
        [
            vec![json!("docs"), json!(2)],
            vec![json!("other"), json!(1)]
        ]
    );
    assert_eq!(
        rows(
            &s,
            "SELECT namespace, arrow_cast(value, 'Utf8') FROM lilia_kv ORDER BY namespace",
            &[]
        ),
        [
            vec![json!("cache"), json!("v")],
            vec![json!("settings"), json!("dark")]
        ]
    );
    assert_eq!(
        rows(
            &s,
            "SELECT n.id, json_get_int(n.metadata, 'age') FROM lilia_graph_nodes n \
             JOIN lilia_graph_edges e ON e.target = n.id WHERE e.source = 'ann'",
            &[]
        ),
        [vec![json!("bob"), json!(40)]]
    );
    assert_eq!(
        rows(
            &s,
            "SELECT series, count(*), sum(value) FROM lilia_timeseries GROUP BY series",
            &[]
        ),
        [vec![json!("cpu"), json!(10), json!(45.0)]]
    );
    assert_eq!(
        rows(&s, "SELECT count(*) FROM empty_space", &[]),
        [vec![json!(0)]]
    );
}

#[test]
fn rows_pass_policies_and_masking_and_the_past_is_queryable() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let s = db.system();
    let first = s
        .json_put(
            "accounts",
            "a1",
            &json!({"owner": "ann", "email": "ann@example.com"}),
        )
        .unwrap();
    s.json_put(
        "accounts",
        "a2",
        &json!({"owner": "bob", "email": "bob@example.com"}),
    )
    .unwrap();
    db.create_policy(
        "own",
        Policy::new("accounts", "$.owner == current_context('user')"),
    )
    .unwrap();
    db.create_masking_rule(MaskingRule::new(
        "accounts",
        "$.email",
        MaskFunction::Redact,
        None,
    ))
    .unwrap();

    let ann = db.session(SecurityContext::from_pair("user", "ann"));
    let seen = rows(&ann, "SELECT id, value->>'email' FROM accounts", &[]);
    assert_eq!(seen.len(), 1, "the policy hides bob's account");
    assert_eq!(seen[0][0], json!("a1"));
    assert_ne!(seen[0][1], json!("ann@example.com"), "the email is masked");
    assert_eq!(
        rows(&ann, "SELECT count(*) FROM lilia_json", &[]),
        [vec![json!(1)]]
    );

    let then = s.at_version(first).unwrap();
    assert_eq!(
        rows(&then, "SELECT id FROM accounts", &[]),
        [vec![json!("a1")]]
    );
}

#[test]
fn writes_and_bad_queries_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let s = db.system();
    s.json_put("docs", "a", &json!({"n": 1})).unwrap();
    for sql in [
        "INSERT INTO docs VALUES ('b', '{}', 1)",
        "DELETE FROM docs",
        "CREATE TABLE t AS SELECT 1",
        "SELECT nope FROM docs",
        "SELECT 1; SELECT 2",
        "not sql",
    ] {
        assert!(matches!(s.sql(sql, &[]), Err(Error::Sql(_))), "{sql}");
    }
    assert!(matches!(
        s.sql("SELECT $1", &[json!({"a": 1})]),
        Err(Error::Sql(_))
    ));
    assert_eq!(rows(&s, "SELECT count(*) FROM docs", &[]), [vec![json!(1)]]);
}
