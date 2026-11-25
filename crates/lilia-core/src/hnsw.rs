use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::{ErrorCode, HnswConfig, LiliaError, Result};
use distance::compute_vector_distance;

/// Max level cap to prevent degenerate deep layers.
const MAX_HNSW_LEVEL: usize = 16;

#[derive(Debug, Clone, PartialEq)]
struct Candidate {
    id: u32,
    distance: f64,
}

impl Eq for Candidate {}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse order for min-heap (lowest distance has highest priority)
        other
            .distance
            .partial_cmp(&self.distance)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq)]
struct FurthestFirst {
    id: u32,
    distance: f64,
}

impl Eq for FurthestFirst {}

impl Ord for FurthestFirst {
    fn cmp(&self, other: &Self) -> Ordering {
        // Standard order for max-heap (highest distance has highest priority)
        self.distance
            .partial_cmp(&other.distance)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for FurthestFirst {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A node in the HNSW multi-layer graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HnswNode {
    pub id: String,
    pub internal_id: u32,
    pub level: usize,
    /// Adjacency lists for each level 0..=level.
    /// `neighbors[l]` contains the internal IDs of neighbors at level `l`.
    pub neighbors: Vec<Vec<u32>>,
}

/// In-memory Hierarchical Navigable Small World (HNSW) index.
#[derive(Debug, Clone)]
pub struct HnswIndex {
    config: HnswConfig,
    ml: f64,
    entry_point: Option<u32>,
    max_level: usize,
    nodes: Vec<Option<HnswNode>>,
    vectors: Vec<Option<Vec<u8>>>,
    id_to_internal: HashMap<String, u32>,
    internal_to_id: Vec<Option<String>>,
    deleted_ids: HashSet<u32>,
    active_count: usize,
}

impl HnswIndex {
    /// Creates a new empty HNSW index with the specified configuration.
    #[must_use]
    pub fn new(config: HnswConfig) -> Self {
        let m = config.m.max(2);
        #[allow(
            clippy::cast_precision_loss,
            reason = "M is a connectivity count of a few dozen"
        )]
        let ml = 1.0 / (m as f64).ln();
        Self {
            config,
            ml,
            entry_point: None,
            max_level: 0,
            nodes: Vec::new(),
            vectors: Vec::new(),
            id_to_internal: HashMap::new(),
            internal_to_id: Vec::new(),
            deleted_ids: HashSet::new(),
            active_count: 0,
        }
    }

    #[must_use]
    pub fn config(&self) -> &HnswConfig {
        &self.config
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.active_count
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.active_count == 0
    }

    #[must_use]
    pub fn max_level(&self) -> usize {
        self.max_level
    }

    #[must_use]
    pub fn entry_point_id(&self) -> Option<&str> {
        self.entry_point.and_then(|id| {
            self.internal_to_id
                .get(id as usize)
                .and_then(|opt| opt.as_deref())
        })
    }

    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.id_to_internal
            .get(id)
            .is_some_and(|&uid| !self.deleted_ids.contains(&uid))
    }

