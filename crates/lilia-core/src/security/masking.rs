use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::context::SecurityContext;

/// Masking transformation applied to sensitive fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MaskFunction {
    /// Masks credit card numbers under the format `****-****-****-1234`, preserving the last 4 digits.
    CreditCard,
    /// Masks email addresses preserving the first and last local characters and domain (e.g. `a***e@example.com`).
    Email,
    /// Completely replaces the value with `[REDACTED]`.
    #[default]
    Redact,
    /// Retains a specified number of leading and trailing characters, replacing the middle with `mask_char`.
    Partial {
        prefix_keep: usize,
        suffix_keep: usize,
        #[serde(default = "default_mask_char")]
        mask_char: char,
    },
    /// Replaces the value with a 16-hex deterministic SHA-256 hash digest.
    Hash,
}

const fn default_mask_char() -> char {
    '*'
}

/// A declarative Dynamic Data Masking rule for a specific table/collection and field path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaskingRule {
    /// Target table, collection, or space.
    pub table: String,
    /// Field name or JSON path (e.g. `"$.credit_card"`, `"ssn"`, `"/account/number"`).
    pub path: String,
    /// Masking algorithm to apply.
    pub mask: MaskFunction,
    /// Optional exemption condition expression (e.g. `"current_context('role') == 'admin'"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exempt_condition: Option<String>,
}

impl MaskingRule {
    /// Create a new masking rule.
    #[must_use]
    pub fn new(
        table: impl Into<String>,
        path: impl Into<String>,
        mask: MaskFunction,
        exempt_condition: Option<String>,
    ) -> Self {
        Self {
            table: table.into(),
            path: path.into(),
            mask,
            exempt_condition,
        }
    }

    /// Checks if a candidate path matches this rule (supports bare name, `$.path`, or `/path`).
    #[must_use]
    pub fn matches_path(&self, candidate: &str) -> bool {
        let norm_rule = self.path.trim_start_matches("$.").trim_start_matches('/');
        let norm_cand = candidate.trim_start_matches("$.").trim_start_matches('/');
        norm_rule == norm_cand
    }
}

/// Masks a string using credit card format `****-****-****-1234`.
#[must_use]
pub fn mask_credit_card(input: &str) -> String {
    let digits: String = input.chars().filter(char::is_ascii_digit).collect();
    if digits.len() >= 4 {
        let last4 = &digits[digits.len() - 4..];
        format!("****-****-****-{last4}")
    } else if input.chars().count() >= 4 {
        let s: String = input.chars().collect();
        let last4: String = s
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("****-****-****-{last4}")
    } else {
        "****-****-****-****".to_string()
    }
}

/// Masks an email address (e.g. `alice@example.com` -> `a***e@example.com`).
#[must_use]
pub fn mask_email(input: &str) -> String {
    let Some((local, domain)) = input.split_once('@') else {
        return "[REDACTED]".to_string();
    };

    let chars: Vec<char> = local.chars().collect();
    let masked_local = if chars.len() <= 2 {
        "***".to_string()
    } else {
        let first = chars[0];
        let last = chars[chars.len() - 1];
        format!("{first}***{last}")
    };

    format!("{masked_local}@{domain}")
}

/// Masks a string using the specified [`MaskFunction`].
#[must_use]
pub fn mask_string(input: &str, mask_fn: &MaskFunction) -> String {
    match mask_fn {
        MaskFunction::CreditCard => mask_credit_card(input),
        MaskFunction::Email => mask_email(input),
        MaskFunction::Redact => "[REDACTED]".to_string(),
        MaskFunction::Partial {
            prefix_keep,
            suffix_keep,
            mask_char,
        } => {
            let chars: Vec<char> = input.chars().collect();
            let total = chars.len();
            if total <= prefix_keep + suffix_keep {
                std::iter::repeat_n(*mask_char, total).collect()
            } else {
                let prefix: String = chars[..*prefix_keep].iter().collect();
                let suffix: String = chars[total - suffix_keep..].iter().collect();
                let repeat_count = total - prefix_keep - suffix_keep;
                let middle: String = std::iter::repeat_n(*mask_char, repeat_count).collect();
                format!("{prefix}{middle}{suffix}")
            }
        }
        MaskFunction::Hash => {
            let mut hasher = Sha256::new();
            hasher.update(input.as_bytes());
            let result = hasher.finalize();
            hex::encode(&result[..8])
        }
    }
}

/// Masks a JSON value based on [`MaskFunction`].
#[must_use]
pub fn mask_value(val: &Value, mask_fn: &MaskFunction) -> Value {
    match val {
        Value::String(s) => Value::String(mask_string(s, mask_fn)),
        Value::Number(n) => Value::String(mask_string(&n.to_string(), mask_fn)),
        Value::Bool(_) | Value::Null => Value::String("[REDACTED]".to_string()),
        Value::Array(arr) => {
            Value::Array(arr.iter().map(|item| mask_value(item, mask_fn)).collect())
        }
        Value::Object(_) => Value::String("[REDACTED]".to_string()),
    }
}

/// Applies matching masking rules to a JSON document in place, unless exempt under the given context.
pub fn mask_document(
    doc: &mut Value,
    rules: &[MaskingRule],
    ctx: Option<&SecurityContext>,
    is_exempt_fn: impl Fn(&str, Option<&SecurityContext>) -> bool,
) {
    if rules.is_empty() {
        return;
    }

    for rule in rules {
        if let Some(ref exempt_expr) = rule.exempt_condition {
            if is_exempt_fn(exempt_expr, ctx) {
                // Caller is exempt from this masking rule
                continue;
            }
        }

        let clean_path = rule.path.trim_start_matches("$.").trim_start_matches('/');
        apply_mask_to_path(doc, clean_path, &rule.mask);
    }
}

fn apply_mask_to_path(doc: &mut Value, path: &str, mask_fn: &MaskFunction) {
    if path.is_empty() {
        *doc = mask_value(doc, mask_fn);
        return;
    }

    let segments: Vec<&str> = path.split('.').collect();
    apply_recursive(doc, &segments, mask_fn);
}

fn apply_recursive(current: &mut Value, segments: &[&str], mask_fn: &MaskFunction) {
    if segments.is_empty() {
        return;
    }

    let head = segments[0];
    let tail = &segments[1..];

    if tail.is_empty() {
        if let Value::Object(ref mut map) = current {
            if let Some(target) = map.get_mut(head) {
                *target = mask_value(target, mask_fn);
            }
        }
    } else if let Value::Object(ref mut map) = current {
        if let Some(next) = map.get_mut(head) {
            apply_recursive(next, tail, mask_fn);
        }
    }
}
