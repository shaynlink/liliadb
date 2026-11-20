use std::str::FromStr;
use std::sync::Arc;

use lilia_core::crdt::{LwwJsonDocument, OrSet, PNCounter};
use lilia_core::hlc::{ConflictResolutionStrategy, Hlc};
use lilia_core::timeseries::{DownsampleAggregation, TimeSeriesLifecyclePolicy, TimeSeriesPoint};
use lilia_core::{
    BatchOperation, CentralityType, ComponentType, Direction, Durability, FtsHit, GpsPoint,
    HnswConfig, KvEntry, PageRankConfig, ResourceProfile, VectorElementType, VectorHit,
    VectorMetric,
};
use lilia_graph::DatabaseGraphExt;
use lilia_storage_sqlite::{Database, DatabaseOptions};
use napi::bindgen_prelude::{Buffer, Error, Result, Status};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::{NativeFtsHit, NativeHybridHit, NativeKvEntry, NativeOpenOptions, NativeVectorHit};

pub fn napi_error(error: lilia_core::LiliaError) -> Error {
    Error::new(
        if error.retryable {
            Status::WouldDeadlock
        } else {
            Status::GenericFailure
        },
        serde_json::to_string(&error).unwrap_or_else(|_| error.to_string()),
    )
}

pub fn invalid_arg(message: impl Into<String>) -> Error {
    Error::new(Status::InvalidArg, message.into())
}

pub fn join_error(error: tokio::task::JoinError) -> Error {
    Error::new(Status::GenericFailure, error.to_string())
}

pub async fn run_async<F, R>(db: &Arc<Database>, f: F) -> Result<R>
where
    F: FnOnce(&Database) -> Result<R> + Send + 'static,
    R: Send + 'static,
{
    let database = Arc::clone(db);
    tokio::task::spawn_blocking(move || f(&database))
        .await
        .map_err(join_error)?
}

pub fn to_json_string<T: Serialize + ?Sized>(val: &T) -> Result<String> {
    serde_json::to_string(val)
        .map_err(|error| Error::new(Status::GenericFailure, error.to_string()))
}

pub fn from_json_str<T: DeserializeOwned>(json: &str) -> Result<T> {
    serde_json::from_str(json).map_err(|error| Error::new(Status::InvalidArg, error.to_string()))
}

pub fn parse_params(params_json: Option<&str>) -> Result<Vec<serde_json::Value>> {
    match params_json {
        Some(s) if !s.trim().is_empty() => from_json_str(s),
        _ => Ok(Vec::new()),
    }
}

pub fn parse_u64(raw: &str, label: &str) -> Result<u64> {
    raw.trim().parse::<u64>().map_err(|_| {
        invalid_arg(format!(
            "invalid {label} '{raw}': expected unsigned integer"
        ))
    })
}

pub fn parse_hlc(raw: &str) -> Result<Hlc> {
    let trimmed = raw.trim();
    if trimmed.starts_with('{') {
        from_json_str(trimmed)
    } else {
        Hlc::from_str(trimmed).map_err(|e| invalid_arg(format!("invalid HLC '{raw}': {e}")))
    }
}

pub fn parse_direction(direction: Option<&str>) -> Direction {
    match direction {
        Some("incoming" | "inbound") => Direction::Inbound,
        Some("both") => Direction::Both,
        _ => Direction::Outbound,
    }
}

pub fn parse_element_type(element_type: Option<&str>) -> VectorElementType {
    match element_type {
        Some("int8") => VectorElementType::Int8,
        Some("bit") => VectorElementType::Bit,
        _ => VectorElementType::Float32,
    }
}

pub fn parse_metric(metric: Option<&str>) -> VectorMetric {
    match metric {
        Some("l2") => VectorMetric::L2,
        Some("dot") => VectorMetric::Dot,
        Some("hamming") => VectorMetric::Hamming,
        _ => VectorMetric::Cosine,
    }
}

pub fn parse_conflict_strategy(strategy: Option<&str>) -> ConflictResolutionStrategy {
    match strategy {
        Some("three_way_merge" | "threeWayMerge") => ConflictResolutionStrategy::ThreeWayMerge,
        Some("manual") => ConflictResolutionStrategy::Manual,
        _ => ConflictResolutionStrategy::LastWriteWins,
    }
}

