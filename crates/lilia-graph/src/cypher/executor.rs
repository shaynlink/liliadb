use std::collections::HashMap;

use lilia_core::{GraphNode, Result};

use super::ast::{CmpOp, CypherQuery, FieldRef, NodePattern, WhereExpr};
use super::result::CypherResult;
use crate::snapshot::GraphView;
use crate::unified::{resolve_node_document, UnifiedNode};
use crate::GraphStore;

pub fn execute(store: &GraphStore<'_>, query: &CypherQuery) -> Result<CypherResult> {
    let view = GraphView::from_store(store, None)?;
    let mut matches = Vec::new();

    // Start with candidate nodes matching the first node pattern
    let first_pattern = &query.pattern.nodes[0];
    let start_candidates = find_node_candidates(store, &view, first_pattern)?;

    for start_node in start_candidates {
        let mut binding = HashMap::new();
        if let Some(ref var) = first_pattern.variable {
            binding.insert(var.clone(), start_node.clone());
        }

        match_chain(
            store,
            &view,
            &query.pattern,
            1,
            start_node.id.clone(),
            binding,
            &mut matches,
        )?;
    }

    // Filter matches using WHERE clause
    let mut filtered_matches = Vec::new();
    for binding in matches {
        if let Some(ref where_clause) = query.where_clause {
            if eval_where(store, &binding, where_clause)? {
                filtered_matches.push(binding);
            }
        } else {
            filtered_matches.push(binding);
        }
    }

    // Apply LIMIT
    if let Some(limit) = query.limit {
        if filtered_matches.len() > limit {
            filtered_matches.truncate(limit);
        }
    }

    // Project columns and rows
    let columns: Vec<String> = query
        .return_clause
        .items
        .iter()
        .map(|item| {
            item.alias.clone().unwrap_or_else(|| {
                if let Some(ref f) = item.target.field {
                    format!("{}.{}", item.target.variable, f)
                } else {
                    item.target.variable.clone()
                }
            })
        })
        .collect();

    let mut rows = Vec::with_capacity(filtered_matches.len());
    for binding in filtered_matches {
        let mut row = Vec::with_capacity(query.return_clause.items.len());
        for item in &query.return_clause.items {
            let val = extract_value(store, &binding, &item.target)?;
            row.push(val);
        }
        rows.push(row);
    }

    Ok(CypherResult { columns, rows })
}

fn find_node_candidates(
    store: &GraphStore<'_>,
    _view: &GraphView,
    pattern: &NodePattern,
) -> Result<Vec<GraphNode>> {
    let nodes = store.list_nodes(pattern.label.as_deref(), u32::MAX)?;
    let mut filtered = Vec::new();

    for node in nodes {
        let mut matches = true;
        for (k, v) in &pattern.properties {
            if k == "id" {
                if let Some(id_str) = v.as_str() {
                    if node.id != id_str {
                        matches = false;
                        break;
                    }
                }
            } else if node.metadata.get(k) != Some(v) {
                // Also check linked document
                if let Some(doc) = resolve_node_document(store.database(), &node)? {
                    if doc.get(k) != Some(v) {
                        matches = false;
                        break;
                    }
                } else {
                    matches = false;
                    break;
                }
            }
        }
        if matches {
            filtered.push(node);
        }
    }

    Ok(filtered)
}

