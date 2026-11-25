//! The operations of the protocol.

use lilia_core::crdt::{LwwJsonDocument, OrSet, PNCounter};
use lilia_core::{
    BatchOperation, CentralityType, ComponentType, Direction, GpsPoint, HnswConfig, PageRankConfig,
    TimeSeriesLifecyclePolicy, TimeSeriesPoint, VectorElementType, VectorMetric,
};
use serde::{Deserialize, Serialize};

/// One operation on a database. A `limit` of 0 reads everything.
#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(crate = rkyv)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Operation {
    Handshake {
        major: u16,
        minor: u16,
    },
    Shutdown,
    /// Reads every stored entry and verifies the ledger strictly.
    IntegrityCheck,
    /// Verifies the ledger, fast.
    QuickCheck,
    /// Writes the memtable to a sorted run.
    Checkpoint,
    StorageStats,
    KvGet {
        namespace: String,
        #[serde(with = "serde_bytes")]
        key: Vec<u8>,
    },
    KvScan {
        namespace: String,
        #[serde(default, with = "serde_bytes")]
        after: Option<Vec<u8>>,
        limit: u32,
    },
    JsonGet {
        space: String,
        id: String,
    },
    JsonScan {
        space: String,
        after: Option<String>,
        limit: u32,
    },
    JsonScanPrefix {
        space: String,
        prefix: String,
        #[serde(default)]
        after_id: Option<String>,
        limit: u32,
    },
    JsonExtract {
        space: String,
        id: String,
        path: String,
    },
    JsonFind {
        space: String,
        #[rkyv(with = lilia_core::zero_copy::AsJsonString)]
        query: serde_json::Value,
        after: Option<String>,
        limit: u32,
    },
    JsonCreateIndex {
        space: String,
        name: String,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<String>,
    },
    JsonDropIndex {
        space: String,
        name: String,
    },
    JsonListIndexes {
        space: String,
    },
    JsonScanByIndex {
        space: String,
        index_name: String,
        value: String,
        after: Option<String>,
        limit: u32,
    },
    JsonGetAtVersion {
        space: String,
        id: String,
        version: u64,
    },
    JsonGetAtTimestamp {
        space: String,
        id: String,
        timestamp_ms: i64,
    },
    JsonHistory {
        space: String,
        id: String,
    },
    JsonDiff {
        space: String,
        id: String,
        from_version: u64,
        to_version: u64,
    },
    Batch {
        operations: Vec<BatchOperation>,
    },
    SqlQuery {
        sql: String,
        #[rkyv(with = rkyv::with::Map<lilia_core::zero_copy::AsJsonString>)]
        params: Vec<serde_json::Value>,
    },
    GraphNeighbors {
        graph: String,
        node: String,
        direction: Direction,
        kind: Option<String>,
    },
    GraphReachability {
        graph: String,
        start_nodes: Vec<String>,
        direction: Direction,
    },
    GraphGetNode {
        graph: String,
        id: String,
    },
    GraphGetNodeDocument {
        graph: String,
        id: String,
    },
    GraphDeleteNode {
        graph: String,
        id: String,
    },
    GraphListNodes {
        graph: String,
        #[serde(default)]
        kind: Option<String>,
        limit: u32,
    },
    GraphRemoveEdge {
        graph: String,
        source: String,
        target: String,
        kind: String,
    },
    GraphListEdges {
        graph: String,
        #[serde(default)]
        source: Option<String>,
        #[serde(default)]
        target: Option<String>,
        #[serde(default)]
        kind: Option<String>,
        limit: u32,
    },
    GraphImpactAnalysis {
        graph: String,
        changed_nodes: Vec<String>,
    },
    GraphShortestPath {
        graph: String,
        start: String,
        target: String,
        direction: Direction,
        #[serde(default)]
        kind_filter: Option<String>,
    },
    GraphDijkstra {
        graph: String,
        start: String,
        #[serde(default)]
        target: Option<String>,
        direction: Direction,
        #[serde(default)]
        kind_filter: Option<String>,
    },
    GraphAllShortestPaths {
        graph: String,
        start: String,
        direction: Direction,
        #[serde(default)]
        kind_filter: Option<String>,
    },
    GraphAstarSpatial {
        graph: String,
        start: String,
        target: String,
        direction: Direction,
        #[serde(default)]
        kind_filter: Option<String>,
    },
    GraphPageRank {
        graph: String,
        #[serde(default)]
        config: PageRankConfig,
    },
    GraphCentrality {
        graph: String,
        centrality: CentralityType,
        direction: Direction,
        #[serde(default)]
        kind_filter: Option<String>,
        #[serde(default)]
        normalized: bool,
    },
    GraphHasCycle {
        graph: String,
        direction: Direction,
        #[serde(default)]
        kind_filter: Option<String>,
    },
    GraphFindCycles {
        graph: String,
        max_cycles: usize,
        #[serde(default)]
        kind_filter: Option<String>,
    },
    GraphConnectedComponents {
        graph: String,
        component_type: ComponentType,
        #[serde(default)]
        kind_filter: Option<String>,
    },
    GraphCypher {
        graph: String,
        query: String,
    },
    FtsSearch {
        collection: String,
        query: String,
        limit: u32,
    },
    FtsDelete {
        collection: String,
        doc_id: String,
    },
    FtsSuggest {
        collection: String,
        prefix: String,
        max_distance: u8,
        limit: u32,
    },
    FtsFuzzySearch {
        collection: String,
        query: String,
        max_distance: u8,
        limit: u32,
    },
    FtsPhoneticSearch {
        collection: String,
        query: String,
        limit: u32,
    },
    /// Adds words to the vocabulary the suggestions and fuzzy searches of `collection` draw
    /// from, besides the indexed terms.
    FtsLoadDictionary {
        collection: String,
        words: Vec<String>,
    },
    VectorGet {
        collection: String,
        id: String,
    },
    VectorSearch {
        collection: String,
        #[serde(with = "serde_bytes")]
        vector: Vec<u8>,
        dim: u32,
        element_type: VectorElementType,
        metric: VectorMetric,
        limit: u32,
    },
    VectorCreateIndex {
        collection: String,
        config: HnswConfig,
    },
    VectorGetIndexInfo {
        collection: String,
    },
    VectorDropIndex {
        collection: String,
    },
    VectorSearchFiltered {
        collection: String,
        #[serde(with = "serde_bytes")]
        query_vector: Vec<u8>,
        dim: u32,
        element_type: VectorElementType,
        metric: VectorMetric,
        candidate_ids: Vec<String>,
        limit: u32,
    },
    VectorSearchInGraphNeighbors {
        collection: String,
        graph: String,
        start_node: String,
        direction: Direction,
        #[serde(with = "serde_bytes")]
        query_vector: Vec<u8>,
        dim: u32,
        element_type: VectorElementType,
        metric: VectorMetric,
        limit: u32,
    },
    VectorSearchByJsonIndex {
        collection: String,
        space: String,
        index_name: String,
        index_value: String,
        #[serde(with = "serde_bytes")]
        query_vector: Vec<u8>,
        dim: u32,
        element_type: VectorElementType,
        metric: VectorMetric,
        limit: u32,
    },
    HybridSearch {
        collection: String,
        fts_query: Option<String>,
        #[serde(default, with = "serde_bytes")]
        vector: Option<Vec<u8>>,
        dim: Option<u32>,
        element_type: Option<VectorElementType>,
        metric: Option<VectorMetric>,
        rrf_k: u32,
        fts_weight: f64,
        vector_weight: f64,
        limit: u32,
    },
    TsAppendPoints {
        series_id: String,
        points: Vec<TimeSeriesPoint>,
        #[serde(default)]
        resolution_ms: Option<i64>,
    },
    TsQueryRange {
        series_id: String,
        start_time: i64,
        end_time: i64,
        #[serde(default)]
        resolution_ms: Option<i64>,
    },
    TsQueryAuto {
        series_id: String,
        start_time: i64,
        end_time: i64,
        max_points: usize,
    },
    TsQueryStats {
        series_id: String,
        start_time: i64,
        end_time: i64,
        #[serde(default)]
        resolution_ms: Option<i64>,
    },
    TsSetPolicy {
        policy: TimeSeriesLifecyclePolicy,
    },
    TsGetPolicy {
        series_pattern: String,
    },
    TsListPolicies,
    TsDeletePolicy {
        series_pattern: String,
    },
    /// Applies every series' lifecycle policy as of `now_ms` (now by default).
    TsApplyRetention {
        #[serde(default)]
        now_ms: Option<i64>,
    },
    GeoAddFence {
        id: String,
        name: String,
        category: String,
        polygon_json: String,
        #[serde(default)]
        metadata: Option<String>,
    },
    GeoRemoveFence {
        id: String,
    },
    GeoContainsPoint {
        polygon_json: String,
        lon: f64,
        lat: f64,
    },
    GeoFindFencesForPoint {
        lon: f64,
        lat: f64,
    },
    GeoRecordGpsPoints {
        track_id: String,
        points: Vec<GpsPoint>,
    },
    GeoGetTrackPoints {
        track_id: String,
        #[serde(default)]
        start_time: Option<i64>,
        #[serde(default)]
        end_time: Option<i64>,
    },
    GeoTrackDistance {
        track_id: String,
    },
    CrdtCounterGet {
        space: String,
        id: String,
    },
    CrdtCounterState {
        space: String,
        id: String,
    },
    CrdtCounterMerge {
        space: String,
        id: String,
        remote: PNCounter,
    },
    CrdtSetGet {
        space: String,
        id: String,
    },
    CrdtSetContains {
        space: String,
        id: String,
        element: String,
    },
    CrdtSetState {
        space: String,
        id: String,
    },
    CrdtSetPrune {
        space: String,
        id: String,
        cutoff_ms: i64,
    },
    CrdtSetMerge {
        space: String,
        id: String,
        remote: OrSet<String>,
    },
    CrdtJsonGet {
        space: String,
        id: String,
    },
    CrdtJsonState {
        space: String,
        id: String,
    },
    /// Merges a remote document state; the result's version is absent when nothing changed.
    CrdtJsonMerge {
        space: String,
        id: String,
        remote: LwwJsonDocument,
    },
    Undo,
    Redo,
    UndoHistory,
}

