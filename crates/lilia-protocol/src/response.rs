//! The results of the operations.

use std::collections::HashMap;

use lilia_core::crdt::{LwwJsonDocument, OrSet, PNCounter};
use lilia_core::{
    CypherResult, FtsFuzzyHit, FtsHit, FtsPhoneticHit, FtsSuggestion, GeofenceMatch, GpsPoint,
    GraphEdge, GraphNode, HybridHit, ImpactAnalysis, JsonEntry, JsonIndexInfo, KvEntry,
    MutationResult, NodeScore, SqlQueryResult, TimeSeriesLifecyclePolicy, TimeSeriesPoint,
    TimeSeriesRetentionStats, TimeSeriesStats, VectorElementType, VectorHit, VectorIndexInfo,
    WeightedPath,
};
use lilia_db::{JsonRevision, StorageStats, UndoStep};
use serde::{Deserialize, Serialize};

/// A stored vector: its dimension, element type and little-endian elements.
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
#[rkyv(crate = rkyv)]
pub struct StoredVector {
    pub dim: u32,
    pub element_type: VectorElementType,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(crate = rkyv)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ResponseValue {
    Handshake {
        major: u16,
        minor: u16,
        capabilities: Vec<String>,
    },
    Shutdown,
    Ack,
    Bool(bool),
    F64(f64),
    U64(u64),
    StorageStats(StorageStats),
    Kv(Option<KvEntry>),
    KvPage(Vec<KvEntry>),
    Json(Option<JsonEntry>),
    JsonPage(Vec<JsonEntry>),
    JsonIndexes(Vec<JsonIndexInfo>),
    JsonExtracted(
        #[rkyv(with = rkyv::with::Map<lilia_core::zero_copy::AsJsonString>)]
        Option<serde_json::Value>,
    ),
    JsonValue(#[rkyv(with = lilia_core::zero_copy::AsJsonString)] serde_json::Value),
    JsonRevisions(Vec<JsonRevision>),
    Mutations(Vec<MutationResult>),
    Sql(SqlQueryResult),
    /// Node ids: neighbours, reachable nodes, the elements of a CRDT set.
    Ids(Vec<String>),
    GraphNode(Option<GraphNode>),
    GraphNodes(Vec<GraphNode>),
    GraphEdges(Vec<GraphEdge>),
    GraphImpact(ImpactAnalysis),
    WeightedPath(Option<WeightedPath>),
    WeightedPathMap(HashMap<String, WeightedPath>),
    NodeScores(Vec<NodeScore>),
    /// Cycles or components, each a list of node ids.
    NodeGroups(Vec<Vec<String>>),
    CypherResult(CypherResult),
    FtsHits(Vec<FtsHit>),
    FtsSuggestions(Vec<FtsSuggestion>),
    FtsFuzzyHits(Vec<FtsFuzzyHit>),
    FtsPhoneticHits(Vec<FtsPhoneticHit>),
    Vector(Option<StoredVector>),
    VectorHits(Vec<VectorHit>),
    VectorIndexInfo(Option<VectorIndexInfo>),
    HybridHits(Vec<HybridHit>),
    TsPoints(Vec<TimeSeriesPoint>),
    TsStats(Option<TimeSeriesStats>),
    TsPolicy(Option<TimeSeriesLifecyclePolicy>),
    TsPolicies(Vec<TimeSeriesLifecyclePolicy>),
    TsRetentionStats(TimeSeriesRetentionStats),
    GeofenceMatches(Vec<GeofenceMatch>),
    GpsPoints(Vec<GpsPoint>),
    CrdtCounter(i64),
    CrdtCounterState(PNCounter),
    CrdtSetState(OrSet<String>),
    CrdtJson(
        #[rkyv(with = rkyv::with::Map<lilia_core::zero_copy::AsJsonString>)]
        Option<serde_json::Value>,
    ),
    CrdtJsonState(Option<LwwJsonDocument>),
    /// The commit an undo or redo reverted or restored, `None` if there was none.
    UndoStep(Option<UndoStep>),
    UndoSteps(Vec<UndoStep>),
}

impl ResponseValue {
    /// The name of the value, as its JSON form tags it.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Handshake { .. } => "handshake",
            Self::Shutdown => "shutdown",
            Self::Ack => "ack",
            Self::Bool(_) => "bool",
            Self::F64(_) => "f64",
            Self::U64(_) => "u64",
            Self::StorageStats(_) => "storage_stats",
            Self::Kv(_) => "kv",
            Self::KvPage(_) => "kv_page",
            Self::Json(_) => "json",
            Self::JsonPage(_) => "json_page",
            Self::JsonIndexes(_) => "json_indexes",
            Self::JsonExtracted(_) => "json_extracted",
            Self::JsonValue(_) => "json_value",
            Self::JsonRevisions(_) => "json_revisions",
            Self::Mutations(_) => "mutations",
            Self::Sql(_) => "sql",
            Self::Ids(_) => "ids",
            Self::GraphNode(_) => "graph_node",
            Self::GraphNodes(_) => "graph_nodes",
            Self::GraphEdges(_) => "graph_edges",
            Self::GraphImpact(_) => "graph_impact",
            Self::WeightedPath(_) => "weighted_path",
            Self::WeightedPathMap(_) => "weighted_path_map",
            Self::NodeScores(_) => "node_scores",
            Self::NodeGroups(_) => "node_groups",
            Self::CypherResult(_) => "cypher_result",
            Self::FtsHits(_) => "fts_hits",
            Self::FtsSuggestions(_) => "fts_suggestions",
            Self::FtsFuzzyHits(_) => "fts_fuzzy_hits",
            Self::FtsPhoneticHits(_) => "fts_phonetic_hits",
            Self::Vector(_) => "vector",
            Self::VectorHits(_) => "vector_hits",
            Self::VectorIndexInfo(_) => "vector_index_info",
            Self::HybridHits(_) => "hybrid_hits",
            Self::TsPoints(_) => "ts_points",
            Self::TsStats(_) => "ts_stats",
            Self::TsPolicy(_) => "ts_policy",
            Self::TsPolicies(_) => "ts_policies",
            Self::TsRetentionStats(_) => "ts_retention_stats",
            Self::GeofenceMatches(_) => "geofence_matches",
            Self::GpsPoints(_) => "gps_points",
            Self::CrdtCounter(_) => "crdt_counter",
            Self::CrdtCounterState(_) => "crdt_counter_state",
            Self::CrdtSetState(_) => "crdt_set_state",
            Self::CrdtJson(_) => "crdt_json",
            Self::CrdtJsonState(_) => "crdt_json_state",
            Self::UndoStep(_) => "undo_step",
            Self::UndoSteps(_) => "undo_steps",
        }
    }
}
