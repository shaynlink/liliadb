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
        let mut next_paths = if self.paths.is_some() {
            Some(Vec::new())
        } else {
            None
        };

        if let Some(ref current_paths) = self.paths {
            for path in current_paths {
                if let Some(last_node) = path.last() {
                    let neighbors = self.store.neighbors_with_kind(last_node, direction, kind)?;
                    for neighbor in neighbors {
                        let mut p = path.clone();
                        p.push(neighbor.clone());
                        next_nodes.push(neighbor);
                        next_paths.as_mut().unwrap().push(p);
                    }
                }
            }
        } else {
            for node in &self.current {
                let neighbors = self.store.neighbors_with_kind(node, direction, kind)?;
                next_nodes.extend(neighbors);
            }
        }

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
        let mut collected_paths = if self.paths.is_some() {
            Some(Vec::new())
        } else {
            None
        };

        if min == 0 {
            collected.extend(self.current.clone());
            if let Some(ref cp) = self.paths {
                collected_paths.as_mut().unwrap().extend(cp.clone());
            }
        }

        for hop in 1..=max {
            self = self.step(direction, kind)?;
            if hop >= min {
                collected.extend(self.current.clone());
                if let Some(ref cp) = self.paths {
                    collected_paths.as_mut().unwrap().extend(cp.clone());
                }
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
            for (node, path) in self.current.into_iter().zip(paths.into_iter()) {
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

    pub fn filter_node<F>(mut self, predicate: F) -> Result<Self>
    where
        F: Fn(&GraphNode) -> bool,
    {
        let mut filtered_nodes = Vec::new();
        let mut filtered_paths = if self.paths.is_some() {
            Some(Vec::new())
        } else {
            None
        };

        if let Some(paths) = self.paths {
            for (node_id, path) in self.current.into_iter().zip(paths.into_iter()) {
                if let Some(node) = self.store.get_node(&node_id)? {
                    if predicate(&node) {
                        filtered_nodes.push(node_id);
                        filtered_paths.as_mut().unwrap().push(path);
                    }
                }
            }
            self.paths = filtered_paths;
        } else {
            for node_id in self.current {
                if let Some(node) = self.store.get_node(&node_id)? {
                    if predicate(&node) {
                        filtered_nodes.push(node_id);
                    }
                }
            }
        }

        self.current = filtered_nodes;
        Ok(self)
    }

    pub fn filter_kind(self, expected_kind: &str) -> Result<Self> {
        let expected = expected_kind.to_string();
        self.filter_node(move |node| node.kind == expected)
    }

    pub fn filter_doc<F>(mut self, predicate: F) -> Result<Self>
    where
        F: Fn(&serde_json::Value) -> bool,
    {
        let mut filtered_nodes = Vec::new();
        let mut filtered_paths = if self.paths.is_some() {
            Some(Vec::new())
        } else {
            None
        };

        if let Some(paths) = self.paths {
            for (node_id, path) in self.current.into_iter().zip(paths.into_iter()) {
                if let Some(node) = self.store.get_node(&node_id)? {
                    if let Some(doc) = resolve_node_document(self.store.database(), &node)? {
                        if predicate(&doc) {
                            filtered_nodes.push(node_id);
                            filtered_paths.as_mut().unwrap().push(path);
                        }
                    }
                }
            }
            self.paths = filtered_paths;
        } else {
            for node_id in self.current {
                if let Some(node) = self.store.get_node(&node_id)? {
                    if let Some(doc) = resolve_node_document(self.store.database(), &node)? {
                        if predicate(&doc) {
                            filtered_nodes.push(node_id);
                        }
                    }
                }
            }
        }

        self.current = filtered_nodes;
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
