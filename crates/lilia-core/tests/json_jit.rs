use lilia_core::json::{
    eval_filter_predicate, parse_expr, BinaryDoc, JitCompiler, JitPredicateCache,
};
use serde_json::json;

#[test]
fn test_jit_numeric_comparisons() {
    let expr = parse_expr("$.age >= 18").unwrap();
    let compiler = JitCompiler::new().expect("JIT compiler creation");
    let compiled = compiler.compile(&expr).expect("compile expression");

    let doc_adult = BinaryDoc::from_json_value(&json!({ "age": 20 })).unwrap();
    let doc_exact = BinaryDoc::from_json_value(&json!({ "age": 18 })).unwrap();
    let doc_minor = BinaryDoc::from_json_value(&json!({ "age": 15 })).unwrap();
    let doc_missing = BinaryDoc::from_json_value(&json!({ "name": "unknown" })).unwrap();

    assert!(compiled.eval(&doc_adult));
    assert!(compiled.eval(&doc_exact));
    assert!(!compiled.eval(&doc_minor));
    assert!(!compiled.eval(&doc_missing));
}

#[test]
fn test_jit_arithmetic_predicate() {
    // (price * 1.2 > 50.0) AND (status == 'paid')
    let expr = parse_expr("($.price * 1.2 > 50.0) AND ($.status == 'paid')").unwrap();
    let compiler = JitCompiler::new().expect("JIT compiler creation");
    let compiled = compiler.compile(&expr).expect("compile expression");

    // price = 50.0 => 50.0 * 1.2 = 60.0 > 50.0, paid => true
    let doc1 = BinaryDoc::from_json_value(&json!({ "price": 50.0, "status": "paid" })).unwrap();
    // price = 40.0 => 40.0 * 1.2 = 48.0 < 50.0, paid => false
    let doc2 = BinaryDoc::from_json_value(&json!({ "price": 40.0, "status": "paid" })).unwrap();
    // price = 50.0, unpaid => false
    let doc3 = BinaryDoc::from_json_value(&json!({ "price": 50.0, "status": "pending" })).unwrap();

    assert!(compiled.eval(&doc1));
    assert!(!compiled.eval(&doc2));
    assert!(!compiled.eval(&doc3));

    // Parity with AST interpreter
    assert_eq!(compiled.eval(&doc1), eval_filter_predicate(&expr, &doc1));
    assert_eq!(compiled.eval(&doc2), eval_filter_predicate(&expr, &doc2));
    assert_eq!(compiled.eval(&doc3), eval_filter_predicate(&expr, &doc3));
}

#[test]
fn test_jit_string_comparisons() {
    let eq_expr = parse_expr("$.role == 'admin'").unwrap();
    let compiler = JitCompiler::new().expect("JIT compiler creation");
    let compiled_eq = compiler.compile(&eq_expr).expect("compile eq");

    let doc_admin = BinaryDoc::from_json_value(&json!({ "role": "admin" })).unwrap();
    let doc_user = BinaryDoc::from_json_value(&json!({ "role": "user" })).unwrap();

    assert!(compiled_eq.eval(&doc_admin));
    assert!(!compiled_eq.eval(&doc_user));

    let ne_expr = parse_expr("$.status != 'deleted'").unwrap();
    let compiled_ne = JitCompiler::new().unwrap().compile(&ne_expr).unwrap();
    let doc_del = BinaryDoc::from_json_value(&json!({ "status": "deleted" })).unwrap();
    let doc_act = BinaryDoc::from_json_value(&json!({ "status": "active" })).unwrap();

    assert!(!compiled_ne.eval(&doc_del));
    assert!(compiled_ne.eval(&doc_act));
}

#[test]
fn test_jit_null_checks() {
    let null_expr = parse_expr("$.deleted_at IS NULL").unwrap();
    let not_null_expr = parse_expr("$.token IS NOT NULL").unwrap();

    let comp_null = JitCompiler::new().unwrap().compile(&null_expr).unwrap();
    let comp_not_null = JitCompiler::new().unwrap().compile(&not_null_expr).unwrap();

    let doc_without_del = BinaryDoc::from_json_value(&json!({ "id": 1, "token": "xyz" })).unwrap();
    let doc_with_del =
        BinaryDoc::from_json_value(&json!({ "id": 2, "deleted_at": 1700000 })).unwrap();

    assert!(comp_null.eval(&doc_without_del));
    assert!(!comp_null.eval(&doc_with_del));

    assert!(comp_not_null.eval(&doc_without_del));
    assert!(!comp_not_null.eval(&doc_with_del));
}

