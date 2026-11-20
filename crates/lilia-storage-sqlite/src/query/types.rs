//! Data structures, result types, and conversion traits for `LiliaQL`.

use lilia_core::{ErrorCode, LiliaError, Result, VectorElementType};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A single row returned by a `LiliaQL` query execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct QueryRow {
    pub(crate) fields: Map<String, Value>,
}

impl QueryRow {
    /// Creates a new query row from a JSON map.
    #[must_use]
    pub fn new(fields: Map<String, Value>) -> Self {
        Self { fields }
    }

    /// Access a field value by its projected name.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fields.get(key)
    }

    /// Extract a string field if present and of type string.
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.fields.get(key).and_then(Value::as_str)
    }

    /// Extract a floating-point numeric field if present.
    #[must_use]
    pub fn get_f64(&self, key: &str) -> Option<f64> {
        self.fields.get(key).and_then(Value::as_f64)
    }

    /// Extract an integer numeric field if present.
    #[must_use]
    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.fields.get(key).and_then(Value::as_i64)
    }

    /// Extract a boolean field if present.
    #[must_use]
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.fields.get(key).and_then(Value::as_bool)
    }

    /// Returns the underlying fields as a JSON object `Value`.
    #[must_use]
    pub fn to_json(&self) -> Value {
        Value::Object(self.fields.clone())
    }

    /// Returns the underlying JSON field map.
    #[must_use]
    pub fn into_inner(self) -> Map<String, Value> {
        self.fields
    }

    /// Deserializes this row into a typed Rust struct.
    ///
    /// # Errors
    /// Returns [`LiliaError`] if deserialization fails.
    pub fn deserialize<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_value(self.to_json()).map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("failed to deserialize QueryRow: {e}"),
                false,
            )
        })
    }
}

/// The collection of rows returned by a `LiliaQL` query execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryResult {
    pub(crate) rows: Vec<QueryRow>,
}

impl QueryResult {
    /// Wrap rows in a `QueryResult`.
    #[must_use]
    pub fn new(rows: Vec<QueryRow>) -> Self {
        Self { rows }
    }

    /// Number of returned rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// True if no rows were returned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// An iterator over references to rows.
    pub fn iter(&self) -> std::slice::Iter<'_, QueryRow> {
        self.rows.iter()
    }

    /// Converts the result set into a `Vec<QueryRow>`.
    #[must_use]
    pub fn into_rows(self) -> Vec<QueryRow> {
        self.rows
    }

    /// Serializes all rows into a JSON array of objects.
    #[must_use]
    pub fn to_json(&self) -> Value {
        Value::Array(self.rows.iter().map(QueryRow::to_json).collect())
    }

    /// Deserializes all rows into a typed `Vec<T>`.
    ///
    /// # Errors
    /// Returns [`LiliaError`] if deserialization of any row fails.
    pub fn deserialize<T: DeserializeOwned>(&self) -> Result<Vec<T>> {
        serde_json::from_value(self.to_json()).map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("failed to deserialize QueryResult: {e}"),
                false,
            )
        })
    }
}

impl IntoIterator for QueryResult {
    type Item = QueryRow;
    type IntoIter = std::vec::IntoIter<QueryRow>;

    fn into_iter(self) -> Self::IntoIter {
        self.rows.into_iter()
    }
}

impl<'a> IntoIterator for &'a QueryResult {
    type Item = &'a QueryRow;
    type IntoIter = std::slice::Iter<'a, QueryRow>;

    fn into_iter(self) -> Self::IntoIter {
        self.rows.iter()
    }
}

/// Configuration for Reciprocal Rank Fusion (RRF).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RrfConfig {
    /// Smoothing constant (default: 60).
    pub k: u32,
    /// Weight applied to the FTS BM25 rank score (default: 1.0).
    pub fts_weight: f64,
    /// Weight applied to the dense vector rank score (default: 1.0).
    pub vector_weight: f64,
}

impl Default for RrfConfig {
    fn default() -> Self {
        Self {
            k: 60,
            fts_weight: 1.0,
            vector_weight: 1.0,
        }
    }
}

/// Information describing the physical query execution plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryPlanExplanation {
    pub stages: Vec<String>,
    pub index_hint: Option<String>,
    pub vector_prefilter: bool,
    pub rrf_active: bool,
}

/// Trait to facilitate passing different vector representations into `.with_vector_near(...)`.
pub trait IntoVectorBytes {
    /// Converts `self` into `(raw_bytes, dimension, element_type)`.
    fn into_vector_query(self) -> (Vec<u8>, u32, VectorElementType);
}

impl IntoVectorBytes for &[f32] {
    fn into_vector_query(self) -> (Vec<u8>, u32, VectorElementType) {
        let dim = u32::try_from(self.len()).unwrap_or(0);
        let mut bytes = Vec::with_capacity(self.len() * 4);
        for f in self {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
        (bytes, dim, VectorElementType::Float32)
    }
}

impl IntoVectorBytes for &Vec<f32> {
    fn into_vector_query(self) -> (Vec<u8>, u32, VectorElementType) {
        self.as_slice().into_vector_query()
    }
}

impl IntoVectorBytes for Vec<f32> {
    fn into_vector_query(self) -> (Vec<u8>, u32, VectorElementType) {
        self.as_slice().into_vector_query()
    }
}

impl<const N: usize> IntoVectorBytes for [f32; N] {
    fn into_vector_query(self) -> (Vec<u8>, u32, VectorElementType) {
        self.as_slice().into_vector_query()
    }
}

impl<const N: usize> IntoVectorBytes for &[f32; N] {
    fn into_vector_query(self) -> (Vec<u8>, u32, VectorElementType) {
        self.as_slice().into_vector_query()
    }
}

impl IntoVectorBytes for (&[u8], u32, VectorElementType) {
    fn into_vector_query(self) -> (Vec<u8>, u32, VectorElementType) {
        (self.0.to_vec(), self.1, self.2)
    }
}

impl IntoVectorBytes for (Vec<u8>, u32, VectorElementType) {
    fn into_vector_query(self) -> (Vec<u8>, u32, VectorElementType) {
        (self.0, self.1, self.2)
    }
}