fn match_chain(
    store: &GraphStore<'_>,
    view: &GraphView,
    pattern: &super::ast::PatternChain,
    node_idx: usize,
    current_node_id: String,
    current_binding: HashMap<String, GraphNode>,
    results: &mut Vec<HashMap<String, GraphNode>>,
) -> Result<()> {
    if node_idx >= pattern.nodes.len() {
        results.push(current_binding);
        return Ok(());
    }

    let edge_pat = &pattern.edges[node_idx - 1];
    let next_node_pat = &pattern.nodes[node_idx];

    // Traverse neighbors
    let neighbors = store.neighbors_with_kind(
        &current_node_id,
        edge_pat.direction,
        edge_pat.kind.as_deref(),
    )?;

    for n_id in neighbors {
        if let Some(next_node) = store.get_node(&n_id)? {
            // Check label
            if let Some(ref expected_label) = next_node_pat.label {
                if &next_node.kind != expected_label {
                    continue;
                }
            }

            // Check properties
            let mut props_match = true;
            for (k, v) in &next_node_pat.properties {
                if k == "id" {
                    if let Some(id_str) = v.as_str() {
                        if next_node.id != id_str {
                            props_match = false;
                            break;
                        }
                    }
                } else if next_node.metadata.get(k) != Some(v) {
                    if let Some(doc) = resolve_node_document(store.database(), &next_node)? {
                        if doc.get(k) != Some(v) {
                            props_match = false;
                            break;
                        }
                    } else {
                        props_match = false;
                        break;
                    }
                }
            }
            if !props_match {
                continue;
            }

            let mut new_binding = current_binding.clone();
            if let Some(ref var) = next_node_pat.variable {
                new_binding.insert(var.clone(), next_node);
            }

            match_chain(
                store,
                view,
                pattern,
                node_idx + 1,
                n_id,
                new_binding,
                results,
            )?;
        }
    }

    Ok(())
}

fn eval_where(
    store: &GraphStore<'_>,
    binding: &HashMap<String, GraphNode>,
    expr: &WhereExpr,
) -> Result<bool> {
    match expr {
        WhereExpr::And(left, right) => {
            Ok(eval_where(store, binding, left)? && eval_where(store, binding, right)?)
        }
        WhereExpr::Or(left, right) => {
            Ok(eval_where(store, binding, left)? || eval_where(store, binding, right)?)
        }
        WhereExpr::Comparison { target, op, value } => {
            let actual = extract_value(store, binding, target)?;
            Ok(compare_values(&actual, *op, value))
        }
    }
}

fn extract_value(
    store: &GraphStore<'_>,
    binding: &HashMap<String, GraphNode>,
    target: &FieldRef,
) -> Result<serde_json::Value> {
    let Some(node) = binding.get(&target.variable) else {
        return Ok(serde_json::Value::Null);
    };

    let doc = resolve_node_document(store.database(), node)?;
    let unified = UnifiedNode::new(node.clone(), doc);

    if let Some(ref field) = target.field {
        if field == "id" {
            return Ok(serde_json::Value::String(node.id.clone()));
        }
        if field == "kind" {
            return Ok(serde_json::Value::String(node.kind.clone()));
        }
        if let Some(v) = unified.get_value(field) {
            return Ok(v.clone());
        }
        Ok(serde_json::Value::Null)
    } else {
        // Returning whole node: if document exists return the document, otherwise the unified node representation
        if let Some(doc) = unified.document {
            Ok(doc)
        } else {
            Ok(unified.to_json())
        }
    }
}

fn compare_values(actual: &serde_json::Value, op: CmpOp, expected: &serde_json::Value) -> bool {
    match op {
        CmpOp::Eq => actual == expected,
        CmpOp::NotEq => actual != expected,
        CmpOp::Gt => match (actual.as_f64(), expected.as_f64()) {
            (Some(a), Some(b)) => a > b,
            _ => actual.as_str().unwrap_or("") > expected.as_str().unwrap_or(""),
        },
        CmpOp::Gte => match (actual.as_f64(), expected.as_f64()) {
            (Some(a), Some(b)) => a >= b,
            _ => actual.as_str().unwrap_or("") >= expected.as_str().unwrap_or(""),
        },
        CmpOp::Lt => match (actual.as_f64(), expected.as_f64()) {
            (Some(a), Some(b)) => a < b,
            _ => actual.as_str().unwrap_or("") < expected.as_str().unwrap_or(""),
        },
        CmpOp::Lte => match (actual.as_f64(), expected.as_f64()) {
            (Some(a), Some(b)) => a <= b,
            _ => actual.as_str().unwrap_or("") <= expected.as_str().unwrap_or(""),
        },
        CmpOp::Contains => {
            if let (Some(s_act), Some(s_exp)) = (actual.as_str(), expected.as_str()) {
                s_act.contains(s_exp)
            } else if let serde_json::Value::Array(arr) = actual {
                arr.contains(expected)
            } else {
                false
            }
        }
        CmpOp::StartsWith => {
            if let (Some(s_act), Some(s_exp)) = (actual.as_str(), expected.as_str()) {
                s_act.starts_with(s_exp)
            } else {
                false
            }
        }
    }
}
