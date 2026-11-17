use std::cmp::Ordering;

use super::ast::{BinaryOp, EvaluatedValue, Expr, LiteralValue, UnaryOp};
use crate::json::{BinaryDoc, BinaryValueRef};
use crate::{ErrorCode, LiliaError, Result};

/// Evaluates an AST expression against a [`BinaryDoc`].
pub fn eval_expr(expr: &Expr, doc: &BinaryDoc) -> Result<EvaluatedValue> {
    eval_node(expr, doc)
}

/// Evaluates a filter predicate against a [`BinaryDoc`], returning `true` if the document matches.
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
            eval_binary_op(*op, &left_val, &right_val)
        }
        Expr::Unary { op, expr } => {
            let val = eval_node(expr, doc)?;
            match op {
                UnaryOp::Not => Ok(EvaluatedValue::Bool(!val.is_truthy())),
                UnaryOp::Neg => match val {
                    EvaluatedValue::Int(i) => Ok(i.checked_neg().map_or(EvaluatedValue::Null, EvaluatedValue::Int)),
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

fn literal_to_evaluated(lit: &LiteralValue) -> EvaluatedValue {
    match lit {
        LiteralValue::Null => EvaluatedValue::Null,
        LiteralValue::Bool(b) => EvaluatedValue::Bool(*b),
        LiteralValue::Int(i) => EvaluatedValue::Int(*i),
        LiteralValue::Float(f) => EvaluatedValue::Float(*f),
        LiteralValue::String(s) => EvaluatedValue::String(s.clone()),
    }
}

pub(super) fn eval_binary_op(
    op: BinaryOp,
    left: &EvaluatedValue,
    right: &EvaluatedValue,
) -> Result<EvaluatedValue> {
    match op {
        BinaryOp::Eq => Ok(EvaluatedValue::Bool(compare_equal(left, right))),
        BinaryOp::NotEq => Ok(EvaluatedValue::Bool(!compare_equal(left, right))),
        BinaryOp::Lt => Ok(EvaluatedValue::Bool(
            compare_ord(left, right) == Some(Ordering::Less),
        )),
        BinaryOp::Lte => Ok(EvaluatedValue::Bool(
            compare_ord(left, right).is_some_and(|o| o != Ordering::Greater),
        )),
        BinaryOp::Gt => Ok(EvaluatedValue::Bool(
            compare_ord(left, right) == Some(Ordering::Greater),
        )),
        BinaryOp::Gte => Ok(EvaluatedValue::Bool(
            compare_ord(left, right).is_some_and(|o| o != Ordering::Less),
        )),
        BinaryOp::And => Ok(EvaluatedValue::Bool(left.is_truthy() && right.is_truthy())),
        BinaryOp::Or => Ok(EvaluatedValue::Bool(left.is_truthy() || right.is_truthy())),
        BinaryOp::Add => match (left, right) {
            (EvaluatedValue::Int(a), EvaluatedValue::Int(b)) => {
                Ok(a.checked_add(*b).map_or(EvaluatedValue::Null, EvaluatedValue::Int))
            }
            (EvaluatedValue::Float(a), EvaluatedValue::Float(b)) => {
                Ok(EvaluatedValue::Float(a + b))
            }
            (EvaluatedValue::Int(a), EvaluatedValue::Float(b)) => {
                Ok(EvaluatedValue::Float(*a as f64 + b))
            }
            (EvaluatedValue::Float(a), EvaluatedValue::Int(b)) => {
                Ok(EvaluatedValue::Float(a + *b as f64))
            }
            (EvaluatedValue::String(a), EvaluatedValue::String(b)) => {
                let mut res = a.clone();
                res.push_str(b);
                Ok(EvaluatedValue::String(res))
            }
            _ => Ok(EvaluatedValue::Null),
        },
        BinaryOp::Sub => match (left, right) {
            (EvaluatedValue::Int(a), EvaluatedValue::Int(b)) => {
                Ok(a.checked_sub(*b).map_or(EvaluatedValue::Null, EvaluatedValue::Int))
            }
            (EvaluatedValue::Float(a), EvaluatedValue::Float(b)) => {
                Ok(EvaluatedValue::Float(a - b))
            }
            (EvaluatedValue::Int(a), EvaluatedValue::Float(b)) => {
                Ok(EvaluatedValue::Float(*a as f64 - b))
            }
            (EvaluatedValue::Float(a), EvaluatedValue::Int(b)) => {
                Ok(EvaluatedValue::Float(a - *b as f64))
            }
            _ => Ok(EvaluatedValue::Null),
        },
        BinaryOp::Mul => match (left, right) {
            (EvaluatedValue::Int(a), EvaluatedValue::Int(b)) => {
                Ok(a.checked_mul(*b).map_or(EvaluatedValue::Null, EvaluatedValue::Int))
            }
            (EvaluatedValue::Float(a), EvaluatedValue::Float(b)) => {
                Ok(EvaluatedValue::Float(a * b))
            }
            (EvaluatedValue::Int(a), EvaluatedValue::Float(b)) => {
                Ok(EvaluatedValue::Float(*a as f64 * b))
            }
            (EvaluatedValue::Float(a), EvaluatedValue::Int(b)) => {
                Ok(EvaluatedValue::Float(a * *b as f64))
            }
            _ => Ok(EvaluatedValue::Null),
        },
        BinaryOp::Div => match (left, right) {
            (EvaluatedValue::Int(a), EvaluatedValue::Int(b)) => {
                Ok(a.checked_div(*b).map_or(EvaluatedValue::Null, EvaluatedValue::Int))
            }
            (EvaluatedValue::Float(a), EvaluatedValue::Float(b)) => {
                if *b == 0.0 {
                    Ok(EvaluatedValue::Null)
                } else {
                    Ok(EvaluatedValue::Float(a / b))
                }
            }
            (EvaluatedValue::Int(a), EvaluatedValue::Float(b)) => {
                if *b == 0.0 {
                    Ok(EvaluatedValue::Null)
                } else {
                    Ok(EvaluatedValue::Float(*a as f64 / b))
                }
            }
            (EvaluatedValue::Float(a), EvaluatedValue::Int(b)) => {
                if *b == 0 {
                    Ok(EvaluatedValue::Null)
                } else {
                    Ok(EvaluatedValue::Float(a / *b as f64))
                }
            }
            _ => Ok(EvaluatedValue::Null),
        },
    }
}

pub(super) fn compare_equal(left: &EvaluatedValue, right: &EvaluatedValue) -> bool {
    match (left, right) {
        (EvaluatedValue::Null, EvaluatedValue::Null) => true,
        (EvaluatedValue::Bool(a), EvaluatedValue::Bool(b)) => a == b,
        (EvaluatedValue::Int(a), EvaluatedValue::Int(b)) => a == b,
        (EvaluatedValue::Float(a), EvaluatedValue::Float(b)) => (a - b).abs() < f64::EPSILON,
        (EvaluatedValue::Int(a), EvaluatedValue::Float(b)) => (*a as f64 - b).abs() < f64::EPSILON,
        (EvaluatedValue::Float(a), EvaluatedValue::Int(b)) => (a - *b as f64).abs() < f64::EPSILON,
        (EvaluatedValue::String(a), EvaluatedValue::String(b)) => a == b,
        _ => false,
    }
}

pub(super) fn compare_ord(left: &EvaluatedValue, right: &EvaluatedValue) -> Option<Ordering> {
    match (left, right) {
        (EvaluatedValue::Int(a), EvaluatedValue::Int(b)) => Some(a.cmp(b)),
        (EvaluatedValue::Float(a), EvaluatedValue::Float(b)) => a.partial_cmp(b),
        (EvaluatedValue::Int(a), EvaluatedValue::Float(b)) => (*a as f64).partial_cmp(b),
        (EvaluatedValue::Float(a), EvaluatedValue::Int(b)) => a.partial_cmp(&(*b as f64)),
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
        "lower" => {
            if args.len() != 1 {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "lower() takes exactly 1 argument",
                    false,
                ));
            }
            let val = eval_node(&args[0], doc)?;
            match val {
                EvaluatedValue::String(s) => Ok(EvaluatedValue::String(s.to_lowercase())),
                EvaluatedValue::Null => Ok(EvaluatedValue::Null),
                other => Ok(EvaluatedValue::String(
                    other.to_index_strings().join(",").to_lowercase(),
                )),
            }
        }
        "upper" => {
            if args.len() != 1 {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "upper() takes exactly 1 argument",
                    false,
                ));
            }
            let val = eval_node(&args[0], doc)?;
            match val {
                EvaluatedValue::String(s) => Ok(EvaluatedValue::String(s.to_uppercase())),
                EvaluatedValue::Null => Ok(EvaluatedValue::Null),
                other => Ok(EvaluatedValue::String(
                    other.to_index_strings().join(",").to_uppercase(),
                )),
            }
        }
        "trim" => {
            if args.len() != 1 {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "trim() takes exactly 1 argument",
                    false,
                ));
            }
            let val = eval_node(&args[0], doc)?;
            match val {
                EvaluatedValue::String(s) => {
                    Ok(EvaluatedValue::String(s.trim().to_string()))
                }
                EvaluatedValue::Null => Ok(EvaluatedValue::Null),
                other => Ok(other),
            }
        }
        "length" | "len" => {
            if args.len() != 1 {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "length() takes exactly 1 argument",
                    false,
                ));
            }
            let val = eval_node(&args[0], doc)?;
            match val {
                EvaluatedValue::String(s) => Ok(EvaluatedValue::Int(s.chars().count() as i64)),
                EvaluatedValue::List(l) => Ok(EvaluatedValue::Int(l.len() as i64)),
                EvaluatedValue::Null => Ok(EvaluatedValue::Null),
                _ => Ok(EvaluatedValue::Int(1)),
            }
        }
        "json_array_length" | "array_length" => {
            if args.len() != 1 {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "json_array_length() takes exactly 1 argument",
                    false,
                ));
            }
            // If argument is a Path, we check directly on BinaryDoc
            if let Expr::Path(path) = &args[0] {
                if let Some(target) = doc.extract(path) {
                    if let Some(arr) = target.as_array() {
                        return Ok(EvaluatedValue::Int(arr.len() as i64));
                    }
                    return Ok(EvaluatedValue::Int(0));
                }
                return Ok(EvaluatedValue::Null);
            }
            let val = eval_node(&args[0], doc)?;
            match val {
                EvaluatedValue::List(l) => Ok(EvaluatedValue::Int(l.len() as i64)),
                EvaluatedValue::Null => Ok(EvaluatedValue::Null),
                _ => Ok(EvaluatedValue::Int(0)),
            }
        }
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
        "substr" | "substring" => {
            if args.len() < 2 || args.len() > 3 {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "substr() takes 2 or 3 arguments (str, start[, len])",
                    false,
                ));
            }
            let str_val = eval_node(&args[0], doc)?;
            let start_val = eval_node(&args[1], doc)?;
            let len_val = if args.len() == 3 {
                Some(eval_node(&args[2], doc)?)
            } else {
                None
            };

            let EvaluatedValue::String(s) = str_val else {
                return Ok(EvaluatedValue::Null);
            };
            let start = match start_val {
                EvaluatedValue::Int(i) => i.max(0) as usize,
                _ => 0,
            };
            let chars: Vec<char> = s.chars().collect();
            if start >= chars.len() {
                return Ok(EvaluatedValue::String(String::new()));
            }
            let length = len_val.and_then(|v| match v {
                EvaluatedValue::Int(i) => Some(i.max(0) as usize),
                _ => None,
            });
            let substr: String = match length {
                Some(len) => chars.into_iter().skip(start).take(len).collect(),
                None => chars.into_iter().skip(start).collect(),
            };
            Ok(EvaluatedValue::String(substr))
        }
        "strftime" => {
            if args.len() != 2 {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "strftime(format, timestamp) takes exactly 2 arguments",
                    false,
                ));
            }
            let fmt_val = eval_node(&args[0], doc)?;
            let ts_val = eval_node(&args[1], doc)?;
            let EvaluatedValue::String(fmt) = fmt_val else {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "strftime format must be a string",
                    false,
                ));
            };
            format_strftime(&fmt, &ts_val)
        }
        other => Err(LiliaError::new(
            ErrorCode::InvalidInput,
            format!("unsupported function in index expression: '{other}'"),
            false,
        )),
    }
}

