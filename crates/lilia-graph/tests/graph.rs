//! Graph algorithms, traversals and Cypher over `lilia-db` graphs.

use lilia_db::{Database, Options, Policy, SecurityContext};
use lilia_graph::{Direction, GraphStore, PageRankConfig};
use lilia_io::HardwareProfile;
use serde_json::json;

fn setup_test_db() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(
        dir.path(),
        Options::for_hardware(&HardwareProfile::minimal()),
    )
    .unwrap();
    (dir, db)
}

#[test]
fn test_graph_store_and_impact() {
    let (_dir, db) = setup_test_db();
    let s = db.system();
    let graph = GraphStore::new(s.graph("workspace_deps"));

    graph
        .set_node("core", "library", &json!({ "pkg": "core" }))
        .unwrap();
    graph
        .set_node("auth", "library", &json!({ "pkg": "auth" }))
        .unwrap();
    graph
        .set_node("api", "service", &json!({ "pkg": "api" }))
        .unwrap();
    graph
        .set_node("web", "frontend", &json!({ "pkg": "web" }))
        .unwrap();

    // api -> auth -> core
    // web -> api
    graph.add_edge("auth", "core", "depends_on").unwrap();
    graph.add_edge("api", "auth", "depends_on").unwrap();
    graph.add_edge("web", "api", "depends_on").unwrap();

    // Neighbors test
    assert_eq!(
        graph.neighbors("auth", Direction::Outbound).unwrap(),
        vec!["core"]
    );
    assert_eq!(
        graph.neighbors("auth", Direction::Inbound).unwrap(),
        vec!["api"]
    );
    let mut both = graph.neighbors("auth", Direction::Both).unwrap();
    both.sort();
    assert_eq!(both, vec!["api", "core"]);
    assert_eq!(
        graph
            .neighbors_with_kind("auth", Direction::Outbound, Some("other"))
            .unwrap(),
        Vec::<String>::new()
    );

    let impact = graph.impact_analysis(&["core".to_string()]).unwrap();
    assert_eq!(impact.direct, vec!["core"]);
    assert_eq!(impact.dependants, vec!["api", "auth", "web"]);
    assert_eq!(impact.total_affected, 4);

    let impact_auth = graph.impact_analysis(&["auth".to_string()]).unwrap();
    assert_eq!(impact_auth.direct, vec!["auth"]);
    assert_eq!(impact_auth.dependants, vec!["api", "web"]);
    assert_eq!(impact_auth.total_affected, 3);
}

#[test]
fn test_weighted_shortest_paths() {
    let (_dir, db) = setup_test_db();
    let s = db.system();
    let graph = GraphStore::new(s.graph("roadmap"));

    // Nodes with spatial coordinates
    graph
        .set_node("A", "city", &json!({ "x": 0.0, "y": 0.0 }))
        .unwrap();
    graph
        .set_node("B", "city", &json!({ "x": 2.0, "y": 0.0 }))
        .unwrap();
    graph
        .set_node("C", "city", &json!({ "x": 0.0, "y": 2.0 }))
        .unwrap();
    graph
        .set_node("D", "city", &json!({ "x": 2.0, "y": 2.0 }))
        .unwrap();

    // Paths:
    // A -> B (weight: 5.0)
    // A -> C (weight: 1.0)
    // C -> D (weight: 1.0)
    // B -> D (weight: 1.0)
    graph.add_weighted_edge("A", "B", "road", 5.0).unwrap();
    graph.add_weighted_edge("A", "C", "road", 1.0).unwrap();
    graph.add_weighted_edge("C", "D", "road", 1.0).unwrap();
    graph.add_weighted_edge("B", "D", "road", 1.0).unwrap();

    // Dijkstra: optimal path A -> C -> D with total_weight 2.0 (instead of A -> B -> D with 6.0)
    let path = graph
        .shortest_path("A", "D", Direction::Outbound, Some("road"))
        .unwrap()
        .unwrap();
    assert_eq!(path.nodes, vec!["A", "C", "D"]);
    assert!((path.total_weight - 2.0).abs() < 1e-6);
    assert_eq!(path.edges.len(), 2);
    assert_eq!(path.edges[0].source, "A");
    assert_eq!(path.edges[0].target, "C");
    assert!((path.edges[0].weight - 1.0).abs() < 1e-6);

    // A* Spatial: should find the same optimal path
    let astar_path = graph
        .astar_spatial("A", "D", Direction::Outbound, Some("road"))
        .unwrap()
        .unwrap();
    assert_eq!(astar_path.nodes, vec!["A", "C", "D"]);
    assert!((astar_path.total_weight - 2.0).abs() < 1e-6);

    // All shortest paths from A
    let all_paths = graph
        .all_shortest_paths("A", Direction::Outbound, None)
        .unwrap();
    assert_eq!(all_paths.len(), 3);
    assert!((all_paths["B"].total_weight - 5.0).abs() < 1e-6);
    assert!((all_paths["C"].total_weight - 1.0).abs() < 1e-6);
    assert!((all_paths["D"].total_weight - 2.0).abs() < 1e-6);
}

