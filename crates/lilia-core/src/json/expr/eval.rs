use std::cmp::Ordering;

use super::ast::{BinaryOp, EvaluatedValue, Expr, LiteralValue, UnaryOp};
use crate::calendar::CivilDate;
use crate::json::binary::int_as_real;
use crate::json::{BinaryDoc, BinaryValueRef};
use crate::{ErrorCode, LiliaError, Result};

/// Evaluates an AST expression against a [`BinaryDoc`].
///
/// # Errors
/// `InvalidInput` when the expression calls an unknown function or passes it the wrong number
/// of arguments.
pub fn eval_expr(expr: &Expr, doc: &BinaryDoc) -> Result<EvaluatedValue> {
    eval_node(expr, doc)
}

/// Evaluates a filter predicate against a [`BinaryDoc`], returning `true` if the document matches.
#[must_use]
pub fn eval_filter_predicate(predicate: &Expr, doc: &BinaryDoc) -> bool {
    match eval_node(predicate, doc) {
        Ok(val) => val.is_truthy(),
        Err(_) => false,
    }
}

pub(super) fn eval_node(expr: &Expr, doc: &BinaryDoc) -> Result<EvaluatedValue> {
    match expr {
        Expr::Path(path) => {
            if let Some(val_ref) = doc.extract(path) {
                Ok(binary_value_to_evaluated(&val_ref))
            } else {
                Ok(EvaluatedValue::Null)
            }
        }
        Expr::Literal(lit) => Ok(literal_to_evaluated(lit)),
        Expr::FunctionCall { name, args } => eval_function(name, args, doc),
        Expr::Binary { op, left, right } => {
            let left_val = eval_node(left, doc)?;
            let right_val = eval_node(right, doc)?;
            Ok(eval_binary_op(*op, &left_val, &right_val))
        }
        Expr::Unary { op, expr } => {
            let val = eval_node(expr, doc)?;
            match op {
                UnaryOp::Not => Ok(EvaluatedValue::Bool(!val.is_truthy())),
                UnaryOp::Neg => match val {
                    EvaluatedValue::Int(i) => Ok(i
                        .checked_neg()
                        .map_or(EvaluatedValue::Null, EvaluatedValue::Int)),
                    EvaluatedValue::Float(f) => Ok(EvaluatedValue::Float(-f)),
                    _ => Ok(EvaluatedValue::Null),
                },
            }
        }
        Expr::IsNull(inner) => {
            let val = eval_node(inner, doc)?;
            Ok(EvaluatedValue::Bool(matches!(val, EvaluatedValue::Null)))
        }
        Expr::IsNotNull(inner) => {
            let val = eval_node(inner, doc)?;
            Ok(EvaluatedValue::Bool(!matches!(val, EvaluatedValue::Null)))
        }
    }
}

fn binary_value_to_evaluated(val: &BinaryValueRef<'_>) -> EvaluatedValue {
    match val {
        BinaryValueRef::Null => EvaluatedValue::Null,
        BinaryValueRef::Bool(b) => EvaluatedValue::Bool(*b),
        BinaryValueRef::Int64(i) => EvaluatedValue::Int(*i),
        BinaryValueRef::Float64(f) => EvaluatedValue::Float(*f),
        BinaryValueRef::String(s) => EvaluatedValue::String((*s).to_string()),
        BinaryValueRef::Array(arr) => {
            let mut list = Vec::with_capacity(arr.len());
            for item in arr.iter() {
                match item {
                    BinaryValueRef::String(s) => list.push(s.to_string()),
                    BinaryValueRef::Int64(i) => list.push(i.to_string()),
                    BinaryValueRef::Float64(f) => list.push(f.to_string()),
                    BinaryValueRef::Bool(b) => list.push(b.to_string()),
                    _ => list.push(item.to_json_value().to_string()),
                }
            }
            EvaluatedValue::List(list)
        }
        _ => EvaluatedValue::String(val.to_json_value().to_string()),
    }
}

pub(crate) fn literal_to_evaluated(lit: &LiteralValue) -> EvaluatedValue {
    match lit {
        LiteralValue::Null => EvaluatedValue::Null,
        LiteralValue::Bool(b) => EvaluatedValue::Bool(*b),
        LiteralValue::Int(i) => EvaluatedValue::Int(*i),
        LiteralValue::Float(f) => EvaluatedValue::Float(*f),
        LiteralValue::String(s) => EvaluatedValue::String(s.clone()),
    }
}