#[test]
fn test_jit_boolean_logic_and_short_circuit() {
    let expr = parse_expr("($.a > 10 OR $.b > 20) AND $.active == true").unwrap();
    let compiled = JitCompiler::new().unwrap().compile(&expr).unwrap();

    let doc_match1 =
        BinaryDoc::from_json_value(&json!({ "a": 15, "b": 5, "active": true })).unwrap();
    let doc_match2 =
        BinaryDoc::from_json_value(&json!({ "a": 5, "b": 25, "active": true })).unwrap();
    let doc_nomatch =
        BinaryDoc::from_json_value(&json!({ "a": 15, "b": 25, "active": false })).unwrap();
    let doc_neither =
        BinaryDoc::from_json_value(&json!({ "a": 1, "b": 1, "active": true })).unwrap();

    assert!(compiled.eval(&doc_match1));
    assert!(compiled.eval(&doc_match2));
    assert!(!compiled.eval(&doc_nomatch));
    assert!(!compiled.eval(&doc_neither));
}

#[test]
fn test_jit_batch_vectorized_scan() {
    let expr = parse_expr("($.val * 2 >= 50) AND $.status == 'ok'").unwrap();
    let compiled = JitCompiler::new().unwrap().compile(&expr).unwrap();

    let mut docs = Vec::new();
    let mut expected_matches = Vec::new();

    for i in 0..100 {
        let val = i as f64;
        let status = if i % 2 == 0 { "ok" } else { "pending" };
        let doc = BinaryDoc::from_json_value(&json!({
            "id": i,
            "val": val,
            "status": status,
        }))
        .unwrap();

        if (val * 2.0 >= 50.0) && status == "ok" {
            expected_matches.push(i);
        }
        docs.push(doc);
    }

    let doc_refs: Vec<&BinaryDoc> = docs.iter().collect();
    let mut batch_matches = Vec::new();
    compiled.eval_batch(&doc_refs, &mut batch_matches);
    assert_eq!(batch_matches, expected_matches);

    // Also test eval_batch_bytes
    let byte_slices: Vec<&[u8]> = docs.iter().map(|d| d.as_bytes()).collect();
    let mut bytes_matches = Vec::new();
    compiled.eval_batch_bytes(&byte_slices, &mut bytes_matches);
    assert_eq!(bytes_matches, expected_matches);
}

#[test]
fn test_jit_cache_get_or_compile() {
    let cache = JitPredicateCache::new(64);
    let expr = parse_expr("$.amount > 100").unwrap();

    let p1 = cache.get_or_compile(&expr).unwrap();
    assert_eq!(cache.stats().cache_misses, 1);
    assert_eq!(cache.stats().cache_hits, 0);
    assert_eq!(cache.stats().compilations, 1);

    let p2 = cache.get_or_compile(&expr).unwrap();
    assert_eq!(cache.stats().cache_misses, 1);
    assert_eq!(cache.stats().cache_hits, 1);
    assert_eq!(cache.stats().compilations, 1);

    // Assert both pointers point to the exact same compiled predicate Arc
    assert!(std::sync::Arc::ptr_eq(&p1, &p2));
}

#[test]
fn test_jit_parity_with_interpreter_fuzz() {
    let expr =
        parse_expr("($.score * 1.5 >= 75.0) AND ($.category == 'premium' OR $.verified == true)")
            .unwrap();

    let compiled = JitCompiler::new().unwrap().compile(&expr).unwrap();

    let categories = ["basic", "standard", "premium", "gold"];
    let booleans = [true, false];

    for i in 0..500 {
        let score = (i as f64) * 0.4;
        let cat = categories[i % categories.len()];
        let verified = booleans[i % booleans.len()];

        let doc = BinaryDoc::from_json_value(&json!({
            "idx": i,
            "score": score,
            "category": cat,
            "verified": verified,
        }))
        .unwrap();

        let jit_res = compiled.eval(&doc);
        let interp_res = eval_filter_predicate(&expr, &doc);

        assert_eq!(
            jit_res, interp_res,
            "Mismatch at doc {i}: score={score}, category={cat}, verified={verified}"
        );
    }
}
