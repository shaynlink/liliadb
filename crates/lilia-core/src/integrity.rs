//! Declarative cross-model integrity contracts.
//!
//! Contracts are declared once, persisted in the catalog and enforced by the storage engine inside
//! the same transaction as the write that could break them.

use serde::{Deserialize, Serialize};

use crate::{ErrorCode, LiliaError, Result};

/// Options accepted when creating a JSON collection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionOptions {
    /// Declarative `CHECK` expression every document must satisfy (SQL three-valued logic),
    /// e.g. `$.balance >= 0.0 AND length($.iban) == 27`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    /// Embeds a text field of every document into the vector collection of the same name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_embed: Option<crate::embed::AutoEmbedConfig>,
}

/// Returns `true` when two document paths (`$.a.b`, `a.b`, `/a/b`) are equal or nested.
#[must_use]
pub fn paths_overlap(left: &str, right: &str) -> bool {
    fn segments(path: &str) -> Vec<&str> {
        path.trim_start_matches('$')
            .split(['.', '/'])
            .filter(|segment| !segment.is_empty())
            .collect()
    }
    let (a, b) = (segments(left), segments(right));
    a.iter().zip(&b).all(|(x, y)| x == y)
}

/// Action applied to child rows when the referenced parent JSON document is deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefAction {
    /// Delete the children (graph node and its incident edges, or KV entries).
    Cascade,
    /// Reject the deletion while children exist.
    Restrict,
    /// Detach the children by clearing their link (graph nodes only).
    SetNull,
}

impl RefAction {
    /// Canonical lowercase identifier used for catalog storage.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cascade => "cascade",
            Self::Restrict => "restrict",
            Self::SetNull => "set_null",
        }
    }

    /// Parses the identifier produced by [`RefAction::as_str`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cascade" => Some(Self::Cascade),
            "restrict" => Some(Self::Restrict),
            "set_null" => Some(Self::SetNull),
            _ => None,
        }
    }
}

/// How a KV key designates its parent JSON document id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyMapping {
    /// The whole key (UTF-8) is the parent document id.
    Whole,
    /// The parent id is the key prefix before the first `separator` (e.g. `user_42:cart`).
    Prefix { separator: char },
}

impl KeyMapping {
    /// Extracts the parent document id designated by `key`, if the key is well formed.
    #[must_use]
    pub fn parent_id(self, key: &[u8]) -> Option<&str> {
        let text = std::str::from_utf8(key).ok()?;
        match self {
            Self::Whole => Some(text),
            Self::Prefix { separator } => text
                .split_once(separator)
                .map(|(parent, _)| parent)
                .filter(|parent| !parent.is_empty()),
        }
    }

    /// Returns the `[lower, upper)` key range holding every child key of `parent`.
    #[must_use]
    pub fn child_range(self, parent: &str) -> (Vec<u8>, Vec<u8>) {
        let mut lower = parent.as_bytes().to_vec();
        match self {
            Self::Whole => {
                let mut upper = lower.clone();
                upper.push(0);
                (lower, upper)
            }
            Self::Prefix { separator } => {
                let mut buf = [0_u8; 4];
                lower.extend_from_slice(separator.encode_utf8(&mut buf).as_bytes());
                let mut upper = lower.clone();
                // UTF-8 never contains 0xFF, so incrementing the last byte cannot overflow.
                if let Some(last) = upper.last_mut() {
                    *last += 1;
                }
                (lower, upper)
            }
        }
    }
}

/// The child side of a reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "model", rename_all = "snake_case")]
pub enum ReferenceSource {
    /// Nodes of `graph` linked to a JSON document through `doc_space` / `doc_id`.
    GraphNodes { graph: String },
    /// Entries of the KV `namespace`, whose key designates the parent document.
    Kv { namespace: String },
}

impl ReferenceSource {
    /// Model identifier (`"graph"` or `"kv"`).
    #[must_use]
    pub const fn model(&self) -> &'static str {
        match self {
            Self::GraphNodes { .. } => "graph",
            Self::Kv { .. } => "kv",
        }
    }

    /// Name of the graph or KV namespace holding the children.
    #[must_use]
    pub fn collection(&self) -> &str {
        match self {
            Self::GraphNodes { graph } => graph,
            Self::Kv { namespace } => namespace,
        }
    }
}

