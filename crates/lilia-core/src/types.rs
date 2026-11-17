use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    #[serde(rename = "value")]
    pub doc: crate::json::BinaryDoc,
    #[serde(with = "crate::version")]
    pub version: u64,
}

impl std::ops::Deref for JsonEntry {
    type Target = crate::json::BinaryDoc;

    fn deref(&self) -> &Self::Target {
        &self.doc
    }
}

impl JsonEntry {
    #[must_use]
    pub fn new(
        space: impl Into<String>,
        id: impl Into<String>,
        doc: crate::json::BinaryDoc,
        version: u64,
    ) -> Self {
        Self {
            space: space.into(),
            id: id.into(),
            doc,
            version,
        }
    }

    #[inline]
    #[must_use]
    pub fn value(&self) -> Value {
        self.doc.to_json_value()
    }

    #[inline]
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        self.doc.to_json_value()
    }

    #[inline]
    pub fn extract(&self, path: &str) -> Option<crate::json::BinaryValueRef<'_>> {
        self.doc.extract(path)
    }

    #[inline]
    pub fn extract_str(&self, path: &str) -> Option<&str> {
        self.doc.extract_str(path)
    }

    #[inline]
    pub fn extract_i64(&self, path: &str) -> Option<i64> {
        self.doc.extract_i64(path)
    }

    #[inline]
    pub fn extract_f64(&self, path: &str) -> Option<f64> {
        self.doc.extract_f64(path)
    }

    #[inline]
    pub fn extract_bool(&self, path: &str) -> Option<bool> {
        self.doc.extract_bool(path)
    }
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
    JsonPutRaw {
        space: String,
        id: String,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
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
    CrdtPnCounterIncrement {
        space: String,
        id: String,
        actor_id: String,
        amount: u64,
    },
    CrdtPnCounterDecrement {
        space: String,
        id: String,
        actor_id: String,
        amount: u64,
    },
    CrdtOrSetAdd {
        space: String,
        id: String,
        actor_id: String,
        element: String,
    },
    CrdtOrSetRemove {
        space: String,
        id: String,
        element: String,
        #[serde(default)]
        timestamp_ms: Option<i64>,
    },
    CrdtJsonSetField {
        space: String,
        id: String,
        actor_id: String,
        path: String,
        value: Value,
        #[serde(default)]
        timestamp_ms: Option<i64>,
    },
    CrdtJsonDeleteField {
        space: String,
        id: String,
        actor_id: String,
        path: String,
        #[serde(default)]
        timestamp_ms: Option<i64>,
    },
    CrdtJsonMerge {
        space: String,
        id: String,
        #[serde(with = "serde_bytes")]
        state: Vec<u8>,
    },
    JsonPutHlc {
        space: String,
        id: String,
        value: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hlc: Option<crate::hlc::Hlc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_hlc: Option<crate::hlc::Hlc>,
        #[serde(default)]
        strategy: crate::hlc::ConflictResolutionStrategy,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MutationResult {
    #[serde(default, with = "crate::version::optional")]
    pub version: Option<u64>,
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "crate::StorageTier::is_disk")]
    pub tier: crate::StorageTier,
    #[serde(default, skip_serializing_if = "is_false")]
    pub buffered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hlc: Option<crate::hlc::Hlc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_detected: Option<bool>,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(value: &bool) -> bool {
    !*value
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FtsSuggestion {
    pub term: String,
    pub distance: u8,
    pub doc_count: u32,
    pub exact_prefix: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtsFuzzyHit {
    pub collection: String,
    pub doc_id: String,
    pub snippet: String,
    pub rank: f64,
    pub edit_distance: u8,
    pub matched_terms: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FtsFuzzyOptions {
    pub max_distance: u8,
    pub prefix: bool,
    pub damerau: bool,
    pub limit: u32,
}

impl Default for FtsFuzzyOptions {
    fn default() -> Self {
        Self {
            max_distance: 1,
            prefix: true,
            damerau: true,
            limit: 10,
        }
    }
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
    CrdtCounterMutated {
        space: String,
        id: String,
        value: i64,
    },
    CrdtSetMutated {
        space: String,
        id: String,
        element: String,
        deleted: bool,
    },
    CrdtJsonMutated {
        space: String,
        id: String,
        version: Option<u64>,
    },
    ConflictDetected {
        model: String,
        space: String,
        id: String,
        base_hlc: Option<String>,
        ours_hlc: String,
        theirs_hlc: String,
        resolution_strategy: String,
    },
    TierChanged {
        kind: crate::CollectionKind,
        name: String,
        tier: crate::StorageTier,
    },
    RingBufferFlushed {
        ops_committed: u64,
        coalesced_ts_points: u64,
        latency_us: u64,
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
            Self::JsonMutated { space, .. }
            | Self::CrdtCounterMutated { space, .. }
            | Self::CrdtSetMutated { space, .. }
            | Self::CrdtJsonMutated { space, .. }
            | Self::ConflictDetected { space, .. } => space == filter,
            Self::GraphNodeMutated { graph, .. } | Self::GraphEdgeMutated { graph, .. } => {
                graph == filter
            }
            Self::FtsMutated { collection, .. } | Self::VectorMutated { collection, .. } => {
                collection == filter
            }
            Self::TierChanged { name, .. } => name == filter,
            Self::RingBufferFlushed { .. } => true,
        }
    }
}

/// Metadata representing an HLC-tracked record in persistent storage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HlcRecord {
    pub model: String,
    pub space: String,
    pub id: String,
    pub hlc: crate::hlc::Hlc,
    pub parent_hlc: Option<crate::hlc::Hlc>,
    pub node_id: String,
    pub updated_at_ms: i64,
}

/// Detailed outcome of an HLC-tracked mutation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HlcMutationOutcome {
    pub hlc: crate::hlc::Hlc,
    pub version: u64,
    pub causal_relation: crate::hlc::CausalRelation,
    pub conflict_detected: bool,
    pub conflict_id: Option<i64>,
    pub resolved_by: Option<crate::hlc::ConflictResolutionStrategy>,
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

/// Precise differential delta emitted for a live query subscription.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveQueryDiff {
    pub subscription_id: u64,
    pub added: Vec<Value>,
    pub updated: Vec<Value>,
    pub removed: Vec<Value>,
    pub is_initial: bool,
}

/// Metadata catalog entry describing an active incremental materialized view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializedViewInfo {
    pub name: String,
    pub source_table: String,
    pub query: String,
    pub storage_table: String,
    pub group_by: Option<String>,
    pub created_at_ms: i64,
}

