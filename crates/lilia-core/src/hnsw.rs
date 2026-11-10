#![allow(
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::similar_names,
    clippy::too_many_lines
)]

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::quantization::{
    cosine_distance_f32, cosine_distance_i8, dot_distance_f32, hamming_distance_bits,
    l2_distance_f32,
};
use crate::{HnswConfig, Result, VectorElementType, VectorHit, VectorMetric};

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

    /// Randomly sample a level for a new element based on exponential decay $P(l) \propto e^{-l / m_L}$.
    fn random_level(&self) -> usize {
        let mut rng = rand::rng();
        let r: f64 = rng.random();
        if r <= 0.0 {
            return 0;
        }
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
    /// If an item with `id` already exists, it is replaced.
    /// Returns the internal node ID and a list of `(node_internal_id, level, neighbors)`
    /// that were modified and should be persisted to disk.
    #[allow(clippy::type_complexity)]
    pub fn insert(
        &mut self,
        id: &str,
        vector: &[u8],
    ) -> Result<(u32, Vec<(String, usize, Vec<String>)>)> {
        // If node already exists, delete it first to ensure clean topology.
        if self.contains(id) {
            let _ = self.delete(id);
        }

        let node_level = self.random_level();
        let internal_id = self.nodes.len() as u32;

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

        let mut modified_nodes: HashSet<(u32, usize)> = HashSet::new();

        if let Some(curr_entry) = self.entry_point {
            let mut curr_obj = curr_entry;
            let mut curr_dist = self.distance_to_node(vector, curr_obj);
            let top_level = self.max_level;

            // Phase 1: 1-greedy search from top_level down to node_level + 1
            if top_level > node_level {
                for lc in (node_level + 1..=top_level).rev() {
                    let mut changed = true;
                    while changed {
                        changed = false;
                        if let Some(Some(ref n)) = self.nodes.get(curr_obj as usize) {
                            if lc < n.neighbors.len() {
                                for &neighbor in &n.neighbors[lc] {
                                    if self.deleted_ids.contains(&neighbor) {
                                        continue;
                                    }
                                    let dist = self.distance_to_node(vector, neighbor);
                                    if dist < curr_dist {
                                        curr_dist = dist;
                                        curr_obj = neighbor;
                                        changed = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }

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
                    n.neighbors[lc] = selected_neighbors.clone();
                }
                modified_nodes.insert((internal_id, lc));

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
                            modified_nodes.insert((neighbor, lc));
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
            for lc in 0..=node_level {
                modified_nodes.insert((internal_id, lc));
            }
        }

        // Collect modified edges for persistence
        let mut updates = Vec::with_capacity(modified_nodes.len());
        for (uid, level) in modified_nodes {
            if let Some(Some(ref n)) = self.nodes.get(uid as usize) {
                if level < n.neighbors.len() {
                    let neighbor_strings: Vec<String> = n.neighbors[level]
                        .iter()
                        .filter_map(|&nid| {
                            self.internal_to_id
                                .get(nid as usize)
                                .and_then(std::clone::Clone::clone)
                        })
                        .collect();
                    updates.push((n.id.clone(), level, neighbor_strings));
                }
            }
        }

        Ok((internal_id, updates))
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
            for (idx, node_opt) in self.nodes.iter().enumerate() {
                if let Some(ref n) = node_opt {
                    let uid = idx as u32;
                    if !self.deleted_ids.contains(&uid) && n.level >= lc {
                        self.entry_point = Some(uid);
                        self.max_level = lc;
                        return;
                    }
                }
            }
        }
    }

    /// Searches for the top `limit` nearest neighbors matching the query vector.
    ///
    /// Supports **Pre-filtering**: `filter` evaluates document IDs during graph traversal.
    /// Graph exploration continues across all nodes to preserve small-world navigability,
    /// but only nodes matching the filter are admitted into the final result set!
    #[must_use]
    pub fn search(
        &self,
        collection: &str,
        query: &[u8],
        limit: usize,
        ef_search: Option<usize>,
        filter: Option<&dyn Fn(&str) -> bool>,
    ) -> Vec<VectorHit> {
        if self.active_count == 0 || limit == 0 {
            return Vec::new();
        }

        let Some(entry_id) = self.entry_point else {
            return Vec::new();
        };

        let ef = ef_search.unwrap_or(self.config.ef_search).max(limit);
        let mut curr_obj = entry_id;
        let mut curr_dist = self.distance_to_node(query, curr_obj);

        // Phase 1: 1-greedy search from max_level down to 1
        for lc in (1..=self.max_level).rev() {
            let mut changed = true;
            while changed {
                changed = false;
                if let Some(Some(ref n)) = self.nodes.get(curr_obj as usize) {
                    if lc < n.neighbors.len() {
                        for &neighbor in &n.neighbors[lc] {
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
            }
        }

        // Phase 2: Beam search at level 0 with pre-filtering
        let mut visited: HashSet<u32> = HashSet::new();
        let mut candidates: BinaryHeap<Candidate> = BinaryHeap::new();
        let mut results: BinaryHeap<FurthestFirst> = BinaryHeap::new();

        visited.insert(curr_obj);
        candidates.push(Candidate {
            id: curr_obj,
            distance: curr_dist,
        });

        // Check if entry point matches filter
        if !self.deleted_ids.contains(&curr_obj) {
            let matches_filter = match filter {
                Some(f) => {
                    let id_str = self
                        .internal_to_id
                        .get(curr_obj as usize)
                        .and_then(|x| x.as_deref())
                        .unwrap_or("");
                    f(id_str)
                }
                None => true,
            };
            if matches_filter {
                results.push(FurthestFirst {
                    id: curr_obj,
                    distance: curr_dist,
                });
            }
        }

        while let Some(curr) = candidates.pop() {
            let furthest_dist = results.peek().map_or(f64::MAX, |f| f.distance);
            if results.len() >= ef && curr.distance > furthest_dist {
                break;
            }

            if let Some(Some(ref n)) = self.nodes.get(curr.id as usize) {
                if !n.neighbors.is_empty() {
                    for &neighbor in &n.neighbors[0] {
                        if visited.insert(neighbor) && !self.deleted_ids.contains(&neighbor) {
                            let dist = self.distance_to_node(query, neighbor);
                            let furthest = results.peek().map_or(f64::MAX, |f| f.distance);

                            if dist < furthest || results.len() < ef {
                                candidates.push(Candidate {
                                    id: neighbor,
                                    distance: dist,
                                });

                                // Check filter admission
                                let matches_filter = match filter {
                                    Some(f) => {
                                        let id_str = self
                                            .internal_to_id
                                            .get(neighbor as usize)
                                            .and_then(|x| x.as_deref())
                                            .unwrap_or("");
                                        f(id_str)
                                    }
                                    None => true,
                                };

                                if matches_filter {
                                    results.push(FurthestFirst {
                                        id: neighbor,
                                        distance: dist,
                                    });
                                    if results.len() > ef {
                                        results.pop();
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Convert sorted furthest-first heap to nearest-first list
        let mut sorted = results.into_sorted_vec();
        sorted.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(Ordering::Equal)
        });

        if sorted.len() > limit {
            sorted.truncate(limit);
        }

        sorted
            .into_iter()
            .filter_map(|item| {
                let id = self
                    .internal_to_id
                    .get(item.id as usize)
                    .and_then(std::clone::Clone::clone)?;
                Some(VectorHit {
                    collection: collection.to_string(),
                    id,
                    distance: item.distance,
                })
            })
            .collect()
    }

    /// Two-Stage Coarse-to-Fine Search & Re-ranking:
    ///
    /// 1. Searches top `limit * oversample` candidates using fast quantized HNSW traversal.
    /// 2. Computes exact distance using full `f32` vectors via `f32_getter` closure.
    /// 3. Re-ranks and returns top `limit` items.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn search_and_rerank<F>(
        &self,
        collection: &str,
        query_f32: &[f32],
        query_quantized_bytes: &[u8],
        limit: usize,
        ef_search: Option<usize>,
        filter: Option<&dyn Fn(&str) -> bool>,
        oversample: usize,
        f32_getter: F,
    ) -> Vec<VectorHit>
    where
        F: Fn(&str) -> Option<Vec<f32>>,
    {
        let candidate_limit = limit.saturating_mul(oversample.max(1));
        let coarse_hits = self.search(
            collection,
            query_quantized_bytes,
            candidate_limit,
            ef_search,
            filter,
        );

        let mut refined_hits = Vec::with_capacity(coarse_hits.len());
        for hit in coarse_hits {
            if let Some(target_f32) = f32_getter(&hit.id) {
                let exact_dist = match self.config.metric {
                    VectorMetric::Cosine => cosine_distance_f32(query_f32, &target_f32),
                    VectorMetric::L2 => l2_distance_f32(query_f32, &target_f32),
                    VectorMetric::Dot => dot_distance_f32(query_f32, &target_f32),
                    VectorMetric::Hamming => hit.distance,
                };
                refined_hits.push(VectorHit {
                    collection: hit.collection,
                    id: hit.id,
                    distance: exact_dist,
                });
            } else {
                refined_hits.push(hit);
            }
        }

        refined_hits.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(Ordering::Equal)
        });

        if refined_hits.len() > limit {
            refined_hits.truncate(limit);
        }
        refined_hits
    }

    /// Rebuilds / compacts the HNSW graph from a list of vectors.
    pub fn rebuild_from_vectors(&mut self, items: &[(&str, &[u8])]) -> Result<()> {
        let config = self.config.clone();
        *self = Self::new(config);
        for (id, vec) in items {
            self.insert(id, vec)?;
        }
        Ok(())
    }

    /// Loads a node with its pre-computed edges into the HNSW index (used during database startup).
    pub fn load_node_edges(
        &mut self,
        id: &str,
        level: usize,
        neighbor_ids: &[&str],
        vector: &[u8],
    ) {
        let internal_id = match self.id_to_internal.get(id) {
            Some(&uid) => uid,
            None => {
                let uid = self.nodes.len() as u32;
                self.id_to_internal.insert(id.to_string(), uid);
                self.internal_to_id.push(Some(id.to_string()));
                self.vectors.push(Some(vector.to_vec()));
                self.nodes.push(Some(HnswNode {
                    id: id.to_string(),
                    internal_id: uid,
                    level,
                    neighbors: vec![Vec::new(); level + 1],
                }));
                self.active_count += 1;
                uid
            }
        };

        if let Some(Some(ref mut n)) = self.nodes.get_mut(internal_id as usize) {
            if level >= n.neighbors.len() {
                n.neighbors.resize(level + 1, Vec::new());
                n.level = level;
            }
            n.neighbors[level] = neighbor_ids
                .iter()
                .filter_map(|nid| self.id_to_internal.get(*nid).copied())
                .collect();
        }

        if level >= self.max_level {
            self.max_level = level;
            self.entry_point = Some(internal_id);
        }
    }
}

fn compute_vector_distance(
    query: &[u8],
    target: &[u8],
    element_type: VectorElementType,
    metric: VectorMetric,
) -> f64 {
    match element_type {
        VectorElementType::Float32 => {
            let q = bytes_to_f32_slice(query);
            let t = bytes_to_f32_slice(target);
            match metric {
                VectorMetric::Cosine => cosine_distance_f32(&q, &t),
                VectorMetric::L2 => l2_distance_f32(&q, &t),
                VectorMetric::Dot => dot_distance_f32(&q, &t),
                VectorMetric::Hamming => f64::from(hamming_distance_bits(query, target)),
            }
        }
        VectorElementType::Int8 => {
            let q = bytes_to_i8_vec(query);
            let t = bytes_to_i8_vec(target);
            match metric {
                VectorMetric::Cosine => cosine_distance_i8(&q, &t),
                VectorMetric::Hamming => f64::from(hamming_distance_bits(query, target)),
                VectorMetric::L2 | VectorMetric::Dot => {
                    let q_f: Vec<f32> = q.iter().map(|&x| f32::from(x)).collect();
                    let t_f: Vec<f32> = t.iter().map(|&x| f32::from(x)).collect();
                    if metric == VectorMetric::L2 {
                        l2_distance_f32(&q_f, &t_f)
                    } else {
                        dot_distance_f32(&q_f, &t_f)
                    }
                }
            }
        }
        VectorElementType::Bit => f64::from(hamming_distance_bits(query, target)),
    }
}

fn bytes_to_f32_slice(bytes: &[u8]) -> Vec<f32> {
    if !bytes.len().is_multiple_of(4) {
        return Vec::new();
    }
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn bytes_to_i8_vec(bytes: &[u8]) -> Vec<i8> {
    bytes.iter().map(|&b| b as i8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_bytes(floats: &[f32]) -> Vec<u8> {
        let mut b = Vec::with_capacity(floats.len() * 4);
        for &f in floats {
            b.extend_from_slice(&f.to_le_bytes());
        }
        b
    }

    #[test]
    fn test_hnsw_basic_insert_search_and_recall() {
        let config = HnswConfig {
            m: 16,
            ef_construction: 64,
            ef_search: 32,
            metric: VectorMetric::Cosine,
            element_type: VectorElementType::Float32,
            dim: 4,
        };
        let mut index = HnswIndex::new(config);

        let v1 = to_bytes(&[1.0, 0.0, 0.0, 0.0]);
        let v2 = to_bytes(&[0.0, 1.0, 0.0, 0.0]);
        let v3 = to_bytes(&[0.9, 0.1, 0.0, 0.0]);
        let v4 = to_bytes(&[0.0, 0.0, 1.0, 0.0]);

        index.insert("d1", &v1).unwrap();
        index.insert("d2", &v2).unwrap();
        index.insert("d3", &v3).unwrap();
        index.insert("d4", &v4).unwrap();

        assert_eq!(index.len(), 4);

        // Query close to d1 and d3
        let q = to_bytes(&[0.95, 0.05, 0.0, 0.0]);
        let hits = index.search("test", &q, 2, None, None);

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, "d1");
        assert_eq!(hits[1].id, "d3");
        assert!(hits[0].distance < hits[1].distance);
    }

    #[test]
    fn test_hnsw_pre_filtered_search() {
        let config = HnswConfig {
            m: 16,
            ef_construction: 64,
            ef_search: 32,
            metric: VectorMetric::Cosine,
            element_type: VectorElementType::Float32,
            dim: 3,
        };
        let mut index = HnswIndex::new(config);

        // d1 is nearest to query, but category is "books"
        // d2 is further, but category is "electronics"
        // d3 is far, category is "electronics"
        index.insert("d1", &to_bytes(&[1.0, 0.0, 0.0])).unwrap();
        index.insert("d2", &to_bytes(&[0.8, 0.2, 0.0])).unwrap();
        index.insert("d3", &to_bytes(&[0.0, 1.0, 0.0])).unwrap();

        let query = to_bytes(&[1.0, 0.0, 0.0]);

        // Unfiltered search: top 1 is d1
        let hits_unfiltered = index.search("items", &query, 1, None, None);
        assert_eq!(hits_unfiltered[0].id, "d1");

        // Filtered search: only "electronics" allowed (d2 and d3)
        let filter = |id: &str| id == "d2" || id == "d3";
        let hits_filtered = index.search("items", &query, 2, None, Some(&filter));

        assert_eq!(hits_filtered.len(), 2);
        assert_eq!(hits_filtered[0].id, "d2");
        assert_eq!(hits_filtered[1].id, "d3");
    }

    #[test]
    fn test_hnsw_quantized_sq8_and_rerank() {
        let config = HnswConfig {
            m: 16,
            ef_construction: 64,
            ef_search: 32,
            metric: VectorMetric::Cosine,
            element_type: VectorElementType::Int8,
            dim: 3,
        };
        let mut index = HnswIndex::new(config);

        let orig_v1 = vec![1.0f32, 0.0, 0.0];
        let orig_v2 = vec![0.8f32, 0.2, 0.0];
        let orig_v3 = vec![0.0f32, 1.0, 0.0];

        let (q1, _) = crate::quantization::quantize_f32_to_i8(&orig_v1);
        let (q2, _) = crate::quantization::quantize_f32_to_i8(&orig_v2);
        let (q3, _) = crate::quantization::quantize_f32_to_i8(&orig_v3);

        let b1: Vec<u8> = q1.iter().map(|&x| x as u8).collect();
        let b2: Vec<u8> = q2.iter().map(|&x| x as u8).collect();
        let b3: Vec<u8> = q3.iter().map(|&x| x as u8).collect();

        index.insert("item1", &b1).unwrap();
        index.insert("item2", &b2).unwrap();
        index.insert("item3", &b3).unwrap();

        let query_f32 = vec![0.95f32, 0.05, 0.0];
        let (query_i8, _) = crate::quantization::quantize_f32_to_i8(&query_f32);
        let query_bytes: Vec<u8> = query_i8.iter().map(|&x| x as u8).collect();

        let raw_store: HashMap<String, Vec<f32>> = [
            ("item1".into(), orig_v1),
            ("item2".into(), orig_v2),
            ("item3".into(), orig_v3),
        ]
        .into_iter()
        .collect();

        let reranked =
            index.search_and_rerank("store", &query_f32, &query_bytes, 2, None, None, 2, |id| {
                raw_store.get(id).cloned()
            });

        assert_eq!(reranked.len(), 2);
        assert_eq!(reranked[0].id, "item1");
        assert_eq!(reranked[1].id, "item2");
    }

    #[test]
    fn test_hnsw_deletion_and_repair() {
        let config = HnswConfig {
            m: 16,
            ef_construction: 64,
            ef_search: 32,
            metric: VectorMetric::Cosine,
            element_type: VectorElementType::Float32,
            dim: 2,
        };
        let mut index = HnswIndex::new(config);

        index.insert("a", &to_bytes(&[1.0, 0.0])).unwrap();
        index.insert("b", &to_bytes(&[0.9, 0.1])).unwrap();
        index.insert("c", &to_bytes(&[0.0, 1.0])).unwrap();

        assert_eq!(index.len(), 3);
        assert!(index.delete("a"));
        assert_eq!(index.len(), 2);
        assert!(!index.contains("a"));

        let hits = index.search("items", &to_bytes(&[1.0, 0.0]), 2, None, None);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, "b");
        assert_eq!(hits[1].id, "c");
    }
}