#[test]
fn test_pagerank_and_centrality() {
    let (_dir, db) = setup_test_db();
    let s = db.system();
    let graph = GraphStore::new(s.graph("social"));

    // Star graph: Hub node H connected to periphery nodes P1, P2, P3, P4
    graph.set_node("H", "user", &json!({})).unwrap();
    for i in 1..=4 {
        let p = format!("P{i}");
        graph.set_node(&p, "user", &json!({})).unwrap();
        graph.add_edge(&p, "H", "FOLLOWS").unwrap();
        graph.add_edge("H", &p, "FOLLOWS").unwrap();
    }

    // Global PageRank
    let pr = graph.pagerank(&PageRankConfig::default()).unwrap();
    assert_eq!(pr.len(), 5);
    // H should have the highest prestige
    assert_eq!(pr[0].node, "H");

    // Personalized / Local PageRank focusing on P1
    let local_cfg = PageRankConfig {
        seeds: Some(vec!["P1".to_string()]),
        ..PageRankConfig::default()
    };
    let local_pr = graph.pagerank(&local_cfg).unwrap();
    let p1_score = local_pr.iter().find(|s| s.node == "P1").unwrap().score;
    let p2_score = local_pr.iter().find(|s| s.node == "P2").unwrap().score;
    // P1 should have significantly higher local score than non-seed periphery nodes
    assert!(p1_score > p2_score * 2.0);

    // Centralities
    let betweenness = graph
        .betweenness_centrality(Direction::Both, None, true)
        .unwrap();
    assert_eq!(betweenness[0].node, "H");
    assert!(betweenness[0].score > betweenness[1].score);

    let degree_out = graph.degree_centrality(Direction::Outbound, None).unwrap();
    assert_eq!(degree_out[0].node, "H");
    assert_eq!(degree_out[0].score, 1.0); // 4 outgoing / 4 = 1.0

    let degree_both = graph.degree_centrality(Direction::Both, None).unwrap();
    assert_eq!(degree_both[0].node, "H");
    assert_eq!(degree_both[0].score, 2.0); // 8 total directed edges / 4 = 2.0
}

#[test]
fn test_cycles_and_components() {
    let (_dir, db) = setup_test_db();
    let s = db.system();
    let graph = GraphStore::new(s.graph("topology"));

    // Graph with cycle: 1 -> 2 -> 3 -> 1
    // And separate component: 4 -> 5
    graph.set_node("1", "task", &json!({})).unwrap();
    graph.set_node("2", "task", &json!({})).unwrap();
    graph.set_node("3", "task", &json!({})).unwrap();
    graph.set_node("4", "task", &json!({})).unwrap();
    graph.set_node("5", "task", &json!({})).unwrap();

    graph.add_edge("1", "2", "NEXT").unwrap();
    graph.add_edge("2", "3", "NEXT").unwrap();
    graph.add_edge("3", "1", "NEXT").unwrap();
    graph.add_edge("4", "5", "NEXT").unwrap();

    assert!(graph.has_cycle(Direction::Outbound, None).unwrap());

    let cycles = graph.find_cycles(10, None).unwrap();
    assert_ne!(cycles, [] as [std::vec::Vec<std::string::String>; 0]);
    assert_eq!(cycles[0].len(), 4); // ["1", "2", "3", "1"] (or rotated)

    // Strongly connected components
    let sccs = graph.strongly_connected_components(None).unwrap();
    assert_eq!(sccs.len(), 3); // {1, 2, 3}, {4}, {5}
    assert_eq!(sccs[0].len(), 3);

    // Weakly connected components
    let wccs = graph.weakly_connected_components(None).unwrap();
    assert_eq!(wccs.len(), 2); // {1, 2, 3} and {4, 5}
}