pub fn parse_centrality(centrality: &str) -> Result<CentralityType> {
    match centrality {
        "betweenness" => Ok(CentralityType::Betweenness),
        "closeness" => Ok(CentralityType::Closeness),
        "degree" => Ok(CentralityType::Degree),
        other => Err(invalid_arg(format!("unknown centrality type '{other}'"))),
    }
}

pub fn parse_component_type(component_type: &str) -> Result<ComponentType> {
    match component_type {
        "strongly_connected" | "stronglyConnected" => Ok(ComponentType::StronglyConnected),
        "weakly_connected" | "weaklyConnected" => Ok(ComponentType::WeaklyConnected),
        other => Err(invalid_arg(format!("unknown component type '{other}'"))),
    }
}

pub fn parse_aggregation(aggregation: Option<&str>) -> Result<DownsampleAggregation> {
    match aggregation.unwrap_or("avg").to_ascii_lowercase().as_str() {
        "avg" => Ok(DownsampleAggregation::Avg),
        "min" => Ok(DownsampleAggregation::Min),
        "max" => Ok(DownsampleAggregation::Max),
        "sum" => Ok(DownsampleAggregation::Sum),
        "first" => Ok(DownsampleAggregation::First),
        "last" => Ok(DownsampleAggregation::Last),
        "count" => Ok(DownsampleAggregation::Count),
        other => Err(invalid_arg(format!(
            "unknown downsample aggregation '{other}'"
        ))),
    }
}

pub fn current_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
pub fn build_database_options(options: NativeOpenOptions) -> Result<DatabaseOptions> {
    let mut settings = DatabaseOptions::durable(options.path);
    if let Some(profile) = options.profile.as_deref() {
        let parsed = match profile {
            "mobile" => ResourceProfile::Mobile,
            "constrained" => ResourceProfile::Constrained,
            "server" => ResourceProfile::Server,
            other => return Err(invalid_arg(format!("unknown resource profile '{other}'"))),
        };
        settings = settings.with_profile(parsed);
    }
    if let Some(durability) = options.durability.as_deref() {
        settings.durability = match durability {
            "balanced" => Durability::Balanced,
            "performance" => Durability::Performance,
            _ => Durability::Durable,
        };
    }
    if let Some(value) = options.busy_timeout_ms {
        settings.busy_timeout_ms = u64::from(value);
    }
    if let Some(value) = options.writer_queue_capacity {
        settings.writer_queue_capacity = value as usize;
    }
    if let Some(value) = options.read_pool_size {
        settings.read_pool_size = value as usize;
    }
    if let Some(bytes) = options.mmap_size_bytes {
        let bytes =
            u64::try_from(bytes).map_err(|_| invalid_arg("mmap_size_bytes must be >= 0"))?;
        settings = settings.with_mmap_size(bytes);
    }
    if let Some(kib) = options.cache_size_kib {
        settings = settings.with_cache_size_kib(kib);
    }
    if let Some(cap) = options.undo_log_capacity {
        settings = settings.with_undo_log_capacity(cap as usize);
    }
    if let Some(policy) = options.checkpoint_policy {
        if let Some(value) = policy.wal_bytes {
            settings.checkpoint_policy.wal_bytes = u64::from(value);
        }
        if let Some(value) = policy.interval_ms {
            settings.checkpoint_policy.interval_ms = u64::from(value);
        }
    }
    if let Some(json_opts) = options.json_options {
        if let Some(v) = json_opts.binary_storage {
            settings.json_options.binary_storage = v;
        }
        if let Some(v) = json_opts.auto_train_dict {
            settings.json_options.auto_train_dict = v;
        }
        if let Some(v) = json_opts.dict_sample_threshold {
            settings.json_options.dict_sample_threshold = v as usize;
        }
        if let Some(v) = json_opts.dict_target_size_bytes {
            settings.json_options.dict_target_size_bytes = v as usize;
        }
        if let Some(v) = json_opts.disabled_auto_train_spaces {
            settings.json_options.disabled_auto_train_spaces = v;
        }
    }
    if let Some(ts_opts) = options.timeseries_options {
        if let Some(v) = ts_opts.auto_retention {
            settings.timeseries_options.auto_retention = v;
        }
        if let Some(v) = ts_opts.retention_interval_ms {
            settings.timeseries_options.retention_interval_ms =
                u64::try_from(v).map_err(|_| invalid_arg("retention_interval_ms must be >= 0"))?;
        }
    }
    if let Some(node_id) = options.node_id {
        settings = settings.with_node_id(node_id);
    }
    if let Some(max_drift_ms) = options.max_drift_ms {
        settings = settings.with_max_drift(max_drift_ms);
    }
    if let Some(key) = options.encryption_key {
        settings = settings.with_codec(Arc::new(lilia_core::Aes256GcmCodec::from_passphrase(&key)));
    }
    if let Some(cas_opts) = options.cas_options {
        if let Some(enabled) = cas_opts.enabled {
            settings.cas_options.enabled = enabled;
        }
        if let Some(threshold) = cas_opts.threshold_bytes {
            settings.cas_options.threshold_bytes = threshold as usize;
        }
        if let Some(algo) = cas_opts.hash_algorithm.as_deref() {
            settings.cas_options.hash_algorithm = match algo.to_ascii_lowercase().as_str() {
                "blake3" => lilia_core::cas::CasHashAlgorithm::Blake3,
                _ => lilia_core::cas::CasHashAlgorithm::Sha256,
            };
        }
        if let Some(dir) = cas_opts.blobs_dir {
            settings.cas_options.blobs_dir = Some(std::path::PathBuf::from(dir));
        }
        if let Some(auto_purge) = cas_opts.auto_purge_on_checkpoint {
            settings.cas_options.auto_purge_on_checkpoint = auto_purge;
        }
    }
    if let Some(sharding) = options.sharding {
        let mut config = lilia_core::sharding::ShardingConfig::new();
        if let Some(name) = sharding.granularity.as_deref() {
            let granularity =
                lilia_core::sharding::ShardGranularity::parse(name).ok_or_else(|| {
                    napi::Error::from_reason(format!(
                        "invalid sharding granularity '{name}': expected monthly, daily or yearly"
                    ))
                })?;
            config = config.with_granularity(granularity);
        }
        if let Some(auto_rollover) = sharding.auto_rollover {
            config = config.with_auto_rollover(auto_rollover);
        }
        if let Some(max) = sharding.max_attached_shards {
            config = config.with_max_attached_shards(max as usize);
        }
        if let Some(dir) = sharding.shards_dir {
            config = config.with_shards_dir(dir);
        }
        if let Some(level) = sharding.zstd_level {
            config = config.with_zstd_level(level);
        }
        if let Some(auto) = sharding.auto_archive_oldest {
            config = config.with_auto_archive_oldest(auto);
        }
        settings = settings.with_sharding(config);
    }
    Ok(settings)
}

