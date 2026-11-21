//! Equality queries over JSON documents, and the choice of a secondary index to serve them.
//!
//! A query maps keys to expected values; a document matches when every term matches:
//!
//! - a key naming a ready index of the space matches when the index's values for the document
//!   include the expected value;
//! - a path (`status`, `$.user.city`, `/user/city`) matches when the value at the path equals the
//!   expected value, or is an array that contains it; a missing path matches `null`;
//! - any other expression (`lower($.email)`) matches when its value equals the expected value.
//!
//! An index serves a query when it is complete for one of its terms: every document matching
//! the term has an entry under the probed value. Candidates read from an index are still tested
//! against the whole query, after the caller's guard, so an index only narrows the search.

use std::collections::BTreeSet;

use lilia_core::{eval_expr, eval_filter_predicate, referenced_paths, BinaryDoc, Expr};
use serde_json::{Map, Value};

use crate::catalog::{Index, Schema};

#[derive(Debug, Clone)]
enum Matcher {
    Path(String),
    Expression(Expr),
    Index(Index),
}

#[derive(Debug, Clone)]
struct Term {
    key: String,
    matcher: Matcher,
    expected: Value,
}

/// A compiled query.
#[derive(Debug, Clone)]
pub struct Query {
    terms: Vec<Term>,
}

/// An index lookup that returns a superset of the documents matching a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub index: u64,
    pub value: String,
}

fn normalize(path: &str) -> &str {
    path.trim_start_matches("$.").trim_start_matches('/')
}

/// The index value of `expected` through `template`, an expression over the path `v`: what an
/// index with that expression stores for a document whose field holds `expected`. `None` unless
/// it is exactly one value.
fn index_value(template: &Expr, expected: &Value) -> Option<String> {
    let mut wrapper = Map::new();
    wrapper.insert("v".to_owned(), expected.clone());
    let doc = BinaryDoc::from_json_value(&Value::Object(wrapper)).ok()?;
    let mut values = eval_expr(template, &doc).ok()?.to_index_strings();
    (values.len() == 1).then(|| values.remove(0))
}

fn scalar(value: &Value) -> bool {
    !matches!(value, Value::Array(_) | Value::Object(_))
}

fn insert_nested(root: &mut Map<String, Value>, path: &str, value: Value) {
    let mut current = root;
    let mut segments = path.split('.').peekable();
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            current.insert(segment.to_owned(), value);
            return;
        }
        let next = current
            .entry(segment.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if !next.is_object() {
            *next = Value::Object(Map::new());
        }
        let Value::Object(map) = next else { return };
        current = map;
    }
}

impl Query {
    /// Compiles `query` for a space with `schema`.
    #[must_use]
    pub fn new(query: &Map<String, Value>, schema: Option<&Schema>) -> Self {
        let terms = query
            .iter()
            .map(|(key, expected)| {
                let index = schema.and_then(|s| s.index(key)).filter(|i| i.ready);
                let matcher = match (index, lilia_core::parse_expr(key)) {
                    (Some(index), _) => Matcher::Index(index.clone()),
                    (None, Ok(Expr::Path(_)) | Err(_)) => Matcher::Path(key.clone()),
                    (None, Ok(expression)) => Matcher::Expression(expression),
                };
                Term {
                    key: key.clone(),
                    matcher,
                    expected: expected.clone(),
                }
            })
            .collect();
        Self { terms }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    #[must_use]
    pub fn matches(&self, doc: &BinaryDoc) -> bool {
        self.terms.iter().all(|term| term.matches(doc))
    }

    /// The first index of `schema` that is complete for a term of the query.
    #[must_use]
    pub fn probe(&self, schema: &Schema) -> Option<Probe> {
        let fixed: BTreeSet<&str> = self
            .terms
            .iter()
            .filter(|t| matches!(t.matcher, Matcher::Path(_)))
            .map(|t| normalize(&t.key))
            .collect();
        let query_doc = self.fixed_document();
        for term in &self.terms {
            if let Matcher::Index(index) = &term.matcher {
                if let Some(value) = index_value(&Expr::Path("v".to_owned()), &term.expected) {
                    return Some(Probe {
                        index: index.id,
                        value,
                    });
                }
                continue;
            }
            if !scalar(&term.expected) {
                continue;
            }
            for index in schema.indexes.iter().filter(|i| i.ready) {
                if !Self::filter_holds(index, &fixed, query_doc.as_ref()) {
                    continue;
                }
                if let Some(value) = term.probe_value(index) {
                    return Some(Probe {
                        index: index.id,
                        value,
                    });
                }
            }
        }
        None
    }

    /// The document the path terms fix, for evaluating partial-index filters.
    fn fixed_document(&self) -> Option<BinaryDoc> {
        let mut root = Map::new();
        for term in self
            .terms
            .iter()
            .filter(|t| matches!(t.matcher, Matcher::Path(_)))
        {
            insert_nested(&mut root, normalize(&term.key), term.expected.clone());
        }
        BinaryDoc::from_json_value(&Value::Object(root)).ok()
    }

    /// A partial index is complete for the query only if its filter depends on fields the query
    /// fixes, and holds for their values.
    fn filter_holds(index: &Index, fixed: &BTreeSet<&str>, query_doc: Option<&BinaryDoc>) -> bool {
        let Some(filter) = &index.filter else {
            return true;
        };
        let Some(query_doc) = query_doc else {
            return false;
        };
        referenced_paths(filter)
            .iter()
            .all(|p| fixed.contains(normalize(p)))
            && eval_filter_predicate(filter, query_doc)
    }
}

impl Term {
    fn matches(&self, doc: &BinaryDoc) -> bool {
        match &self.matcher {
            Matcher::Path(path) => match doc.extract(path) {
                Some(found) => {
                    found.as_array().is_some_and(|items| {
                        items
                            .iter()
                            .any(|item| item.to_json_value() == self.expected)
                    }) || found.to_json_value() == self.expected
                }
                None => self.expected.is_null(),
            },
            Matcher::Expression(expression) => {
                let Some(expected) = index_value(&Expr::Path("v".to_owned()), &self.expected)
                else {
                    return false;
                };
                eval_expr(expression, doc)
                    .is_ok_and(|value| value.to_index_strings().contains(&expected))
            }
            Matcher::Index(index) => index_value(&Expr::Path("v".to_owned()), &self.expected)
                .is_some_and(|expected| index.values(doc).contains(&expected)),
        }
    }