/// Fast native Gregorian calendar time decomposition for `strftime` without external crates.
#[derive(Debug, Clone, Copy)]
struct DateTimeParts {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    min: u32,
    sec: u32,
    day_of_year: u32,
    day_of_week: u32, // 0 = Sunday, 1 = Monday, ...
}

impl DateTimeParts {
    fn from_epoch_seconds(mut secs: i64) -> Self {
        if secs < 0 {
            secs = 0;
        }
        let sec = (secs % 60) as u32;
        let mins = secs / 60;
        let min = (mins % 60) as u32;
        let hours = mins / 60;
        let hour = (hours % 24) as u32;
        let days = hours / 24;

        // Day of week (1970-01-01 was Thursday = 4)
        let day_of_week = ((days + 4) % 7) as u32;

        // Gregorian calendar algorithm (Civil from Days, Howard Hinnant algorithm)
        let z = days + 719468;
        let era = if z >= 0 { z } else { z - 146096 } / 146097;
        let doe = (z - era * 146097) as u32;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe as i32 + (era * 400) as i32;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = if m <= 2 { y + 1 } else { y };

        Self {
            year,
            month: m,
            day: d,
            hour,
            min,
            sec,
            day_of_year: doy + 1,
            day_of_week,
        }
    }
}

fn parse_iso8601_parts(s: &str) -> Option<DateTimeParts> {
    // Supports formats like "2026-11-13T01:31:34Z", "2026-11-13 01:31:34", "2026-11-13"
    let clean = s.trim();
    if clean.len() < 10 {
        return None;
    }
    let year: i32 = clean.get(0..4)?.parse().ok()?;
    let month: u32 = clean.get(5..7)?.parse().ok()?;
    let day: u32 = clean.get(8..10)?.parse().ok()?;

    let (hour, min, sec) = if clean.len() >= 19 {
        let h: u32 = clean.get(11..13)?.parse().ok()?;
        let m: u32 = clean.get(14..16)?.parse().ok()?;
        let sc: u32 = clean.get(17..19)?.parse().ok()?;
        (h, m, sc)
    } else {
        (0, 0, 0)
    };

    Some(DateTimeParts {
        year,
        month,
        day,
        hour,
        min,
        sec,
        day_of_year: day, // simplified
        day_of_week: 0,
    })
}

