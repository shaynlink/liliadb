//! RFC 6902 JSON Patch implementation (pure Rust).

use serde_json::{json, Value};

use crate::{ErrorCode, LiliaError, Result};

/// Escape a token according to RFC 6901 (JSON Pointer).
#[must_use]
pub fn escape_pointer(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

/// Unescape a token according to RFC 6901 (JSON Pointer).
#[must_use]
pub fn unescape_pointer(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

/// Parse a JSON Pointer into tokens.
fn parse_pointer(pointer: &str) -> Result<Vec<String>> {
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    if !pointer.starts_with('/') {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            format!("JSON pointer must start with '/': {pointer}"),
            false,
        ));
    }
    Ok(pointer[1..].split('/').map(unescape_pointer).collect())
}

/// Compute an RFC 6902 JSON Patch from `source` to `target`.
/// Applying the resulting patch to `source` yields `target`.
#[must_use]
pub fn diff(source: &Value, target: &Value) -> Value {
    let mut operations = Vec::new();
    diff_recursive(source, target, "", &mut operations);
    Value::Array(operations)
}

fn diff_recursive(source: &Value, target: &Value, path: &str, ops: &mut Vec<Value>) {
    if source == target {
        return;
    }

    match (source, target) {
        (Value::Object(src_map), Value::Object(tgt_map)) => {
            // Keys removed in target
            for key in src_map.keys() {
                if !tgt_map.contains_key(key) {
                    let sub_path = format!("{path}/{}", escape_pointer(key));
                    ops.push(json!({
                        "op": "remove",
                        "path": sub_path,
                    }));
                }
            }

            // Keys added or modified in target
            for (key, tgt_val) in tgt_map {
                let sub_path = format!("{path}/{}", escape_pointer(key));
                if let Some(src_val) = src_map.get(key) {
                    diff_recursive(src_val, tgt_val, &sub_path, ops);
                } else {
                    ops.push(json!({
                        "op": "add",
                        "path": sub_path,
                        "value": tgt_val.clone(),
                    }));
                }
            }
        }
        (Value::Array(src_arr), Value::Array(tgt_arr)) => {
            let common_len = src_arr.len().min(tgt_arr.len());
            for i in 0..common_len {
                let sub_path = format!("{path}/{i}");
                diff_recursive(&src_arr[i], &tgt_arr[i], &sub_path, ops);
            }

            // Extra elements in target: add
            for (idx, item) in tgt_arr.iter().enumerate().skip(common_len) {
                let sub_path = format!("{path}/{idx}");
                ops.push(json!({
                    "op": "add",
                    "path": sub_path,
                    "value": item.clone(),
                }));
            }

            // Extra elements in source: remove (in reverse order to preserve indexes)
            for i in (common_len..src_arr.len()).rev() {
                let sub_path = format!("{path}/{i}");
                ops.push(json!({
                    "op": "remove",
                    "path": sub_path,
                }));
            }
        }
        _ => {
            // Scalar change or type change: replace
            ops.push(json!({
                "op": "replace",
                "path": if path.is_empty() { "" } else { path },
                "value": target.clone(),
            }));
        }
    }
}

