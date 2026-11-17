use lilia_core::json::{
    eval_check, parse_expr, referenced_paths, validate_check_expr, BinaryDoc, CheckOutcome,
};
use lilia_core::ErrorCode;
use serde_json::{json, Value};

const ACCOUNT_CHECK: &str = "$.balance >= 0.0 AND length($.iban) == 27";

fn outcome(expression: &str, document: &Value) -> CheckOutcome {
    let expr = parse_expr(expression).unwrap();
    validate_check_expr(&expr).unwrap();
    eval_check(&expr, &BinaryDoc::from_json_value(document).unwrap()).unwrap()
}

fn iban() -> String {
    "F".repeat(27)
}

#[test]
fn account_example_accepts_valid_and_rejects_violations() {
    let valid = json!({ "balance": 12.5, "iban": iban() });
    assert_eq!(outcome(ACCOUNT_CHECK, &valid), CheckOutcome::True);

    let negative = json!({ "balance": -0.01, "iban": iban() });
    assert_eq!(outcome(ACCOUNT_CHECK, &negative), CheckOutcome::False);

    let short_iban = json!({ "balance": 1.0, "iban": "FR76" });
    assert_eq!(outcome(ACCOUNT_CHECK, &short_iban), CheckOutcome::False);
}

#[test]
fn missing_or_null_fields_are_unknown_and_satisfy_the_check() {
    let missing = json!({ "iban": iban() });
    assert_eq!(outcome("$.balance >= 0", &missing), CheckOutcome::Unknown);
    assert!(outcome("$.balance >= 0", &missing).is_satisfied());

    let null = json!({ "balance": null });
    assert_eq!(outcome("$.balance >= 0", &null), CheckOutcome::Unknown);
}

#[test]
fn mandatory_fields_are_expressed_with_is_not_null() {
    let missing = json!({});
    assert_eq!(
        outcome("$.balance IS NOT NULL AND $.balance >= 0", &missing),
        CheckOutcome::False
    );
}

#[test]
fn kleene_logic_for_and_or_not() {
    let doc = json!({ "a": 1 });
    assert_eq!(outcome("$.b > 0 AND $.a > 5", &doc), CheckOutcome::False);
    assert_eq!(outcome("$.b > 0 AND $.a > 0", &doc), CheckOutcome::Unknown);
    assert_eq!(outcome("$.b > 0 OR $.a > 0", &doc), CheckOutcome::True);
    assert_eq!(outcome("$.b > 0 OR $.a > 5", &doc), CheckOutcome::Unknown);
    assert_eq!(outcome("NOT ($.b > 0)", &doc), CheckOutcome::Unknown);
    assert_eq!(outcome("NOT ($.a > 5)", &doc), CheckOutcome::True);
}

#[test]
fn type_mismatch_is_a_violation() {
    let doc = json!({ "balance": "abc" });
    assert_eq!(outcome("$.balance >= 0.0", &doc), CheckOutcome::False);
}

#[test]
fn arithmetic_overflow_and_division_by_zero_never_panic() {
    let overflow = json!({ "n": i64::MAX });
    assert_eq!(outcome("$.n + 1 > 0", &overflow), CheckOutcome::Unknown);
    let zero = json!({ "n": 4, "d": 0 });
    assert_eq!(outcome("$.n / $.d > 0", &zero), CheckOutcome::Unknown);
}

#[test]
fn validation_rejects_unknown_functions_and_bad_arity() {
    let unknown = parse_expr("composite($.a, $.b) == 'x'").unwrap();
    assert_eq!(
        validate_check_expr(&unknown).unwrap_err().code,
        ErrorCode::InvalidInput
    );
    let arity = parse_expr("length($.a, $.b) > 1").unwrap();
    assert_eq!(
        validate_check_expr(&arity).unwrap_err().code,
        ErrorCode::InvalidInput
    );
}

#[test]
fn referenced_paths_lists_every_path() {
    let expr = parse_expr(ACCOUNT_CHECK).unwrap();
    let mut paths = referenced_paths(&expr);
    paths.sort_unstable();
    assert_eq!(paths, ["$.balance", "$.iban"]);
}
