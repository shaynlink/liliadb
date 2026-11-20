//! Causal relation detection between HLC events.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::timestamp::Hlc;

/// The causal relationship between two mutations on the same key/document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq))]
#[serde(rename_all = "snake_case")]
pub enum CausalRelation {
    /// A happened before B (A -> B): B causally replaces A without conflict.
    Ancestor,
    /// B happened before A (B -> A): A causally replaces B without conflict.
    Descendant,
    /// Identical mutation (idempotent duplicate).
    Identical,
    /// Concurrent modifications (A || B): neither mutation observed the other!
    Concurrent,
}

impl fmt::Display for CausalRelation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ancestor => write!(f, "ancestor (sequential)"),
            Self::Descendant => write!(f, "descendant (sequential)"),
            Self::Identical => write!(f, "identical"),
            Self::Concurrent => write!(f, "concurrent (A ∥ B)"),
        }
    }
}

/// Evaluates the causal relationship between two events A and B.
///
/// `a_hlc`: Timestamp of A.
/// `a_parent`: Parent timestamp that A observed before writing.
/// `b_hlc`: Timestamp of B.
/// `b_parent`: Parent timestamp that B observed before writing.
#[must_use]
pub fn detect_causality(
    a_hlc: &Hlc,
    a_parent: Option<&Hlc>,
    b_hlc: &Hlc,
    b_parent: Option<&Hlc>,
) -> CausalRelation {
    if a_hlc == b_hlc {
        return CausalRelation::Identical;
    }

    // Did B observe A? (A -> B)
    if b_parent == Some(a_hlc) {
        return CausalRelation::Ancestor;
    }

    // Did A observe B? (B -> A)
    if a_parent == Some(b_hlc) {
        return CausalRelation::Descendant;
    }

    // Neither directly observed the other: concurrent writes!
    CausalRelation::Concurrent
}
