use lilia_core::Direction;

use crate::snapshot::GraphView;

pub fn has_cycle(view: &GraphView, direction: Direction, kind_filter: Option<&str>) -> bool {
    let n = view.node_count();
    let kind_id_filter = kind_filter.and_then(|k| view.kind_id(k));

    // 0 = White (unvisited), 1 = Gray (in current DFS stack), 2 = Black (visited & completed)
    let mut state = vec![0u8; n];

    for start in 0..n {
        if state[start] == 0 && dfs_has_cycle(view, start, direction, kind_id_filter, &mut state) {
            return true;
        }
    }

    false
}

fn dfs_has_cycle(
    view: &GraphView,
    u: usize,
    direction: Direction,
    kind_id_filter: Option<u16>,
    state: &mut [u8],
) -> bool {
    state[u] = 1; // Gray

    for edge in view.edges_dir(u, direction) {
        if let Some(kid) = kind_id_filter {
            if edge.kind_id != kid {
                continue;
            }
        }
        let v = edge.target;
        if state[v] == 1 {
            return true; // Cycle found
        }
        if state[v] == 0 && dfs_has_cycle(view, v, direction, kind_id_filter, state) {
            return true;
        }
    }

    state[u] = 2; // Black
    false
}

pub fn find_cycles(
    view: &GraphView,
    max_cycles: usize,
    kind_filter: Option<&str>,
) -> Vec<Vec<String>> {
    let n = view.node_count();
    let kind_id_filter = kind_filter.and_then(|k| view.kind_id(k));
    let mut cycles = Vec::new();

    let mut state = vec![0u8; n];
    let mut current_path = Vec::new();

    for start in 0..n {
        if state[start] == 0 {
            dfs_find_cycles(
                view,
                start,
                kind_id_filter,
                &mut state,
                &mut current_path,
                &mut cycles,
                max_cycles,
            );
            if cycles.len() >= max_cycles {
                break;
            }
        }
    }

    cycles
}

fn dfs_find_cycles(
    view: &GraphView,
    u: usize,
    kind_id_filter: Option<u16>,
    state: &mut [u8],
    current_path: &mut Vec<usize>,
    cycles: &mut Vec<Vec<String>>,
    max_cycles: usize,
) {
    state[u] = 1; // Gray
    current_path.push(u);

    for edge in view.out_edges(u) {
        if let Some(kid) = kind_id_filter {
            if edge.kind_id != kid {
                continue;
            }
        }
        let v = edge.target;
        if state[v] == 1 {
            // Cycle detected: extract subpath from v to u
            if let Some(pos) = current_path.iter().position(|&node| node == v) {
                let cycle_indices = &current_path[pos..];
                let mut cycle = cycle_indices
                    .iter()
                    .map(|&idx| view.node_id(idx).to_string())
                    .collect::<Vec<_>>();
                cycle.push(view.node_id(v).to_string());
                cycles.push(cycle);
                if cycles.len() >= max_cycles {
                    return;
                }
            }
        } else if state[v] == 0 {
            dfs_find_cycles(
                view,
                v,
                kind_id_filter,
                state,
                current_path,
                cycles,
                max_cycles,
            );
            if cycles.len() >= max_cycles {
                return;
            }
        }
    }

    current_path.pop();
    state[u] = 2; // Black
}

pub fn strongly_connected_components(
    view: &GraphView,
    kind_filter: Option<&str>,
) -> Vec<Vec<String>> {
    let n = view.node_count();
    let kind_id_filter = kind_filter.and_then(|k| view.kind_id(k));

    let mut index = 0;
    let mut indices = vec![usize::MAX; n];
    let mut lowlink = vec![usize::MAX; n];
    let mut on_stack = vec![false; n];
    let mut stack = Vec::new();
    let mut sccs = Vec::new();

    for u in 0..n {
        if indices[u] == usize::MAX {
            strongconnect(
                u,
                view,
                kind_id_filter,
                &mut index,
                &mut indices,
                &mut lowlink,
                &mut on_stack,
                &mut stack,
                &mut sccs,
            );
        }
    }

    sccs.sort_by(|a, b| b.len().cmp(&a.len()));
    sccs
}

#[allow(clippy::too_many_arguments)]
fn strongconnect(
    u: usize,
    view: &GraphView,
    kind_id_filter: Option<u16>,
    index: &mut usize,
    indices: &mut [usize],
    lowlink: &mut [usize],
    on_stack: &mut [bool],
    stack: &mut Vec<usize>,
    sccs: &mut Vec<Vec<String>>,
) {
    indices[u] = *index;
    lowlink[u] = *index;
    *index += 1;
    stack.push(u);
    on_stack[u] = true;

    for edge in view.out_edges(u) {
        if let Some(kid) = kind_id_filter {
            if edge.kind_id != kid {
                continue;
            }
        }
        let v = edge.target;
        if indices[v] == usize::MAX {
            strongconnect(
                v,
                view,
                kind_id_filter,
                index,
                indices,
                lowlink,
                on_stack,
                stack,
                sccs,
            );
            lowlink[u] = lowlink[u].min(lowlink[v]);
        } else if on_stack[v] {
            lowlink[u] = lowlink[u].min(indices[v]);
        }
    }

    if lowlink[u] == indices[u] {
        let mut component = Vec::new();
        while let Some(w) = stack.pop() {
            on_stack[w] = false;
            component.push(view.node_id(w).to_string());
            if w == u {
                break;
            }
        }
        sccs.push(component);
    }
}

pub fn weakly_connected_components(
    view: &GraphView,
    kind_filter: Option<&str>,
) -> Vec<Vec<String>> {
    let n = view.node_count();
    let kind_id_filter = kind_filter.and_then(|k| view.kind_id(k));

    let mut parent: Vec<usize> = (0..n).collect();

    fn find(parent: &mut [usize], i: usize) -> usize {
        if parent[i] == i {
            i
        } else {
            parent[i] = find(parent, parent[i]);
            parent[i]
        }
    }

    fn union(parent: &mut [usize], i: usize, j: usize) {
        let root_i = find(parent, i);
        let root_j = find(parent, j);
        if root_i != root_j {
            parent[root_i] = root_j;
        }
    }

    for u in 0..n {
        for edge in view.out_edges(u) {
            if let Some(kid) = kind_id_filter {
                if edge.kind_id != kid {
                    continue;
                }
            }
            union(&mut parent, u, edge.target);
        }
    }

    let mut groups: std::collections::HashMap<usize, Vec<String>> =
        std::collections::HashMap::new();
    for u in 0..n {
        let root = find(&mut parent, u);
        groups
            .entry(root)
            .or_default()
            .push(view.node_id(u).to_string());
    }

    let mut wccs: Vec<Vec<String>> = groups.into_values().collect();
    wccs.sort_by(|a, b| b.len().cmp(&a.len()));
    wccs
}