/// Apply an RFC 6902 JSON Patch to a document.
pub fn apply(mut doc: Value, patch: &Value) -> Result<Value> {
    let Value::Array(operations) = patch else {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "patch must be a JSON array",
            false,
        ));
    };

    for op_val in operations {
        let op_obj = op_val.as_object().ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "patch operation must be an object",
                false,
            )
        })?;

        let op = op_obj
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| LiliaError::new(ErrorCode::InvalidInput, "missing 'op'", false))?;

        let path_str = op_obj
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| LiliaError::new(ErrorCode::InvalidInput, "missing 'path'", false))?;

        let tokens = parse_pointer(path_str)?;

        match op {
            "add" => {
                let value = op_obj.get("value").ok_or_else(|| {
                    LiliaError::new(ErrorCode::InvalidInput, "missing 'value' for add", false)
                })?;
                op_add(&mut doc, &tokens, value.clone())?;
            }
            "remove" => {
                op_remove(&mut doc, &tokens)?;
            }
            "replace" => {
                let value = op_obj.get("value").ok_or_else(|| {
                    LiliaError::new(
                        ErrorCode::InvalidInput,
                        "missing 'value' for replace",
                        false,
                    )
                })?;
                op_replace(&mut doc, &tokens, value.clone())?;
            }
            "move" => {
                let from_str = op_obj.get("from").and_then(Value::as_str).ok_or_else(|| {
                    LiliaError::new(ErrorCode::InvalidInput, "missing 'from' for move", false)
                })?;
                let from_tokens = parse_pointer(from_str)?;
                let moved_val = op_get(&doc, &from_tokens)?.clone();
                op_remove(&mut doc, &from_tokens)?;
                op_add(&mut doc, &tokens, moved_val)?;
            }
            "copy" => {
                let from_str = op_obj.get("from").and_then(Value::as_str).ok_or_else(|| {
                    LiliaError::new(ErrorCode::InvalidInput, "missing 'from' for copy", false)
                })?;
                let from_tokens = parse_pointer(from_str)?;
                let copied_val = op_get(&doc, &from_tokens)?.clone();
                op_add(&mut doc, &tokens, copied_val)?;
            }
            "test" => {
                let value = op_obj.get("value").ok_or_else(|| {
                    LiliaError::new(ErrorCode::InvalidInput, "missing 'value' for test", false)
                })?;
                let current_val = op_get(&doc, &tokens)?;
                if current_val != value {
                    return Err(LiliaError::new(
                        ErrorCode::Conflict,
                        format!("test op failed: current {current_val} != expected {value}"),
                        false,
                    ));
                }
            }
            other => {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("unsupported patch operation: {other}"),
                    false,
                ));
            }
        }
    }

    Ok(doc)
}

fn op_get<'a>(doc: &'a Value, tokens: &[String]) -> Result<&'a Value> {
    let mut current = doc;
    for token in tokens {
        match current {
            Value::Object(map) => {
                current = map.get(token).ok_or_else(|| {
                    LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!("key '{token}' not found"),
                        false,
                    )
                })?;
            }
            Value::Array(arr) => {
                let idx: usize = token.parse().map_err(|_| {
                    LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!("invalid array index: '{token}'"),
                        false,
                    )
                })?;
                current = arr.get(idx).ok_or_else(|| {
                    LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!("array index {idx} out of bounds"),
                        false,
                    )
                })?;
            }
            _ => {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("cannot index into primitive with '{token}'"),
                    false,
                ));
            }
        }
    }
    Ok(current)
}

fn navigate_mut<'a>(doc: &'a mut Value, tokens: &[String]) -> Result<(&'a mut Value, String)> {
    if tokens.is_empty() {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "tokens cannot be empty for parent navigation",
            false,
        ));
    }
    let (parent_tokens, last_token) = tokens.split_at(tokens.len() - 1);
    let mut current = doc;
    for token in parent_tokens {
        match current {
            Value::Object(map) => {
                current = map.get_mut(token).ok_or_else(|| {
                    LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!("key '{token}' not found"),
                        false,
                    )
                })?;
            }
            Value::Array(arr) => {
                let idx: usize = token.parse().map_err(|_| {
                    LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!("invalid array index '{token}'"),
                        false,
                    )
                })?;
                current = arr.get_mut(idx).ok_or_else(|| {
                    LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!("array index {idx} out of bounds"),
                        false,
                    )
                })?;
            }
            _ => {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("cannot traverse primitive at '{token}'"),
                    false,
                ));
            }
        }
    }
    Ok((current, last_token[0].clone()))
}

