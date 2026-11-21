//! The graph model: nodes, edges in both directions, cascades, traversals, transactions, ledger.

mod common;

use common::small_options;
use lilia_db::{Batch, Database, Direction, Error, GraphWrite, LedgerVerifyMode, Scan, Write};
use lilia_engine::Durability;
use serde_json::json;

fn open(dir: &std::path::Path, ledger: bool) -> Database {
    Database::open(dir, small_options(Durability::Performance, ledger)).unwrap()
}

fn names(nodes: &[String]) -> Vec<&str> {
    nodes.iter().map(String::as_str).collect()
}

#[test]
fn nodes_edges_and_traversals() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();
    let g = s.graph("deps");
    for (id, kind) in [
        ("app", "service"),
        ("api", "service"),
        ("db", "store"),
        ("cache", "store"),
        ("log", "lib"),
    ] {
        g.set_node(id, kind, &json!({"name": id})).unwrap();
    }
    g.add_edge("app", "api", "calls", 1.0).unwrap();
    g.add_edge("api", "db", "reads", 2.5).unwrap();
    g.add_edge("api", "cache", "reads", 0.5).unwrap();
    g.add_edge("api", "db", "writes", 3.0).unwrap();
    g.add_edge("app", "log", "uses", 1.0).unwrap();
    g.add_edge("db", "db", "replicates", 1.0).unwrap();

    assert_eq!(g.node("db").unwrap().unwrap().kind, "store");
    assert!(g.node("missing").unwrap().is_none());
    let stores: Vec<String> = g
        .nodes(Some("store"), &Scan::all())
        .unwrap()
        .into_iter()
        .map(|n| n.id)
        .collect();
    assert_eq!(stores, ["cache", "db"]);
    assert_eq!(
        g.nodes(None, &Scan::all().limit(2).after("app"))
            .unwrap()
            .len(),
        2
    );

    assert_eq!(g.edge("api", "db", "reads").unwrap().unwrap().weight, 2.5);
    assert!(g.edge("db", "api", "reads").unwrap().is_none());
    assert_eq!(
        names(&g.neighbors("api", Direction::Outbound, None).unwrap()),
        ["cache", "db"]
    );
    assert_eq!(
        names(
            &g.neighbors("api", Direction::Outbound, Some("writes"))
                .unwrap()
        ),
        ["db"]
    );
    assert_eq!(
        names(&g.neighbors("db", Direction::Inbound, None).unwrap()),
        ["api", "db"]
    );
    assert_eq!(
        names(&g.neighbors("api", Direction::Both, None).unwrap()),
        ["app", "cache", "db"]
    );
    assert_eq!(
        names(&g.reachable(&["app"], Direction::Outbound, None).unwrap()),
        ["api", "app", "cache", "db", "log"]
    );
    assert_eq!(
        names(&g.reachable(&["db"], Direction::Inbound, None).unwrap()),
        ["api", "app", "db"]
    );
    assert_eq!(
        names(
            &g.reachable(&["app"], Direction::Outbound, Some("calls"))
                .unwrap()
        ),
        ["api", "app"]
    );

    let from_api = g.edges(Some("api"), None, None, None).unwrap();
    assert_eq!(
        from_api
            .iter()
            .map(|e| (e.target.as_str(), e.kind.as_str()))
            .collect::<Vec<_>>(),
        [("cache", "reads"), ("db", "reads"), ("db", "writes")]
    );
    let into_db = g.edges(None, Some("db"), Some("reads"), None).unwrap();
    assert_eq!(into_db.len(), 1);
    assert_eq!(
        (into_db[0].source.as_str(), into_db[0].weight),
        ("api", 2.5)
    );
    assert_eq!(g.edges(None, None, Some("reads"), None).unwrap().len(), 2);
    assert_eq!(g.edges(None, None, None, Some(3)).unwrap().len(), 3);

    g.add_edge("api", "db", "reads", 9.0).unwrap();
    assert_eq!(
        g.edge("api", "db", "reads").unwrap().unwrap().weight,
        9.0,
        "adding an edge again replaces its weight"
    );
    assert!(g.remove_edge("api", "cache", "reads").unwrap());
    assert!(!g.remove_edge("api", "cache", "reads").unwrap());
    assert_eq!(
        names(&g.neighbors("cache", Direction::Inbound, None).unwrap()),
        Vec::<&str>::new()
    );

    assert!(g.delete_node("db").unwrap());
    assert!(!g.delete_node("db").unwrap());
    assert!(
        g.edges(None, Some("db"), None, None).unwrap().is_empty(),
        "inbound edges of a deleted node are gone"
    );
    assert!(
        g.edges(Some("db"), None, None, None).unwrap().is_empty(),
        "and its outbound edges, self-loop included"
    );
    assert_eq!(
        names(&g.neighbors("api", Direction::Outbound, None).unwrap()),
        Vec::<&str>::new()
    );

    assert!(matches!(
        g.add_edge("a", "b", "k", f64::NAN),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        g.set_node("bad\0id", "k", &json!({})),
        Err(Error::InvalidArgument(_))
    ));
    assert!(
        s.graph("other")
            .nodes(None, &Scan::all())
            .unwrap()
            .is_empty(),
        "graphs are separate"
    );
}

