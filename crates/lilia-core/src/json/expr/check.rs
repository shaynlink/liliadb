//! Three-valued (SQL-style) evaluation of declarative `CHECK` constraints on JSON documents.
//!
//! A constraint is violated only when it evaluates to [`CheckOutcome::False`]. A missing path or a
//! `null` value makes comparisons [`CheckOutcome::Unknown`], which — exactly like a SQL `CHECK` —
//! lets the document through. Express a mandatory field with `$.field IS NOT NULL`.

use std::cmp::Ordering;

use super::ast::{BinaryOp, EvaluatedValue, Expr, UnaryOp};
use super::eval::{compare_equal, compare_ord, eval_node};
use crate::json::BinaryDoc;
use crate::{ErrorCode, LiliaError, Result};

/// Result of evaluating a `CHECK` expression against one document (Kleene logic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOutcome {
    True,
    False,
    Unknown,
}

impl CheckOutcome {
    /// Returns `true` unless the outcome is [`CheckOutcome::False`] (SQL `CHECK` acceptance rule).
    #[must_use]
    pub const fn is_satisfied(self) -> bool {
        !matches!(self, Self::False)
    }

    const fn not(self) -> Self {
        match self {
            Self::True => Self::False,
            Self::False => Self::True,
            Self::Unknown => Self::Unknown,
        }
    }

    const fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::False, _) | (_, Self::False) => Self::False,
            (Self::True, Self::True) => Self::True,
            _ => Self::Unknown,
        }
    }

    const fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::True, _) | (_, Self::True) => Self::True,
            (Self::False, Self::False) => Self::False,
            _ => Self::Unknown,
        }
    }

    const fn from_bool(value: bool) -> Self {
        if value {
            Self::True
        } else {
            Self::False
        }
    }
}

/// Evaluates `expr` against `doc` using three-valued logic.
///
/// # Errors
///
/// Returns `InvalidInput` when the expression itself is malformed (unsupported function, wrong
/// arity). Expressions accepted by [`validate_check_expr`] never fail.
pub fn eval_check(expr: &Expr, doc: &BinaryDoc) -> Result<CheckOutcome> {
    match expr {
        Expr::Binary {
            op: BinaryOp::And,
            left,
            right,
        } => Ok(eval_check(left, doc)?.and(eval_check(right, doc)?)),
        Expr::Binary {
            op: BinaryOp::Or,
            left,
            right,
        } => Ok(eval_check(left, doc)?.or(eval_check(right, doc)?)),
        Expr::Binary {
            op:
                op @ (BinaryOp::Eq
                | BinaryOp::NotEq
                | BinaryOp::Lt
                | BinaryOp::Lte
                | BinaryOp::Gt
                | BinaryOp::Gte),
            left,
            right,
        } => {
            let left_val = eval_node(left, doc)?;
            let right_val = eval_node(right, doc)?;
            Ok(compare(*op, &left_val, &right_val))
        }
        Expr::Unary {
            op: UnaryOp::Not,
            expr,
        } => Ok(eval_check(expr, doc)?.not()),
        Expr::IsNull(inner) => Ok(CheckOutcome::from_bool(matches!(
            eval_node(inner, doc)?,
            EvaluatedValue::Null
        ))),
        Expr::IsNotNull(inner) => Ok(CheckOutcome::from_bool(!matches!(
            eval_node(inner, doc)?,
            EvaluatedValue::Null
        ))),
        other => Ok(match eval_node(other, doc)? {
            EvaluatedValue::Null => CheckOutcome::Unknown,
            value => CheckOutcome::from_bool(value.is_truthy()),
        }),
    }
}

fn is_numeric(value: &EvaluatedValue) -> bool {
    matches!(value, EvaluatedValue::Int(_) | EvaluatedValue::Float(_))
}

fn comparable(left: &EvaluatedValue, right: &EvaluatedValue) -> bool {
    (is_numeric(left) && is_numeric(right))
        || matches!(
            (left, right),
            (EvaluatedValue::String(_), EvaluatedValue::String(_))
                | (EvaluatedValue::Bool(_), EvaluatedValue::Bool(_))
        )
}

fn compare(op: BinaryOp, left: &EvaluatedValue, right: &EvaluatedValue) -> CheckOutcome {
    if matches!(left, EvaluatedValue::Null) || matches!(right, EvaluatedValue::Null) {
        return CheckOutcome::Unknown;
    }
    if !comparable(left, right) {
        return CheckOutcome::False;
    }
    let ordering = compare_ord(left, right);
    CheckOutcome::from_bool(match op {
        BinaryOp::Eq => compare_equal(left, right),
        BinaryOp::NotEq => !compare_equal(left, right),
        BinaryOp::Lt => ordering == Some(Ordering::Less),
        BinaryOp::Lte => ordering.is_some_and(|o| o != Ordering::Greater),
        BinaryOp::Gt => ordering == Some(Ordering::Greater),
        BinaryOp::Gte => ordering.is_some_and(|o| o != Ordering::Less),
        _ => false,
    })
}

/// Accepted `(name, min_args, max_args)` for functions usable inside a `CHECK` expression.
const CHECK_FUNCTIONS: &[(&str, usize, usize)] = &[
    ("lower", 1, 1),
    ("upper", 1, 1),
    ("trim", 1, 1),
    ("length", 1, 1),
    ("len", 1, 1),
    ("json_array_length", 1, 1),
    ("array_length", 1, 1),
    ("coalesce", 1, usize::MAX),
    ("substr", 2, 3),
    ("substring", 2, 3),
];

/// Rejects expressions that could fail at write time (unknown function or wrong arity).
///
/// # Errors
///
/// Returns `InvalidInput` naming the offending function.
pub fn validate_check_expr(expr: &Expr) -> Result<()> {
    match expr {
        Expr::Path(_) | Expr::Literal(_) => Ok(()),
        Expr::FunctionCall { name, args } => {
            let lowered = name.to_ascii_lowercase();
            let Some((_, min, max)) = CHECK_FUNCTIONS.iter().find(|(n, _, _)| *n == lowered) else {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("function '{name}' is not allowed in a CHECK expression"),
                    false,
                ));
            };
            if args.len() < *min || args.len() > *max {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("function '{name}' called with {} argument(s)", args.len()),
                    false,
                ));
            }
            args.iter().try_for_each(validate_check_expr)
        }
        Expr::Binary { left, right, .. } => {
            validate_check_expr(left)?;
            validate_check_expr(right)
        }
        Expr::Unary { expr, .. } | Expr::IsNull(expr) | Expr::IsNotNull(expr) => {
            validate_check_expr(expr)
        }
    }
}

/// Collects every document path referenced by `expr`.
#[must_use]
pub fn referenced_paths(expr: &Expr) -> Vec<&str> {
    let mut paths = Vec::new();
    collect_paths(expr, &mut paths);
    paths
}

fn collect_paths<'a>(expr: &'a Expr, out: &mut Vec<&'a str>) {
    match expr {
        Expr::Path(path) => out.push(path),
        Expr::Literal(_) => {}
        Expr::FunctionCall { args, .. } => args.iter().for_each(|arg| collect_paths(arg, out)),
        Expr::Binary { left, right, .. } => {
            collect_paths(left, out);
            collect_paths(right, out);
        }
        Expr::Unary { expr, .. } | Expr::IsNull(expr) | Expr::IsNotNull(expr) => {
            collect_paths(expr, out);
        }
    }
}
