use serde::{Deserialize, Serialize};

use crate::json::{parse_expr, Expr};

/// Maximum number of fields combined in one synthesized composite index.
pub const MAX_COMPOSITE_FIELDS: usize = 4;

const MAX_INDEX_NAME_LEN: usize = 128;
const HASH_SUFFIX_LEN: usize = 16;
const RESERVED_WORDS: [&str; 7] = ["and", "or", "not", "is", "true", "false", "null"];

/// Normalizes a query key into a field path (`$.a.b` and `/a/b` become `a.b`).
#[must_use]
pub fn normalize_field(key: &str) -> String {
    key.trim_start_matches("$.")
        .trim_start_matches('/')
        .replace('/', ".")
}

/// Whether a normalized field path can safely be embedded in a synthesized index expression.
#[must_use]
pub fn is_indexable_field(field: &str) -> bool {
    !field.is_empty()
        && field.split('.').all(|segment| {
            let mut chars = segment.chars();
            chars
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !RESERVED_WORDS
                    .iter()
                    .any(|word| word.eq_ignore_ascii_case(segment))
        })
}

/// The shape of a `find` query: the collection and the sorted set of fields it filters on.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct QueryFingerprint {
    pub space: String,
    pub fields: Vec<String>,
}

impl QueryFingerprint {
    /// Builds a fingerprint from raw query keys, normalizing, sorting and de-duplicating them so
    /// that `{store_id, status}` and `{status, store_id}` share one shape.
    #[must_use]
    pub fn new<'a>(space: &str, keys: impl IntoIterator<Item = &'a str>) -> Self {
        let mut fields: Vec<String> = keys.into_iter().map(normalize_field).collect();
        fields.sort();
        fields.dedup();
        Self {
            space: space.to_owned(),
            fields,
        }
    }
}

/// The index expression covering `fields` (already normalized and sorted).
///
/// One field yields a plain path, several yield `composite($.a, $.b)`.
#[must_use]
pub fn composite_expression(fields: &[String]) -> String {
    match fields {
        [single] => format!("$.{single}"),
        many => {
            let args: Vec<String> = many.iter().map(|field| format!("$.{field}")).collect();
            format!("composite({})", args.join(", "))
        }
    }
}

/// Extracts the sorted, normalized field set an index expression covers.
///
/// Only plain paths and `composite(path, ...)` expressions are recognized; transformed or
/// computed expressions return `None`.
#[must_use]
pub fn index_fields(expression: &str) -> Option<Vec<String>> {
    let mut fields = match parse_expr(expression).ok()? {
        Expr::Path(path) => vec![normalize_field(&path)],
        Expr::FunctionCall { name, args } if name.eq_ignore_ascii_case("composite") => args
            .into_iter()
            .map(|arg| match arg {
                Expr::Path(path) => Some(normalize_field(&path)),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?,
        _ => return None,
    };
    fields.sort();
    fields.dedup();
    Some(fields)
}

fn sanitize(part: &str) -> String {
    part.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

fn readable_name(space: &str, fields: &[String]) -> String {
    let joined: Vec<String> = fields.iter().map(|field| sanitize(field)).collect();
    format!("idx_{}_{}", sanitize(space), joined.join("_"))
}

/// The hash-suffixed variant of [`index_name`], used when the readable name is already taken.
#[must_use]
pub fn index_name_hashed(space: &str, fields: &[String]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(space.as_bytes());
    for field in fields {
        hasher.update(&[0x1f]);
        hasher.update(field.as_bytes());
    }
    let digest = hasher.finalize().to_hex();
    let readable = readable_name(space, fields);
    let keep = MAX_INDEX_NAME_LEN - HASH_SUFFIX_LEN - 1;
    let prefix: String = readable.chars().take(keep).collect();
    format!("{prefix}_{}", &digest.as_str()[..HASH_SUFFIX_LEN])
}

/// Deterministic name for the index covering `fields` of `space`, e.g.
/// `idx_orders_status_store_id`. Over-long names fall back to the hashed form.
#[must_use]
pub fn index_name(space: &str, fields: &[String]) -> String {
    let readable = readable_name(space, fields);
    if readable.len() <= MAX_INDEX_NAME_LEN {
        readable
    } else {
        index_name_hashed(space, fields)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::{eval_expr, BinaryDoc};
    use serde_json::json;

    #[test]
    fn fingerprint_is_order_and_prefix_insensitive() {
        let a = QueryFingerprint::new("orders", ["status", "store_id"]);
        let b = QueryFingerprint::new("orders", ["$.store_id", "/status", "status"]);
        assert_eq!(a, b);
        assert_eq!(a.fields, vec!["status", "store_id"]);
    }

    #[test]
    fn index_name_matches_the_documented_shape() {
        let fields = vec!["status".to_owned(), "store_id".to_owned()];
        assert_eq!(index_name("orders", &fields), "idx_orders_status_store_id");
    }

    #[test]
    fn long_names_are_hashed_and_bounded() {
        let fields = vec!["f".repeat(200)];
        let name = index_name("orders", &fields);
        assert!(name.len() <= MAX_INDEX_NAME_LEN);
        assert_eq!(name, index_name("orders", &fields));
        assert_ne!(name, index_name("orders", &["g".repeat(200)]));
    }

    #[test]
    fn hashed_name_differs_from_readable_name() {
        let fields = vec!["status".to_owned()];
        assert_ne!(index_name("orders", &fields), index_name_hashed("orders", &fields));
    }

    #[test]
    fn indexable_fields() {
        assert!(is_indexable_field("status"));
        assert!(is_indexable_field("address.city_2"));
        assert!(!is_indexable_field(""));
        assert!(!is_indexable_field("lower($.email)"));
        assert!(!is_indexable_field("a-b"));
        assert!(!is_indexable_field("null"));
        assert!(!is_indexable_field("a..b"));
        assert!(!is_indexable_field("1abc"));
    }

    #[test]
    fn expression_round_trips_through_index_fields() {
        let fields = vec!["status".to_owned(), "store_id".to_owned()];
        let expression = composite_expression(&fields);
        assert_eq!(expression, "composite($.status, $.store_id)");
        assert_eq!(index_fields(&expression), Some(fields));
        assert_eq!(
            index_fields(&composite_expression(&["email".to_owned()])),
            Some(vec!["email".to_owned()])
        );
        assert_eq!(index_fields("composite($.b, $.a)"), Some(vec!["a".to_owned(), "b".to_owned()]));
        assert_eq!(index_fields("lower($.email)"), None);
    }

    #[test]
    fn composite_keys_are_unambiguous() {
        let expr = parse_expr("composite($.a, $.b)").unwrap();
        let key = |doc: serde_json::Value| {
            eval_expr(&expr, &BinaryDoc::from_json_value(&doc)).unwrap()
        };
        assert_ne!(key(json!({"a": "x", "b": "yz"})), key(json!({"a": "xy", "b": "z"})));
        assert_eq!(key(json!({"a": "paid", "b": 4})), key(json!({"b": 4, "a": "paid"})));
    }
}
