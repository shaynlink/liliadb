#![allow(clippy::missing_errors_doc)]

pub mod algorithms;
pub mod cypher;
pub mod snapshot;
pub mod traversal;
pub mod unified;

use std::collections::{BTreeSet, HashMap};

pub use algorithms::{
    all_shortest_paths, astar, betweenness_centrality, closeness_centrality, degree_centrality,
    dijkstra, find_cycles, has_cycle, pagerank, strongly_connected_components,
    weakly_connected_components, PageRankConfig,
};
pub use cypher::{execute_cypher, CypherResult};
pub use lilia_core::{
    CentralityType, ComponentType, Direction, GraphEdge, GraphNode, NodeScore, PathEdge, Result,
    WeightedPath,
};
pub use lilia_storage_sqlite::Database;
use serde::{Deserialize, Serialize};
pub use snapshot::{GraphView, ViewEdge};
pub use traversal::GraphTraversal;
pub use unified::{resolve_node_document, UnifiedNode};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactAnalysis {
    pub direct: Vec<String>,
    pub dependants: Vec<String>,
    pub total_affected: usize,
}

pub trait DatabaseGraphExt {
    fn graph<'a>(&'a self, graph_id: impl Into<String>) -> GraphStore<'a>;
}

impl DatabaseGraphExt for Database {
    fn graph<'a>(&'a self, graph_id: impl Into<String>) -> GraphStore<'a> {
        GraphStore::new(self, graph_id)
    }
}

#[derive(Debug)]
pub struct GraphStore<'a> {
    db: &'a Database,
    graph_id: String,
}

impl<'a> GraphStore<'a> {
    #[must_use]
    pub fn new(db: &'a Database, graph_id: impl Into<String>) -> Self {
        Self {
            db,
            graph_id: graph_id.into(),
        }
    }

    #[must_use]
    pub fn graph_id(&self) -> &str {
        &self.graph_id
    }

    #[must_use]
    pub fn database(&self) -> &Database {
        self.db
    }

    // --- Storage Operations ---

    pub fn set_node(&self, id: &str, kind: &str, metadata: &serde_json::Value) -> Result<()> {
        self.db.graph_set_node(&self.graph_id, id, kind, metadata)
    }

    pub fn set_node_doc(
        &self,
        id: &str,
        kind: &str,
        doc_space: Option<&str>,
        doc_id: Option<&str>,
        metadata: &serde_json::Value,
    ) -> Result<()> {
        self.db
            .graph_set_node_doc(&self.graph_id, id, kind, doc_space, doc_id, metadata)
    }

    pub fn get_node(&self, id: &str) -> Result<Option<GraphNode>> {
        self.db.graph_get_node(&self.graph_id, id)
    }

    pub fn get_node_document(&self, id: &str) -> Result<Option<serde_json::Value>> {
        self.db.graph_get_node_document(&self.graph_id, id)
    }

    pub fn delete_node(&self, id: &str) -> Result<bool> {
        self.db.graph_delete_node(&self.graph_id, id)
    }

    pub fn list_nodes(&self, kind: Option<&str>, limit: u32) -> Result<Vec<GraphNode>> {
        self.db.graph_list_nodes(&self.graph_id, kind, limit)
    }

    pub fn add_edge(&self, source: &str, target: &str, kind: &str) -> Result<()> {
        self.db
            .graph_add_edge_weighted(&self.graph_id, source, target, kind, 1.0)
    }

    pub fn add_weighted_edge(
        &self,
        source: &str,
        target: &str,
        kind: &str,
        weight: f64,
    ) -> Result<()> {
        self.db
            .graph_add_edge_weighted(&self.graph_id, source, target, kind, weight)
    }

    pub fn remove_edge(&self, source: &str, target: &str, kind: &str) -> Result<bool> {
        self.db
            .graph_remove_edge(&self.graph_id, source, target, kind)
    }

    pub fn list_edges(
        &self,
        source: Option<&str>,
        target: Option<&str>,
        kind: Option<&str>,
        limit: u32,
    ) -> Result<Vec<GraphEdge>> {
        self.db
            .graph_list_edges(&self.graph_id, source, target, kind, limit)
    }

    pub fn neighbors(&self, node: &str, direction: Direction) -> Result<Vec<String>> {
        self.db
            .graph_neighbors(&self.graph_id, node, direction, None)
    }

