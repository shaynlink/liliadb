use std::collections::VecDeque;

pub use lilia_core::PageRankConfig;
use lilia_core::{Direction, NodeScore};

use crate::snapshot::GraphView;

#[must_use]
pub fn pagerank(view: &GraphView, config: &PageRankConfig) -> Vec<NodeScore> {
    let n = view.node_count();
    if n == 0 {
        return Vec::new();
    }

    let kind_id_filter = config.edge_kind.as_deref().and_then(|k| view.kind_id(k));

    // Teleportation / personalization vector
    let mut teleport = vec![0.0; n];
    if let Some(ref seeds) = config.seeds {
        let valid_seeds: Vec<usize> = seeds.iter().filter_map(|s| view.node_idx(s)).collect();
        if valid_seeds.is_empty() {
            let uniform = 1.0 / super::count_f64(n);
            teleport.fill(uniform);
        } else {
            let w = 1.0 / super::count_f64(valid_seeds.len());
            for s in valid_seeds {
                teleport[s] = w;
            }
        }
    } else {
        let uniform = 1.0 / super::count_f64(n);
        teleport.fill(uniform);
    }

    let mut rank = teleport.clone();
    let mut next_rank = vec![0.0; n];
    let d = config.damping_factor;

    // Precalculate out-degree or out-weight per node for the selected direction
    let mut out_weights = vec![0.0; n];
    for (u, out_weight) in out_weights.iter_mut().enumerate() {
        let edges = view.edges_dir(u, config.direction);
        let mut total_w = 0.0;
        for edge in edges {
            if let Some(kid) = kind_id_filter {
                if edge.kind_id != kid {
                    continue;
                }
            }
            total_w += edge.weight;
        }
        *out_weight = total_w;
    }

    for _ in 0..config.max_iterations {
        let mut dangling_sum = 0.0;
        for (u, &w) in out_weights.iter().enumerate() {
            if w == 0.0 {
                dangling_sum += rank[u];
            }
        }

        // Initialize with teleport + distributed dangling mass
        for i in 0..n {
            next_rank[i] = (1.0 - d) * teleport[i] + d * dangling_sum * teleport[i];
        }

        // Distribute mass across directed edges
        for u in 0..n {
            let w = out_weights[u];
            if w > 0.0 {
                let share = (d * rank[u]) / w;
                for edge in view.edges_dir(u, config.direction) {
                    if let Some(kid) = kind_id_filter {
                        if edge.kind_id != kid {
                            continue;
                        }
                    }
                    next_rank[edge.target] += share * edge.weight;
                }
            }
        }

        // Check convergence
        let diff: f64 = rank
            .iter()
            .zip(next_rank.iter())
            .map(|(a, b)| (a - b).abs())
            .sum();

        rank.copy_from_slice(&next_rank);

        if diff < config.tolerance {
            break;
        }
    }

    let mut scores: Vec<NodeScore> = rank
        .into_iter()
        .enumerate()
        .map(|(idx, score)| NodeScore {
            node: view.node_id(idx).to_string(),
            score,
        })
        .collect();

    scores.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    scores
}

#[must_use]
pub fn betweenness_centrality(
    view: &GraphView,
    direction: Direction,
    kind_filter: Option<&str>,
    normalized: bool,
) -> Vec<NodeScore> {
    let n = view.node_count();
    if n == 0 {
        return Vec::new();
    }

    let kind_id_filter = kind_filter.and_then(|k| view.kind_id(k));
    let mut cb = vec![0.0; n];

    // Ulrik Brandes' algorithm
    for s in 0..n {
        let mut stack = Vec::new();
        let mut p: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut sigma = vec![0.0; n];
        let mut d = vec![-1i32; n];
        let mut delta = vec![0.0; n];
        let mut queue = VecDeque::new();

        sigma[s] = 1.0;
        d[s] = 0;
        queue.push_back(s);

        while let Some(v) = queue.pop_front() {
            stack.push(v);

            for edge in view.edges_dir(v, direction) {
                if let Some(kid) = kind_id_filter {
                    if edge.kind_id != kid {
                        continue;
                    }
                }
                let w = edge.target;

                // Path discovery
                if d[w] < 0 {
                    d[w] = d[v] + 1;
                    queue.push_back(w);
                }

                // Path counting
                if d[w] == d[v] + 1 {
                    sigma[w] += sigma[v];
                    p[w].push(v);
                }
            }
        }

        // Accumulation
        while let Some(w) = stack.pop() {
            for &v in &p[w] {
                if sigma[w] > 0.0 {
                    delta[v] += (sigma[v] / sigma[w]) * (1.0 + delta[w]);
                }
            }
            if w != s {
                cb[w] += delta[w];
            }
        }
    }

    if normalized && n > 2 {
        let scale = 1.0 / super::count_f64((n - 1) * (n - 2));
        for score in &mut cb {
            *score *= scale;
        }
    }

    let mut scores: Vec<NodeScore> = cb
        .into_iter()
        .enumerate()
        .map(|(idx, score)| NodeScore {
            node: view.node_id(idx).to_string(),
            score,
        })
        .collect();

    scores.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    scores
}

#[must_use]
pub fn closeness_centrality(
    view: &GraphView,
    direction: Direction,
    kind_filter: Option<&str>,
) -> Vec<NodeScore> {
    let n = view.node_count();
    if n == 0 {
        return Vec::new();
    }

    let kind_id_filter = kind_filter.and_then(|k| view.kind_id(k));
    let mut scores = Vec::with_capacity(n);

    for s in 0..n {
        let mut distances = vec![-1i32; n];
        let mut queue = VecDeque::new();
        distances[s] = 0;
        queue.push_back(s);

        let mut sum_dist = 0;
        let mut reachable = 0;

        while let Some(v) = queue.pop_front() {
            for edge in view.edges_dir(v, direction) {
                if let Some(kid) = kind_id_filter {
                    if edge.kind_id != kid {
                        continue;
                    }
                }
                let w = edge.target;
                if distances[w] < 0 {
                    distances[w] = distances[v] + 1;
                    sum_dist += distances[w];
                    reachable += 1;
                    queue.push_back(w);
                }
            }
        }

        let score = if sum_dist > 0 && reachable > 0 {
            f64::from(reachable) / f64::from(sum_dist)
        } else {
            0.0
        };

        scores.push(NodeScore {
            node: view.node_id(s).to_string(),
            score,
        });
    }

    scores.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    scores
}

#[must_use]
pub fn degree_centrality(
    view: &GraphView,
    direction: Direction,
    kind_filter: Option<&str>,
) -> Vec<NodeScore> {
    let n = view.node_count();
    if n == 0 {
        return Vec::new();
    }

    let kind_id_filter = kind_filter.and_then(|k| view.kind_id(k));
    let mut scores = Vec::with_capacity(n);
    let denom = if n > 1 { super::count_f64(n - 1) } else { 1.0 };

    for s in 0..n {
        let edges = view.edges_dir(s, direction);
        let count = if let Some(kid) = kind_id_filter {
            edges.iter().filter(|e| e.kind_id == kid).count()
        } else {
            edges.len()
        };

        scores.push(NodeScore {
            node: view.node_id(s).to_string(),
            score: super::count_f64(count) / denom,
        });
    }

    scores.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    scores
}
