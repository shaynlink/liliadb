//! A graph of `lilia-db` as the algorithms read it: its nodes, edges and linked documents,
//! through a session or transaction, so the caller's read policies apply.

use std::collections::BTreeSet;

use lilia_core::{Direction, GraphEdge, GraphNode, ImpactAnalysis, Result};
use lilia_db::{Context, Graph, Scan};
use serde_json::Value;

/// Graph `name` of a session or transaction, with the algorithms of this crate.
#[derive(Debug)]
pub struct GraphStore<'c, C: Context> {
    graph: Graph<'c, C>,
}

impl<'c, C: Context> GraphStore<'c, C> {
    /// The algorithms over `graph` (`session.graph(name)`, `transaction.graph(name)`).
    #[must_use]
    pub fn new(graph: Graph<'c, C>) -> Self {
        Self { graph }
    }

    #[must_use]
    pub fn graph_id(&self) -> &str {
        self.graph.name()
    }

    /// The `lilia-db` graph handle, for the operations this crate does not wrap.
    #[must_use]
    pub fn graph(&self) -> &Graph<'c, C> {
        &self.graph
    }

    pub fn set_node(&self, id: &str, kind: &str, metadata: &Value) -> Result<()> {
        self.graph.set_node(id, kind, metadata)?;
        Ok(())
    }

    /// Stores node `id` linked to document `doc_id` (the node's own id by default) of
    /// `doc_space`; without a space, an unlinked node.
    pub fn set_node_doc(
        &self,
        id: &str,
        kind: &str,
        doc_space: Option<&str>,
        doc_id: Option<&str>,
        metadata: &Value,
    ) -> Result<()> {
        match doc_space {
            Some(space) => {
                self.graph.set_node_with_document(
                    id,
                    kind,
                    metadata,
                    space,
                    doc_id.unwrap_or(id),
                )?;
            }
            None => {
                self.graph.set_node(id, kind, metadata)?;
            }
        }
        Ok(())
    }

    pub fn get_node(&self, id: &str) -> Result<Option<GraphNode>> {
        Ok(self.graph.node(id)?)
    }

    /// The document node `id` links to, if any and if the caller may read it.
    pub fn get_node_document(&self, id: &str) -> Result<Option<Value>> {
        Ok(self.graph.node_document(id)?.map(|entry| entry.value()))
    }

    /// The document `node` links to, if any and if the caller may read it.
    pub fn node_document(&self, node: &GraphNode) -> Result<Option<Value>> {
        if node.doc_space.is_none() {
            return Ok(None);
        }
        self.get_node_document(&node.id)
    }

    pub fn delete_node(&self, id: &str) -> Result<bool> {
        Ok(self.graph.delete_node(id)?)
    }

    /// Nodes in id order, optionally of one kind; at most `limit` of them, every one without.
    pub fn list_nodes(&self, kind: Option<&str>, limit: Option<usize>) -> Result<Vec<GraphNode>> {
        let scan = limit.map_or_else(Scan::all, |limit| Scan::all().limit(limit));
        Ok(self.graph.nodes(kind, &scan)?)
    }

    pub fn add_edge(&self, source: &str, target: &str, kind: &str) -> Result<()> {
        self.add_weighted_edge(source, target, kind, 1.0)
    }

    pub fn add_weighted_edge(
        &self,
        source: &str,
        target: &str,
        kind: &str,
        weight: f64,
    ) -> Result<()> {
        self.graph.add_edge(source, target, kind, weight)?;
        Ok(())
    }

    pub fn remove_edge(&self, source: &str, target: &str, kind: &str) -> Result<bool> {
        Ok(self.graph.remove_edge(source, target, kind)?)
    }

    /// Edges matching the given ends and kind; at most `limit` of them, every one without.
    pub fn list_edges(
        &self,
        source: Option<&str>,
        target: Option<&str>,
        kind: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<GraphEdge>> {
        Ok(self.graph.edges(source, target, kind, limit)?)
    }

    pub fn neighbors(&self, node: &str, direction: Direction) -> Result<Vec<String>> {
        self.neighbors_with_kind(node, direction, None)
    }

    pub fn neighbors_with_kind(
        &self,
        node: &str,
        direction: Direction,
        kind: Option<&str>,
    ) -> Result<Vec<String>> {
        Ok(self.graph.neighbors(node, direction, kind)?)
    }

    /// Every node reachable from `start_nodes` (included) in `direction`.
    pub fn reachability(
        &self,
        start_nodes: &[String],
        direction: Direction,
    ) -> Result<Vec<String>> {
        let starts: Vec<&str> = start_nodes.iter().map(String::as_str).collect();
        Ok(self.graph.reachable(&starts, direction, None)?)
    }

    /// The nodes that depend on `changed_nodes`: those that reach them through inbound edges.
    pub fn impact_analysis(&self, changed_nodes: &[String]) -> Result<ImpactAnalysis> {
        if changed_nodes.is_empty() {
            return Ok(ImpactAnalysis {
                direct: Vec::new(),
                dependants: Vec::new(),
                total_affected: 0,
            });
        }
        let direct: BTreeSet<String> = changed_nodes.iter().cloned().collect();
        let dependants: Vec<String> = self
            .reachability(changed_nodes, Direction::Inbound)?
            .into_iter()
            .filter(|node| !direct.contains(node))
            .collect();
        let total_affected = direct.len() + dependants.len();
        Ok(ImpactAnalysis {
            direct: direct.into_iter().collect(),
            dependants,
            total_affected,
        })
    }
}
