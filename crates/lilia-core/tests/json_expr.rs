use lilia_core::json::{
    eval_expr, eval_filter_predicate, parse_expr, BinaryDoc, EvaluatedValue,
};
use serde_json::json;

#[test]
fn test_parse_simple_paths_and_literals() {
    let expr = parse_expr("$.priority").unwrap();
    let doc = BinaryDoc::from_json_value(&json!({ "priority": "high" }));
    let val = eval_expr(&expr, &doc).unwrap();
    assert_eq!(val, EvaluatedValue::String("high".to_string()));

    let expr = parse_expr("$.user.age").unwrap();
    let doc = BinaryDoc::from_json_value(&json!({ "user": { "age": 30 } }));
    let val = eval_expr(&expr, &doc).unwrap();
    assert_eq!(val, EvaluatedValue::Int(30));
}

#[test]
fn test_expression_lower_and_upper() {
    let expr = parse_expr("lower($.email)").unwrap();
    let doc = BinaryDoc::from_json_value(&json!({ "email": "Alice@Example.COM" }));
    let val = eval_expr(&expr, &doc).unwrap();
    assert_eq!(val, EvaluatedValue::String("alice@example.com".to_string()));

    let expr = parse_expr("upper($.country)").unwrap();
    let doc = BinaryDoc::from_json_value(&json!({ "country": "fr" }));
    let val = eval_expr(&expr, &doc).unwrap();
    assert_eq!(val, EvaluatedValue::String("FR".to_string()));
}

#[test]
fn test_expression_trim_and_length() {
    let expr = parse_expr("trim($.code)").unwrap();
    let doc = BinaryDoc::from_json_value(&json!({ "code": "   ABC   " }));
    let val = eval_expr(&expr, &doc).unwrap();
    assert_eq!(val, EvaluatedValue::String("ABC".to_string()));

    let expr = parse_expr("length($.name)").unwrap();
    let doc = BinaryDoc::from_json_value(&json!({ "name": "Lilia" }));
    let val = eval_expr(&expr, &doc).unwrap();
    assert_eq!(val, EvaluatedValue::Int(5));
}

#[test]
fn test_expression_json_array_length() {
    let expr = parse_expr("json_array_length($.items)").unwrap();

    let doc3 = BinaryDoc::from_json_value(&json!({ "items": ["apple", "banana", "orange"] }));
    assert_eq!(eval_expr(&expr, &doc3).unwrap(), EvaluatedValue::Int(3));

    let doc0 = BinaryDoc::from_json_value(&json!({ "items": [] }));
    assert_eq!(eval_expr(&expr, &doc0).unwrap(), EvaluatedValue::Int(0));

    let doc_none = BinaryDoc::from_json_value(&json!({ "other": 123 }));
    assert_eq!(eval_expr(&expr, &doc_none).unwrap(), EvaluatedValue::Null);
}

#[test]
fn test_expression_strftime_timestamp() {
    // 1. Integer timestamp in milliseconds (e.g. 2026-11-13 approx: 1794537600000)
    let expr_year = parse_expr("strftime('%Y', $.timestamp)").unwrap();
    let expr_month = parse_expr("strftime('%m', $.timestamp)").unwrap();
    let expr_date = parse_expr("strftime('%F', $.timestamp)").unwrap();

    // 1700000000 is 2023-11-14T22:13:20Z
    let doc_sec = BinaryDoc::from_json_value(&json!({ "timestamp": 1700000000 }));
    assert_eq!(
        eval_expr(&expr_year, &doc_sec).unwrap(),
        EvaluatedValue::String("2023".to_string())
    );
    assert_eq!(
        eval_expr(&expr_month, &doc_sec).unwrap(),
        EvaluatedValue::String("11".to_string())
    );
    assert_eq!(
        eval_expr(&expr_date, &doc_sec).unwrap(),
        EvaluatedValue::String("2023-11-14".to_string())
    );

    // 2. Milliseconds timestamp: 1700000000000
    let doc_ms = BinaryDoc::from_json_value(&json!({ "timestamp": 1700000000000i64 }));
    assert_eq!(
        eval_expr(&expr_year, &doc_ms).unwrap(),
        EvaluatedValue::String("2023".to_string())
    );

    // 3. ISO8601 string: "2026-11-13T01:31:34Z"
    let doc_iso = BinaryDoc::from_json_value(&json!({ "timestamp": "2026-11-13T01:31:34Z" }));
    assert_eq!(
        eval_expr(&expr_year, &doc_iso).unwrap(),
        EvaluatedValue::String("2026".to_string())
    );
    assert_eq!(
        eval_expr(&expr_month, &doc_iso).unwrap(),
        EvaluatedValue::String("11".to_string())
    );
    assert_eq!(
        eval_expr(&expr_date, &doc_iso).unwrap(),
        EvaluatedValue::String("2026-11-13".to_string())
    );
}