pub fn kv_to_native(entry: KvEntry) -> NativeKvEntry {
    NativeKvEntry {
        namespace: entry.namespace,
        key: entry.key.into(),
        value: entry.value.into(),
        version: entry.version.to_string(),
        expires_at_ms: entry.expires_at_ms,
    }
}

pub fn fts_hits_to_native(hits: Vec<FtsHit>) -> Vec<NativeFtsHit> {
    hits.into_iter()
        .map(|h| NativeFtsHit {
            collection: h.collection,
            doc_id: h.doc_id,
            snippet: h.snippet,
            rank: h.rank,
        })
        .collect()
}

pub fn vector_hits_to_native(hits: Vec<VectorHit>) -> Vec<NativeVectorHit> {
    hits.into_iter()
        .map(|h| NativeVectorHit {
            collection: h.collection,
            id: h.id,
            distance: h.distance,
        })
        .collect()
}

pub fn opt_json_string<T: Serialize>(opt: Option<&T>) -> Result<Option<String>> {
    opt.map(to_json_string).transpose()
}

// Shared synchronous core operations invoked by both async (`run_async`) and `*Sync` N-API methods.

pub fn kv_get(db: &Database, namespace: &str, key: &[u8]) -> Result<Option<NativeKvEntry>> {
    db.kv_get(namespace, key)
        .map(|e| e.map(kv_to_native))
        .map_err(napi_error)
}

pub fn kv_scan(
    db: &Database,
    namespace: &str,
    after: Option<&[u8]>,
    limit: u32,
) -> Result<Vec<NativeKvEntry>> {
    db.kv_scan(namespace, after, limit)
        .map(|entries| entries.into_iter().map(kv_to_native).collect())
        .map_err(napi_error)
}

pub fn json_get(db: &Database, space: &str, id: &str) -> Result<Option<String>> {
    let entry = db.json_get(space, id).map_err(napi_error)?;
    opt_json_string(entry.as_ref())
}

