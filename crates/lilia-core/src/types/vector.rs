//! Vector element types, distance metrics, and HNSW index configurations.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq))]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum VectorElementType {
    Float32 = 0,
    Int8 = 1,
    Bit = 2,
}

impl VectorElementType {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Float32 => "float32",
            Self::Int8 => "int8",
            Self::Bit => "1bit",
        }
    }
}

impl TryFrom<i64> for VectorElementType {
    type Error = crate::LiliaError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Float32),
            1 => Ok(Self::Int8),
            2 => Ok(Self::Bit),
            _ => Err(crate::LiliaError::new(
                crate::ErrorCode::InvalidInput,
                format!("invalid vector element type: {value}"),
                false,
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq))]
#[serde(rename_all = "snake_case")]
pub enum VectorMetric {
    Cosine,
    L2,
    Dot,
    Hamming,
}

impl VectorMetric {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cosine => "cosine",
            Self::L2 => "l2",
            Self::Dot => "dot",
            Self::Hamming => "hamming",
        }
    }
}

impl std::str::FromStr for VectorMetric {
    type Err = crate::LiliaError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "cosine" => Ok(Self::Cosine),
            "l2" | "euclidean" => Ok(Self::L2),
            "dot" | "inner_product" => Ok(Self::Dot),
            "hamming" => Ok(Self::Hamming),
            _ => Err(crate::LiliaError::new(
                crate::ErrorCode::InvalidInput,
                format!("invalid vector metric: {s}"),
                false,
            )),
        }
    }
}

/// The parameters of an HNSW index; fields left out take their [`Default`] values.
#[derive(Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug))]
#[serde(default)]
pub struct HnswConfig {
    pub m: usize,
    pub ef_construction: usize,
    pub ef_search: usize,
    pub metric: VectorMetric,
    pub element_type: VectorElementType,
    pub dim: u32,
}

impl Default for HnswConfig {
    fn default() -> Self {
        Self {
            m: 16,
            ef_construction: 64,
            ef_search: 32,
            metric: VectorMetric::Cosine,
            element_type: VectorElementType::Float32,
            dim: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct VectorIndexInfo {
    pub collection: String,
    pub node_count: usize,
    pub max_level: usize,
    pub m: usize,
    pub ef_construction: usize,
    pub ef_search: usize,
    pub metric: VectorMetric,
    pub element_type: VectorElementType,
    pub dim: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct VectorHit {
    pub collection: String,
    pub id: String,
    pub distance: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct HybridHit {
    pub id: String,
    pub rrf_score: f64,
    pub fts_rank: Option<usize>,
    pub fts_snippet: Option<String>,
    pub vector_rank: Option<usize>,
    pub vector_distance: Option<f64>,
}
