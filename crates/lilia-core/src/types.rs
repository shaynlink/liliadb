use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Durability {
    #[default]
    Durable,
    Balanced,
    Performance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KvEntry {
    pub namespace: String,
    #[serde(with = "serde_bytes")]
    pub key: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub value: Vec<u8>,
    #[serde(with = "crate::version")]
    pub version: u64,
    pub expires_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonEntry {
    pub space: String,
    pub id: String,
    pub value: Value,
    #[serde(with = "crate::version")]
    pub version: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "model", rename_all = "snake_case")]
pub enum BatchOperation {
    KvSet {
        namespace: String,
        #[serde(with = "serde_bytes")]
        key: Vec<u8>,
        #[serde(with = "serde_bytes")]
        value: Vec<u8>,
        #[serde(default, with = "crate::version::optional")]
        if_version: Option<u64>,
        expires_at_ms: Option<i64>,
    },
    KvDelete {
        namespace: String,
        #[serde(with = "serde_bytes")]
        key: Vec<u8>,
        #[serde(default, with = "crate::version::optional")]
        if_version: Option<u64>,
    },
    JsonPut {
        space: String,
        id: String,
        value: Value,
        #[serde(default, with = "crate::version::optional")]
        if_version: Option<u64>,
    },
    JsonDelete {
        space: String,
        id: String,
        #[serde(default, with = "crate::version::optional")]
        if_version: Option<u64>,
    },
    GraphSetNode {
        graph: String,
        id: String,
        kind: String,
        metadata: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        doc_space: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        doc_id: Option<String>,
    },
    GraphDeleteNode {
        graph: String,
        id: String,
    },
    GraphAddEdge {
        graph: String,
        source: String,
        target: String,
        kind: String,
        #[serde(default = "default_edge_weight")]
        weight: f64,
    },
    GraphRemoveEdge {
        graph: String,
        source: String,
        target: String,
        kind: String,
    },
    FtsIndex {
        collection: String,
        doc_id: String,
        content: String,
    },
    FtsDelete {
        collection: String,
        doc_id: String,
    },
    VectorSet {
        collection: String,
        id: String,
        dim: u32,
        element_type: VectorElementType,
        #[serde(with = "serde_bytes")]
        vector: Vec<u8>,
    },
    VectorDelete {
        collection: String,
        id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationResult {
    #[serde(default, with = "crate::version::optional")]
    pub version: Option<u64>,
    pub deleted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Outgoing,
    Incoming,
    Both,
}

const fn default_edge_weight() -> f64 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub graph: String,
    pub id: String,
    pub kind: String,
    pub metadata: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_space: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub graph: String,
    pub source: String,
    pub target: String,
    pub kind: String,
    #[serde(default = "default_edge_weight")]
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathEdge {
    pub source: String,
    pub target: String,
    pub kind: String,
    #[serde(default = "default_edge_weight")]
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightedPath {
    pub nodes: Vec<String>,
    pub total_weight: f64,
    pub edges: Vec<PathEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeScore {
    pub node: String,
    pub score: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CentralityType {
    Betweenness,
    Closeness,
    Degree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentType {
    StronglyConnected,
    WeaklyConnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorHit {
    pub collection: String,
    pub id: String,
    pub distance: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridHit {
    pub id: String,
    pub rrf_score: f64,
    pub fts_rank: Option<usize>,
    pub fts_snippet: Option<String>,
    pub vector_rank: Option<usize>,
    pub vector_distance: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtsHit {
    pub collection: String,
    pub doc_id: String,
    pub snippet: String,
    pub rank: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DatabaseEvent {
    KvMutated {
        namespace: String,
        #[serde(with = "serde_bytes")]
        key: Vec<u8>,
        version: Option<u64>,
        deleted: bool,
    },
    JsonMutated {
        space: String,
        id: String,
        version: Option<u64>,
        deleted: bool,
    },
    GraphNodeMutated {
        graph: String,
        id: String,
        deleted: bool,
    },
    GraphEdgeMutated {
        graph: String,
        source: String,
        target: String,
        kind: String,
        deleted: bool,
    },
    FtsMutated {
        collection: String,
        doc_id: String,
        deleted: bool,
    },
    VectorMutated {
        collection: String,
        id: String,
        deleted: bool,
    },
}

impl DatabaseEvent {
    #[must_use]
    pub fn matches_filter(&self, filter: Option<&str>) -> bool {
        let Some(filter) = filter else { return true };
        if filter.is_empty() || filter == "*" {
            return true;
        }
        match self {
            Self::KvMutated { namespace, .. } => namespace == filter,
            Self::JsonMutated { space, .. } => space == filter,
            Self::GraphNodeMutated { graph, .. } | Self::GraphEdgeMutated { graph, .. } => {
                graph == filter
            }
            Self::FtsMutated { collection, .. } | Self::VectorMutated { collection, .. } => {
                collection == filter
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SqlQueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub rows_affected: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonIndexInfo {
    pub space: String,
    pub index_name: String,
    pub json_path: String,
}

/// 64-bit strictly monotonic logical version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

/// Metadata and diff information for a historical JSON document revision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRevisionInfo {
    pub space: String,
    pub id: String,
    pub version: Version,
    pub timestamp_ms: i64,
    pub is_deletion: bool,
    pub is_keyframe: bool,
    pub patch: Option<Value>,
}

/// Result returned after undoing a database mutation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoResult {
    pub id: i64,
    pub summary: String,
    pub timestamp_ms: i64,
}

/// Result returned after redoing a database mutation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedoResult {
    pub id: i64,
    pub summary: String,
    pub timestamp_ms: i64,
}

/// A transaction recorded in the database's native undo/redo log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoEntry {
    pub id: i64,
    pub timestamp_ms: i64,
    pub summary: String,
    pub undone: bool,
}
