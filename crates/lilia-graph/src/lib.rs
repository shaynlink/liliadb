//! Graph algorithms, traversals and Cypher queries over the graphs of `lilia-db`
//! ([`GraphStore`]): shortest paths (Dijkstra, A*), centralities, `PageRank`, cycles and
//! components, fluent traversals that resolve the documents nodes link to, and a subset of
//! Cypher. Every read goes through the session or transaction of the graph handle, so the
//! caller's read policies apply.

#![allow(clippy::missing_errors_doc)]

pub mod algorithms;
mod analysis;
pub mod cypher;
pub mod snapshot;
mod store;
pub mod traversal;
pub mod unified;

pub use algorithms::{
    all_shortest_paths, astar, betweenness_centrality, closeness_centrality, degree_centrality,
    dijkstra, find_cycles, has_cycle, pagerank, strongly_connected_components,
    weakly_connected_components, PageRankConfig,
};
pub use cypher::{execute_cypher, CypherResult};
pub use lilia_core::{
    CentralityType, ComponentType, Direction, GraphEdge, GraphNode, ImpactAnalysis, NodeScore,
    PathEdge, Result, WeightedPath,
};
pub use snapshot::{GraphView, ViewEdge};
pub use store::GraphStore;
pub use traversal::GraphTraversal;
pub use unified::UnifiedNode;