fn format_strftime(format: &str, ts_val: &EvaluatedValue) -> Result<EvaluatedValue> {
    let parts = match ts_val {
        EvaluatedValue::Int(i) => {
            let secs = if *i > 100_000_000_000 {
                // Milliseconds timestamp
                *i / 1000
            } else {
                *i
            };
            DateTimeParts::from_epoch_seconds(secs)
        }
        EvaluatedValue::Float(f) => {
            let secs = if *f > 100_000_000_000.0 {
                (*f / 1000.0) as i64
            } else {
                *f as i64
            };
            DateTimeParts::from_epoch_seconds(secs)
        }
        EvaluatedValue::String(s) => {
            if let Ok(i) = s.parse::<i64>() {
                let secs = if i > 100_000_000_000 { i / 1000 } else { i };
                DateTimeParts::from_epoch_seconds(secs)
            } else if let Some(p) = parse_iso8601_parts(s) {
                p
            } else {
                return Ok(EvaluatedValue::Null);
            }
        }
        _ => return Ok(EvaluatedValue::Null),
    };

    let mut out = String::new();
    let chars: Vec<char> = format.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%' && i + 1 < chars.len() {
            i += 1;
            match chars[i] {
                'Y' => out.push_str(&format!("{:04}", parts.year)),
                'm' => out.push_str(&format!("{:02}", parts.month)),
                'd' => out.push_str(&format!("{:02}", parts.day)),
                'H' => out.push_str(&format!("{:02}", parts.hour)),
                'M' => out.push_str(&format!("{:02}", parts.min)),
                'S' => out.push_str(&format!("{:02}", parts.sec)),
                'F' => out.push_str(&format!(
                    "{:04}-{:02}-{:02}",
                    parts.year, parts.month, parts.day
                )),
                'j' => out.push_str(&format!("{:03}", parts.day_of_year)),
                'w' => out.push_str(&format!("{}", parts.day_of_week)),
                '%' => out.push('%'),
                other => {
                    out.push('%');
                    out.push(other);
                }
            }
        } else {
            out.push(chars[i]);
        }
        i += 1;
    }

    Ok(EvaluatedValue::String(out))
}