/// Applies `int` to two integers (`None` on overflow gives NULL) and `real` to any other pair
/// of numbers; non-numeric operands give NULL.
fn arithmetic(
    left: &EvaluatedValue,
    right: &EvaluatedValue,
    int: fn(i64, i64) -> Option<i64>,
    real: fn(f64, f64) -> Option<f64>,
) -> EvaluatedValue {
    let (a, b) = match (left, right) {
        (EvaluatedValue::Int(a), EvaluatedValue::Int(b)) => {
            return int(*a, *b).map_or(EvaluatedValue::Null, EvaluatedValue::Int);
        }
        (EvaluatedValue::Float(a), EvaluatedValue::Float(b)) => (*a, *b),
        (EvaluatedValue::Int(a), EvaluatedValue::Float(b)) => (int_as_real(*a), *b),
        (EvaluatedValue::Float(a), EvaluatedValue::Int(b)) => (*a, int_as_real(*b)),
        _ => return EvaluatedValue::Null,
    };
    real(a, b).map_or(EvaluatedValue::Null, EvaluatedValue::Float)
}

pub(crate) fn eval_binary_op(
    op: BinaryOp,
    left: &EvaluatedValue,
    right: &EvaluatedValue,
) -> EvaluatedValue {
    match op {
        BinaryOp::Eq => EvaluatedValue::Bool(compare_equal(left, right)),
        BinaryOp::NotEq => EvaluatedValue::Bool(!compare_equal(left, right)),
        BinaryOp::Lt => EvaluatedValue::Bool(compare_ord(left, right) == Some(Ordering::Less)),
        BinaryOp::Lte => {
            EvaluatedValue::Bool(compare_ord(left, right).is_some_and(|o| o != Ordering::Greater))
        }
        BinaryOp::Gt => EvaluatedValue::Bool(compare_ord(left, right) == Some(Ordering::Greater)),
        BinaryOp::Gte => {
            EvaluatedValue::Bool(compare_ord(left, right).is_some_and(|o| o != Ordering::Less))
        }
        BinaryOp::And => EvaluatedValue::Bool(left.is_truthy() && right.is_truthy()),
        BinaryOp::Or => EvaluatedValue::Bool(left.is_truthy() || right.is_truthy()),
        BinaryOp::Add => match (left, right) {
            (EvaluatedValue::String(a), EvaluatedValue::String(b)) => {
                EvaluatedValue::String(format!("{a}{b}"))
            }
            _ => arithmetic(left, right, i64::checked_add, |a, b| Some(a + b)),
        },
        BinaryOp::Sub => arithmetic(left, right, i64::checked_sub, |a, b| Some(a - b)),
        BinaryOp::Mul => arithmetic(left, right, i64::checked_mul, |a, b| Some(a * b)),
        BinaryOp::Div => arithmetic(left, right, i64::checked_div, |a, b| {
            (b != 0.0).then_some(a / b)
        }),
    }
}

pub(crate) fn compare_equal(left: &EvaluatedValue, right: &EvaluatedValue) -> bool {
    match (left, right) {
        (EvaluatedValue::Null, EvaluatedValue::Null) => true,
        (EvaluatedValue::Bool(a), EvaluatedValue::Bool(b)) => a == b,
        (EvaluatedValue::Int(a), EvaluatedValue::Int(b)) => a == b,
        (EvaluatedValue::Float(a), EvaluatedValue::Float(b)) => (a - b).abs() < f64::EPSILON,
        (EvaluatedValue::Int(a), EvaluatedValue::Float(b)) => {
            (int_as_real(*a) - b).abs() < f64::EPSILON
        }
        (EvaluatedValue::Float(a), EvaluatedValue::Int(b)) => {
            (a - int_as_real(*b)).abs() < f64::EPSILON
        }
        (EvaluatedValue::String(a), EvaluatedValue::String(b)) => a == b,
        _ => false,
    }
}

pub(crate) fn compare_ord(left: &EvaluatedValue, right: &EvaluatedValue) -> Option<Ordering> {
    match (left, right) {
        (EvaluatedValue::Int(a), EvaluatedValue::Int(b)) => Some(a.cmp(b)),
        (EvaluatedValue::Float(a), EvaluatedValue::Float(b)) => a.partial_cmp(b),
        (EvaluatedValue::Int(a), EvaluatedValue::Float(b)) => int_as_real(*a).partial_cmp(b),
        (EvaluatedValue::Float(a), EvaluatedValue::Int(b)) => a.partial_cmp(&int_as_real(*b)),
        (EvaluatedValue::String(a), EvaluatedValue::String(b)) => Some(a.cmp(b)),
        (EvaluatedValue::Bool(a), EvaluatedValue::Bool(b)) => Some(a.cmp(b)),
        _ => None,
    }
}

/// Upper bound on the number of index keys a single `composite(...)` evaluation may produce
/// when several of its arguments are multi-valued (arrays).
const COMPOSITE_MAX_KEYS: usize = 1024;