#[test]
fn nodes_link_documents_through_the_callers_guard() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();
    s.json_put("people", "p1", &json!({"name": "Ada", "tenant": "a"}))
        .unwrap();
    s.graph("social")
        .set_node_with_document("n1", "person", &json!({}), "people", "p1")
        .unwrap();
    let node = s.graph("social").node("n1").unwrap().unwrap();
    assert_eq!(
        (node.doc_space.as_deref(), node.doc_id.as_deref()),
        (Some("people"), Some("p1"))
    );
    assert_eq!(
        s.graph("social")
            .node_document("n1")
            .unwrap()
            .unwrap()
            .extract_str("name"),
        Some("Ada")
    );
    db.create_policy(
        "tenant",
        lilia_db::Policy::new("people", "$.tenant == current_context('tenant')"),
    )
    .unwrap();
    let other = db.session(lilia_db::SecurityContext::from_pair("tenant", "b"));
    assert!(other.graph("social").node_document("n1").unwrap().is_none());
}

#[test]
fn transactions_see_their_own_graph_writes_and_batches_mix_models() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();
    s.graph("g").add_edge("a", "b", "k", 1.0).unwrap();
    s.graph("g").add_edge("c", "a", "k", 1.0).unwrap();

    let tx = s.transaction();
    let g = tx.graph("g");
    g.add_edge("b", "c", "k", 1.0).unwrap();
    assert_eq!(
        names(&g.reachable(&["a"], Direction::Outbound, None).unwrap()),
        ["a", "b", "c"]
    );
    assert!(
        !g.delete_node("a").unwrap(),
        "a has edges but no node record"
    );
    assert!(
        g.edges(None, Some("a"), None, None).unwrap().is_empty(),
        "the cascade is visible in the transaction"
    );
    assert_eq!(
        names(&g.neighbors("b", Direction::Inbound, None).unwrap()),
        Vec::<&str>::new()
    );
    assert!(
        s.graph("g").edge("a", "b", "k").unwrap().is_some(),
        "nothing is written before commit"
    );
    tx.commit().unwrap();
    assert!(s.graph("g").edge("a", "b", "k").unwrap().is_none());
    assert!(s.graph("g").edge("c", "a", "k").unwrap().is_none());
    assert!(s.graph("g").edge("b", "c", "k").unwrap().is_some());

    let mut batch = Batch::new();
    batch.json_put("docs", "d1", &json!({"x": 1})).unwrap();
    batch.push(Write::Graph(GraphWrite::SetNode {
        graph: "g".into(),
        id: "d1".into(),
        kind: "doc".into(),
        metadata: json!({}),
        document: Some(("docs".into(), "d1".into())),
    }));
    batch.push(Write::Graph(GraphWrite::AddEdge {
        graph: "g".into(),
        source: "d1".into(),
        target: "b".into(),
        kind: "cites".into(),
        weight: 1.0,
    }));
    batch.push(Write::Graph(GraphWrite::DeleteNode {
        graph: "g".into(),
        id: "d1".into(),
    }));
    let outcomes = s.commit(&batch).unwrap();
    assert!(
        outcomes[3].deleted,
        "a later write of a batch sees the node an earlier one created"
    );
    assert!(
        s.graph("g").edge("d1", "b", "cites").unwrap().is_none(),
        "and the edge an earlier one added"
    );
    assert!(s.json_get("docs", "d1").unwrap().is_some());

    let reader = s.transaction();
    assert!(reader.graph("g").edge("b", "c", "k").unwrap().is_some());
    s.graph("g").remove_edge("b", "c", "k").unwrap();
    reader.kv_put("ns", b"x", b"y").unwrap();
    assert!(
        matches!(reader.commit(), Err(Error::Conflict { .. })),
        "a graph read is validated at commit"
    );
}

#[test]
fn graph_writes_are_ledger_records_and_persist() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = open(dir.path(), true);
        let g = db.system();
        let g = g.graph("net");
        g.set_node("n1", "host", &json!({"ip": "10.0.0.1"}))
            .unwrap();
        g.set_node("n2", "host", &json!({})).unwrap();
        g.add_edge("n1", "n2", "link", 4.0).unwrap();
        g.add_edge("n2", "n1", "link", 4.0).unwrap();
        g.delete_node("n2").unwrap();
        let report = db.verify_ledger(LedgerVerifyMode::Strict).unwrap();
        assert!(report.is_valid, "{report:?}");
        assert_eq!((report.commit_count, report.leaves_verified), (5, 1));
        let proof = db.prove_node("net", "n1").unwrap();
        assert!(
            db.prove_edge("net", "n1", "n2", "link").is_err(),
            "a deleted edge has no proof"
        );
        assert!(proof.verify(&report.merkle_root));
        db.close().unwrap();
    }
    let db = open(dir.path(), true);
    let s = db.system();
    let node = s.graph("net").node("n1").unwrap().unwrap();
    assert_eq!(node.metadata, json!({"ip": "10.0.0.1"}));
    assert!(s
        .graph("net")
        .edges(None, None, None, None)
        .unwrap()
        .is_empty());
}
