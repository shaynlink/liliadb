#![allow(clippy::too_many_lines)]
//! Result shaping and projection expression evaluator for `LiliaQL`.

use lilia_core::GraphNode;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use super::types::QueryRow;

/// Declarative specification of projected result fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Projection {
    pub(crate) fields: Vec<(String, String)>,
}

impl Projection {
    /// Creates an empty projection.
    #[must_use]
    pub fn new() -> Self {
        Self { fields: Vec::new() }
    }

    /// Adds a field projection to the specification.
    #[must_use]
    pub fn field(mut self, name: impl Into<String>, expr: impl Into<String>) -> Self {
        self.add_field(name, expr);
        self
    }

    /// Appends a field projection.
    pub fn add_field(&mut self, name: impl Into<String>, expr: impl Into<String>) {
        self.fields.push((name.into(), expr.into()));
    }

    /// Returns true if no fields have been specified.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

/// Runtime context holding source documents, graph traversed entities, and hybrid scores.
#[derive(Debug, Clone, Default)]
pub struct ProjectionContext<'a> {
    pub source_id: Option<&'a str>,
    pub source_doc: Option<&'a Value>,
    pub graph_source_id: Option<&'a str>,
    pub graph_target_id: Option<&'a str>,
    pub graph_target_node: Option<&'a GraphNode>,
    pub graph_target_doc: Option<&'a Value>,
    pub graph_edge_kind: Option<&'a str>,
    pub graph_edge_weight: Option<f64>,
    pub fts_rank: Option<usize>,
    pub fts_snippet: Option<&'a str>,
    pub fts_score: Option<f64>,
    pub vector_rank: Option<usize>,
    pub vector_distance: Option<f64>,
    pub vector_score: Option<f64>,
    pub rrf_score: Option<f64>,
}

impl Projection {
    /// Evaluates this projection against the given runtime context to produce a [`QueryRow`].
    #[must_use]
    pub fn evaluate(&self, ctx: &ProjectionContext<'_>) -> QueryRow {
        if self.fields.is_empty() {
            // Default projection: if target doc exists, emit it; else source doc or empty
            if let Some(Value::Object(map)) = ctx.graph_target_doc {
                return QueryRow::new(map.clone());
            }
            if let Some(Value::Object(map)) = ctx.source_doc {
                return QueryRow::new(map.clone());
            }
            return QueryRow::new(Map::new());
        }

        let mut row_map = Map::new();
        for (out_key, expr) in &self.fields {
            let val = resolve_expression(expr, ctx);
            row_map.insert(out_key.clone(), val);
        }
        QueryRow::new(row_map)
    }
}