/// Evaluates `composite(a, b, ...)`: a single index key combining several fields.
///
/// Each component is length-prefixed (`<len>:<value>`) before concatenation so that two
/// different tuples can never produce the same key. Multi-valued components yield the cartesian
/// product of their values, bounded by [`COMPOSITE_MAX_KEYS`].
fn eval_composite(args: &[Expr], doc: &BinaryDoc) -> Result<EvaluatedValue> {
    if args.is_empty() {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "composite() requires at least 1 argument",
            false,
        ));
    }
    let mut keys = vec![String::new()];
    for arg in args {
        let parts = eval_node(arg, doc)?.to_index_strings();
        if keys.len().saturating_mul(parts.len().max(1)) > COMPOSITE_MAX_KEYS {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!("composite() would produce more than {COMPOSITE_MAX_KEYS} index keys"),
                false,
            ));
        }
        let mut next = Vec::with_capacity(keys.len() * parts.len());
        for prefix in &keys {
            for part in &parts {
                next.push(format!("{prefix}{}:{part}", part.len()));
            }
        }
        keys = next;
    }
    if keys.len() == 1 {
        Ok(EvaluatedValue::String(keys.remove(0)))
    } else {
        Ok(EvaluatedValue::List(keys))
    }
}

fn eval_function(name: &str, args: &[Expr], doc: &BinaryDoc) -> Result<EvaluatedValue> {
    match name.to_ascii_lowercase().as_str() {
        "lower" => Ok(map_text(single_arg("lower", args, doc)?, str::to_lowercase)),
        "upper" => Ok(map_text(single_arg("upper", args, doc)?, str::to_uppercase)),
        "trim" => Ok(match single_arg("trim", args, doc)? {
            EvaluatedValue::String(s) => EvaluatedValue::String(s.trim().to_string()),
            other => other,
        }),
        "length" | "len" => Ok(match single_arg("length", args, doc)? {
            EvaluatedValue::String(s) => count(s.chars().count()),
            EvaluatedValue::List(l) => count(l.len()),
            EvaluatedValue::Null => EvaluatedValue::Null,
            _ => EvaluatedValue::Int(1),
        }),
        "json_array_length" | "array_length" => eval_array_length(args, doc),
        "composite" => eval_composite(args, doc),
        "coalesce" => {
            if args.is_empty() {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "coalesce() requires at least 1 argument",
                    false,
                ));
            }
            for arg in args {
                let val = eval_node(arg, doc)?;
                if !matches!(val, EvaluatedValue::Null) {
                    return Ok(val);
                }
            }
            Ok(EvaluatedValue::Null)
        }
        "substr" | "substring" => eval_substr(args, doc),
        "strftime" => {
            let [format, timestamp] = args else {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "strftime(format, timestamp) takes exactly 2 arguments",
                    false,
                ));
            };
            let EvaluatedValue::String(format) = eval_node(format, doc)? else {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "strftime format must be a string",
                    false,
                ));
            };
            Ok(format_strftime(&format, &eval_node(timestamp, doc)?))
        }
        other => Err(LiliaError::new(
            ErrorCode::InvalidInput,
            format!("unsupported function in index expression: '{other}'"),
            false,
        )),
    }
}

/// Evaluates the only argument of `name(...)`.
fn single_arg(name: &str, args: &[Expr], doc: &BinaryDoc) -> Result<EvaluatedValue> {
    let [arg] = args else {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            format!("{name}() takes exactly 1 argument"),
            false,
        ));
    };
    eval_node(arg, doc)
}

/// Applies `f` to a string, or to the comma-joined index strings of any other non-null value.
fn map_text(value: EvaluatedValue, f: fn(&str) -> String) -> EvaluatedValue {
    match value {
        EvaluatedValue::String(s) => EvaluatedValue::String(f(&s)),
        EvaluatedValue::Null => EvaluatedValue::Null,
        other => EvaluatedValue::String(f(&other.to_index_strings().join(","))),
    }
}

fn count(n: usize) -> EvaluatedValue {
    EvaluatedValue::Int(i64::try_from(n).unwrap_or(i64::MAX))
}

fn eval_array_length(args: &[Expr], doc: &BinaryDoc) -> Result<EvaluatedValue> {
    // A path is measured on the stored array itself rather than on its index strings.
    if let [Expr::Path(path)] = args {
        return Ok(match doc.extract(path) {
            Some(target) => target
                .as_array()
                .map_or(EvaluatedValue::Int(0), |arr| count(arr.len())),
            None => EvaluatedValue::Null,
        });
    }
    Ok(match single_arg("json_array_length", args, doc)? {
        EvaluatedValue::List(l) => count(l.len()),
        EvaluatedValue::Null => EvaluatedValue::Null,
        _ => EvaluatedValue::Int(0),
    })
}

