use serde_json::Value;

use super::context::SecurityContext;
use crate::json::expr::ast::{EvaluatedValue, Expr, UnaryOp};
use crate::json::expr::eval::{eval_binary_op, literal_to_evaluated};
use crate::json::parse_expr;
use crate::Result;

/// Evaluates an RLS condition expression against a JSON document and an optional security context.
#[must_use]
pub fn eval_security_condition(expr: &Expr, doc: &Value, ctx: Option<&SecurityContext>) -> bool {
    eval_node(expr, doc, ctx).is_truthy()
}

/// Evaluates a raw string condition against a JSON document and an optional security context.
///
/// # Errors
///
/// [`ErrorCode::InvalidInput`](crate::ErrorCode::InvalidInput) when `condition` does not parse.
pub fn eval_security_condition_str(
    condition: &str,
    doc: &Value,
    ctx: Option<&SecurityContext>,
) -> Result<bool> {
    let expr = parse_expr(condition)?;
    Ok(eval_security_condition(&expr, doc, ctx))
}

/// Evaluates an RLS condition against a KV entry (namespace, key, value bytes) and security context.
#[must_use]
pub fn eval_security_condition_kv(
    expr: &Expr,
    namespace: &str,
    key: &[u8],
    value: &[u8],
    ctx: Option<&SecurityContext>,
) -> bool {
    let key_str = String::from_utf8_lossy(key);
    let val_str = String::from_utf8_lossy(value);

    // 1. If KV value is valid JSON object, augment with key and evaluate
    if let Ok(mut json_val) = serde_json::from_slice::<Value>(value) {
        if let Value::Object(ref mut map) = json_val {
            map.entry("$key")
                .or_insert_with(|| Value::String(key_str.to_string()));
            map.entry("key")
                .or_insert_with(|| Value::String(key_str.to_string()));
            map.entry("namespace")
                .or_insert_with(|| Value::String(namespace.to_string()));
        }
        return eval_security_condition(expr, &json_val, ctx);
    }

    // 2. Synthesize a synthetic document for string/binary KV values
    let synthetic = serde_json::json!({
        "namespace": namespace,
        "key": key_str,
        "value": val_str,
    });
    eval_security_condition(expr, &synthetic, ctx)
}

/// Evaluates an exemption condition expression (e.g. `current_context('role') == 'admin'`).
#[must_use]
pub fn eval_exemption_condition(condition: &str, ctx: Option<&SecurityContext>) -> bool {
    let Ok(expr) = parse_expr(condition) else {
        return false;
    };
    let dummy_doc = Value::Object(serde_json::Map::new());
    eval_node(&expr, &dummy_doc, ctx).is_truthy()
}

fn eval_node(expr: &Expr, doc: &Value, ctx: Option<&SecurityContext>) -> EvaluatedValue {
    match expr {
        Expr::Path(path) => {
            if path == "data" || path == "$data" || path == "value" || path == "$value" {
                value_to_evaluated(doc)
            } else {
                extract_path_from_value(doc, path)
            }
        }
        Expr::Literal(lit) => literal_to_evaluated(lit),
        Expr::FunctionCall { name, args } => eval_function(name, args, doc, ctx),
        Expr::Binary { op, left, right } => {
            let left_val = eval_node(left, doc, ctx);
            let right_val = eval_node(right, doc, ctx);
            eval_binary_op(*op, &left_val, &right_val)
        }
        Expr::Unary { op, expr } => {
            let val = eval_node(expr, doc, ctx);
            match op {
                UnaryOp::Not => EvaluatedValue::Bool(!val.is_truthy()),
                UnaryOp::Neg => match val {
                    EvaluatedValue::Int(i) => i
                        .checked_neg()
                        .map_or(EvaluatedValue::Null, EvaluatedValue::Int),
                    EvaluatedValue::Float(f) => EvaluatedValue::Float(-f),
                    _ => EvaluatedValue::Null,
                },
            }
        }
        Expr::IsNull(inner) => {
            let val = eval_node(inner, doc, ctx);
            EvaluatedValue::Bool(matches!(val, EvaluatedValue::Null))
        }
        Expr::IsNotNull(inner) => {
            let val = eval_node(inner, doc, ctx);
            EvaluatedValue::Bool(!matches!(val, EvaluatedValue::Null))
        }
    }
}