/// Resource footprint profile configuring SQLite cache, mmap, WAL checkpoints, and retention defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceProfile {
    #[default]
    Server,
    Mobile,
    Constrained,
}

/// Background WAL checkpoint worker counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointStats {
    pub attempts: u64,
    pub incomplete: u64,
    pub log_frames: i64,
    pub checkpointed_frames: i64,
    pub last_error: Option<crate::ErrorCode>,
}

/// Physical page and payload storage statistics for a single SQLite B-tree/table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableStorageStat {
    pub name: String,
    pub page_count: i64,
    pub payload_bytes: i64,
    pub unused_bytes: i64,
}

/// Metadata and compression stats for a trained Zstandard JSON dictionary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictStats {
    pub space: String,
    pub dict_id: u32,
    pub sample_count: usize,
    pub dict_size: usize,
    pub created_at_ms: i64,
}

/// Transitive graph dependency impact summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImpactAnalysis {
    pub direct: Vec<String>,
    pub dependants: Vec<String>,
    pub total_affected: usize,
}

/// Tabular output returned by embedded Cypher queries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CypherResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

impl CypherResult {
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut list = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            let mut map = serde_json::Map::new();
            for (col, val) in self.columns.iter().zip(row.iter()) {
                map.insert(col.clone(), val.clone());
            }
            list.push(Value::Object(map));
        }
        Value::Array(list)
    }

    #[must_use]
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

const fn default_pagerank_damping() -> f64 {
    0.85
}

const fn default_pagerank_iterations() -> usize {
    50
}

const fn default_pagerank_tolerance() -> f64 {
    1e-6
}

const fn default_pagerank_direction() -> Direction {
    Direction::Outgoing
}

/// Configuration for weighted / personalized PageRank graph computation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageRankConfig {
    #[serde(default = "default_pagerank_damping")]
    pub damping_factor: f64,
    #[serde(default = "default_pagerank_iterations")]
    pub max_iterations: usize,
    #[serde(default = "default_pagerank_tolerance")]
    pub tolerance: f64,
    pub seeds: Option<Vec<String>>,
    #[serde(default = "default_pagerank_direction")]
    pub direction: Direction,
    pub edge_kind: Option<String>,
}

impl Default for PageRankConfig {
    fn default() -> Self {
        Self {
            damping_factor: 0.85,
            max_iterations: 50,
            tolerance: 1e-6,
            seeds: None,
            direction: Direction::Outgoing,
            edge_kind: None,
        }
    }
}