    pub fn neighbors_with_kind(
        &self,
        node: &str,
        direction: Direction,
        kind: Option<&str>,
    ) -> Result<Vec<String>> {
        self.db
            .graph_neighbors(&self.graph_id, node, direction, kind)
    }

    pub fn reachability(
        &self,
        start_nodes: &[String],
        direction: Direction,
    ) -> Result<Vec<String>> {
        self.db
            .graph_reachability(&self.graph_id, start_nodes, direction)
    }

    pub fn impact_analysis(&self, changed_nodes: &[String]) -> Result<ImpactAnalysis> {
        if changed_nodes.is_empty() {
            return Ok(ImpactAnalysis {
                direct: Vec::new(),
                dependants: Vec::new(),
                total_affected: 0,
            });
        }
        let direct_set: BTreeSet<String> = changed_nodes.iter().cloned().collect();
        let reached = self.reachability(changed_nodes, Direction::Incoming)?;
        let mut dependants = Vec::new();
        for node in reached {
            if !direct_set.contains(&node) {
                dependants.push(node);
            }
        }
        let total_affected = direct_set.len() + dependants.len();
        Ok(ImpactAnalysis {
            direct: direct_set.into_iter().collect(),
            dependants,
            total_affected,
        })
    }

    // --- Fast In-Memory View & Algorithms ---

    pub fn load_view(&self, kind_filter: Option<&str>) -> Result<GraphView> {
        GraphView::from_store(self, kind_filter)
    }

    pub fn shortest_path(
        &self,
        start: &str,
        target: &str,
        direction: Direction,
        kind_filter: Option<&str>,
    ) -> Result<Option<WeightedPath>> {
        let view = self.load_view(kind_filter)?;
        Ok(dijkstra(&view, start, Some(target), direction, kind_filter))
    }

    pub fn dijkstra(
        &self,
        start: &str,
        target: Option<&str>,
        direction: Direction,
        kind_filter: Option<&str>,
    ) -> Result<Option<WeightedPath>> {
        let view = self.load_view(kind_filter)?;
        Ok(dijkstra(&view, start, target, direction, kind_filter))
    }

    pub fn all_shortest_paths(
        &self,
        start: &str,
        direction: Direction,
        kind_filter: Option<&str>,
    ) -> Result<HashMap<String, WeightedPath>> {
        let view = self.load_view(kind_filter)?;
        Ok(all_shortest_paths(&view, start, direction, kind_filter))
    }

    pub fn astar<H>(
        &self,
        start: &str,
        target: &str,
        direction: Direction,
        kind_filter: Option<&str>,
        heuristic: H,
    ) -> Result<Option<WeightedPath>>
    where
        H: Fn(&str) -> f64,
    {
        let view = self.load_view(kind_filter)?;
        Ok(astar(
            &view,
            start,
            target,
            direction,
            kind_filter,
            heuristic,
        ))
    }

    pub fn astar_spatial(
        &self,
        start: &str,
        target: &str,
        direction: Direction,
        kind_filter: Option<&str>,
    ) -> Result<Option<WeightedPath>> {
        let view = self.load_view(kind_filter)?;

        // Target coordinates
        let target_node = self.get_node(target)?;
        let target_coords = target_node.as_ref().and_then(|node| {
            extract_coordinates(&node.metadata).or_else(|| {
                self.get_node_document(target)
                    .ok()
                    .flatten()
                    .and_then(|doc| extract_coordinates(&doc))
            })
        });

        if let Some((tx, ty)) = target_coords {
            Ok(astar(
                &view,
                start,
                target,
                direction,
                kind_filter,
                |node_id| {
                    if let Ok(Some(n)) = self.get_node(node_id) {
                        let coords = extract_coordinates(&n.metadata).or_else(|| {
                            self.get_node_document(node_id)
                                .ok()
                                .flatten()
                                .and_then(|doc| extract_coordinates(&doc))
                        });
                        if let Some((nx, ny)) = coords {
                            let dx = nx - tx;
                            let dy = ny - ty;
                            return (dx * dx + dy * dy).sqrt();
                        }
                    }
                    0.0
                },
            ))
        } else {
            // Fallback to Dijkstra
            Ok(dijkstra(&view, start, Some(target), direction, kind_filter))
        }
    }

    pub fn pagerank(&self, config: &PageRankConfig) -> Result<Vec<NodeScore>> {
        let view = self.load_view(config.edge_kind.as_deref())?;
        Ok(pagerank(&view, config))
    }

