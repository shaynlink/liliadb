//! The graph operations: nodes and edges, traversals, analyses and Cypher.

use lilia_core::{CentralityType, ComponentType};
use lilia_db::Session;
use lilia_graph::GraphStore;

use super::{bound, invalid, Result};
use crate::{Operation, ResponseValue};

fn store<'c, 'db>(session: &'c Session<'db>, graph: &str) -> GraphStore<'c, Session<'db>> {
    GraphStore::new(session.graph(graph))
}

#[allow(
    clippy::too_many_lines,
    reason = "one arm per graph operation, each a call on the graph store"
)]
pub(super) fn execute_graph(s: &Session<'_>, operation: Operation) -> Result<ResponseValue> {
    Ok(match operation {
        Operation::GraphNeighbors {
            graph,
            node,
            direction,
            kind,
        } => ResponseValue::Ids(store(s, &graph).neighbors_with_kind(
            &node,
            direction,
            kind.as_deref(),
        )?),
        Operation::GraphReachability {
            graph,
            start_nodes,
            direction,
        } => ResponseValue::Ids(store(s, &graph).reachability(&start_nodes, direction)?),
        Operation::GraphGetNode { graph, id } => {
            ResponseValue::GraphNode(store(s, &graph).get_node(&id)?)
        }
        Operation::GraphGetNodeDocument { graph, id } => {
            ResponseValue::JsonExtracted(store(s, &graph).get_node_document(&id)?)
        }
        Operation::GraphDeleteNode { graph, id } => {
            ResponseValue::Bool(store(s, &graph).delete_node(&id)?)
        }
        Operation::GraphListNodes { graph, kind, limit } => {
            ResponseValue::GraphNodes(store(s, &graph).list_nodes(kind.as_deref(), bound(limit))?)
        }
        Operation::GraphRemoveEdge {
            graph,
            source,
            target,
            kind,
        } => ResponseValue::Bool(store(s, &graph).remove_edge(&source, &target, &kind)?),
        Operation::GraphListEdges {
            graph,
            source,
            target,
            kind,
            limit,
        } => ResponseValue::GraphEdges(store(s, &graph).list_edges(
            source.as_deref(),
            target.as_deref(),
            kind.as_deref(),
            bound(limit),
        )?),
        Operation::GraphImpactAnalysis {
            graph,
            changed_nodes,
        } => ResponseValue::GraphImpact(store(s, &graph).impact_analysis(&changed_nodes)?),
        Operation::GraphShortestPath {
            graph,
            start,
            target,
            direction,
            kind_filter,
        } => ResponseValue::WeightedPath(store(s, &graph).shortest_path(
            &start,
            &target,
            direction,
            kind_filter.as_deref(),
        )?),
        Operation::GraphDijkstra {
            graph,
            start,
            target,
            direction,
            kind_filter,
        } => ResponseValue::WeightedPath(store(s, &graph).dijkstra(
            &start,
            target.as_deref(),
            direction,
            kind_filter.as_deref(),
        )?),
        Operation::GraphAllShortestPaths {
            graph,
            start,
            direction,
            kind_filter,
        } => ResponseValue::WeightedPathMap(store(s, &graph).all_shortest_paths(
            &start,
            direction,
            kind_filter.as_deref(),
        )?),
        Operation::GraphAstarSpatial {
            graph,
            start,
            target,
            direction,
            kind_filter,
        } => ResponseValue::WeightedPath(store(s, &graph).astar_spatial(
            &start,
            &target,
            direction,
            kind_filter.as_deref(),
        )?),
        Operation::GraphPageRank { graph, config } => {
            ResponseValue::NodeScores(store(s, &graph).pagerank(&config)?)
        }
        Operation::GraphCentrality {
            graph,
            centrality,
            direction,
            kind_filter,
            normalized,
        } => {
            let graph = store(s, &graph);
            let kind = kind_filter.as_deref();
            ResponseValue::NodeScores(match centrality {
                CentralityType::Betweenness => {
                    graph.betweenness_centrality(direction, kind, normalized)?
                }
                CentralityType::Closeness => graph.closeness_centrality(direction, kind)?,
                CentralityType::Degree => graph.degree_centrality(direction, kind)?,
            })
        }
        Operation::GraphHasCycle {
            graph,
            direction,
            kind_filter,
        } => ResponseValue::Bool(store(s, &graph).has_cycle(direction, kind_filter.as_deref())?),
        Operation::GraphFindCycles {
            graph,
            max_cycles,
            kind_filter,
        } => ResponseValue::NodeGroups(
            store(s, &graph).find_cycles(max_cycles, kind_filter.as_deref())?,
        ),
        Operation::GraphConnectedComponents {
            graph,
            component_type,
            kind_filter,
        } => {
            let graph = store(s, &graph);
            let kind = kind_filter.as_deref();
            ResponseValue::NodeGroups(match component_type {
                ComponentType::StronglyConnected => graph.strongly_connected_components(kind)?,
                ComponentType::WeaklyConnected => graph.weakly_connected_components(kind)?,
            })
        }
        Operation::GraphCypher { graph, query } => {
            ResponseValue::CypherResult(store(s, &graph).cypher(&query)?)
        }
        other => return Err(invalid(format!("not a graph operation: {}", other.name()))),
    })
}