fn op_add(doc: &mut Value, tokens: &[String], value: Value) -> Result<()> {
    if tokens.is_empty() {
        *doc = value;
        return Ok(());
    }
    let (parent, last_token) = navigate_mut(doc, tokens)?;
    match parent {
        Value::Object(map) => {
            map.insert(last_token, value);
            Ok(())
        }
        Value::Array(arr) => {
            if last_token == "-" {
                arr.push(value);
                Ok(())
            } else {
                let idx: usize = last_token.parse().map_err(|_| {
                    LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!("invalid array index '{last_token}'"),
                        false,
                    )
                })?;
                if idx > arr.len() {
                    return Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!(
                            "index {idx} out of bounds for insertion (len {})",
                            arr.len()
                        ),
                        false,
                    ));
                }
                arr.insert(idx, value);
                Ok(())
            }
        }
        _ => Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "parent is neither object nor array",
            false,
        )),
    }
}

fn op_remove(doc: &mut Value, tokens: &[String]) -> Result<()> {
    if tokens.is_empty() {
        *doc = Value::Null;
        return Ok(());
    }
    let (parent, last_token) = navigate_mut(doc, tokens)?;
    match parent {
        Value::Object(map) => {
            if map.remove(&last_token).is_none() {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("key '{last_token}' does not exist to remove"),
                    false,
                ));
            }
            Ok(())
        }
        Value::Array(arr) => {
            let idx: usize = last_token.parse().map_err(|_| {
                LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("invalid array index '{last_token}'"),
                    false,
                )
            })?;
            if idx >= arr.len() {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("array index {idx} out of bounds to remove"),
                    false,
                ));
            }
            arr.remove(idx);
            Ok(())
        }
        _ => Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "parent is neither object nor array",
            false,
        )),
    }
}

fn op_replace(doc: &mut Value, tokens: &[String], value: Value) -> Result<()> {
    if tokens.is_empty() {
        *doc = value;
        return Ok(());
    }
    let (parent, last_token) = navigate_mut(doc, tokens)?;
    match parent {
        Value::Object(map) => {
            if !map.contains_key(&last_token) {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("key '{last_token}' does not exist to replace"),
                    false,
                ));
            }
            map.insert(last_token, value);
            Ok(())
        }
        Value::Array(arr) => {
            let idx: usize = last_token.parse().map_err(|_| {
                LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("invalid array index '{last_token}'"),
                    false,
                )
            })?;
            if idx >= arr.len() {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("array index {idx} out of bounds to replace"),
                    false,
                ));
            }
            arr[idx] = value;
            Ok(())
        }
        _ => Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "parent is neither object nor array",
            false,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_diff_and_apply_roundtrip() {
        let v1 = json!({
            "title": "Contract v1",
            "author": "Alice",
            "amount": 100,
            "tags": ["legal", "draft"],
            "meta": { "active": true, "reviewer": null }
        });

        let v2 = json!({
            "title": "Contract v2",
            "amount": 150,
            "tags": ["legal", "reviewed", "final"],
            "meta": { "active": true, "reviewer": "Bob" },
            "published": true
        });

        let patch = diff(&v1, &v2);
        assert!(patch.is_array());
        let operations = patch.as_array().unwrap();
        assert!(!operations.is_empty());

        let reconstructed = apply(v1.clone(), &patch).unwrap();
        assert_eq!(reconstructed, v2);

        // Reverse patch
        let reverse_patch = diff(&v2, &v1);
        let back = apply(v2.clone(), &reverse_patch).unwrap();
        assert_eq!(back, v1);
    }

    #[test]
    fn test_patch_operations_explicit() {
        let doc = json!({ "foo": "bar", "numbers": [1, 2, 3] });
        let patch = json!([
            { "op": "test", "path": "/foo", "value": "bar" },
            { "op": "replace", "path": "/foo", "value": "baz" },
            { "op": "add", "path": "/numbers/1", "value": 42 },
            { "op": "copy", "from": "/foo", "path": "/copied" },
            { "op": "move", "from": "/copied", "path": "/moved" },
            { "op": "remove", "path": "/numbers/0" }
        ]);

        let result = apply(doc, &patch).unwrap();
        assert_eq!(result["foo"], "baz");
        assert_eq!(result["moved"], "baz");
        assert_eq!(result["numbers"], json!([42, 2, 3]));
    }
}