pub fn json_extract(db: &Database, space: &str, id: &str, path: &str) -> Result<Option<String>> {
    let val = db.json_extract(space, id, path).map_err(napi_error)?;
    opt_json_string(val.as_ref())
}

pub fn json_find(
    db: &Database,
    space: &str,
    query_json: &str,
    after: Option<&str>,
    limit: u32,
) -> Result<String> {
    let query_val: serde_json::Value = from_json_str(query_json)?;
    let query_map = query_val
        .as_object()
        .ok_or_else(|| invalid_arg("query must be a JSON object"))?;
    let entries = db
        .json_find(space, query_map, after, limit)
        .map_err(napi_error)?;
    to_json_string(&entries)
}

pub fn json_get_at_version(
    db: &Database,
    space: &str,
    id: &str,
    version: &str,
) -> Result<Option<String>> {
    let ver = parse_u64(version, "version")?;
    let entry = db.json_get_at_version(space, id, ver).map_err(napi_error)?;
    opt_json_string(entry.as_ref())
}

pub fn json_diff(
    db: &Database,
    space: &str,
    id: &str,
    from_version: &str,
    to_version: &str,
) -> Result<String> {
    let from = parse_u64(from_version, "fromVersion")?;
    let to = parse_u64(to_version, "toVersion")?;
    let diff = db.json_diff(space, id, from, to).map_err(napi_error)?;
    to_json_string(&diff)
}

pub fn json_put_hlc(
    db: &Database,
    space: &str,
    id: &str,
    value_json: &str,
    hlc: Option<&str>,
    parent_hlc: Option<&str>,
    strategy: Option<&str>,
) -> Result<String> {
    let value: serde_json::Value = from_json_str(value_json)?;
    let parsed_hlc = hlc.map(parse_hlc).transpose()?;
    let parsed_parent = parent_hlc.map(parse_hlc).transpose()?;
    let strat = parse_conflict_strategy(strategy);
    let outcome = db
        .json_put_hlc(space, id, value, parsed_hlc, parsed_parent, strat)
        .map_err(napi_error)?;
    to_json_string(&outcome)
}

pub fn batch_json(db: &Database, operations_json: &str) -> Result<String> {
    let operations: Vec<BatchOperation> = from_json_str(operations_json)?;
    let result = db.batch(&operations).map_err(napi_error)?;
    to_json_string(&result)
}

pub fn batch_msgpack(db: &Database, payload: &[u8]) -> Result<Buffer> {
    let operations: Vec<BatchOperation> =
        rmp_serde::from_slice(payload).map_err(|error| invalid_arg(error.to_string()))?;
    let result = db.batch(&operations).map_err(napi_error)?;
    let bytes = rmp_serde::to_vec_named(&result)
        .map_err(|error| Error::new(Status::GenericFailure, error.to_string()))?;
    Ok(Buffer::from(bytes))
}

pub fn sql_query(db: &Database, sql: &str, params_json: Option<&str>) -> Result<String> {
    let params = parse_params(params_json)?;
    let result = db.sql_query(sql, &params).map_err(napi_error)?;
    to_json_string(&result)
}

pub fn graph_centrality(
    db: &Database,
    graph: &str,
    centrality: &str,
    direction: Option<&str>,
    kind_filter: Option<&str>,
    normalized: Option<bool>,
) -> Result<String> {
    let c_type = parse_centrality(centrality)?;
    let dir = parse_direction(direction);
    let store = db.graph(graph);
    let scores = match c_type {
        CentralityType::Betweenness => {
            store.betweenness_centrality(dir, kind_filter, normalized.unwrap_or(false))
        }
        CentralityType::Closeness => store.closeness_centrality(dir, kind_filter),
        CentralityType::Degree => store.degree_centrality(dir, kind_filter),
    }
    .map_err(napi_error)?;
    to_json_string(&scores)
}

pub fn graph_pagerank(db: &Database, graph: &str, config_json: Option<&str>) -> Result<String> {
    let config: PageRankConfig = match config_json {
        Some(s) if !s.trim().is_empty() => from_json_str(s)?,
        _ => PageRankConfig::default(),
    };
    let scores = db.graph(graph).pagerank(&config).map_err(napi_error)?;
    to_json_string(&scores)
}

pub fn graph_connected_components(
    db: &Database,
    graph: &str,
    component_type: &str,
    kind_filter: Option<&str>,
) -> Result<String> {
    let kind = parse_component_type(component_type)?;
    let store = db.graph(graph);
    let comps = match kind {
        ComponentType::StronglyConnected => store.strongly_connected_components(kind_filter),
        ComponentType::WeaklyConnected => store.weakly_connected_components(kind_filter),
    }
    .map_err(napi_error)?;
    to_json_string(&comps)
}

