//! Indexing, versions, undo/redo revisions, and query result models.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(
    Debug,
    Clone,
    PartialEq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct SqlQueryResult {
    pub columns: Vec<String>,
    #[rkyv(with = rkyv::with::Map<rkyv::with::Map<crate::zero_copy::AsJsonString>>)]
    pub rows: Vec<Vec<Value>>,
    pub rows_affected: u64,
}

impl SqlQueryResult {
    #[must_use]
    pub fn scalar_f64(&self) -> Option<f64> {
        self.rows.first()?.first()?.as_f64()
    }

    #[must_use]
    pub fn scalar_i64(&self) -> Option<i64> {
        self.rows.first()?.first()?.as_i64()
    }

    #[must_use]
    pub fn scalar_str(&self) -> Option<&str> {
        self.rows.first()?.first()?.as_str()
    }

    #[must_use]
    pub fn first_row(&self) -> Option<&[Value]> {
        self.rows.first().map(Vec::as_slice)
    }
}

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq))]
pub struct JsonIndexInfo {
    pub space: String,
    pub index_name: String,
    #[serde(alias = "expression")]
    pub json_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

impl JsonIndexInfo {
    #[must_use]
    pub fn expression(&self) -> &str {
        &self.json_path
    }

    #[must_use]
    pub fn is_partial(&self) -> bool {
        self.filter.is_some()
    }
}

/// Options for configuring a JSON secondary index (e.g. partial index filter predicate).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

impl IndexOptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn filter(mut self, predicate: impl Into<String>) -> Self {
        self.filter = Some(predicate.into());
        self
    }
}

impl From<&str> for IndexOptions {
    fn from(filter_predicate: &str) -> Self {
        Self::new().filter(filter_predicate)
    }
}

impl From<String> for IndexOptions {
    fn from(filter_predicate: String) -> Self {
        Self::new().filter(filter_predicate)
    }
}

impl From<Option<String>> for IndexOptions {
    fn from(filter: Option<String>) -> Self {
        Self { filter }
    }
}

/// 64-bit strictly monotonic logical version.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash))]
#[repr(transparent)]
pub struct Version(pub u64);

impl Version {
    #[must_use]
    pub const fn new(v: u64) -> Self {
        Self(v)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for Version {
    fn from(v: u64) -> Self {
        Self(v)
    }
}

impl From<Version> for u64 {
    fn from(v: Version) -> Self {
        v.0
    }
}

impl std::ops::Deref for Version {
    type Target = u64;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