    /// Randomly sample a level for a new element based on exponential decay $P(l) \propto e^{-l / `m_L`}$.
    fn random_level(&self) -> usize {
        let mut rng = rand::rng();
        let r: f64 = rng.random();
        if r <= 0.0 {
            return 0;
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "r is in (0, 1), so the level is finite and non-negative; the cast saturates"
        )]
        let l = (-r.ln() * self.ml).floor() as usize;
        l.min(MAX_HNSW_LEVEL)
    }

    fn max_m(&self, level: usize) -> usize {
        if level == 0 {
            self.config.m * 2
        } else {
            self.config.m
        }
    }

    /// Calculates distance between query vector and a node's stored vector.
    fn distance_to_node(&self, query_bytes: &[u8], node_id: u32) -> f64 {
        if let Some(Some(ref target_bytes)) = self.vectors.get(node_id as usize) {
            compute_vector_distance(
                query_bytes,
                target_bytes,
                self.config.element_type,
                self.config.metric,
            )
        } else {
            f64::MAX
        }
    }

    /// Calculates distance between two stored nodes.
    fn distance_between_nodes(&self, node_a: u32, node_b: u32) -> f64 {
        if let (Some(Some(vec_a)), Some(Some(vec_b))) = (
            self.vectors.get(node_a as usize),
            self.vectors.get(node_b as usize),
        ) {
            compute_vector_distance(vec_a, vec_b, self.config.element_type, self.config.metric)
        } else {
            f64::MAX
        }
    }

    /// Inserts a vector into the HNSW index.
    ///
    /// If an item with `id` already exists, it is replaced. Returns the internal node ID.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidInput`] once the index holds 2^32 nodes.
    pub fn insert(&mut self, id: &str, vector: &[u8]) -> Result<u32> {
        // If node already exists, delete it first to ensure clean topology.
        if self.contains(id) {
            self.delete(id);
        }

        let node_level = self.random_level();
        let internal_id = u32::try_from(self.nodes.len()).map_err(|_| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "an HNSW index holds at most 2^32 nodes",
                false,
            )
        })?;

        let node = HnswNode {
            id: id.to_string(),
            internal_id,
            level: node_level,
            neighbors: vec![Vec::new(); node_level + 1],
        };

        self.nodes.push(Some(node.clone()));
        self.vectors.push(Some(vector.to_vec()));
        self.id_to_internal.insert(id.to_string(), internal_id);
        self.internal_to_id.push(Some(id.to_string()));
        self.active_count += 1;

        if let Some(curr_entry) = self.entry_point {
            let top_level = self.max_level;
            // Phase 1: 1-greedy search from top_level down to node_level + 1
            let (curr_obj, curr_dist) =
                self.greedy_descend(vector, curr_entry, (node_level + 1..=top_level).rev());

            // Phase 2: Search layer and establish bidirectional connections from min(top_level, node_level) down to 0
            let start_level = top_level.min(node_level);
            let mut enter_points = vec![Candidate {
                id: curr_obj,
                distance: curr_dist,
            }];

            for lc in (0..=start_level).rev() {
                let candidates = self.search_layer_internal(
                    vector,
                    &enter_points,
                    self.config.ef_construction,
                    lc,
                );

                let selected_neighbors =
                    self.select_neighbors_heuristic(vector, &candidates, self.max_m(lc));

                // Set neighbors for new node
                if let Some(Some(ref mut n)) = self.nodes.get_mut(internal_id as usize) {
                    n.neighbors[lc].clone_from(&selected_neighbors);
                }

                // Add bidirectional links and shrink if necessary
                for &neighbor in &selected_neighbors {
                    let mut neighbor_needs_shrink = false;
                    let max_connections = self.max_m(lc);

                    if let Some(Some(ref mut n_obj)) = self.nodes.get_mut(neighbor as usize) {
                        if lc < n_obj.neighbors.len() {
                            if !n_obj.neighbors[lc].contains(&internal_id) {
                                n_obj.neighbors[lc].push(internal_id);
                            }
                            if n_obj.neighbors[lc].len() > max_connections {
                                neighbor_needs_shrink = true;
                            }
                        }
                    }

                    if neighbor_needs_shrink {
                        self.shrink_neighbors(neighbor, lc, max_connections);
                    }
                }

                enter_points = candidates;
            }

            if node_level > self.max_level {
                self.max_level = node_level;
                self.entry_point = Some(internal_id);
            }
        } else {
            // First element in index
            self.entry_point = Some(internal_id);
            self.max_level = node_level;
        }
        Ok(internal_id)
    }

    /// Greedy 1-nearest descent from `entry` through `levels`: at each level, moves to the
    /// closest live neighbor until none is closer. Returns the node reached and its distance.
    fn greedy_descend(
        &self,
        query: &[u8],
        entry: u32,
        levels: impl Iterator<Item = usize>,
    ) -> (u32, f64) {
        let mut curr_obj = entry;
        let mut curr_dist = self.distance_to_node(query, curr_obj);
        for lc in levels {
            let mut changed = true;
            while changed {
                changed = false;
                let Some(Some(n)) = self.nodes.get(curr_obj as usize) else {
                    break;
                };
                let Some(neighbors) = n.neighbors.get(lc) else {
                    break;
                };
                for &neighbor in neighbors {
                    if self.deleted_ids.contains(&neighbor) {
                        continue;
                    }
                    let dist = self.distance_to_node(query, neighbor);
                    if dist < curr_dist {
                        curr_dist = dist;
                        curr_obj = neighbor;
                        changed = true;
                    }
                }
            }
        }
        (curr_obj, curr_dist)
    }

    /// Internal beam search at a specific layer `lc`.
    fn search_layer_internal(
        &self,
        query: &[u8],
        enter_points: &[Candidate],
        ef: usize,
        lc: usize,
    ) -> Vec<Candidate> {
        let mut visited: HashSet<u32> = HashSet::new();
        let mut candidates: BinaryHeap<Candidate> = BinaryHeap::new();
        let mut w: BinaryHeap<FurthestFirst> = BinaryHeap::new();

        for ep in enter_points {
            visited.insert(ep.id);
            candidates.push(ep.clone());
            w.push(FurthestFirst {
                id: ep.id,
                distance: ep.distance,
            });
        }

        while let Some(curr) = candidates.pop() {
            let furthest_dist = w.peek().map_or(f64::MAX, |f| f.distance);
            if curr.distance > furthest_dist {
                break;
            }

            if let Some(Some(ref n)) = self.nodes.get(curr.id as usize) {
                if lc < n.neighbors.len() {
                    for &neighbor in &n.neighbors[lc] {
                        if visited.insert(neighbor) && !self.deleted_ids.contains(&neighbor) {
                            let dist = self.distance_to_node(query, neighbor);
                            let furthest = w.peek().map_or(f64::MAX, |f| f.distance);

                            if dist < furthest || w.len() < ef {
                                candidates.push(Candidate {
                                    id: neighbor,
                                    distance: dist,
                                });
                                w.push(FurthestFirst {
                                    id: neighbor,
                                    distance: dist,
                                });
                                if w.len() > ef {
                                    w.pop();
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut result = Vec::with_capacity(w.len());
        for item in w.into_sorted_vec() {
            result.push(Candidate {
                id: item.id,
                distance: item.distance,
            });
        }
        result
    }

    /// Selects neighbors according to the heuristic from Malkov & Yashunin (2018).
    ///
    /// Keeps candidates that are closer to the base vector than to any already chosen neighbor,
    /// ensuring diversity and high connectivity across spatial clusters.
    fn select_neighbors_heuristic(
        &self,
        _query: &[u8],
        candidates: &[Candidate],
        max_m: usize,
    ) -> Vec<u32> {
        if candidates.len() <= max_m {
            return candidates.iter().map(|c| c.id).collect();
        }

        let mut sorted = candidates.to_vec();
        sorted.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(Ordering::Equal)
        });

        let mut selected: Vec<u32> = Vec::with_capacity(max_m);
        for cand in sorted {
            if selected.len() >= max_m {
                break;
            }
            let mut keep = true;
            for &already_selected in &selected {
                let dist_between = self.distance_between_nodes(cand.id, already_selected);
                if dist_between < cand.distance {
                    keep = false;
                    break;
                }
            }
            if keep {
                selected.push(cand.id);
            }
        }

        // If heuristic pruned too aggressively, fill up to max_m with closest remaining
        if selected.len() < max_m {
            for cand in candidates {
                if selected.len() >= max_m {
                    break;
                }
                if !selected.contains(&cand.id) {
                    selected.push(cand.id);
                }
            }
        }

        selected
    }

    /// Shrinks the connections of a node at layer `lc` to `max_m` using the heuristic.
    fn shrink_neighbors(&mut self, node_id: u32, lc: usize, max_m: usize) {
        let neighbors = if let Some(Some(ref n)) = self.nodes.get(node_id as usize) {
            if lc < n.neighbors.len() {
                n.neighbors[lc].clone()
            } else {
                return;
            }
        } else {
            return;
        };

        if neighbors.len() <= max_m {
            return;
        }

        let mut candidates = Vec::with_capacity(neighbors.len());
        for &nid in &neighbors {
            let dist = self.distance_between_nodes(node_id, nid);
            candidates.push(Candidate {
                id: nid,
                distance: dist,
            });
        }

        let dummy_vec = if let Some(Some(ref v)) = self.vectors.get(node_id as usize) {
            v.as_slice()
        } else {
            return;
        };

        let selected = self.select_neighbors_heuristic(dummy_vec, &candidates, max_m);
        if let Some(Some(ref mut n)) = self.nodes.get_mut(node_id as usize) {
            if lc < n.neighbors.len() {
                n.neighbors[lc] = selected;
            }
        }
    }

    /// Deletes an item from the HNSW index by ID.
    ///
    /// Marks the node as deleted and updates neighbor links.
    /// Returns true if the node was found and removed.
    pub fn delete(&mut self, id: &str) -> bool {
        let Some(&internal_id) = self.id_to_internal.get(id) else {
            return false;
        };
        if !self.deleted_ids.insert(internal_id) {
            return false; // already deleted
        }

        self.active_count = self.active_count.saturating_sub(1);

        // Remove node from all neighbors' adjacency lists
        let neighbor_levels: Vec<(usize, Vec<u32>)> =
            if let Some(Some(ref n)) = self.nodes.get(internal_id as usize) {
                n.neighbors
                    .iter()
                    .enumerate()
                    .map(|(lc, nb)| (lc, nb.clone()))
                    .collect()
            } else {
                Vec::new()
            };
        for (lc, level_neighbors) in neighbor_levels {
            for neighbor in level_neighbors {
                if let Some(Some(ref mut n_obj)) = self.nodes.get_mut(neighbor as usize) {
                    if lc < n_obj.neighbors.len() {
                        n_obj.neighbors[lc].retain(|&x| x != internal_id);
                    }
                }
            }
        }

        // If entry point was deleted, find a replacement
        if self.entry_point == Some(internal_id) {
            self.repair_entry_point();
        }

        true
    }

    fn repair_entry_point(&mut self) {
        self.entry_point = None;
        if self.active_count == 0 {
            self.max_level = 0;
            return;
        }

        for lc in (0..=self.max_level).rev() {
            // `insert` refuses a node past `u32::MAX`, so every index fits.
            for (uid, node_opt) in (0_u32..).zip(&self.nodes) {
                if let Some(ref n) = node_opt {
                    if !self.deleted_ids.contains(&uid) && n.level >= lc {
                        self.entry_point = Some(uid);
                        self.max_level = lc;
                        return;
                    }
                }
            }
        }
    }

    /// Rebuilds / compacts the HNSW graph from a list of vectors.
    ///
    /// # Errors
    ///
    /// As [`HnswIndex::insert`].
    pub fn rebuild_from_vectors(&mut self, items: &[(&str, &[u8])]) -> Result<()> {
        let config = self.config.clone();
        *self = Self::new(config);
        for (id, vec) in items {
            self.insert(id, vec)?;
        }
        Ok(())
    }
}

mod distance;
mod search;

#[cfg(test)]
mod tests;