    /// The value to look up in `index` for this term, when the index is complete for it.
    fn probe_value(&self, index: &Index) -> Option<String> {
        match (&self.matcher, &index.expr) {
            (Matcher::Path(key), Expr::Path(path)) if normalize(path) == normalize(key) => {
                index_value(&Expr::Path("v".to_owned()), &self.expected)
            }
            (Matcher::Path(key), Expr::FunctionCall { name, args }) => match args.as_slice() {
                [Expr::Path(path)] if normalize(path) == normalize(key) => index_value(
                    &Expr::FunctionCall {
                        name: name.clone(),
                        args: vec![Expr::Path("v".to_owned())],
                    },
                    &self.expected,
                ),
                _ => None,
            },
            (Matcher::Expression(expression), indexed) if expression == indexed => {
                index_value(&Expr::Path("v".to_owned()), &self.expected)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn doc(value: &Value) -> BinaryDoc {
        BinaryDoc::from_json_value(value).unwrap()
    }

    fn query(value: &Value, schema: Option<&Schema>) -> Query {
        Query::new(value.as_object().unwrap(), schema)
    }

    fn ready(id: u64, name: &str, expression: &str, filter: Option<&str>) -> Index {
        let mut index = Index::new(id, name, expression, filter).unwrap();
        index.ready = true;
        index
    }

    #[test]
    fn terms_match_paths_arrays_missing_fields_and_expressions() {
        let d = doc(
            &json!({"status": "active", "tags": ["a", "b"], "n": 5, "email": "Alice@X.io", "user": {"city": "Paris"}}),
        );
        assert!(query(&json!({"status": "active", "n": 5}), None).matches(&d));
        assert!(query(&json!({"tags": "b", "$.user.city": "Paris"}), None).matches(&d));
        assert!(query(&json!({"missing": null}), None).matches(&d));
        assert!(!query(&json!({"missing": "x"}), None).matches(&d));
        assert!(query(&json!({"lower($.email)": "alice@x.io"}), None).matches(&d));
        assert!(!query(&json!({"status": "inactive"}), None).matches(&d));
        assert!(query(&json!({}), None).matches(&d));
    }

    #[test]
    fn probes_pick_complete_indexes_only() {
        let schema = Schema {
            indexes: vec![
                ready(1, "by_email", "lower($.email)", None),
                ready(2, "by_status_live", "$.status", Some("$.deleted != true")),
                ready(3, "by_kind", "$.kind", Some("$.kind == 'a'")),
                Index::new(4, "building", "$.n", None).unwrap(),
            ],
            ..Schema::default()
        };
        let probe = |q: Value| query(&q, Some(&schema)).probe(&schema);
        assert_eq!(
            probe(json!({"email": "Alice@X.io"})),
            Some(Probe {
                index: 1,
                value: "alice@x.io".into()
            })
        );
        assert_eq!(
            probe(json!({"by_email": "bob@y.io"})),
            Some(Probe {
                index: 1,
                value: "bob@y.io".into()
            })
        );
        assert_eq!(
            probe(json!({"status": "on"})),
            None,
            "the filter depends on a field the query leaves free"
        );
        assert_eq!(
            probe(json!({"status": "on", "deleted": false})),
            Some(Probe {
                index: 2,
                value: "on".into()
            })
        );
        assert_eq!(
            probe(json!({"kind": "a"})),
            Some(Probe {
                index: 3,
                value: "a".into()
            })
        );
        assert_eq!(
            probe(json!({"kind": "b"})),
            None,
            "the filter excludes the queried value"
        );
        assert_eq!(
            probe(json!({"n": 1})),
            None,
            "indexes being built serve nothing"
        );
        assert_eq!(probe(json!({"email": ["x"]})), None);

        let by_name = query(&json!({"by_email": "alice@x.io"}), Some(&schema));
        assert!(by_name.matches(&doc(&json!({"email": "ALICE@x.io"}))));
        assert!(!by_name.matches(&doc(&json!({"email": "bob@x.io"}))));
    }
}
