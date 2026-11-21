//! The graph handle of a session or transaction.

use std::collections::{BTreeSet, VecDeque};
use std::ops::Bound;

use lilia_core::{Direction, GraphEdge, GraphNode, JsonEntry};
use serde_json::Value;

use crate::batch::{Outcome, Write};
use crate::context::{guard, Context};
use crate::error::{corruption, Result};
use crate::keys::{self, validate_name};
use crate::read::{self, Scan, Source};

use super::store::{
    adjacency, decode_node, decode_weight, edge_key, node_key, prefix_bounds, split_edge,
    validate_edge,
};
use super::GraphWrite;

/// The graph `name` of a session or transaction.
#[derive(Debug)]
pub struct Graph<'c, C: Context> {
    context: &'c C,
    name: String,
}

impl<'c, C: Context> Graph<'c, C> {
    pub(crate) fn new(context: &'c C, name: &str) -> Self {
        Self {
            context,
            name: name.to_owned(),
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    fn submit(&self, write: GraphWrite) -> Result<Outcome> {
        let mut outcomes = self.context.submit(vec![Write::Graph(write)])?;
        outcomes
            .pop()
            .ok_or_else(|| corruption("a graph write produced no outcome"))
    }

    /// Creates or replaces node `id`.
    ///
    /// # Errors
    ///
    /// Invalid names; as [`crate::Session::commit`].
    pub fn set_node(&self, id: &str, kind: &str, metadata: &Value) -> Result<Outcome> {
        self.submit(GraphWrite::SetNode {
            graph: self.name.clone(),
            id: id.to_owned(),
            kind: kind.to_owned(),
            metadata: metadata.clone(),
            document: None,
        })
    }

    /// Creates or replaces node `id`, linked to document `doc_id` of JSON space `space`.
    ///
    /// # Errors
    ///
    /// As [`Graph::set_node`].
    pub fn set_node_with_document(
        &self,
        id: &str,
        kind: &str,
        metadata: &Value,
        space: &str,
        doc_id: &str,
    ) -> Result<Outcome> {
        self.submit(GraphWrite::SetNode {
            graph: self.name.clone(),
            id: id.to_owned(),
            kind: kind.to_owned(),
            metadata: metadata.clone(),
            document: Some((space.to_owned(), doc_id.to_owned())),
        })
    }

    /// Deletes node `id` and every edge it is an end of; `true` if the node existed.
    ///
    /// # Errors
    ///
    /// As [`Graph::set_node`].
    pub fn delete_node(&self, id: &str) -> Result<bool> {
        Ok(self
            .submit(GraphWrite::DeleteNode {
                graph: self.name.clone(),
                id: id.to_owned(),
            })?
            .deleted)
    }

    /// Creates or replaces the edge `source -kind-> target`.
    ///
    /// # Errors
    ///
    /// Invalid names, a weight that is not finite; as [`crate::Session::commit`].
    pub fn add_edge(&self, source: &str, target: &str, kind: &str, weight: f64) -> Result<Outcome> {
        self.submit(GraphWrite::AddEdge {
            graph: self.name.clone(),
            source: source.to_owned(),
            target: target.to_owned(),
            kind: kind.to_owned(),
            weight,
        })
    }

    /// Removes the edge `source -kind-> target`; `true` if it existed.
    ///
    /// # Errors
    ///
    /// As [`Graph::add_edge`].
    pub fn remove_edge(&self, source: &str, target: &str, kind: &str) -> Result<bool> {
        let write = GraphWrite::RemoveEdge {
            graph: self.name.clone(),
            source: source.to_owned(),
            target: target.to_owned(),
            kind: kind.to_owned(),
        };
        Ok(self.submit(write)?.deleted)
    }

    /// Node `id`.
    ///
    /// # Errors
    ///
    /// Invalid names, engine errors.
    pub fn node(&self, id: &str) -> Result<Option<GraphNode>> {
        validate_name("graph", &self.name)?;
        validate_name("node id", id)?;
        let reader = self.context.reader(false);
        reader
            .get(&node_key(&self.name, id))?
            .map(|item| decode_node(&self.name, id, &item.value))
            .transpose()
    }

    /// The document node `id` links to, read through the caller's guard of its space.
    ///
    /// # Errors
    ///
    /// As [`Graph::node`].
    pub fn node_document(&self, id: &str) -> Result<Option<JsonEntry>> {
        let Some(node) = self.node(id)? else {
            return Ok(None);
        };
        let (Some(space), Some(doc_id)) = (node.doc_space, node.doc_id) else {
            return Ok(None);
        };
        read::doc_get(
            &self.context.reader(false),
            &guard(self.context, &space),
            &space,
            &doc_id,
        )
    }

    /// Nodes in id order, optionally of one kind.
    ///
    /// # Errors
    ///
    /// As [`Graph::node`].
    pub fn nodes(&self, kind: Option<&str>, scan: &Scan) -> Result<Vec<GraphNode>> {
        validate_name("graph", &self.name)?;
        let reader = self.context.reader(true);
        let base = keys::collection(keys::GRAPH_NODE, &self.name);
        let mut nodes = Vec::new();
        for item in read::scan_range(&reader, &base, scan)? {
            let item = item?;
            reader.observe(&item);
            let id = std::str::from_utf8(keys::suffix(&item.key, &base))
                .map_err(|_| corruption("node id is not UTF-8"))?;
            let node = decode_node(&self.name, id, &item.value)?;
            if kind.is_none_or(|k| node.kind == k) {
                nodes.push(node);
                if scan.limit.is_some_and(|limit| nodes.len() >= limit) {
                    break;
                }
            }
        }
        Ok(nodes)
    }

    /// The edge `source -kind-> target`.
    ///
    /// # Errors
    ///
    /// As [`Graph::node`].
    pub fn edge(&self, source: &str, target: &str, kind: &str) -> Result<Option<GraphEdge>> {
        validate_edge(&self.name, source, target, kind)?;
        let reader = self.context.reader(false);
        let Some(item) = reader.get(&edge_key(keys::EDGE_OUT, &self.name, source, kind, target))?
        else {
            return Ok(None);
        };
        Ok(Some(GraphEdge {
            graph: self.name.clone(),
            source: source.to_owned(),
            target: target.to_owned(),
            kind: kind.to_owned(),
            weight: decode_weight(&item.value)?,
        }))
    }

    /// Edges matching the given ends and kind. With a source, in `(kind, target)` order; with
    /// only a target, in `(kind, source)` order; otherwise in `(source, kind, target)` order.
    ///
    /// # Errors
    ///
    /// As [`Graph::node`].
    pub fn edges(
        &self,
        source: Option<&str>,
        target: Option<&str>,
        kind: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<GraphEdge>> {
        validate_name("graph", &self.name)?;
        let reader = self.context.reader(true);
        let graph_base = |tag| keys::collection(tag, &self.name);
        let (tag, prefix) = match (source, target) {
            (Some(source), _) => (
                keys::EDGE_OUT,
                adjacency(keys::EDGE_OUT, &self.name, source),
            ),
            (None, Some(target)) => (keys::EDGE_IN, adjacency(keys::EDGE_IN, &self.name, target)),
            (None, None) => (keys::EDGE_OUT, graph_base(keys::EDGE_OUT)),
        };
        let mut prefix = prefix;
        if source.is_some() || target.is_some() {
            if let Some(kind) = kind {
                prefix.extend_from_slice(kind.as_bytes());
                prefix.push(0);
            }
        }
        let (start, end) = prefix_bounds(&prefix);
        let base = graph_base(tag);
        let mut edges = Vec::new();
        for item in reader.range(Bound::Included(&start), Bound::Excluded(&end))? {
            let item = item?;
            reader.observe(&item);
            let rest = keys::suffix(&item.key, &base);
            let split = rest
                .iter()
                .position(|&b| b == 0)
                .ok_or_else(|| corruption("edge key without an end"))?;
            let near = std::str::from_utf8(&rest[..split])
                .map_err(|_| corruption("edge key is not UTF-8"))?;
            let (edge_kind, far) = split_edge(&rest[split + 1..])?;
            let (from, to) = if tag == keys::EDGE_OUT {
                (near.to_owned(), far)
            } else {
                (far, near.to_owned())
            };
            let matches = kind.is_none_or(|k| k == edge_kind) && target.is_none_or(|t| t == to);
            if matches {
                edges.push(GraphEdge {
                    graph: self.name.clone(),
                    source: from,
                    target: to,
                    kind: edge_kind,
                    weight: decode_weight(&item.value)?,
                });
                if limit.is_some_and(|limit| edges.len() >= limit) {
                    break;
                }
            }
        }
        Ok(edges)
    }

    fn neighbors_in(
        &self,
        reader: &impl Source,
        node: &str,
        direction: Direction,
        kind: Option<&str>,
        out: &mut BTreeSet<String>,
    ) -> Result<()> {
        let tags: &[u8] = match direction {
            Direction::Outbound => &[keys::EDGE_OUT],
            Direction::Inbound => &[keys::EDGE_IN],
            Direction::Both => &[keys::EDGE_OUT, keys::EDGE_IN],
        };
        for &tag in tags {
            let mut prefix = adjacency(tag, &self.name, node);
            let base = prefix.clone();
            if let Some(kind) = kind {
                prefix.extend_from_slice(kind.as_bytes());
                prefix.push(0);
            }
            let (start, end) = prefix_bounds(&prefix);
            for item in reader.range(Bound::Included(&start), Bound::Excluded(&end))? {
                let item = item?;
                reader.observe(&item);
                out.insert(split_edge(keys::suffix(&item.key, &base))?.1);
            }
        }
        Ok(())
    }

    /// Distinct neighbors of `node` in `direction`, optionally through edges of one kind, in id
    /// order.
    ///
    /// # Errors
    ///
    /// As [`Graph::node`].
    pub fn neighbors(
        &self,
        node: &str,
        direction: Direction,
        kind: Option<&str>,
    ) -> Result<Vec<String>> {
        validate_name("graph", &self.name)?;
        validate_name("node id", node)?;
        let mut out = BTreeSet::new();
        self.neighbors_in(&self.context.reader(true), node, direction, kind, &mut out)?;
        Ok(out.into_iter().collect())
    }

    /// Every node reachable from `starts` (included) in `direction`, optionally through edges of
    /// one kind, in id order. Reads one consistent state.
    ///
    /// # Errors
    ///
    /// As [`Graph::node`].
    pub fn reachable(
        &self,
        starts: &[&str],
        direction: Direction,
        kind: Option<&str>,
    ) -> Result<Vec<String>> {
        validate_name("graph", &self.name)?;
        let reader = self.context.reader(true);
        let mut visited: BTreeSet<String> = BTreeSet::new();
        let mut queue = VecDeque::new();
        for &start in starts {
            validate_name("node id", start)?;
            if visited.insert(start.to_owned()) {
                queue.push_back(start.to_owned());
            }
        }
        while let Some(node) = queue.pop_front() {
            let mut next = BTreeSet::new();
            self.neighbors_in(&reader, &node, direction, kind, &mut next)?;
            for neighbor in next {
                if visited.insert(neighbor.clone()) {
                    queue.push_back(neighbor);
                }
            }
        }
        Ok(visited.into_iter().collect())
    }
}