fn eval_substr(args: &[Expr], doc: &BinaryDoc) -> Result<EvaluatedValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "substr() takes 2 or 3 arguments (str, start[, len])",
            false,
        ));
    }
    let EvaluatedValue::String(s) = eval_node(&args[0], doc)? else {
        return Ok(EvaluatedValue::Null);
    };
    // Negative positions and lengths count as 0.
    let non_negative = |value: EvaluatedValue| match value {
        EvaluatedValue::Int(i) => Some(usize::try_from(i.max(0)).unwrap_or(usize::MAX)),
        _ => None,
    };
    let start = non_negative(eval_node(&args[1], doc)?).unwrap_or(0);
    let length = match args.get(2) {
        Some(arg) => non_negative(eval_node(arg, doc)?),
        None => None,
    };
    let chars = s.chars().skip(start);
    Ok(EvaluatedValue::String(match length {
        Some(len) => chars.take(len).collect(),
        None => chars.collect(),
    }))
}

/// Calendar fields of a UTC instant, for `strftime`.
#[derive(Debug, Clone, Copy)]
struct DateTimeParts {
    date: CivilDate,
    hour: i64,
    min: i64,
    sec: i64,
}

impl DateTimeParts {
    /// Instants before 1970-01-01 are clamped to it.
    fn from_epoch_seconds(secs: i64) -> Self {
        let secs = secs.max(0);
        let secs_of_day = secs % 86_400;
        Self {
            date: CivilDate::from_days(secs / 86_400),
            hour: secs_of_day / 3_600,
            min: secs_of_day % 3_600 / 60,
            sec: secs_of_day % 60,
        }
    }

    /// Parses `2026-11-13T01:31:34Z`, `2026-11-13 01:31:34` or `2026-11-13`.
    fn parse_iso8601(s: &str) -> Option<Self> {
        let clean = s.trim();
        if clean.len() < 10 {
            return None;
        }
        let date = CivilDate::new(
            clean.get(0..4)?.parse().ok()?,
            clean.get(5..7)?.parse().ok()?,
            clean.get(8..10)?.parse().ok()?,
        )?;
        let (hour, min, sec) = if clean.len() >= 19 {
            (
                clean.get(11..13)?.parse().ok()?,
                clean.get(14..16)?.parse().ok()?,
                clean.get(17..19)?.parse().ok()?,
            )
        } else {
            (0, 0, 0)
        };
        if hour > 23 || min > 59 || sec > 60 {
            return None;
        }
        Some(Self {
            date,
            hour,
            min,
            sec,
        })
    }
}

/// Integer timestamps above this are read as milliseconds, below as seconds.
const MILLIS_THRESHOLD: i64 = 100_000_000_000;

/// Seconds of a real timestamp, truncated toward zero.
#[allow(
    clippy::cast_possible_truncation,
    reason = "float-to-int casts saturate: out-of-range instants clamp"
)]
fn real_seconds(timestamp: f64) -> i64 {
    timestamp.trunc() as i64
}

fn format_strftime(format: &str, ts_val: &EvaluatedValue) -> EvaluatedValue {
    let from_integer =
        |i: i64| DateTimeParts::from_epoch_seconds(if i > MILLIS_THRESHOLD { i / 1000 } else { i });
    let parts = match ts_val {
        EvaluatedValue::Int(i) => from_integer(*i),
        EvaluatedValue::Float(f) => {
            let millis = int_as_real(MILLIS_THRESHOLD);
            DateTimeParts::from_epoch_seconds(real_seconds(if *f > millis {
                *f / 1000.0
            } else {
                *f
            }))
        }
        EvaluatedValue::String(s) => match s.parse::<i64>() {
            Ok(i) => from_integer(i),
            Err(_) => match DateTimeParts::parse_iso8601(s) {
                Some(parts) => parts,
                None => return EvaluatedValue::Null,
            },
        },
        _ => return EvaluatedValue::Null,
    };

    let CivilDate { year, month, day } = parts.date;
    let mut out = String::new();
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let field = match chars.next() {
            Some('Y') => format!("{year:04}"),
            Some('m') => format!("{month:02}"),
            Some('d') => format!("{day:02}"),
            Some('H') => format!("{:02}", parts.hour),
            Some('M') => format!("{:02}", parts.min),
            Some('S') => format!("{:02}", parts.sec),
            Some('F') => format!("{year:04}-{month:02}-{day:02}"),
            Some('j') => format!("{:03}", parts.date.day_of_year()),
            Some('w') => format!("{}", parts.date.day_of_week()),
            Some('%') | None => "%".to_owned(),
            Some(other) => format!("%{other}"),
        };
        out.push_str(&field);
    }
    EvaluatedValue::String(out)
}
