use std::collections::HashSet;

use lilia_core::{Direction, GraphNode, Result};

use crate::unified::{resolve_node_document, UnifiedNode};
use crate::GraphStore;

#[derive(Debug)]
pub struct GraphTraversal<'a> {
    store: &'a GraphStore<'a>,
    current: Vec<String>,
    paths: Option<Vec<Vec<String>>>,
}

impl<'a> GraphTraversal<'a> {
    pub(crate) fn new(store: &'a GraphStore<'a>, start_nodes: Vec<String>) -> Self {
        Self {
            store,
            current: start_nodes,
            paths: None,
        }
    }

    #[must_use]
    pub fn track_paths(mut self) -> Self {
        let initial_paths = self.current.iter().map(|n| vec![n.clone()]).collect();
        self.paths = Some(initial_paths);
        self
    }

    pub fn out<'b>(self, kind: impl Into<Option<&'b str>>) -> Result<Self> {
        self.step(Direction::Outgoing, kind.into())
    }

    pub fn in_<'b>(self, kind: impl Into<Option<&'b str>>) -> Result<Self> {
        self.step(Direction::Incoming, kind.into())
    }

    pub fn both<'b>(self, kind: impl Into<Option<&'b str>>) -> Result<Self> {
        self.step(Direction::Both, kind.into())
    }

    pub fn step(mut self, direction: Direction, kind: Option<&str>) -> Result<Self> {
        let mut next_nodes = Vec::new();
        let next_paths = if let Some(current_paths) = &self.paths {
            let mut next_paths = Vec::new();
            for path in current_paths {
                let Some(last_node) = path.last() else {
                    continue;
                };
                for neighbor in self.store.neighbors_with_kind(last_node, direction, kind)? {
                    let mut extended = path.clone();
                    extended.push(neighbor.clone());
                    next_nodes.push(neighbor);
                    next_paths.push(extended);
                }
            }
            Some(next_paths)
        } else {
            for node in &self.current {
                next_nodes.extend(self.store.neighbors_with_kind(node, direction, kind)?);
            }
            None
        };

        self.current = next_nodes;
        self.paths = next_paths;
        Ok(self)
    }

    pub fn hops(
        mut self,
        min: usize,
        max: usize,
        direction: Direction,
        kind: Option<&str>,
    ) -> Result<Self> {
        let mut collected = Vec::new();
        let mut collected_paths = self.paths.as_ref().map(|_| Vec::new());
        let mut collect = |traversal: &Self| {
            collected.extend(traversal.current.iter().cloned());
            if let (Some(all), Some(paths)) = (collected_paths.as_mut(), &traversal.paths) {
                all.extend(paths.iter().cloned());
            }
        };

        if min == 0 {
            collect(&self);
        }
        for hop in 1..=max {
            self = self.step(direction, kind)?;
            if hop >= min {
                collect(&self);
            }
        }

        self.current = collected;
        self.paths = collected_paths;
        Ok(self)
    }

    #[must_use]
    pub fn distinct(mut self) -> Self {
        let mut seen = HashSet::new();
        let mut deduped = Vec::new();

        if let Some(paths) = self.paths {
            let mut deduped_paths = Vec::new();
            for (node, path) in self.current.into_iter().zip(paths) {
                if seen.insert(node.clone()) {
                    deduped.push(node);
                    deduped_paths.push(path);
                }
            }
            self.paths = Some(deduped_paths);
        } else {
            for node in self.current {
                if seen.insert(node.clone()) {
                    deduped.push(node);
                }
            }
        }

        self.current = deduped;
        self
    }

    #[must_use]
    pub fn limit(mut self, n: usize) -> Self {
        if self.current.len() > n {
            self.current.truncate(n);
            if let Some(ref mut paths) = self.paths {
                paths.truncate(n);
            }
        }
        self
    }

    #[must_use]
    pub fn take(self, n: usize) -> Self {
        self.limit(n)
    }

    pub fn filter_node<F>(self, predicate: F) -> Result<Self>
    where
        F: Fn(&GraphNode) -> bool,
    {
        self.retain(|node| Ok(predicate(node)))
    }

    pub fn filter_kind(self, expected_kind: &str) -> Result<Self> {
        let expected = expected_kind.to_string();
        self.filter_node(move |node| node.kind == expected)
    }

    pub fn filter_doc<F>(self, predicate: F) -> Result<Self>
    where
        F: Fn(&serde_json::Value) -> bool,
    {
        let database = self.store.database();
        self.retain(|node| {
            Ok(resolve_node_document(database, node)?.is_some_and(|doc| predicate(&doc)))
        })
    }

    /// Keeps the current nodes, with their tracked paths, that exist and satisfy `keep`.
    fn retain(mut self, keep: impl Fn(&GraphNode) -> Result<bool>) -> Result<Self> {
        let mut kept_nodes = Vec::new();
        let mut kept_paths = self.paths.as_ref().map(|_| Vec::new());
        let mut paths = self.paths.take().map(Vec::into_iter);
        for node_id in std::mem::take(&mut self.current) {
            let path = paths.as_mut().and_then(Iterator::next);
            let Some(node) = self.store.get_node(&node_id)? else {
                continue;
            };
            if keep(&node)? {
                kept_nodes.push(node_id);
                if let (Some(kept), Some(path)) = (kept_paths.as_mut(), path) {
                    kept.push(path);
                }
            }
        }
        self.current = kept_nodes;
        self.paths = kept_paths;
        Ok(self)
    }

    pub fn to_ids(self) -> Result<Vec<String>> {
        Ok(self.current)
    }

    pub fn to_nodes(self) -> Result<Vec<GraphNode>> {
        let mut nodes = Vec::with_capacity(self.current.len());
        for id in &self.current {
            if let Some(node) = self.store.get_node(id)? {
                nodes.push(node);
            }
        }
        Ok(nodes)
    }

    pub fn to_documents(self) -> Result<Vec<serde_json::Value>> {
        let mut docs = Vec::with_capacity(self.current.len());
        for id in &self.current {
            if let Some(node) = self.store.get_node(id)? {
                if let Some(doc) = resolve_node_document(self.store.database(), &node)? {
                    docs.push(doc);
                }
            }
        }
        Ok(docs)
    }

    pub fn to_unified(self) -> Result<Vec<UnifiedNode>> {
        let mut unified = Vec::with_capacity(self.current.len());
        for id in &self.current {
            if let Some(node) = self.store.get_node(id)? {
                let doc = resolve_node_document(self.store.database(), &node)?;
                unified.push(UnifiedNode::new(node, doc));
            }
        }
        Ok(unified)
    }

    pub fn to_json(self) -> Result<serde_json::Value> {
        let unified = self.to_unified()?;
        let json_items: Vec<serde_json::Value> = unified
            .into_iter()
            .map(|u| {
                if let Some(doc) = u.document {
                    doc
                } else {
                    u.to_json()
                }
            })
            .collect();
        Ok(serde_json::Value::Array(json_items))
    }

    pub fn to_paths(self) -> Result<Vec<Vec<String>>> {
        Ok(self
            .paths
            .unwrap_or_else(|| self.current.iter().map(|n| vec![n.clone()]).collect()))
    }

    pub fn count(self) -> Result<usize> {
        Ok(self.current.len())
    }
}