fn eval_function(
    name: &str,
    args: &[Expr],
    doc: &Value,
    ctx: Option<&SecurityContext>,
) -> EvaluatedValue {
    match name.to_ascii_lowercase().as_str() {
        "current_context" => {
            let Some(first_arg) = args.first() else {
                return EvaluatedValue::Null;
            };
            let EvaluatedValue::String(key) = eval_node(first_arg, doc, ctx) else {
                return EvaluatedValue::Null;
            };
            match ctx.and_then(|c| c.get(&key)) {
                Some(val) => EvaluatedValue::String(val.to_string()),
                None => EvaluatedValue::Null,
            }
        }
        "has_role" => {
            let Some(first_arg) = args.first() else {
                return EvaluatedValue::Bool(false);
            };
            let EvaluatedValue::String(role) = eval_node(first_arg, doc, ctx) else {
                return EvaluatedValue::Bool(false);
            };
            EvaluatedValue::Bool(ctx.is_some_and(|c| c.has_role(&role)))
        }
        "json_extract" => {
            if args.is_empty() {
                return EvaluatedValue::Null;
            }
            let (target_doc, path_expr) = if args.len() >= 2 {
                let first_val = eval_node(&args[0], doc, ctx);
                // If first arg evaluates to a string or value, check if path or doc
                match first_val {
                    EvaluatedValue::String(ref s) if s.starts_with('{') => {
                        let parsed = serde_json::from_str::<Value>(s).unwrap_or(Value::Null);
                        (parsed, &args[1])
                    }
                    _ => (doc.clone(), &args[1]),
                }
            } else {
                (doc.clone(), &args[0])
            };

            let EvaluatedValue::String(path_str) = eval_node(path_expr, doc, ctx) else {
                return EvaluatedValue::Null;
            };
            extract_path_from_value(&target_doc, &path_str)
        }
        "coalesce" => {
            for arg in args {
                let val = eval_node(arg, doc, ctx);
                if !matches!(val, EvaluatedValue::Null) {
                    return val;
                }
            }
            EvaluatedValue::Null
        }
        "lower" => {
            if let Some(first) = args.first() {
                match eval_node(first, doc, ctx) {
                    EvaluatedValue::String(s) => EvaluatedValue::String(s.to_lowercase()),
                    other => other,
                }
            } else {
                EvaluatedValue::Null
            }
        }
        "upper" => {
            if let Some(first) = args.first() {
                match eval_node(first, doc, ctx) {
                    EvaluatedValue::String(s) => EvaluatedValue::String(s.to_uppercase()),
                    other => other,
                }
            } else {
                EvaluatedValue::Null
            }
        }
        _ => EvaluatedValue::Null,
    }
}

fn extract_path_from_value(doc: &Value, path: &str) -> EvaluatedValue {
    let clean = path.trim_start_matches("$.").trim_start_matches('/');
    if clean.is_empty() {
        return value_to_evaluated(doc);
    }

    let ptr = format!("/{}", clean.replace('.', "/"));
    if let Some(found) = doc.pointer(&ptr) {
        return value_to_evaluated(found);
    }

    // Direct object key lookup fallback
    if let Value::Object(map) = doc {
        if let Some(val) = map.get(clean) {
            return value_to_evaluated(val);
        }
    }

    EvaluatedValue::Null
}

fn value_to_evaluated(val: &Value) -> EvaluatedValue {
    match val {
        Value::Null => EvaluatedValue::Null,
        Value::Bool(b) => EvaluatedValue::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                EvaluatedValue::Int(i)
            } else if let Some(f) = n.as_f64() {
                EvaluatedValue::Float(f)
            } else {
                EvaluatedValue::String(n.to_string())
            }
        }
        Value::String(s) => EvaluatedValue::String(s.clone()),
        Value::Array(arr) => {
            let mut list = Vec::with_capacity(arr.len());
            for item in arr {
                match item {
                    Value::String(s) => list.push(s.clone()),
                    _ => list.push(item.to_string()),
                }
            }
            EvaluatedValue::List(list)
        }
        Value::Object(_) => EvaluatedValue::String(val.to_string()),
    }
}