fn resolve_expression(expr: &str, ctx: &ProjectionContext<'_>) -> Value {
    let expr = expr.trim();

    // 1. Relevance and rank scores
    match expr {
        "$rrf_score" => {
            return ctx
                .rrf_score
                .and_then(Number::from_f64)
                .map_or(Value::Null, Value::Number);
        }
        "$fts_snippet" => {
            return ctx
                .fts_snippet
                .map_or(Value::Null, |s| Value::String(s.to_string()));
        }
        "$fts_rank" => {
            return ctx
                .fts_rank
                .map_or(Value::Null, |r| Value::Number(Number::from(r)));
        }
        "$fts_score" => {
            return ctx
                .fts_score
                .and_then(Number::from_f64)
                .map_or(Value::Null, Value::Number);
        }
        "$vector_rank" => {
            return ctx
                .vector_rank
                .map_or(Value::Null, |r| Value::Number(Number::from(r)));
        }
        "$vector_distance" => {
            return ctx
                .vector_distance
                .and_then(Number::from_f64)
                .map_or(Value::Null, Value::Number);
        }
        "$vector_score" => {
            return ctx
                .vector_score
                .and_then(Number::from_f64)
                .map_or(Value::Null, Value::Number);
        }
        _ => {}
    }

    // 2. Graph target expressions
    if expr == "$graph.target" {
        if let Some(tdoc) = ctx.graph_target_doc {
            return tdoc.clone();
        }
        if let Some(node) = ctx.graph_target_node {
            return serde_json::to_value(node).unwrap_or(Value::Null);
        }
        return Value::Null;
    }

    if let Some(subpath) = expr.strip_prefix("$graph.target.") {
        match subpath {
            "id" => {
                if let Some(id) = ctx
                    .graph_target_id
                    .or_else(|| ctx.graph_target_node.map(|n| n.id.as_str()))
                {
                    return Value::String(id.to_string());
                }
            }
            "kind" => {
                if let Some(node) = ctx.graph_target_node {
                    return Value::String(node.kind.clone());
                }
            }
            _ => {
                // Check in target node metadata first
                if let Some(node) = ctx.graph_target_node {
                    if let Some(v) = extract_path(&node.metadata, subpath) {
                        return v;
                    }
                }
                // Then check in linked target document
                if let Some(tdoc) = ctx.graph_target_doc {
                    if let Some(v) = extract_path(tdoc, subpath) {
                        return v;
                    }
                }
            }
        }
        return Value::Null;
    }

    // 3. Graph source expressions
    if expr == "$graph.source" {
        if let Some(sdoc) = ctx.source_doc {
            return sdoc.clone();
        }
        if let Some(id) = ctx.graph_source_id {
            return Value::String(id.to_string());
        }
        return Value::Null;
    }

    if let Some(subpath) = expr.strip_prefix("$graph.source.") {
        match subpath {
            "id" => {
                if let Some(id) = ctx.graph_source_id.or(ctx.source_id) {
                    return Value::String(id.to_string());
                }
            }
            _ => {
                if let Some(sdoc) = ctx.source_doc {
                    if let Some(v) = extract_path(sdoc, subpath) {
                        return v;
                    }
                }
            }
        }
        return Value::Null;
    }

    // 4. Graph edge expressions
    if let Some(subpath) = expr.strip_prefix("$graph.edge.") {
        match subpath {
            "kind" => {
                return ctx
                    .graph_edge_kind
                    .map_or(Value::Null, |k| Value::String(k.to_string()));
            }
            "weight" => {
                return ctx
                    .graph_edge_weight
                    .and_then(Number::from_f64)
                    .map_or(Value::Null, Value::Number);
            }
            _ => {}
        }
        return Value::Null;
    }

    // 5. Source document expressions: `$.path`, `$doc.path`, or `path`
    let doc_path = if let Some(p) = expr.strip_prefix("$.") {
        p
    } else if let Some(p) = expr.strip_prefix("$doc.") {
        p
    } else if expr == "$" || expr == "$doc" {
        return ctx.source_doc.cloned().unwrap_or(Value::Null);
    } else {
        expr
    };

    if doc_path == "id" {
        if let Some(id) = ctx.source_id {
            return Value::String(id.to_string());
        }
    }

    if let Some(sdoc) = ctx.source_doc {
        if let Some(v) = extract_path(sdoc, doc_path) {
            return v;
        }
    }

    // 6. If not found in source doc and target doc exists, check target doc as fallback
    if let Some(tdoc) = ctx.graph_target_doc {
        if let Some(v) = extract_path(tdoc, doc_path) {
            return v;
        }
    }

    Value::Null
}

fn extract_path(val: &Value, path: &str) -> Option<Value> {
    let clean = path.trim_start_matches("$.").trim_start_matches('/');
    let mut curr = val;
    for segment in clean.split('.') {
        if segment.is_empty() {
            continue;
        }
        // Handle array indexing like foo[0]
        if let Some((field, idx_str)) = segment.split_once('[') {
            let idx_str = idx_str.trim_end_matches(']');
            if !field.is_empty() {
                curr = curr.get(field)?;
            }
            let idx: usize = idx_str.parse().ok()?;
            curr = curr.get(idx)?;
        } else {
            match curr {
                Value::Object(map) => {
                    curr = map.get(segment)?;
                }
                Value::Array(arr) => {
                    let idx: usize = segment.parse().ok()?;
                    curr = arr.get(idx)?;
                }
                _ => return None,
            }
        }
    }
    Some(curr.clone())
}
