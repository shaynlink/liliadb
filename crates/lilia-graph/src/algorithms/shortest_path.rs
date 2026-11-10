use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use lilia_core::{Direction, PathEdge, WeightedPath};

use crate::snapshot::GraphView;

#[derive(Copy, Clone, PartialEq)]
struct MinHeapItem {
    cost: f64,
    node: usize,
}

impl Eq for MinHeapItem {}

impl Ord for MinHeapItem {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse order for min-heap
        other
            .cost
            .partial_cmp(&self.cost)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for MinHeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub fn dijkstra(
    view: &GraphView,
    start: &str,
    target: Option<&str>,
    direction: Direction,
    kind_filter: Option<&str>,
) -> Option<WeightedPath> {
    let start_idx = view.node_idx(start)?;
    let target_idx = match target {
        Some(t) => Some(view.node_idx(t)?),
        None => None,
    };

    let kind_id_filter = kind_filter.and_then(|k| view.kind_id(k));
    if kind_filter.is_some() && kind_id_filter.is_none() {
        return None;
    }

    let n = view.node_count();
    let mut distances = vec![f64::INFINITY; n];
    let mut predecessors: Vec<Option<(usize, u16, f64)>> = vec![None; n];
    let mut heap = BinaryHeap::new();

    distances[start_idx] = 0.0;
    heap.push(MinHeapItem {
        cost: 0.0,
        node: start_idx,
    });

    while let Some(MinHeapItem { cost, node }) = heap.pop() {
        if cost > distances[node] {
            continue;
        }

        if let Some(t_idx) = target_idx {
            if node == t_idx {
                return Some(reconstruct_path(
                    view,
                    start_idx,
                    t_idx,
                    &predecessors,
                    distances[t_idx],
                ));
            }
        }

        for edge in view.edges_dir(node, direction) {
            if let Some(filter_kid) = kind_id_filter {
                if edge.kind_id != filter_kid {
                    continue;
                }
            }

            let next_cost = cost + edge.weight;
            if next_cost < distances[edge.target] {
                distances[edge.target] = next_cost;
                predecessors[edge.target] = Some((node, edge.kind_id, edge.weight));
                heap.push(MinHeapItem {
                    cost: next_cost,
                    node: edge.target,
                });
            }
        }
    }

    if let Some(t_idx) = target_idx {
        if distances[t_idx].is_finite() {
            Some(reconstruct_path(
                view,
                start_idx,
                t_idx,
                &predecessors,
                distances[t_idx],
            ))
        } else {
            None
        }
    } else {
        None
    }
}

pub fn all_shortest_paths(
    view: &GraphView,
    start: &str,
    direction: Direction,
    kind_filter: Option<&str>,
) -> HashMap<String, WeightedPath> {
    let mut results = HashMap::new();
    let Some(start_idx) = view.node_idx(start) else {
        return results;
    };

    let kind_id_filter = kind_filter.and_then(|k| view.kind_id(k));
    if kind_filter.is_some() && kind_id_filter.is_none() {
        return results;
    }

    let n = view.node_count();
    let mut distances = vec![f64::INFINITY; n];
    let mut predecessors: Vec<Option<(usize, u16, f64)>> = vec![None; n];
    let mut heap = BinaryHeap::new();

    distances[start_idx] = 0.0;
    heap.push(MinHeapItem {
        cost: 0.0,
        node: start_idx,
    });

    while let Some(MinHeapItem { cost, node }) = heap.pop() {
        if cost > distances[node] {
            continue;
        }

        for edge in view.edges_dir(node, direction) {
            if let Some(filter_kid) = kind_id_filter {
                if edge.kind_id != filter_kid {
                    continue;
                }
            }

            let next_cost = cost + edge.weight;
            if next_cost < distances[edge.target] {
                distances[edge.target] = next_cost;
                predecessors[edge.target] = Some((node, edge.kind_id, edge.weight));
                heap.push(MinHeapItem {
                    cost: next_cost,
                    node: edge.target,
                });
            }
        }
    }

    for (target_idx, &dist) in distances.iter().enumerate() {
        if dist.is_finite() && target_idx != start_idx {
            let target_name = view.node_id(target_idx).to_string();
            let path = reconstruct_path(view, start_idx, target_idx, &predecessors, dist);
            results.insert(target_name, path);
        }
    }

    results
}

pub fn astar<H>(
    view: &GraphView,
    start: &str,
    target: &str,
    direction: Direction,
    kind_filter: Option<&str>,
    heuristic: H,
) -> Option<WeightedPath>
where
    H: Fn(&str) -> f64,
{
    let start_idx = view.node_idx(start)?;
    let target_idx = view.node_idx(target)?;

    let kind_id_filter = kind_filter.and_then(|k| view.kind_id(k));
    if kind_filter.is_some() && kind_id_filter.is_none() {
        return None;
    }

    let n = view.node_count();
    let mut g_score = vec![f64::INFINITY; n];
    let mut predecessors: Vec<Option<(usize, u16, f64)>> = vec![None; n];
    let mut heap = BinaryHeap::new();

    g_score[start_idx] = 0.0;
    let initial_f = heuristic(start);
    heap.push(MinHeapItem {
        cost: initial_f,
        node: start_idx,
    });

    while let Some(MinHeapItem { cost: _, node }) = heap.pop() {
        if node == target_idx {
            return Some(reconstruct_path(
                view,
                start_idx,
                target_idx,
                &predecessors,
                g_score[target_idx],
            ));
        }

        let current_g = g_score[node];

        for edge in view.edges_dir(node, direction) {
            if let Some(filter_kid) = kind_id_filter {
                if edge.kind_id != filter_kid {
                    continue;
                }
            }

            let tentative_g = current_g + edge.weight;
            if tentative_g < g_score[edge.target] {
                g_score[edge.target] = tentative_g;
                predecessors[edge.target] = Some((node, edge.kind_id, edge.weight));
                let h = heuristic(view.node_id(edge.target));
                heap.push(MinHeapItem {
                    cost: tentative_g + h,
                    node: edge.target,
                });
            }
        }
    }

    None
}

fn reconstruct_path(
    view: &GraphView,
    start_idx: usize,
    target_idx: usize,
    predecessors: &[Option<(usize, u16, f64)>],
    total_weight: f64,
) -> WeightedPath {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut curr = target_idx;

    while curr != start_idx {
        nodes.push(view.node_id(curr).to_string());
        if let Some((pred, kind_id, weight)) = predecessors[curr] {
            edges.push(PathEdge {
                source: view.node_id(pred).to_string(),
                target: view.node_id(curr).to_string(),
                kind: view.kind_str(kind_id).to_string(),
                weight,
            });
            curr = pred;
        } else {
            break;
        }
    }
    nodes.push(view.node_id(start_idx).to_string());
    nodes.reverse();
    edges.reverse();

    WeightedPath {
        nodes,
        total_weight,
        edges,
    }
}
