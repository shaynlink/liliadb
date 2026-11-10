use std::collections::HashMap;

use lilia_core::{Direction, Result};

use crate::GraphStore;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewEdge {
    pub target: usize,
    pub kind_id: u16,
    pub weight: f64,
}

#[derive(Debug, Clone)]
pub struct GraphView {
    pub node_names: Vec<String>,
    pub node_indices: HashMap<String, usize>,
    pub kinds: Vec<String>,
    pub kind_indices: HashMap<String, u16>,
    pub outgoing: Vec<Vec<ViewEdge>>,
    pub incoming: Vec<Vec<ViewEdge>>,
}

impl GraphView {
    pub fn from_store(store: &GraphStore<'_>, kind_filter: Option<&str>) -> Result<Self> {
        let nodes = store.list_nodes(None, u32::MAX)?;
        let edges = store.list_edges(None, None, kind_filter, u32::MAX)?;

        let mut node_names = Vec::with_capacity(nodes.len());
        let mut node_indices = HashMap::with_capacity(nodes.len());

        for (idx, node) in nodes.into_iter().enumerate() {
            node_indices.insert(node.id.clone(), idx);
            node_names.push(node.id);
        }

        let mut kinds = Vec::new();
        let mut kind_indices = HashMap::new();

        // Ensure all edge endpoints exist in the node mapping
        for edge in &edges {
            if !node_indices.contains_key(&edge.source) {
                let idx = node_names.len();
                node_indices.insert(edge.source.clone(), idx);
                node_names.push(edge.source.clone());
            }
            if !node_indices.contains_key(&edge.target) {
                let idx = node_names.len();
                node_indices.insert(edge.target.clone(), idx);
                node_names.push(edge.target.clone());
            }
            if !kind_indices.contains_key(&edge.kind) {
                let kid = kinds.len() as u16;
                kind_indices.insert(edge.kind.clone(), kid);
                kinds.push(edge.kind.clone());
            }
        }

        let n = node_names.len();
        let mut outgoing: Vec<Vec<ViewEdge>> = vec![Vec::new(); n];
        let mut incoming: Vec<Vec<ViewEdge>> = vec![Vec::new(); n];

        for edge in edges {
            let src_idx = node_indices[&edge.source];
            let tgt_idx = node_indices[&edge.target];
            let kind_id = kind_indices[&edge.kind];
            let weight = if edge.weight.is_finite() && edge.weight >= 0.0 {
                edge.weight
            } else {
                1.0
            };

            outgoing[src_idx].push(ViewEdge {
                target: tgt_idx,
                kind_id,
                weight,
            });
            incoming[tgt_idx].push(ViewEdge {
                target: src_idx,
                kind_id,
                weight,
            });
        }

        Ok(Self {
            node_names,
            node_indices,
            kinds,
            kind_indices,
            outgoing,
            incoming,
        })
    }

    #[must_use]
    pub fn node_count(&self) -> usize {
        self.node_names.len()
    }

    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.outgoing.iter().map(Vec::len).sum()
    }

    #[must_use]
    pub fn node_idx(&self, id: &str) -> Option<usize> {
        self.node_indices.get(id).copied()
    }

    #[must_use]
    pub fn node_id(&self, idx: usize) -> &str {
        &self.node_names[idx]
    }

    #[must_use]
    pub fn kind_id(&self, kind: &str) -> Option<u16> {
        self.kind_indices.get(kind).copied()
    }

    #[must_use]
    pub fn kind_str(&self, kind_id: u16) -> &str {
        &self.kinds[kind_id as usize]
    }

    #[must_use]
    pub fn out_edges(&self, idx: usize) -> &[ViewEdge] {
        &self.outgoing[idx]
    }

    #[must_use]
    pub fn in_edges(&self, idx: usize) -> &[ViewEdge] {
        &self.incoming[idx]
    }

    #[must_use]
    pub fn edges_dir(&self, idx: usize, dir: Direction) -> Vec<ViewEdge> {
        match dir {
            Direction::Outgoing => self.out_edges(idx).to_vec(),
            Direction::Incoming => self.in_edges(idx).to_vec(),
            Direction::Both => {
                let mut all = self.out_edges(idx).to_vec();
                all.extend_from_slice(self.in_edges(idx));
                all
            }
        }
    }
}
