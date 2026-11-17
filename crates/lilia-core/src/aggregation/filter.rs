use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::json::BinaryDoc;

/// A filter predicate expression applied to documents in a streaming aggregation pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilterExpr {
    predicate: Value,
}

impl FilterExpr {
    #[must_use]
    pub fn new(predicate: Value) -> Self {
        Self { predicate }
    }

    #[must_use]
    pub fn from_json(val: Value) -> Self {
        Self { predicate: val }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        match &self.predicate {
            Value::Object(map) => map.is_empty(),
            Value::Null => true,
            _ => false,
        }
    }

    /// Evaluates this filter expression against a `BinaryDoc`.
    #[must_use]
    pub fn matches_binary(&self, doc: &BinaryDoc) -> bool {
        match &self.predicate {
            Value::Object(map) => {
                for (k, expected) in map {
                    let actual = doc.extract(k).map(|v| v.to_json_value());
                    if !match_field_condition(actual.as_ref(), expected) {
                        return false;
                    }
                }
                true
            }
            Value::Null => true,
            _ => false,
        }
    }

    /// Evaluates this filter expression against a `serde_json::Value`.
    #[must_use]
    pub fn matches_json(&self, val: &Value) -> bool {
        match &self.predicate {
            Value::Object(map) => {
                for (k, expected) in map {
                    let actual = extract_json_path(val, k);
                    if !match_field_condition(actual.as_ref(), expected) {
                        return false;
                    }
                }
                true
            }
            Value::Null => true,
            _ => false,
        }
    }

    #[must_use]
    pub fn into_inner(self) -> Value {
        self.predicate
    }

    #[must_use]
    pub fn as_value(&self) -> &Value {
        &self.predicate
    }

    /// Extract query candidate tokens from this filter expression for compressed-domain filtering.
    #[must_use]
    pub fn extract_tokens(&self) -> Vec<String> {
        let mut tokens = Vec::new();
        if let Value::Object(map) = &self.predicate {
            for (k, v) in map {
                tokens.push(k.clone());
                match v {
                    Value::String(s) => tokens.push(s.clone()),
                    Value::Number(n) => tokens.push(n.to_string()),
                    Value::Bool(b) => tokens.push(if *b { "true".into() } else { "false".into() }),
                    Value::Object(inner) => {
                        for (op, target) in inner {
                            if op == "$eq" {
                                if let Value::String(s) = target {
                                    tokens.push(s.clone());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        tokens
    }
}

impl From<FilterExpr> for Value {
    fn from(f: FilterExpr) -> Self {
        f.predicate
    }
}

fn extract_json_path(val: &Value, path: &str) -> Option<Value> {
    let clean = path.trim_start_matches("$.").trim_start_matches('/');
    let mut curr = val;
    for segment in clean.split('.') {
        if segment.is_empty() {
            continue;
        }
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
    Some(curr.clone())
}

fn match_field_condition(actual: Option<&Value>, expected: &Value) -> bool {
    if let Value::Object(op_map) = expected {
        if op_map.keys().any(|k| k.starts_with('$')) {
            for (op, target) in op_map {
                if !evaluate_operator(actual, op, target) {
                    return false;
                }
            }
            return true;
        }
    }

    match actual {
        Some(val) => {
            if let Value::Array(arr) = val {
                if arr.iter().any(|item| item == expected) {
                    return true;
                }
            }
            val == expected
        }
        None => expected.is_null(),
    }
}

fn evaluate_operator(actual: Option<&Value>, op: &str, target: &Value) -> bool {
    match op {
        "$exists" => {
            let should_exist = target.as_bool().unwrap_or(true);
            actual.is_some() == should_exist
        }
        "$eq" => match actual {
            Some(val) => val == target,
            None => target.is_null(),
        },
        "$ne" => match actual {
            Some(val) => val != target,
            None => !target.is_null(),
        },
        "$gt" => actual.and_then(|val| compare_values(val, target)) == Some(Ordering::Greater),
        "$gte" => actual
            .and_then(|val| compare_values(val, target))
            .is_some_and(|ord| ord != Ordering::Less),
        "$lt" => actual.and_then(|val| compare_values(val, target)) == Some(Ordering::Less),
        "$lte" => actual
            .and_then(|val| compare_values(val, target))
            .is_some_and(|ord| ord != Ordering::Greater),
        "$in" => {
            if let Value::Array(items) = target {
                match actual {
                    Some(val) => items.contains(val),
                    None => false,
                }
            } else {
                false
            }
        }
        "$nin" => {
            if let Value::Array(items) = target {
                match actual {
                    Some(val) => !items.contains(val),
                    None => true,
                }
            } else {
                true
            }
        }
        _ => false,
    }
}

fn compare_values(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Number(n1), Value::Number(n2)) => {
            if let (Some(i1), Some(i2)) = (n1.as_i64(), n2.as_i64()) {
                return Some(i1.cmp(&i2));
            }
            if let (Some(f1), Some(f2)) = (n1.as_f64(), n2.as_f64()) {
                return f1.partial_cmp(&f2);
            }
            None
        }
        (Value::String(s1), Value::String(s2)) => Some(s1.cmp(s2)),
        (Value::Bool(b1), Value::Bool(b2)) => Some(b1.cmp(b2)),
        _ => None,
    }
}

/// Convenient macro to construct a [`FilterExpr`] using JSON-like syntax.
///
/// # Example
/// ```rust
/// use lilia_core::doc;
/// let f = doc! { "status": "completed", "year": 2026 };
/// ```
#[macro_export]
macro_rules! doc {
    ($($json:tt)+) => {
        $crate::aggregation::FilterExpr::from_json(::serde_json::json!({ $($json)+ }))
    };
}
