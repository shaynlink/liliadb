//! The algorithms, traversals and Cypher queries of a [`GraphStore`], over an in-memory view of
//! the whole graph loaded for each call.

use std::collections::HashMap;

use lilia_core::{Direction, NodeScore, Result, WeightedPath};
use lilia_db::Context;

use crate::algorithms::{
    all_shortest_paths, astar, betweenness_centrality, closeness_centrality, degree_centrality,
    dijkstra, find_cycles, has_cycle, pagerank, strongly_connected_components,
    weakly_connected_components, PageRankConfig,
};
use crate::cypher::{execute_cypher, CypherResult};
use crate::snapshot::GraphView;
use crate::store::GraphStore;
use crate::traversal::GraphTraversal;

impl<'a, C: Context> GraphStore<'a, C> {
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
    pub fn traverse(&'a self, start_node: impl Into<String>) -> GraphTraversal<'a, C> {
        GraphTraversal::new(self, vec![start_node.into()])
    }

    #[must_use]
    pub fn traverse_nodes(&'a self, start_nodes: Vec<String>) -> GraphTraversal<'a, C> {
        GraphTraversal::new(self, start_nodes)
    }

    pub fn traverse_kind(&'a self, kind: &str) -> Result<GraphTraversal<'a, C>> {
        let nodes = self.list_nodes(Some(kind), None)?;
        let ids = nodes.into_iter().map(|n| n.id).collect();
        Ok(GraphTraversal::new(self, ids))
    }

    pub fn traverse_all(&'a self) -> Result<GraphTraversal<'a, C>> {
        let nodes = self.list_nodes(None, None)?;
        let ids = nodes.into_iter().map(|n| n.id).collect();
        Ok(GraphTraversal::new(self, ids))
    }

    pub fn cypher(&self, query: &str) -> Result<CypherResult> {
        execute_cypher(self, query)
    }
}

fn extract_coordinates(val: &serde_json::Value) -> Option<(f64, f64)> {
    if let (Some(x), Some(y)) = (
        val.get("x").and_then(serde_json::Value::as_f64),
        val.get("y").and_then(serde_json::Value::as_f64),
    ) {
        return Some((x, y));
    }
    if let (Some(lat), Some(lon)) = (
        val.get("lat").and_then(serde_json::Value::as_f64),
        val.get("lon").and_then(serde_json::Value::as_f64),
    ) {
        return Some((lon, lat));
    }
    None
}