impl Operation {
    /// The name of the operation, as its JSON form tags it.
    #[must_use]
    #[allow(clippy::too_many_lines, reason = "one arm per operation")]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Handshake { .. } => "handshake",
            Self::Shutdown => "shutdown",
            Self::IntegrityCheck => "integrity_check",
            Self::QuickCheck => "quick_check",
            Self::Checkpoint => "checkpoint",
            Self::StorageStats => "storage_stats",
            Self::KvGet { .. } => "kv_get",
            Self::KvScan { .. } => "kv_scan",
            Self::JsonGet { .. } => "json_get",
            Self::JsonScan { .. } => "json_scan",
            Self::JsonScanPrefix { .. } => "json_scan_prefix",
            Self::JsonExtract { .. } => "json_extract",
            Self::JsonFind { .. } => "json_find",
            Self::JsonCreateIndex { .. } => "json_create_index",
            Self::JsonDropIndex { .. } => "json_drop_index",
            Self::JsonListIndexes { .. } => "json_list_indexes",
            Self::JsonScanByIndex { .. } => "json_scan_by_index",
            Self::JsonGetAtVersion { .. } => "json_get_at_version",
            Self::JsonGetAtTimestamp { .. } => "json_get_at_timestamp",
            Self::JsonHistory { .. } => "json_history",
            Self::JsonDiff { .. } => "json_diff",
            Self::Batch { .. } => "batch",
            Self::SqlQuery { .. } => "sql_query",
            Self::GraphNeighbors { .. } => "graph_neighbors",
            Self::GraphReachability { .. } => "graph_reachability",
            Self::GraphGetNode { .. } => "graph_get_node",
            Self::GraphGetNodeDocument { .. } => "graph_get_node_document",
            Self::GraphDeleteNode { .. } => "graph_delete_node",
            Self::GraphListNodes { .. } => "graph_list_nodes",
            Self::GraphRemoveEdge { .. } => "graph_remove_edge",
            Self::GraphListEdges { .. } => "graph_list_edges",
            Self::GraphImpactAnalysis { .. } => "graph_impact_analysis",
            Self::GraphShortestPath { .. } => "graph_shortest_path",
            Self::GraphDijkstra { .. } => "graph_dijkstra",
            Self::GraphAllShortestPaths { .. } => "graph_all_shortest_paths",
            Self::GraphAstarSpatial { .. } => "graph_astar_spatial",
            Self::GraphPageRank { .. } => "graph_page_rank",
            Self::GraphCentrality { .. } => "graph_centrality",
            Self::GraphHasCycle { .. } => "graph_has_cycle",
            Self::GraphFindCycles { .. } => "graph_find_cycles",
            Self::GraphConnectedComponents { .. } => "graph_connected_components",
            Self::GraphCypher { .. } => "graph_cypher",
            Self::FtsSearch { .. } => "fts_search",
            Self::FtsDelete { .. } => "fts_delete",
            Self::FtsSuggest { .. } => "fts_suggest",
            Self::FtsFuzzySearch { .. } => "fts_fuzzy_search",
            Self::FtsPhoneticSearch { .. } => "fts_phonetic_search",
            Self::FtsLoadDictionary { .. } => "fts_load_dictionary",
            Self::VectorGet { .. } => "vector_get",
            Self::VectorSearch { .. } => "vector_search",
            Self::VectorCreateIndex { .. } => "vector_create_index",
            Self::VectorGetIndexInfo { .. } => "vector_get_index_info",
            Self::VectorDropIndex { .. } => "vector_drop_index",
            Self::VectorSearchFiltered { .. } => "vector_search_filtered",
            Self::VectorSearchInGraphNeighbors { .. } => "vector_search_in_graph_neighbors",
            Self::VectorSearchByJsonIndex { .. } => "vector_search_by_json_index",
            Self::HybridSearch { .. } => "hybrid_search",
            Self::TsAppendPoints { .. } => "ts_append_points",
            Self::TsQueryRange { .. } => "ts_query_range",
            Self::TsQueryAuto { .. } => "ts_query_auto",
            Self::TsQueryStats { .. } => "ts_query_stats",
            Self::TsSetPolicy { .. } => "ts_set_policy",
            Self::TsGetPolicy { .. } => "ts_get_policy",
            Self::TsListPolicies => "ts_list_policies",
            Self::TsDeletePolicy { .. } => "ts_delete_policy",
            Self::TsApplyRetention { .. } => "ts_apply_retention",
            Self::GeoAddFence { .. } => "geo_add_fence",
            Self::GeoRemoveFence { .. } => "geo_remove_fence",
            Self::GeoContainsPoint { .. } => "geo_contains_point",
            Self::GeoFindFencesForPoint { .. } => "geo_find_fences_for_point",
            Self::GeoRecordGpsPoints { .. } => "geo_record_gps_points",
            Self::GeoGetTrackPoints { .. } => "geo_get_track_points",
            Self::GeoTrackDistance { .. } => "geo_track_distance",
            Self::CrdtCounterGet { .. } => "crdt_counter_get",
            Self::CrdtCounterState { .. } => "crdt_counter_state",
            Self::CrdtCounterMerge { .. } => "crdt_counter_merge",
            Self::CrdtSetGet { .. } => "crdt_set_get",
            Self::CrdtSetContains { .. } => "crdt_set_contains",
            Self::CrdtSetState { .. } => "crdt_set_state",
            Self::CrdtSetPrune { .. } => "crdt_set_prune",
            Self::CrdtSetMerge { .. } => "crdt_set_merge",
            Self::CrdtJsonGet { .. } => "crdt_json_get",
            Self::CrdtJsonState { .. } => "crdt_json_state",
            Self::CrdtJsonMerge { .. } => "crdt_json_merge",
            Self::Undo => "undo",
            Self::Redo => "redo",
            Self::UndoHistory => "undo_history",
        }
    }
}