#[test]
fn test_expression_coalesce_and_substr() {
    let expr = parse_expr("coalesce($.nickname, $.username, 'guest')").unwrap();
    let doc1 = BinaryDoc::from_json_value(&json!({ "username": "alice" }));
    assert_eq!(
        eval_expr(&expr, &doc1).unwrap(),
        EvaluatedValue::String("alice".to_string())
    );

    let doc2 = BinaryDoc::from_json_value(&json!({ "other": 1 }));
    assert_eq!(
        eval_expr(&expr, &doc2).unwrap(),
        EvaluatedValue::String("guest".to_string())
    );

    let expr_sub = parse_expr("substr($.code, 0, 4)").unwrap();
    let doc_sub = BinaryDoc::from_json_value(&json!({ "code": "ABCDEF" }));
    assert_eq!(
        eval_expr(&expr_sub, &doc_sub).unwrap(),
        EvaluatedValue::String("ABCD".to_string())
    );
}

#[test]
fn test_filter_predicates() {
    let filter = parse_expr("$.resolved == false").unwrap();
    let doc_unresolved = BinaryDoc::from_json_value(&json!({ "priority": 1, "resolved": false }));
    let doc_resolved = BinaryDoc::from_json_value(&json!({ "priority": 1, "resolved": true }));
    let doc_missing = BinaryDoc::from_json_value(&json!({ "priority": 1 }));

    assert!(eval_filter_predicate(&filter, &doc_unresolved));
    assert!(!eval_filter_predicate(&filter, &doc_resolved));
    assert!(!eval_filter_predicate(&filter, &doc_missing));

    // Numeric comparison and conjunction
    let filter_comp = parse_expr("$.priority >= 2 AND $.status != 'archived'").unwrap();
    let doc_match = BinaryDoc::from_json_value(&json!({ "priority": 3, "status": "active" }));
    let doc_nomatch1 = BinaryDoc::from_json_value(&json!({ "priority": 1, "status": "active" }));
    let doc_nomatch2 = BinaryDoc::from_json_value(&json!({ "priority": 3, "status": "archived" }));

    assert!(eval_filter_predicate(&filter_comp, &doc_match));
    assert!(!eval_filter_predicate(&filter_comp, &doc_nomatch1));
    assert!(!eval_filter_predicate(&filter_comp, &doc_nomatch2));

    // IS NULL and IS NOT NULL
    let filter_null = parse_expr("$.deleted_at IS NULL").unwrap();
    let doc_live = BinaryDoc::from_json_value(&json!({ "name": "active" }));
    let doc_del = BinaryDoc::from_json_value(&json!({ "name": "inactive", "deleted_at": 12345 }));

    assert!(eval_filter_predicate(&filter_null, &doc_live));
    assert!(!eval_filter_predicate(&filter_null, &doc_del));
}

#[test]
fn test_composite_builds_one_unambiguous_key_per_tuple() {
    let expr = parse_expr("composite($.status, $.store_id)").unwrap();
    let doc = BinaryDoc::from_json_value(&json!({ "status": "paid", "store_id": 4 }));
    assert_eq!(
        eval_expr(&expr, &doc).unwrap(),
        EvaluatedValue::String("4:paid1:4".to_string())
    );

    let missing = BinaryDoc::from_json_value(&json!({ "status": "paid" }));
    assert_eq!(
        eval_expr(&expr, &missing).unwrap(),
        EvaluatedValue::String("4:paid4:null".to_string())
    );
}

#[test]
fn test_composite_expands_multi_valued_components() {
    let expr = parse_expr("composite($.tags, $.status)").unwrap();
    let doc = BinaryDoc::from_json_value(&json!({ "tags": ["a", "b"], "status": "x" }));
    assert_eq!(
        eval_expr(&expr, &doc).unwrap(),
        EvaluatedValue::List(vec!["1:a1:x".to_string(), "1:b1:x".to_string()])
    );
}

#[test]
fn test_composite_rejects_empty_and_oversized_expansions() {
    let empty = parse_expr("composite()").unwrap();
    let doc = BinaryDoc::from_json_value(&json!({}));
    assert!(eval_expr(&empty, &doc).is_err());

    let wide: Vec<u32> = (0..40).collect();
    let doc = BinaryDoc::from_json_value(&json!({ "a": wide, "b": wide }));
    let expr = parse_expr("composite($.a, $.b)").unwrap();
    assert!(eval_expr(&expr, &doc).is_err());
}
