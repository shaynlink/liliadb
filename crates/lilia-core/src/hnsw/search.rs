//! Searches of an [`HnswIndex`]: filtered beam search, and search with re-ranking on full vectors.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};

use super::{Candidate, FurthestFirst, HnswIndex};
use crate::quantization::{cosine_distance_f32, dot_distance_f32, l2_distance_f32};
use crate::{VectorHit, VectorMetric};

impl HnswIndex {
    /// Whether `filter` admits the node `uid` (every node, without a filter).
    fn admits(&self, filter: Option<&dyn Fn(&str) -> bool>, uid: u32) -> bool {
        filter.is_none_or(|f| {
            f(self
                .internal_to_id
                .get(uid as usize)
                .and_then(|x| x.as_deref())
                .unwrap_or(""))
        })
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
        // Phase 1: 1-greedy search from max_level down to 1
        let (curr_obj, curr_dist) =
            self.greedy_descend(query, entry_id, (1..=self.max_level).rev());

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
        if !self.deleted_ids.contains(&curr_obj) && self.admits(filter, curr_obj) {
            results.push(FurthestFirst {
                id: curr_obj,
                distance: curr_dist,
            });
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

                                if self.admits(filter, neighbor) {
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
    #[allow(
        clippy::too_many_arguments,
        reason = "the query in both forms, the search bounds and the source of full vectors are independent inputs"
    )]
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
}