#[test]
fn test_unified_document_and_traversal() {
    let (_dir, db) = setup_test_db();

    // 1. Store documents in JSON collections.
    let s = db.system();
    s.json_put(
        "users",
        "user_alice",
        &json!({"name": "Alice", "email": "alice@example.com", "role": "admin"}),
    )
    .unwrap();
    s.json_put(
        "users",
        "user_bob",
        &json!({"name": "Bob", "email": "bob@example.com", "role": "user"}),
    )
    .unwrap();
    s.json_put(
        "posts",
        "post_101",
        &json!({"title": "Native Graph in Rust", "status": "published", "likes_count": 42}),
    )
    .unwrap();

    let graph = GraphStore::new(s.graph("social"));

    // 2. Create nodes referencing the documents (ZERO DUPLICATION) through explicit links
    for (id, kind, space) in [
        ("user_alice", "users", "users"),
        ("user_bob", "users", "users"),
        ("post_101", "content", "posts"),
    ] {
        graph
            .set_node_doc(id, kind, Some(space), Some(id), &json!({}))
            .unwrap();
    }

    // 3. Connect them: alice -FRIEND-> bob -LIKES-> post_101
    graph.add_edge("user_alice", "user_bob", "FRIEND").unwrap();
    graph.add_edge("user_bob", "post_101", "LIKES").unwrap();

    // 4. Fluent traversal: graph.traverse("user_alice").out("FRIEND").out("LIKES").to_json()
    let result_json = graph
        .traverse("user_alice")
        .out("FRIEND")
        .unwrap()
        .out("LIKES")
        .unwrap()
        .to_json()
        .unwrap();

    assert_eq!(
        result_json,
        json!([{
            "title": "Native Graph in Rust",
            "status": "published",
            "likes_count": 42
        }])
    );

    // 5. Zero-duplication update test: Update JSON document directly
    s.json_put(
        "posts",
        "post_101",
        &json!({"title": "Native Graph in Rust (Updated)", "status": "published", "likes_count": 43}),
    )
    .unwrap();

    // Traversal immediately reflects the updated document without any graph mutation!
    let updated_traversal = graph
        .traverse("user_alice")
        .out("FRIEND")
        .unwrap()
        .out("LIKES")
        .unwrap()
        .to_json()
        .unwrap();

    assert_eq!(
        updated_traversal[0]["title"],
        "Native Graph in Rust (Updated)"
    );
    assert_eq!(updated_traversal[0]["likes_count"], 43);

    // 6. Mini-Cypher query test
    let cypher_res = graph
        .cypher(
            "MATCH (u:users)-[:FRIEND]->(f:users)-[:LIKES]->(p:content) \
             WHERE u.id = 'user_alice' \
             RETURN p.title AS post_title, f.name AS friend_name",
        )
        .unwrap();

    assert_eq!(cypher_res.columns, vec!["post_title", "friend_name"]);
    assert_eq!(cypher_res.rows.len(), 1);
    assert_eq!(
        cypher_res.rows[0],
        vec![json!("Native Graph in Rust (Updated)"), json!("Bob")]
    );
}

#[test]
fn views_and_queries_see_every_node_of_a_large_graph() {
    let (_dir, db) = setup_test_db();
    let s = db.system();
    let graph = GraphStore::new(s.graph("chain"));
    let n = 1_500;
    for i in 0..n {
        graph
            .set_node(&format!("n{i:04}"), "step", &json!({}))
            .unwrap();
        if i > 0 {
            graph
                .add_edge(&format!("n{:04}", i - 1), &format!("n{i:04}"), "next")
                .unwrap();
        }
    }
    let path = graph
        .shortest_path(
            "n0000",
            &format!("n{:04}", n - 1),
            Direction::Outbound,
            None,
        )
        .unwrap()
        .unwrap();
    assert_eq!(path.nodes.len(), n);
    assert_eq!(graph.pagerank(&PageRankConfig::default()).unwrap().len(), n);
    let rows = graph
        .cypher("MATCH (s:step) RETURN s.id AS id")
        .unwrap()
        .rows;
    assert_eq!(rows.len(), n);
    assert_eq!(graph.list_nodes(None, Some(10)).unwrap().len(), 10);
}

#[test]
fn traversals_resolve_only_the_documents_the_caller_may_read() {
    let (_dir, db) = setup_test_db();
    let s = db.system();
    s.json_put(
        "posts",
        "public",
        &json!({"title": "Open", "visibility": "public"}),
    )
    .unwrap();
    s.json_put(
        "posts",
        "secret",
        &json!({"title": "Hidden", "visibility": "private"}),
    )
    .unwrap();
    let graph = GraphStore::new(s.graph("feed"));
    graph.set_node("me", "user", &json!({})).unwrap();
    for post in ["public", "secret"] {
        graph
            .set_node_doc(post, "post", Some("posts"), None, &json!({}))
            .unwrap();
        graph.add_edge("me", post, "LIKES").unwrap();
    }
    db.create_policy("visible", Policy::new("posts", "$.visibility == 'public'"))
        .unwrap();
    let reader = db.session(SecurityContext::from_pair("role", "reader"));
    let graph = GraphStore::new(reader.graph("feed"));
    let docs = graph
        .traverse("me")
        .out("LIKES")
        .unwrap()
        .to_documents()
        .unwrap();
    assert_eq!(docs, [json!({"title": "Open", "visibility": "public"})]);
    assert!(graph.get_node_document("secret").unwrap().is_none());
}