#[allow(clippy::too_many_arguments)]
pub fn hybrid_search(
    db: &Database,
    collection: &str,
    fts_query: Option<&str>,
    vector_query: Option<&[u8]>,
    dim: Option<u32>,
    element_type: Option<&str>,
    metric: Option<&str>,
    rrf_k: Option<u32>,
    fts_weight: Option<f64>,
    vector_weight: Option<f64>,
    limit: Option<u32>,
) -> Result<Vec<NativeHybridHit>> {
    let elem_t = parse_element_type(element_type);
    let met = parse_metric(metric);
    let vec_arg = vector_query.map(|v| (v, dim.unwrap_or(0), elem_t, met));
    let hits = db
        .hybrid_search(
            collection,
            fts_query,
            vec_arg,
            rrf_k.unwrap_or(60),
            fts_weight.unwrap_or(1.0),
            vector_weight.unwrap_or(1.0),
            limit.unwrap_or(10),
        )
        .map_err(napi_error)?;
    Ok(hits
        .into_iter()
        .map(|h| NativeHybridHit {
            id: h.id,
            rrf_score: h.rrf_score,
            fts_rank: h.fts_rank.map(|r| u32::try_from(r).unwrap_or(u32::MAX)),
            fts_snippet: h.fts_snippet,
            vector_rank: h.vector_rank.map(|r| u32::try_from(r).unwrap_or(u32::MAX)),
            vector_distance: h.vector_distance,
        })
        .collect())
}

pub fn vector_create_index(db: &Database, collection: &str, config_json: &str) -> Result<()> {
    let config: HnswConfig = from_json_str(config_json)?;
    db.vector_create_index(collection, config)
        .map_err(napi_error)
}

pub fn ts_append_points(
    db: &Database,
    series_id: &str,
    points_json: &str,
    resolution_ms: Option<i64>,
) -> Result<()> {
    let points: Vec<TimeSeriesPoint> = from_json_str(points_json)?;
    match resolution_ms {
        Some(res) => db.ts_append_chunk(series_id, res, &points),
        None => db.ts_append_points(series_id, &points),
    }
    .map_err(napi_error)
}

pub fn ts_apply_retention(
    db: &Database,
    series_id: Option<&str>,
    policy_json: Option<&str>,
    now_ms: Option<i64>,
) -> Result<String> {
    let stats = match (series_id, policy_json) {
        (Some(sid), Some(pjson)) => {
            let policy: TimeSeriesLifecyclePolicy = from_json_str(pjson)?;
            db.ts_apply_retention_for_series(sid, &policy, now_ms.unwrap_or_else(current_epoch_ms))
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(invalid_arg(
                "ts_apply_retention requires both seriesId and policy together, or neither",
            ));
        }
        (None, None) => match now_ms {
            Some(now) => db.ts_apply_retention_at(now),
            None => db.ts_apply_retention(),
        },
    }
    .map_err(napi_error)?;
    to_json_string(&stats)
}

pub fn geo_record_gps_points(db: &Database, track_id: &str, points_json: &str) -> Result<String> {
    let points: Vec<GpsPoint> = from_json_str(points_json)?;
    let matches = db
        .geo_record_gps_points(track_id, &points)
        .map_err(napi_error)?;
    to_json_string(&matches)
}

pub fn crdt_counter_merge(db: &Database, space: &str, id: &str, remote_json: &str) -> Result<i64> {
    let remote: PNCounter = from_json_str(remote_json)?;
    db.crdt()
        .counter(space, id)
        .merge(&remote)
        .map_err(napi_error)
}

pub fn crdt_set_merge(db: &Database, space: &str, id: &str, remote_json: &str) -> Result<()> {
    let remote: OrSet<String> = from_json_str(remote_json)?;
    db.crdt().set(space, id).merge(&remote).map_err(napi_error)
}

pub fn crdt_json_merge(db: &Database, space: &str, id: &str, remote_json: &str) -> Result<String> {
    let remote: LwwJsonDocument = from_json_str(remote_json)?;
    let version = db
        .crdt()
        .doc(space, id)
        .merge(&remote)
        .map_err(napi_error)?;
    Ok(version.to_string())
}
