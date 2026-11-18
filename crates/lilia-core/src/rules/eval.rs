use crate::json::expr::eval::{eval_binary_op, literal_to_evaluated};
use crate::json::expr::{eval_expr, EvaluatedValue, Expr, UnaryOp};
use crate::json::BinaryDoc;
use crate::Result;
use serde_json::Value;

/// Evaluates a rule's compiled `Expr` condition against a JSON document in a specified collection.
#[must_use]
pub fn eval_rule_condition_json(expr: &Expr, collection: &str, doc: &BinaryDoc) -> bool {
    eval_rule_node(expr, collection, doc).is_truthy()
}

/// Evaluates a rule's compiled `Expr` condition against a `serde_json::Value`.
///
/// # Errors
/// `InvalidInput` when `val` cannot be encoded as a binary document.
pub fn eval_rule_condition_value(expr: &Expr, collection: &str, val: &Value) -> Result<bool> {
    let mut bytes = serde_json::to_vec(val).unwrap_or_default();
    let bin_doc = crate::json::parse_json_bytes_to_binary(&mut bytes)?;
    Ok(eval_rule_condition_json(expr, collection, &bin_doc))
}

/// Evaluates a rule condition against a KV entry (namespace, key, value bytes).
#[must_use]
pub fn eval_rule_condition_kv(expr: &Expr, namespace: &str, key: &[u8], value: &[u8]) -> bool {
    let key_str = String::from_utf8_lossy(key);
    let val_str = String::from_utf8_lossy(value);

    // 1. Try parsing KV value as JSON object
    if let Ok(mut json_val) = serde_json::from_slice::<Value>(value) {
        if let Value::Object(ref mut map) = json_val {
            map.entry("$key")
                .or_insert_with(|| Value::String(key_str.to_string()));
            map.entry("key")
                .or_insert_with(|| Value::String(key_str.to_string()));
        }
        if let Ok(res) = eval_rule_condition_value(expr, namespace, &json_val) {
            return res;
        }
    }

    // 2. Synthesize a synthetic document for string/status KV values
    let synthetic = serde_json::json!({
        "key": key_str,
        "value": val_str,
        "status": val_str,
    });
    eval_rule_condition_value(expr, namespace, &synthetic).unwrap_or(false)
}

/// Infer the target collection name from expression paths if not specified.
#[must_use]
pub fn infer_target_collection(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Path(p) => {
            if let Some((coll, _rest)) = p.split_once('.') {
                let clean = coll.trim_start_matches('$');
                if !clean.is_empty() {
                    return Some(clean.to_string());
                }
            }
            None
        }
        Expr::Binary { left, right, .. } => {
            infer_target_collection(left).or_else(|| infer_target_collection(right))
        }
        Expr::Unary { expr, .. } | Expr::IsNull(expr) | Expr::IsNotNull(expr) => {
            infer_target_collection(expr)
        }
        Expr::FunctionCall { args, .. } => {
            for arg in args {
                if let Some(target) = infer_target_collection(arg) {
                    return Some(target);
                }
            }
            None
        }
        Expr::Literal(_) => None,
    }
}

fn eval_rule_node(expr: &Expr, collection: &str, doc: &BinaryDoc) -> EvaluatedValue {
    match expr {
        Expr::Path(path) => {
            // 1. Try direct extraction
            if let Some(val_ref) = doc.extract(path) {
                return binary_ref_to_evaluated(&val_ref);
            }

            // 2. If path is "collection.field", try stripping "collection."
            let prefix = format!("{collection}.");
            if let Some(stripped) = path.strip_prefix(&prefix) {
                if let Some(val_ref) = doc.extract(stripped) {
                    return binary_ref_to_evaluated(&val_ref);
                }
            }

            // 3. If path is "$key", try "_id" or "id"
            if path == "$key" || path == "$id" || path == "key" || path == "id" {
                if let Some(val_ref) = doc.extract("_id").or_else(|| doc.extract("id")) {
                    return binary_ref_to_evaluated(&val_ref);
                }
            }

            EvaluatedValue::Null
        }
        Expr::Literal(lit) => literal_to_evaluated(lit),
        Expr::Binary { op, left, right } => {
            let left_val = eval_rule_node(left, collection, doc);
            let right_val = eval_rule_node(right, collection, doc);
            eval_binary_op(*op, &left_val, &right_val)
        }
        Expr::Unary { op, expr } => {
            let val = eval_rule_node(expr, collection, doc);
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
            let val = eval_rule_node(inner, collection, doc);
            EvaluatedValue::Bool(matches!(val, EvaluatedValue::Null))
        }
        Expr::IsNotNull(inner) => {
            let val = eval_rule_node(inner, collection, doc);
            EvaluatedValue::Bool(!matches!(val, EvaluatedValue::Null))
        }
        Expr::FunctionCall { .. } => {
            // Fall back to standard expression evaluation
            eval_expr(expr, doc).unwrap_or(EvaluatedValue::Null)
        }
    }
}

fn binary_ref_to_evaluated(val_ref: &crate::json::BinaryValueRef<'_>) -> EvaluatedValue {
    match val_ref {
        crate::json::BinaryValueRef::Null => EvaluatedValue::Null,
        crate::json::BinaryValueRef::Bool(b) => EvaluatedValue::Bool(*b),
        crate::json::BinaryValueRef::Int64(i) => EvaluatedValue::Int(*i),
        crate::json::BinaryValueRef::Float64(f) => EvaluatedValue::Float(*f),
        crate::json::BinaryValueRef::String(s) => EvaluatedValue::String((*s).to_string()),
        crate::json::BinaryValueRef::Bytes(b) => {
            EvaluatedValue::String(String::from_utf8_lossy(b).to_string())
        }
        crate::json::BinaryValueRef::Array(_) | crate::json::BinaryValueRef::Object(_) => {
            EvaluatedValue::String(val_ref.to_json_value().to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_expr;

    #[test]
    fn test_eval_heartrate_motion_rule() {
        let expr = parse_expr("events.heart_rate > 140 AND events.motion == 'still'").unwrap();
        let mut bytes = serde_json::to_vec(&serde_json::json!({
            "heart_rate": 155,
            "motion": "still"
        }))
        .unwrap();
        let doc = crate::json::parse_json_bytes_to_binary(&mut bytes).unwrap();

        assert!(eval_rule_condition_json(&expr, "events", &doc));

        let mut bytes2 = serde_json::to_vec(&serde_json::json!({
            "heart_rate": 120,
            "motion": "still"
        }))
        .unwrap();
        let doc2 = crate::json::parse_json_bytes_to_binary(&mut bytes2).unwrap();
        assert!(!eval_rule_condition_json(&expr, "events", &doc2));
    }
}