    pub fn betweenness_centrality(
        &self,
        direction: Direction,
        kind_filter: Option<&str>,
        normalized: bool,
    ) -> Result<Vec<NodeScore>> {
        let view = self.load_view(kind_filter)?;
        Ok(betweenness_centrality(
            &view,
            direction,
            kind_filter,
            normalized,
        ))
    }

    pub fn closeness_centrality(
        &self,
        direction: Direction,
        kind_filter: Option<&str>,
    ) -> Result<Vec<NodeScore>> {
        let view = self.load_view(kind_filter)?;
        Ok(closeness_centrality(&view, direction, kind_filter))
    }

    pub fn degree_centrality(
        &self,
        direction: Direction,
        kind_filter: Option<&str>,
    ) -> Result<Vec<NodeScore>> {
        let view = self.load_view(kind_filter)?;
        Ok(degree_centrality(&view, direction, kind_filter))
    }

    pub fn has_cycle(&self, direction: Direction, kind_filter: Option<&str>) -> Result<bool> {
        let view = self.load_view(kind_filter)?;
        Ok(has_cycle(&view, direction, kind_filter))
    }

    pub fn find_cycles(
        &self,
        max_cycles: usize,
        kind_filter: Option<&str>,
    ) -> Result<Vec<Vec<String>>> {
        let view = self.load_view(kind_filter)?;
        Ok(find_cycles(&view, max_cycles, kind_filter))
    }

    pub fn strongly_connected_components(
        &self,
        kind_filter: Option<&str>,
    ) -> Result<Vec<Vec<String>>> {
        let view = self.load_view(kind_filter)?;
        Ok(strongly_connected_components(&view, kind_filter))
    }

    pub fn weakly_connected_components(
        &self,
        kind_filter: Option<&str>,
    ) -> Result<Vec<Vec<String>>> {
        let view = self.load_view(kind_filter)?;
        Ok(weakly_connected_components(&view, kind_filter))
    }

    // --- Traversal & Query ---