/// A cross-model foreign key: children in a graph or KV namespace point to a JSON document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reference {
    pub name: String,
    pub source: ReferenceSource,
    /// JSON space holding the parent documents.
    pub target_space: String,
    pub on_delete: RefAction,
    pub key: KeyMapping,
    /// Graph only: every node of the graph must be linked to a parent document.
    #[serde(default)]
    pub required: bool,
}

impl Reference {
    /// Declares that nodes of `graph` link to documents of the JSON space `target_space`.
    /// Deleting the document deletes the node and its edges unless overridden by [`Self::on_delete`].
    #[must_use]
    pub fn graph_nodes(graph: impl Into<String>, target_space: impl Into<String>) -> Self {
        let graph = graph.into();
        let target_space = target_space.into();
        Self {
            name: format!("fk_graph_{graph}_{target_space}"),
            source: ReferenceSource::GraphNodes { graph },
            target_space,
            on_delete: RefAction::Cascade,
            key: KeyMapping::Whole,
            required: false,
        }
    }

    /// Declares that keys of the KV `namespace` designate an existing document of `target_space`.
    /// Deleting the document is rejected while keys remain unless overridden by [`Self::on_delete`].
    #[must_use]
    pub fn kv(namespace: impl Into<String>, target_space: impl Into<String>) -> Self {
        let namespace = namespace.into();
        let target_space = target_space.into();
        Self {
            name: format!("fk_kv_{namespace}_{target_space}"),
            source: ReferenceSource::Kv { namespace },
            target_space,
            on_delete: RefAction::Restrict,
            key: KeyMapping::Whole,
            required: false,
        }
    }

    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    #[must_use]
    pub const fn on_delete(mut self, action: RefAction) -> Self {
        self.on_delete = action;
        self
    }

    #[must_use]
    pub const fn key(mut self, key: KeyMapping) -> Self {
        self.key = key;
        self
    }

    #[must_use]
    pub const fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }

    /// Checks that the declaration is internally consistent.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for an empty name or an incompatible combination
    /// (`SetNull` on KV children, or on a `required` graph link).
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "reference name cannot be empty",
                false,
            ));
        }
        match (&self.source, self.on_delete) {
            (ReferenceSource::Kv { .. }, RefAction::SetNull) => Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "SET NULL is not applicable to KV references",
                false,
            )),
            (ReferenceSource::GraphNodes { .. }, RefAction::SetNull) if self.required => {
                Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "a required graph reference cannot use SET NULL",
                    false,
                ))
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_mapping_extracts_parent_and_bounds_children() {
        let mapping = KeyMapping::Prefix { separator: ':' };
        assert_eq!(mapping.parent_id(b"user_42:cart"), Some("user_42"));
        assert_eq!(mapping.parent_id(b"nosep"), None);
        assert_eq!(mapping.parent_id(b":x"), None);
        let (lower, upper) = mapping.child_range("user_42");
        assert_eq!(lower, b"user_42:");
        assert_eq!(upper, b"user_42;");
        assert!(b"user_42:cart".as_slice() >= lower.as_slice());
        assert!(b"user_42:cart".as_slice() < upper.as_slice());
        let sibling = b"user_420:cart".as_slice();
        assert!(sibling < lower.as_slice() || sibling >= upper.as_slice());
    }

    #[test]
    fn nested_paths_overlap() {
        assert!(paths_overlap("$.profile.ssn", "profile"));
        assert!(paths_overlap("/profile/ssn", "$.profile.ssn"));
        assert!(!paths_overlap("$.balance", "$.iban"));
    }

    #[test]
    fn whole_mapping_range_holds_exactly_the_parent_key() {
        let (lower, upper) = KeyMapping::Whole.child_range("a");
        assert_eq!(lower, b"a");
        assert_eq!(upper, b"a\0");
    }

    #[test]
    fn invalid_combinations_are_rejected() {
        let kv = Reference::kv("s", "users").on_delete(RefAction::SetNull);
        assert_eq!(kv.validate().unwrap_err().code, ErrorCode::InvalidInput);
        let graph = Reference::graph_nodes("g", "users")
            .on_delete(RefAction::SetNull)
            .required(true);
        assert_eq!(graph.validate().unwrap_err().code, ErrorCode::InvalidInput);
        assert!(Reference::graph_nodes("g", "users").validate().is_ok());
    }
}
