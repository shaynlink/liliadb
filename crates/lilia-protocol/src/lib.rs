use std::collections::HashMap;
use std::io;

use lilia_core::crdt::{LwwJsonDocument, OrSet, PNCounter};
use lilia_core::{
    BatchOperation, CentralityType, CheckpointStats, ComponentType, ConflictRecord,
    ConflictResolutionStrategy, CypherResult, DatabaseEvent, DictStats, Direction, FtsHit,
    GeofenceMatch, GpsPoint, GraphEdge, GraphNode, Hlc, HlcMutationOutcome, HlcRecord, HnswConfig,
    HybridHit, ImpactAnalysis, InvertibleBloomFilter, JsonEntry, JsonIndexInfo, JsonRevisionInfo,
    KvEntry, LiliaError, LiveQueryDiff, MaterializedViewInfo, MutationResult, NodeScore,
    P2pDocumentPayload, PageRankConfig, ReconciliationSummary, RedoResult, StrataEstimator,
    TableStorageStat, TimeSeriesLifecyclePolicy, TimeSeriesPoint, TimeSeriesRetentionStats,
    TimeSeriesStats, UndoEntry, UndoResult, VectorElementType, VectorHit, VectorIndexInfo,
    VectorMetric, WeightedPath,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
pub use serde_bytes::ByteBuf;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTOCOL_MAJOR: u16 = 1;
pub const PROTOCOL_MINOR: u16 = 2;
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub token: String,
    /// Optional relative admission budget, starting after the full frame is decoded.
    /// Includes waiting for a daemon blocking worker, but does not cancel running storage work.
    pub deadline_ms: Option<u64>,
    pub operation: Operation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Operation {
    Handshake {
        major: u16,
        minor: u16,
    },
    Shutdown,
    IntegrityCheck,
    Backup {
        destination: String,
    },
    KvGet {
        namespace: String,
        #[serde(with = "serde_bytes")]
        key: Vec<u8>,
    },
    KvScan {
        namespace: String,
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
    JsonFind {
        space: String,
        query: serde_json::Value,
        after: Option<String>,
        limit: u32,
    },
    Batch {
        operations: Vec<BatchOperation>,
    },
    SqlQuery {
        sql: String,
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
    FtsSearch {
        collection: String,
        query: String,
        limit: u32,
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
    Subscribe {
        namespace: Option<String>,
    },
    CrdtCounterGet {
        space: String,
        id: String,
    },
    CrdtSetGet {
        space: String,
        id: String,
    },
    CrdtJsonGet {
        space: String,
        id: String,
    },
    QuickCheck,
    Checkpoint,
    CheckpointStats,
    StorageStats,
    FtsDelete {
        collection: String,
        doc_id: String,
    },
    JsonExtract {
        space: String,
        id: String,
        path: String,
    },
    JsonScanPrefix {
        space: String,
        prefix: String,
        #[serde(default)]
        after_id: Option<String>,
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
    JsonTrainDictionary {
        space: String,
        sample_count: usize,
        dict_size: usize,
    },
    JsonSetAutoTrain {
        space: String,
        enabled: bool,
    },
    JsonIsAutoTrainEnabled {
        space: String,
    },
    JsonPutHlc {
        space: String,
        id: String,
        value: serde_json::Value,
        #[serde(default)]
        hlc: Option<Hlc>,
        #[serde(default)]
        parent_hlc: Option<Hlc>,
        #[serde(default)]
        strategy: ConflictResolutionStrategy,
    },
    JsonGetHlc {
        space: String,
        id: String,
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
    TsApplyRetention {
        #[serde(default)]
        series_id: Option<String>,
        #[serde(default)]
        policy: Option<TimeSeriesLifecyclePolicy>,
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
    CrdtCounterState {
        space: String,
        id: String,
    },
    CrdtCounterMerge {
        space: String,
        id: String,
        remote: PNCounter,
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
    CrdtJsonState {
        space: String,
        id: String,
    },
    CrdtJsonMerge {
        space: String,
        id: String,
        remote: LwwJsonDocument,
    },
    HlcNext,
    HlcObserve {
        remote: Hlc,
    },
    ListConflicts {
        #[serde(default)]
        space: Option<String>,
    },
    ResolveConflict {
        conflict_id: i64,
        resolved_value: serde_json::Value,
    },
    Undo,
    Redo,
    UndoHistory,
    CreateMaterializedView {
        name: String,
        query: String,
    },
    DropMaterializedView {
        name: String,
    },
    RefreshMaterializedView {
        name: String,
    },
    ListMaterializedViews,
    LiveQuerySubscribe {
        sql: String,
        #[serde(default)]
        params: Vec<serde_json::Value>,
    },
    P2pSync {
        message: P2pSyncMessage,
    },
}

/// Decentralized P2P Set Reconciliation protocol messages using Strata Estimators and IBFs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum P2pSyncMessage {
    EstimateRequest {
        space: Option<String>,
        strata: StrataEstimator,
    },
    FastReconcileResponse {
        space: Option<String>,
        changesets: Vec<P2pDocumentPayload>,
        requested_id_hashes: Vec<u64>,
    },
    IbfNegotiation {
        space: Option<String>,
        estimated_diff: usize,
        ibf: InvertibleBloomFilter,
    },
    IbfReconcileResponse {
        space: Option<String>,
        changesets: Vec<P2pDocumentPayload>,
        requested_id_hashes: Vec<u64>,
    },
    ChangesetDelivery {
        space: Option<String>,
        changesets: Vec<P2pDocumentPayload>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: String,
    pub result: std::result::Result<ResponseValue, LiliaError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ResponseValue {
    Handshake {
        major: u16,
        minor: u16,
        capabilities: Vec<String>,
    },
    Shutdown,
    Integrity(bool),
    Ack,
    Kv(Option<KvEntry>),
    KvPage(Vec<KvEntry>),
    Json(Option<JsonEntry>),
    JsonPage(Vec<JsonEntry>),
    JsonIndexes(Vec<JsonIndexInfo>),
    IndexDropped(bool),
    Mutations(Vec<MutationResult>),
    Sql(lilia_core::SqlQueryResult),
    GraphNeighbors(Vec<String>),
    GraphReachability(Vec<String>),
    FtsHits(Vec<FtsHit>),
    Vector(Option<(u32, VectorElementType, serde_bytes::ByteBuf)>),
    VectorHits(Vec<VectorHit>),
    HybridHits(Vec<HybridHit>),
    Event(DatabaseEvent),
    CrdtCounter(i64),
    CrdtSet(Vec<String>),
    CrdtJson(Option<serde_json::Value>),
    Bool(bool),
    F64(f64),
    Usize(usize),
    U64(u64),
    CheckpointStats(CheckpointStats),
    TableStorageStats(Vec<TableStorageStat>),
    JsonExtracted(Option<serde_json::Value>),
    JsonValue(serde_json::Value),
    JsonRevisions(Vec<JsonRevisionInfo>),
    DictStats(DictStats),
    Hlc(Hlc),
    HlcRecord(Option<HlcRecord>),
    HlcOutcome(HlcMutationOutcome),
    Conflicts(Vec<ConflictRecord>),
    GraphNode(Option<GraphNode>),
    GraphNodes(Vec<GraphNode>),
    GraphEdges(Vec<GraphEdge>),
    GraphImpact(ImpactAnalysis),
    WeightedPath(Option<WeightedPath>),
    WeightedPathMap(HashMap<String, WeightedPath>),
    NodeScores(Vec<NodeScore>),
    CycleList(Vec<Vec<String>>),
    CypherResult(CypherResult),
    VectorIndexInfo(Option<VectorIndexInfo>),
    TsPoints(Vec<TimeSeriesPoint>),
    TsStats(Option<TimeSeriesStats>),
    TsPolicy(Option<TimeSeriesLifecyclePolicy>),
    TsPolicies(Vec<TimeSeriesLifecyclePolicy>),
    TsRetentionStats(TimeSeriesRetentionStats),
    GeofenceMatches(Vec<GeofenceMatch>),
    GpsPoints(Vec<GpsPoint>),
    CrdtCounterState(PNCounter),
    CrdtSetState(OrSet<String>),
    CrdtJsonState(Option<LwwJsonDocument>),
    UndoResult(Option<UndoResult>),
    RedoResult(Option<RedoResult>),
    UndoEntries(Vec<UndoEntry>),
    MaterializedViews(Vec<MaterializedViewInfo>),
    LiveQueryDiff(LiveQueryDiff),
    P2pSync(P2pSyncMessage),
    ReconciliationSummary(ReconciliationSummary),
}

/// Decode one size-prefixed `MessagePack` frame.
///
/// # Errors
/// Returns an I/O error for truncated, oversized, empty, or malformed frames.
pub async fn read_frame<T: DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin),
) -> io::Result<T> {
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid frame length",
        ));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    rmp_serde::from_slice(&bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Encode and write one size-prefixed `MessagePack` frame.
///
/// # Errors
/// Returns an I/O error when serialization or writing fails, or when the frame is oversized.
pub async fn write_frame<T: Serialize>(
    stream: &mut (impl AsyncWrite + Unpin),
    value: &T,
) -> io::Result<()> {
    let bytes = rmp_serde::to_vec_named(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds size limit",
        ));
    }
    let length = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame exceeds u32 length"))?;
    stream.write_u32(length).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await
}

/// Encode and write one zero-copy frame using rkyv.
///
/// # Errors
/// Returns an I/O error when serialization fails or the frame exceeds limits.
pub async fn write_archived_frame<T>(
    stream: &mut (impl AsyncWrite + Unpin),
    value: &T,
) -> io::Result<()>
where
    T: for<'a> rkyv::Serialize<
        rkyv::api::high::HighSerializer<
            rkyv::util::AlignedVec,
            rkyv::ser::allocator::ArenaHandle<'a>,
            rkyv::rancor::Error,
        >,
    >,
{
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds size limit",
        ));
    }
    let length = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame exceeds u32 length"))?;
    stream.write_u32(length).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await
}

/// Read a zero-copy frame from stream into an aligned buffer with bytecheck validation.
///
/// Returns a borrowed reference `&Archived<T>` without allocating or deserializing into heap objects!
///
/// # Errors
/// Returns an I/O error for truncated or invalid frames.
pub async fn read_archived_frame<'a, T>(
    stream: &mut (impl AsyncRead + Unpin),
    buffer: &'a mut rkyv::util::AlignedVec,
) -> io::Result<&'a T::Archived>
where
    T: rkyv::Archive,
    T::Archived: 'a + for<'b> rkyv::bytecheck::CheckBytes<
        rkyv::api::high::HighValidator<'b, rkyv::rancor::Error>,
    >,
{
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid frame length",
        ));
    }
    buffer.clear();
    buffer.resize(length, 0);
    stream.read_exact(buffer).await?;
    rkyv::access::<T::Archived, rkyv::rancor::Error>(buffer)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
}

/// 0-nanosecond unchecked read of an archived frame from trusted local IPC stream.
///
/// # Safety
/// The stream payload must be a trusted, well-formed instance of `T::Archived`.
///
/// # Errors
/// Returns an I/O error for truncated frames.
#[allow(unsafe_code)]
pub async fn read_archived_frame_unchecked<'a, T>(
    stream: &mut (impl AsyncRead + Unpin),
    buffer: &'a mut rkyv::util::AlignedVec,
) -> io::Result<&'a T::Archived>
where
    T: rkyv::Archive,
    T::Archived: 'a,
{
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid frame length",
        ));
    }
    buffer.clear();
    buffer.resize(length, 0);
    stream.read_exact(buffer).await?;
    unsafe { Ok(rkyv::access_unchecked::<T::Archived>(buffer)) }
}