    #[must_use]
    pub fn traverse(&'a self, start_node: impl Into<String>) -> GraphTraversal<'a> {
        GraphTraversal::new(self, vec![start_node.into()])
    }

    #[must_use]
    pub fn traverse_nodes(&'a self, start_nodes: Vec<String>) -> GraphTraversal<'a> {
        GraphTraversal::new(self, start_nodes)
    }

    pub fn traverse_kind(&'a self, kind: &str) -> Result<GraphTraversal<'a>> {
        let nodes = self.list_nodes(Some(kind), u32::MAX)?;
        let ids = nodes.into_iter().map(|n| n.id).collect();
        Ok(GraphTraversal::new(self, ids))
    }

    pub fn traverse_all(&'a self) -> Result<GraphTraversal<'a>> {
        let nodes = self.list_nodes(None, u32::MAX)?;
        let ids = nodes.into_iter().map(|n| n.id).collect();
        Ok(GraphTraversal::new(self, ids))
    }

    pub fn cypher(&self, query: &str) -> Result<CypherResult> {
        execute_cypher(self, query)
    }

    pub fn resolve_document(&self, node: &GraphNode) -> Result<Option<serde_json::Value>> {
        resolve_node_document(self.db, node)
    }
}

fn extract_coordinates(val: &serde_json::Value) -> Option<(f64, f64)> {
    if let (Some(x), Some(y)) = (
        val.get("x").and_then(|v| v.as_f64()),
        val.get("y").and_then(|v| v.as_f64()),
    ) {
        return Some((x, y));
    }
    if let (Some(lat), Some(lon)) = (
        val.get("lat").and_then(|v| v.as_f64()),
        val.get("lon").and_then(|v| v.as_f64()),
    ) {
        return Some((lon, lat));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use lilia_storage_sqlite::DatabaseOptions;
    use serde_json::json;

    fn setup_test_db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(DatabaseOptions::durable(
            dir.path().join("graph_test.lilia"),
        ))
        .unwrap();
        (dir, db)
    }

    #[test]
    fn test_graph_store_and_impact() {
        let (_dir, db) = setup_test_db();
        let graph = GraphStore::new(&db, "workspace_deps");

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
            graph.neighbors("auth", Direction::Outgoing).unwrap(),
            vec!["core"]
        );
        assert_eq!(
            graph.neighbors("auth", Direction::Incoming).unwrap(),
            vec!["api"]
        );
        let mut both = graph.neighbors("auth", Direction::Both).unwrap();
        both.sort();
        assert_eq!(both, vec!["api", "core"]);
        assert_eq!(
            graph
                .neighbors_with_kind("auth", Direction::Outgoing, Some("other"))
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
        let graph = db.graph("roadmap");

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
            .shortest_path("A", "D", Direction::Outgoing, Some("road"))
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
            .astar_spatial("A", "D", Direction::Outgoing, Some("road"))
            .unwrap()
            .unwrap();
        assert_eq!(astar_path.nodes, vec!["A", "C", "D"]);
        assert!((astar_path.total_weight - 2.0).abs() < 1e-6);

        // All shortest paths from A
        let all_paths = graph
            .all_shortest_paths("A", Direction::Outgoing, None)
            .unwrap();
        assert_eq!(all_paths.len(), 3);
        assert!((all_paths["B"].total_weight - 5.0).abs() < 1e-6);
        assert!((all_paths["C"].total_weight - 1.0).abs() < 1e-6);
        assert!((all_paths["D"].total_weight - 2.0).abs() < 1e-6);
    }

    #[test]
    fn test_pagerank_and_centrality() {
        let (_dir, db) = setup_test_db();
        let graph = db.graph("social");

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
        let mut local_cfg = PageRankConfig::default();
        local_cfg.seeds = Some(vec!["P1".to_string()]);
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

        let degree_out = graph.degree_centrality(Direction::Outgoing, None).unwrap();
        assert_eq!(degree_out[0].node, "H");
        assert_eq!(degree_out[0].score, 1.0); // 4 outgoing / 4 = 1.0

        let degree_both = graph.degree_centrality(Direction::Both, None).unwrap();
        assert_eq!(degree_both[0].node, "H");
        assert_eq!(degree_both[0].score, 2.0); // 8 total directed edges / 4 = 2.0
    }

    #[test]
    fn test_cycles_and_components() {
        let (_dir, db) = setup_test_db();
        let graph = db.graph("topology");

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

        assert!(graph.has_cycle(Direction::Outgoing, None).unwrap());

        let cycles = graph.find_cycles(10, None).unwrap();
        assert!(!cycles.is_empty());
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

        // 1. Store documents in JSON collections via batch operations
        db.batch(&[
            lilia_core::BatchOperation::JsonPut {
                space: "users".to_string(),
                id: "user_alice".to_string(),
                value: json!({
                    "name": "Alice",
                    "email": "alice@example.com",
                    "role": "admin"
                }),
                if_version: None,
            },
            lilia_core::BatchOperation::JsonPut {
                space: "users".to_string(),
                id: "user_bob".to_string(),
                value: json!({
                    "name": "Bob",
                    "email": "bob@example.com",
                    "role": "user"
                }),
                if_version: None,
            },
            lilia_core::BatchOperation::JsonPut {
                space: "posts".to_string(),
                id: "post_101".to_string(),
                value: json!({
                    "title": "Native Graph in Rust",
                    "status": "published",
                    "likes_count": 42
                }),
                if_version: None,
            },
        ])
        .unwrap();

        let graph = db.graph("social");

        // 2. Create nodes referencing the documents (ZERO DUPLICATION)
        // Auto-resolution cascade: kind = "users" automatically maps to space "users"
        graph.set_node("user_alice", "users", &json!({})).unwrap();
        graph.set_node("user_bob", "users", &json!({})).unwrap();
        // Explicit doc_space: kind = "content", doc_space = "posts"
        graph
            .set_node_doc(
                "post_101",
                "content",
                Some("posts"),
                Some("post_101"),
                &json!({}),
            )
            .unwrap();

        // 3. Connect them: alice -FRIEND-> bob -LIKES-> post_101
        graph.add_edge("user_alice", "user_bob", "FRIEND").unwrap();
        graph.add_edge("user_bob", "post_101", "LIKES").unwrap();

        // 4. Fluent Traversal: db.graph().traverse("user_alice").out("FRIEND").out("LIKES").to_json()
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
        db.batch(&[lilia_core::BatchOperation::JsonPut {
            space: "posts".to_string(),
            id: "post_101".to_string(),
            value: json!({
                "title": "Native Graph in Rust (Updated)",
                "status": "published",
                "likes_count": 43
            }),
            if_version: None,
        }])
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
}
