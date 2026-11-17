#![allow(
    clippy::missing_errors_doc,
    clippy::needless_pass_by_value,
    clippy::unused_async,
    clippy::needless_continue,
    clippy::too_many_arguments
)]

mod helpers;

use std::sync::{Arc, Mutex};

use lilia_core::timeseries::{downsample_points, TimeSeriesLifecyclePolicy, TimeSeriesPoint};
use lilia_graph::DatabaseGraphExt;
use lilia_storage_sqlite::Database;
use napi::bindgen_prelude::{Buffer, Result};
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;

use helpers::{
    build_database_options, from_json_str, fts_hits_to_native,
    napi_error, opt_json_string, parse_aggregation, parse_direction, parse_element_type,
    parse_hlc, parse_metric, parse_params, run_async, to_json_string, vector_hits_to_native,
};

#[napi]
#[derive(Debug)]
pub struct NativeDatabase {
    inner: Arc<Database>,
}

#[napi]
#[derive(Debug)]
pub struct NativeSubscription {
    cancel_tx: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

#[napi]
impl NativeSubscription {
    #[napi]
    pub fn unsubscribe(&self) {
        if let Ok(mut guard) = self.cancel_tx.lock() {
            if let Some(tx) = guard.take() {
                let _ = tx.send(());
            }
        }
    }
}

#[napi]
#[derive(Debug)]
pub struct NativeLiveQuerySubscription {
    handle: Arc<Mutex<Option<lilia_storage_sqlite::LiveQueryHandle>>>,
}

#[napi]
impl NativeLiveQuerySubscription {
    #[napi]
    pub fn unsubscribe(&self) {
        if let Ok(mut guard) = self.handle.lock() {
            if let Some(h) = guard.take() {
                h.unsubscribe();
            }
        }
    }
}

#[napi(object)]
#[derive(Debug)]
pub struct NativeOpenOptions {
    pub path: String,
    pub durability: Option<String>,
    pub profile: Option<String>,
    pub busy_timeout_ms: Option<u32>,
    pub writer_queue_capacity: Option<u32>,
    pub read_pool_size: Option<u32>,
    pub mmap_size_bytes: Option<i64>,
    pub cache_size_kib: Option<i32>,
    pub undo_log_capacity: Option<u32>,
    pub checkpoint_policy: Option<NativeCheckpointPolicy>,
    pub json_options: Option<NativeJsonOptions>,
    pub timeseries_options: Option<NativeTimeSeriesOptions>,
    pub node_id: Option<String>,
    pub max_drift_ms: Option<i64>,
    pub encryption_key: Option<String>,
    pub cas_options: Option<NativeCasOptions>,
    pub sharding: Option<NativeShardingConfig>,
}

#[napi(object)]
#[derive(Debug)]
pub struct NativeShardingConfig {
    pub granularity: Option<String>,
    pub auto_rollover: Option<bool>,
    pub max_attached_shards: Option<u32>,
    pub shards_dir: Option<String>,
    pub zstd_level: Option<i32>,
    pub auto_archive_oldest: Option<bool>,
}

#[napi(object)]
#[derive(Debug)]
pub struct NativeCasOptions {
    pub enabled: Option<bool>,
    pub threshold_bytes: Option<u32>,
    pub hash_algorithm: Option<String>,
    pub blobs_dir: Option<String>,
    pub auto_purge_on_checkpoint: Option<bool>,
}

#[napi(object)]
#[derive(Debug)]
pub struct NativeCheckpointPolicy {
    pub wal_bytes: Option<u32>,
    pub interval_ms: Option<u32>,
}

#[napi(object)]
#[derive(Debug)]
pub struct NativeJsonOptions {
    pub binary_storage: Option<bool>,
    pub auto_train_dict: Option<bool>,
    pub dict_sample_threshold: Option<u32>,
    pub dict_target_size_bytes: Option<u32>,
    pub disabled_auto_train_spaces: Option<Vec<String>>,
}

#[napi(object)]
#[derive(Debug)]
pub struct NativeTimeSeriesOptions {
    pub auto_retention: Option<bool>,
    pub retention_interval_ms: Option<i64>,
}

#[napi(object)]
pub struct NativeKvEntry {
    pub namespace: String,
    pub key: Buffer,
    pub value: Buffer,
    pub version: String,
    pub expires_at_ms: Option<i64>,
}

impl std::fmt::Debug for NativeKvEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeKvEntry")
            .field("namespace", &self.namespace)
            .field("key_bytes", &self.key.len())
            .field("value_bytes", &self.value.len())
            .field("version", &self.version)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct NativeFtsHit {
    pub collection: String,
    pub doc_id: String,
    pub snippet: String,
    pub rank: f64,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct NativeFtsSuggestion {
    pub term: String,
    pub distance: u32,
    pub doc_count: u32,
    pub exact_prefix: bool,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct NativeFtsFuzzyHit {
    pub collection: String,
    pub doc_id: String,
    pub snippet: String,
    pub rank: f64,
    pub edit_distance: u32,
    pub matched_terms: Vec<String>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct NativeVectorHit {
    pub collection: String,
    pub id: String,
    pub distance: f64,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct NativeHybridHit {
    pub id: String,
    pub rrf_score: f64,
    pub fts_rank: Option<u32>,
    pub fts_snippet: Option<String>,
    pub vector_rank: Option<u32>,
    pub vector_distance: Option<f64>,
}

// Standalone pure utilities mirrored from lilia-core
#[napi]
pub fn ts_downsample_points(
    points_json: String,
    target_bucket_ms: i64,
    aggregation: Option<String>,
) -> Result<String> {
    let points: Vec<TimeSeriesPoint> = from_json_str(&points_json)?;
    let agg = parse_aggregation(aggregation.as_deref())?;
    to_json_string(&downsample_points(&points, target_bucket_ms, agg))
}

#[napi]
#[must_use]
pub fn geo_haversine_distance(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    lilia_core::haversine_distance_meters(lat1, lon1, lat2, lon2)
}

#[napi]
#[must_use]
pub fn geo_circle_to_geopoly(
    center_lon: f64,
    center_lat: f64,
    radius_meters: f64,
    n_vertices: Option<u32>,
) -> String {
    lilia_core::circle_to_geopoly_json(
        center_lon,
        center_lat,
        radius_meters,
        n_vertices.unwrap_or(16) as usize,
    )
}

#[napi]
pub fn geo_coords_to_geopoly(coords_json: String) -> Result<String> {
    let pairs: Vec<[f64; 2]> = from_json_str(&coords_json)?;
    let coords: Vec<(f64, f64)> = pairs.into_iter().map(|[lon, lat]| (lon, lat)).collect();
    Ok(lilia_core::coords_to_geopoly_json(&coords))
}

#[napi]
impl NativeDatabase {
    #[napi(factory)]
    pub async fn open(options: NativeOpenOptions) -> Result<Self> {
        let database = tokio::task::spawn_blocking(move || {
            let settings = build_database_options(options)?;
            Database::open(settings).map_err(napi_error)
        })
        .await
        .map_err(helpers::join_error)??;
        Ok(Self {
            inner: Arc::new(database),
        })
    }

    // -----------------------------------------------------------------------
    // KV
    // -----------------------------------------------------------------------

    #[napi]
    pub async fn kv_get(&self, namespace: String, key: Buffer) -> Result<Option<NativeKvEntry>> {
        run_async(&self.inner, move |db| helpers::kv_get(db, &namespace, &key)).await
    }

    #[napi]
    pub fn kv_get_sync(&self, namespace: String, key: Buffer) -> Result<Option<NativeKvEntry>> {
        helpers::kv_get(&self.inner, &namespace, &key)
    }

    #[napi]
    pub async fn kv_scan(
        &self,
        namespace: String,
        after: Option<Buffer>,
        limit: u32,
    ) -> Result<Vec<NativeKvEntry>> {
        run_async(&self.inner, move |db| {
            helpers::kv_scan(db, &namespace, after.as_deref(), limit)
        })
        .await
    }

    #[napi]
    pub fn kv_scan_sync(
        &self,
        namespace: String,
        after: Option<Buffer>,
        limit: u32,
    ) -> Result<Vec<NativeKvEntry>> {
        helpers::kv_scan(&self.inner, &namespace, after.as_deref(), limit)
    }

    // -----------------------------------------------------------------------
    // JSON (CRUD, Indexing, Query, Prefix Scan, Time-Travel, Zstd, HLC)
    // -----------------------------------------------------------------------

    #[napi]
    pub async fn json_get(&self, space: String, id: String) -> Result<Option<String>> {
        run_async(&self.inner, move |db| helpers::json_get(db, &space, &id)).await
    }

    #[napi]
    pub fn json_get_sync(&self, space: String, id: String) -> Result<Option<String>> {
        helpers::json_get(&self.inner, &space, &id)
    }

    #[napi]
    pub async fn json_extract(
        &self,
        space: String,
        id: String,
        path: String,
    ) -> Result<Option<String>> {
        run_async(&self.inner, move |db| {
            helpers::json_extract(db, &space, &id, &path)
        })
        .await
    }

    #[napi]
    pub fn json_extract_sync(
        &self,
        space: String,
        id: String,
        path: String,
    ) -> Result<Option<String>> {
        helpers::json_extract(&self.inner, &space, &id, &path)
    }

    #[napi]
    pub async fn json_scan(
        &self,
        space: String,
        after: Option<String>,
        limit: u32,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(
                &db.json_scan(&space, after.as_deref(), limit)
                    .map_err(napi_error)?,
            )
        })
        .await
    }

    #[napi]
    pub fn json_scan_sync(
        &self,
        space: String,
        after: Option<String>,
        limit: u32,
    ) -> Result<String> {
        to_json_string(
            &self
                .inner
                .json_scan(&space, after.as_deref(), limit)
                .map_err(napi_error)?,
        )
    }

    #[napi]
    pub async fn json_scan_prefix(
        &self,
        space: String,
        prefix: String,
        after_id: Option<String>,
        limit: u32,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(
                &db.json_scan_prefix(&space, &prefix, after_id.as_deref(), limit)
                    .map_err(napi_error)?,
            )
        })
        .await
    }

    #[napi]
    pub fn json_scan_prefix_sync(
        &self,
        space: String,
        prefix: String,
        after_id: Option<String>,
        limit: u32,
    ) -> Result<String> {
        to_json_string(
            &self
                .inner
                .json_scan_prefix(&space, &prefix, after_id.as_deref(), limit)
                .map_err(napi_error)?,
        )
    }

    #[napi]
    pub async fn json_create_index(
        &self,
        space: String,
        name: String,
        path: String,
        filter: Option<String>,
    ) -> Result<()> {
        run_async(&self.inner, move |db| {
            let options = lilia_core::IndexOptions { filter };
            db.create_json_index_with_options(&space, &name, &path, options)
                .map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn json_create_index_sync(
        &self,
        space: String,
        name: String,
        path: String,
        filter: Option<String>,
    ) -> Result<()> {
        let options = lilia_core::IndexOptions { filter };
        self.inner
            .create_json_index_with_options(&space, &name, &path, options)
            .map_err(napi_error)
    }

    #[napi]
    pub async fn json_drop_index(&self, space: String, name: String) -> Result<bool> {
        run_async(&self.inner, move |db| {
            db.drop_json_index(&space, &name).map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn json_drop_index_sync(&self, space: String, name: String) -> Result<bool> {
        self.inner
            .drop_json_index(&space, &name)
            .map_err(napi_error)
    }

    #[napi]
    pub async fn json_list_indexes(&self, space: String) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(&db.list_json_indexes(&space).map_err(napi_error)?)
        })
        .await
    }

    #[napi]
    pub fn json_list_indexes_sync(&self, space: String) -> Result<String> {
        to_json_string(&self.inner.list_json_indexes(&space).map_err(napi_error)?)
    }

    #[napi]
    pub async fn json_scan_by_index(
        &self,
        space: String,
        index_name: String,
        value: String,
        after: Option<String>,
        limit: u32,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(
                &db.json_scan_by_index(&space, &index_name, &value, after.as_deref(), limit)
                    .map_err(napi_error)?,
            )
        })
        .await
    }

    #[napi]
    pub fn json_scan_by_index_sync(
        &self,
        space: String,
        index_name: String,
        value: String,
        after: Option<String>,
        limit: u32,
    ) -> Result<String> {
        to_json_string(
            &self
                .inner
                .json_scan_by_index(&space, &index_name, &value, after.as_deref(), limit)
                .map_err(napi_error)?,
        )
    }

    #[napi]
    pub async fn json_find(
        &self,
        space: String,
        query_json: String,
        after: Option<String>,
        limit: u32,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            helpers::json_find(db, &space, &query_json, after.as_deref(), limit)
        })
        .await
    }

    #[napi]
    pub fn json_find_sync(
        &self,
        space: String,
        query_json: String,
        after: Option<String>,
        limit: u32,
    ) -> Result<String> {
        helpers::json_find(&self.inner, &space, &query_json, after.as_deref(), limit)
    }

    #[napi]
    pub async fn json_get_at_version(
        &self,
        space: String,
        id: String,
        version: String,
    ) -> Result<Option<String>> {
        run_async(&self.inner, move |db| {
            helpers::json_get_at_version(db, &space, &id, &version)
        })
        .await
    }

    #[napi]
    pub fn json_get_at_version_sync(
        &self,
        space: String,
        id: String,
        version: String,
    ) -> Result<Option<String>> {
        helpers::json_get_at_version(&self.inner, &space, &id, &version)
    }

    #[napi]
    pub async fn json_get_at_timestamp(
        &self,
        space: String,
        id: String,
        timestamp_ms: i64,
    ) -> Result<Option<String>> {
        run_async(&self.inner, move |db| {
            let entry = db
                .json_get_at_timestamp(&space, &id, timestamp_ms)
                .map_err(napi_error)?;
            opt_json_string(entry.as_ref())
        })
        .await
    }

    #[napi]
    pub fn json_get_at_timestamp_sync(
        &self,
        space: String,
        id: String,
        timestamp_ms: i64,
    ) -> Result<Option<String>> {
        let entry = self
            .inner
            .json_get_at_timestamp(&space, &id, timestamp_ms)
            .map_err(napi_error)?;
        opt_json_string(entry.as_ref())
    }

    #[napi]
    pub async fn json_history(&self, space: String, id: String) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(&db.json_history(&space, &id).map_err(napi_error)?)
        })
        .await
    }

    #[napi]
    pub fn json_history_sync(&self, space: String, id: String) -> Result<String> {
        to_json_string(&self.inner.json_history(&space, &id).map_err(napi_error)?)
    }

    #[napi]
    pub async fn json_diff(
        &self,
        space: String,
        id: String,
        from_version: String,
        to_version: String,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            helpers::json_diff(db, &space, &id, &from_version, &to_version)
        })
        .await
    }

    #[napi]
    pub fn json_diff_sync(
        &self,
        space: String,
        id: String,
        from_version: String,
        to_version: String,
    ) -> Result<String> {
        helpers::json_diff(&self.inner, &space, &id, &from_version, &to_version)
    }

    #[napi]
    pub async fn json_train_dictionary(
        &self,
        space: String,
        sample_count: Option<u32>,
        dict_size: Option<u32>,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            let stats = db
                .train_json_dictionary(
                    &space,
                    sample_count.unwrap_or(100) as usize,
                    dict_size.unwrap_or(16_384) as usize,
                )
                .map_err(napi_error)?;
            to_json_string(&stats)
        })
        .await
    }

    #[napi]
    pub fn json_train_dictionary_sync(
        &self,
        space: String,
        sample_count: Option<u32>,
        dict_size: Option<u32>,
    ) -> Result<String> {
        let stats = self
            .inner
            .train_json_dictionary(
                &space,
                sample_count.unwrap_or(100) as usize,
                dict_size.unwrap_or(16_384) as usize,
            )
            .map_err(napi_error)?;
        to_json_string(&stats)
    }

    #[napi]
    pub async fn json_set_auto_train(&self, space: String, enabled: bool) -> Result<()> {
        run_async(&self.inner, move |db| {
            db.set_space_auto_train(&space, enabled).map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn json_set_auto_train_sync(&self, space: String, enabled: bool) -> Result<()> {
        self.inner
            .set_space_auto_train(&space, enabled)
            .map_err(napi_error)
    }

    #[napi]
    pub async fn json_is_auto_train_enabled(&self, space: String) -> Result<bool> {
        run_async(&self.inner, move |db| Ok(db.is_space_auto_train_enabled(&space))).await
    }

    #[napi]
    #[must_use]
    pub fn json_is_auto_train_enabled_sync(&self, space: String) -> bool {
        self.inner.is_space_auto_train_enabled(&space)
    }

    #[napi]
    pub async fn json_put_hlc(
        &self,
        space: String,
        id: String,
        value_json: String,
        hlc: Option<String>,
        parent_hlc: Option<String>,
        strategy: Option<String>,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            helpers::json_put_hlc(
                db,
                &space,
                &id,
                &value_json,
                hlc.as_deref(),
                parent_hlc.as_deref(),
                strategy.as_deref(),
            )
        })
        .await
    }

    #[napi]
    pub fn json_put_hlc_sync(
        &self,
        space: String,
        id: String,
        value_json: String,
        hlc: Option<String>,
        parent_hlc: Option<String>,
        strategy: Option<String>,
    ) -> Result<String> {
        helpers::json_put_hlc(
            &self.inner,
            &space,
            &id,
            &value_json,
            hlc.as_deref(),
            parent_hlc.as_deref(),
            strategy.as_deref(),
        )
    }

    #[napi]
    pub async fn json_get_hlc(&self, space: String, id: String) -> Result<Option<String>> {
        run_async(&self.inner, move |db| {
            let rec = db.json_get_hlc(&space, &id).map_err(napi_error)?;
            opt_json_string(rec.as_ref())
        })
        .await
    }

    #[napi]
    pub fn json_get_hlc_sync(&self, space: String, id: String) -> Result<Option<String>> {
        let rec = self.inner.json_get_hlc(&space, &id).map_err(napi_error)?;
        opt_json_string(rec.as_ref())
    }

    // -----------------------------------------------------------------------
    // Batch & SQL
    // -----------------------------------------------------------------------

    #[napi]
    pub async fn batch(&self, operations_json: String) -> Result<String> {
        run_async(&self.inner, move |db| helpers::batch_json(db, &operations_json)).await
    }

    #[napi]
    pub fn batch_sync(&self, operations_json: String) -> Result<String> {
        helpers::batch_json(&self.inner, &operations_json)
    }

    #[napi]
    pub async fn batch_msgpack(&self, payload: Buffer) -> Result<Buffer> {
        run_async(&self.inner, move |db| helpers::batch_msgpack(db, &payload)).await
    }

    #[napi]
    pub fn batch_msgpack_sync(&self, payload: Buffer) -> Result<Buffer> {
        helpers::batch_msgpack(&self.inner, &payload)
    }

    #[napi]
    pub async fn sql_query(&self, sql: String, params_json: Option<String>) -> Result<String> {
        run_async(&self.inner, move |db| {
            helpers::sql_query(db, &sql, params_json.as_deref())
        })
        .await
    }

    #[napi]
    pub fn sql_query_sync(&self, sql: String, params_json: Option<String>) -> Result<String> {
        helpers::sql_query(&self.inner, &sql, params_json.as_deref())
    }

    // -----------------------------------------------------------------------
    // Graph CRUD, Traversal, Algorithms & Cypher
    // -----------------------------------------------------------------------

    #[napi]
    pub async fn graph_neighbors(
        &self,
        graph: String,
        node: String,
        direction: Option<String>,
        kind: Option<String>,
    ) -> Result<Vec<String>> {
        run_async(&self.inner, move |db| {
            db.graph_neighbors(&graph, &node, parse_direction(direction.as_deref()), kind.as_deref())
                .map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn graph_neighbors_sync(
        &self,
        graph: String,
        node: String,
        direction: Option<String>,
        kind: Option<String>,
    ) -> Result<Vec<String>> {
        self.inner
            .graph_neighbors(
                &graph,
                &node,
                parse_direction(direction.as_deref()),
                kind.as_deref(),
            )
            .map_err(napi_error)
    }

    #[napi]
    pub async fn graph_reachability(
        &self,
        graph: String,
        start_nodes: Vec<String>,
        direction: Option<String>,
    ) -> Result<Vec<String>> {
        run_async(&self.inner, move |db| {
            db.graph_reachability(&graph, &start_nodes, parse_direction(direction.as_deref()))
                .map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn graph_reachability_sync(
        &self,
        graph: String,
        start_nodes: Vec<String>,
        direction: Option<String>,
    ) -> Result<Vec<String>> {
        self.inner
            .graph_reachability(&graph, &start_nodes, parse_direction(direction.as_deref()))
            .map_err(napi_error)
    }

    #[napi]
    pub async fn graph_get_node(&self, graph: String, id: String) -> Result<Option<String>> {
        run_async(&self.inner, move |db| {
            let node = db.graph_get_node(&graph, &id).map_err(napi_error)?;
            opt_json_string(node.as_ref())
        })
        .await
    }

    #[napi]
    pub fn graph_get_node_sync(&self, graph: String, id: String) -> Result<Option<String>> {
        let node = self.inner.graph_get_node(&graph, &id).map_err(napi_error)?;
        opt_json_string(node.as_ref())
    }

    #[napi]
    pub async fn graph_get_node_document(
        &self,
        graph: String,
        id: String,
    ) -> Result<Option<String>> {
        run_async(&self.inner, move |db| {
            let doc = db.graph_get_node_document(&graph, &id).map_err(napi_error)?;
            opt_json_string(doc.as_ref())
        })
        .await
    }

    #[napi]
    pub fn graph_get_node_document_sync(
        &self,
        graph: String,
        id: String,
    ) -> Result<Option<String>> {
        let doc = self
            .inner
            .graph_get_node_document(&graph, &id)
            .map_err(napi_error)?;
        opt_json_string(doc.as_ref())
    }

    #[napi]
    pub async fn graph_delete_node(&self, graph: String, id: String) -> Result<bool> {
        run_async(&self.inner, move |db| {
            db.graph_delete_node(&graph, &id).map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn graph_delete_node_sync(&self, graph: String, id: String) -> Result<bool> {
        self.inner
            .graph_delete_node(&graph, &id)
            .map_err(napi_error)
    }

    #[napi]
    pub async fn graph_list_nodes(
        &self,
        graph: String,
        kind: Option<String>,
        limit: Option<u32>,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(
                &db.graph_list_nodes(&graph, kind.as_deref(), limit.unwrap_or(100))
                    .map_err(napi_error)?,
            )
        })
        .await
    }

    #[napi]
    pub fn graph_list_nodes_sync(
        &self,
        graph: String,
        kind: Option<String>,
        limit: Option<u32>,
    ) -> Result<String> {
        to_json_string(
            &self
                .inner
                .graph_list_nodes(&graph, kind.as_deref(), limit.unwrap_or(100))
                .map_err(napi_error)?,
        )
    }

    #[napi]
    pub async fn graph_remove_edge(
        &self,
        graph: String,
        source: String,
        target: String,
        kind: String,
    ) -> Result<bool> {
        run_async(&self.inner, move |db| {
            db.graph_remove_edge(&graph, &source, &target, &kind)
                .map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn graph_remove_edge_sync(
        &self,
        graph: String,
        source: String,
        target: String,
        kind: String,
    ) -> Result<bool> {
        self.inner
            .graph_remove_edge(&graph, &source, &target, &kind)
            .map_err(napi_error)
    }

    #[napi]
    pub async fn graph_list_edges(
        &self,
        graph: String,
        source: Option<String>,
        target: Option<String>,
        kind: Option<String>,
        limit: Option<u32>,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(
                &db.graph_list_edges(
                    &graph,
                    source.as_deref(),
                    target.as_deref(),
                    kind.as_deref(),
                    limit.unwrap_or(100),
                )
                .map_err(napi_error)?,
            )
        })
        .await
    }

    #[napi]
    pub fn graph_list_edges_sync(
        &self,
        graph: String,
        source: Option<String>,
        target: Option<String>,
        kind: Option<String>,
        limit: Option<u32>,
    ) -> Result<String> {
        to_json_string(
            &self
                .inner
                .graph_list_edges(
                    &graph,
                    source.as_deref(),
                    target.as_deref(),
                    kind.as_deref(),
                    limit.unwrap_or(100),
                )
                .map_err(napi_error)?,
        )
    }

    #[napi]
    pub async fn graph_impact_analysis(
        &self,
        graph: String,
        changed_nodes: Vec<String>,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            let analysis = db
                .graph(&graph)
                .impact_analysis(&changed_nodes)
                .map_err(napi_error)?;
            to_json_string(&analysis)
        })
        .await
    }

    #[napi]
    pub fn graph_impact_analysis_sync(
        &self,
        graph: String,
        changed_nodes: Vec<String>,
    ) -> Result<String> {
        let analysis = self
            .inner
            .graph(&graph)
            .impact_analysis(&changed_nodes)
            .map_err(napi_error)?;
        to_json_string(&analysis)
    }

    #[napi]
    pub async fn graph_shortest_path(
        &self,
        graph: String,
        start: String,
        target: String,
        direction: Option<String>,
        kind_filter: Option<String>,
    ) -> Result<Option<String>> {
        run_async(&self.inner, move |db| {
            let path = db
                .graph(&graph)
                .shortest_path(
                    &start,
                    &target,
                    parse_direction(direction.as_deref()),
                    kind_filter.as_deref(),
                )
                .map_err(napi_error)?;
            opt_json_string(path.as_ref())
        })
        .await
    }

    #[napi]
    pub fn graph_shortest_path_sync(
        &self,
        graph: String,
        start: String,
        target: String,
        direction: Option<String>,
        kind_filter: Option<String>,
    ) -> Result<Option<String>> {
        let path = self
            .inner
            .graph(&graph)
            .shortest_path(
                &start,
                &target,
                parse_direction(direction.as_deref()),
                kind_filter.as_deref(),
            )
            .map_err(napi_error)?;
        opt_json_string(path.as_ref())
    }

    #[napi]
    pub async fn graph_dijkstra(
        &self,
        graph: String,
        start: String,
        target: Option<String>,
        direction: Option<String>,
        kind_filter: Option<String>,
    ) -> Result<Option<String>> {
        run_async(&self.inner, move |db| {
            let path = db
                .graph(&graph)
                .dijkstra(
                    &start,
                    target.as_deref(),
                    parse_direction(direction.as_deref()),
                    kind_filter.as_deref(),
                )
                .map_err(napi_error)?;
            opt_json_string(path.as_ref())
        })
        .await
    }

    #[napi]
    pub fn graph_dijkstra_sync(
        &self,
        graph: String,
        start: String,
        target: Option<String>,
        direction: Option<String>,
        kind_filter: Option<String>,
    ) -> Result<Option<String>> {
        let path = self
            .inner
            .graph(&graph)
            .dijkstra(
                &start,
                target.as_deref(),
                parse_direction(direction.as_deref()),
                kind_filter.as_deref(),
            )
            .map_err(napi_error)?;
        opt_json_string(path.as_ref())
    }

    #[napi]
    pub async fn graph_all_shortest_paths(
        &self,
        graph: String,
        start: String,
        direction: Option<String>,
        kind_filter: Option<String>,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            let paths = db
                .graph(&graph)
                .all_shortest_paths(
                    &start,
                    parse_direction(direction.as_deref()),
                    kind_filter.as_deref(),
                )
                .map_err(napi_error)?;
            to_json_string(&paths)
        })
        .await
    }

    #[napi]
    pub fn graph_all_shortest_paths_sync(
        &self,
        graph: String,
        start: String,
        direction: Option<String>,
        kind_filter: Option<String>,
    ) -> Result<String> {
        let paths = self
            .inner
            .graph(&graph)
            .all_shortest_paths(
                &start,
                parse_direction(direction.as_deref()),
                kind_filter.as_deref(),
            )
            .map_err(napi_error)?;
        to_json_string(&paths)
    }

    #[napi]
    pub async fn graph_astar_spatial(
        &self,
        graph: String,
        start: String,
        target: String,
        direction: Option<String>,
        kind_filter: Option<String>,
    ) -> Result<Option<String>> {
        run_async(&self.inner, move |db| {
            let path = db
                .graph(&graph)
                .astar_spatial(
                    &start,
                    &target,
                    parse_direction(direction.as_deref()),
                    kind_filter.as_deref(),
                )
                .map_err(napi_error)?;
            opt_json_string(path.as_ref())
        })
        .await
    }

    #[napi]
    pub fn graph_astar_spatial_sync(
        &self,
        graph: String,
        start: String,
        target: String,
        direction: Option<String>,
        kind_filter: Option<String>,
    ) -> Result<Option<String>> {
        let path = self
            .inner
            .graph(&graph)
            .astar_spatial(
                &start,
                &target,
                parse_direction(direction.as_deref()),
                kind_filter.as_deref(),
            )
            .map_err(napi_error)?;
        opt_json_string(path.as_ref())
    }

    #[napi]
    pub async fn graph_pagerank(
        &self,
        graph: String,
        config_json: Option<String>,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            helpers::graph_pagerank(db, &graph, config_json.as_deref())
        })
        .await
    }

    #[napi]
    pub fn graph_pagerank_sync(
        &self,
        graph: String,
        config_json: Option<String>,
    ) -> Result<String> {
        helpers::graph_pagerank(&self.inner, &graph, config_json.as_deref())
    }

    #[napi]
    pub async fn graph_centrality(
        &self,
        graph: String,
        centrality: String,
        direction: Option<String>,
        kind_filter: Option<String>,
        normalized: Option<bool>,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            helpers::graph_centrality(
                db,
                &graph,
                &centrality,
                direction.as_deref(),
                kind_filter.as_deref(),
                normalized,
            )
        })
        .await
    }

    #[napi]
    pub fn graph_centrality_sync(
        &self,
        graph: String,
        centrality: String,
        direction: Option<String>,
        kind_filter: Option<String>,
        normalized: Option<bool>,
    ) -> Result<String> {
        helpers::graph_centrality(
            &self.inner,
            &graph,
            &centrality,
            direction.as_deref(),
            kind_filter.as_deref(),
            normalized,
        )
    }

    #[napi]
    pub async fn graph_has_cycle(
        &self,
        graph: String,
        direction: Option<String>,
        kind_filter: Option<String>,
    ) -> Result<bool> {
        run_async(&self.inner, move |db| {
            db.graph(&graph)
                .has_cycle(parse_direction(direction.as_deref()), kind_filter.as_deref())
                .map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn graph_has_cycle_sync(
        &self,
        graph: String,
        direction: Option<String>,
        kind_filter: Option<String>,
    ) -> Result<bool> {
        self.inner
            .graph(&graph)
            .has_cycle(parse_direction(direction.as_deref()), kind_filter.as_deref())
            .map_err(napi_error)
    }

    #[napi]
    pub async fn graph_find_cycles(
        &self,
        graph: String,
        max_cycles: Option<u32>,
        kind_filter: Option<String>,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            let cycles = db
                .graph(&graph)
                .find_cycles(max_cycles.unwrap_or(100) as usize, kind_filter.as_deref())
                .map_err(napi_error)?;
            to_json_string(&cycles)
        })
        .await
    }

    #[napi]
    pub fn graph_find_cycles_sync(
        &self,
        graph: String,
        max_cycles: Option<u32>,
        kind_filter: Option<String>,
    ) -> Result<String> {
        let cycles = self
            .inner
            .graph(&graph)
            .find_cycles(max_cycles.unwrap_or(100) as usize, kind_filter.as_deref())
            .map_err(napi_error)?;
        to_json_string(&cycles)
    }

    #[napi]
    pub async fn graph_connected_components(
        &self,
        graph: String,
        component_type: String,
        kind_filter: Option<String>,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            helpers::graph_connected_components(db, &graph, &component_type, kind_filter.as_deref())
        })
        .await
    }

    #[napi]
    pub fn graph_connected_components_sync(
        &self,
        graph: String,
        component_type: String,
        kind_filter: Option<String>,
    ) -> Result<String> {
        helpers::graph_connected_components(
            &self.inner,
            &graph,
            &component_type,
            kind_filter.as_deref(),
        )
    }

    #[napi]
    pub async fn graph_cypher(&self, graph: String, query: String) -> Result<String> {
        run_async(&self.inner, move |db| {
            let res = db.graph(&graph).cypher(&query).map_err(napi_error)?;
            to_json_string(&res)
        })
        .await
    }

    #[napi]
    pub fn graph_cypher_sync(&self, graph: String, query: String) -> Result<String> {
        let res = self.inner.graph(&graph).cypher(&query).map_err(napi_error)?;
        to_json_string(&res)
    }

    // -----------------------------------------------------------------------
    // FTS, Vector, HNSW & Hybrid
    // -----------------------------------------------------------------------

    #[napi]
    pub async fn fts_delete(&self, collection: String, doc_id: String) -> Result<bool> {
        run_async(&self.inner, move |db| {
            db.fts_delete(&collection, &doc_id).map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn fts_delete_sync(&self, collection: String, doc_id: String) -> Result<bool> {
        self.inner
            .fts_delete(&collection, &doc_id)
            .map_err(napi_error)
    }

    #[napi]
    pub async fn fts_search(
        &self,
        collection: String,
        query: String,
        limit: Option<u32>,
    ) -> Result<Vec<NativeFtsHit>> {
        run_async(&self.inner, move |db| {
            Ok(fts_hits_to_native(
                db.fts_search(&collection, &query, limit.unwrap_or(10))
                    .map_err(napi_error)?,
            ))
        })
        .await
    }

    #[napi]
    pub fn fts_search_sync(
        &self,
        collection: String,
        query: String,
        limit: Option<u32>,
    ) -> Result<Vec<NativeFtsHit>> {
        Ok(fts_hits_to_native(
            self.inner
                .fts_search(&collection, &query, limit.unwrap_or(10))
                .map_err(napi_error)?,
        ))
    }

    #[napi]
    pub async fn fts_suggest(
        &self,
        collection: String,
        prefix: String,
        max_distance: Option<u32>,
        limit: Option<u32>,
    ) -> Result<Vec<NativeFtsSuggestion>> {
        run_async(&self.inner, move |db| {
            let suggestions = db
                .fts_suggest(
                    &collection,
                    &prefix,
                    max_distance.unwrap_or(1) as u8,
                    limit.unwrap_or(10),
                )
                .map_err(napi_error)?;
            Ok(suggestions
                .into_iter()
                .map(|s| NativeFtsSuggestion {
                    term: s.term,
                    distance: u32::from(s.distance),
                    doc_count: s.doc_count,
                    exact_prefix: s.exact_prefix,
                })
                .collect())
        })
        .await
    }

    #[napi]
    pub fn fts_suggest_sync(
        &self,
        collection: String,
        prefix: String,
        max_distance: Option<u32>,
        limit: Option<u32>,
    ) -> Result<Vec<NativeFtsSuggestion>> {
        let suggestions = self
            .inner
            .fts_suggest(
                &collection,
                &prefix,
                max_distance.unwrap_or(1) as u8,
                limit.unwrap_or(10),
            )
            .map_err(napi_error)?;
        Ok(suggestions
            .into_iter()
            .map(|s| NativeFtsSuggestion {
                term: s.term,
                distance: u32::from(s.distance),
                doc_count: s.doc_count,
                exact_prefix: s.exact_prefix,
            })
            .collect())
    }

    #[napi]
    pub async fn fts_fuzzy_search(
        &self,
        collection: String,
        query: String,
        max_distance: Option<u32>,
        limit: Option<u32>,
    ) -> Result<Vec<NativeFtsFuzzyHit>> {
        run_async(&self.inner, move |db| {
            let hits = db
                .fts_fuzzy_search(
                    &collection,
                    &query,
                    max_distance.unwrap_or(1) as u8,
                    limit.unwrap_or(10),
                )
                .map_err(napi_error)?;
            Ok(hits
                .into_iter()
                .map(|h| NativeFtsFuzzyHit {
                    collection: h.collection,
                    doc_id: h.doc_id,
                    snippet: h.snippet,
                    rank: h.rank,
                    edit_distance: u32::from(h.edit_distance),
                    matched_terms: h.matched_terms,
                })
                .collect())
        })
        .await
    }

    #[napi]
    pub fn fts_fuzzy_search_sync(
        &self,
        collection: String,
        query: String,
        max_distance: Option<u32>,
        limit: Option<u32>,
    ) -> Result<Vec<NativeFtsFuzzyHit>> {
        let hits = self
            .inner
            .fts_fuzzy_search(
                &collection,
                &query,
                max_distance.unwrap_or(1) as u8,
                limit.unwrap_or(10),
            )
            .map_err(napi_error)?;
        Ok(hits
            .into_iter()
            .map(|h| NativeFtsFuzzyHit {
                collection: h.collection,
                doc_id: h.doc_id,
                snippet: h.snippet,
                rank: h.rank,
                edit_distance: u32::from(h.edit_distance),
                matched_terms: h.matched_terms,
            })
            .collect())
    }

    #[napi]
    pub fn fts_load_dictionary_sync(
        &self,
        collection: String,
        words: Vec<String>,
    ) -> Result<()> {
        let refs: Vec<&str> = words.iter().map(String::as_str).collect();
        self.inner
            .fts_load_dictionary(&collection, &refs)
            .map_err(napi_error)?;
        Ok(())
    }

    #[napi]
    pub async fn vector_get(&self, collection: String, id: String) -> Result<Option<Buffer>> {
        run_async(&self.inner, move |db| {
            let res = db.vector_get(&collection, &id).map_err(napi_error)?;
            Ok(res.map(|(_, _, bytes)| Buffer::from(bytes)))
        })
        .await
    }

    #[napi]
    pub fn vector_get_sync(&self, collection: String, id: String) -> Result<Option<Buffer>> {
        let res = self.inner.vector_get(&collection, &id).map_err(napi_error)?;
        Ok(res.map(|(_, _, bytes)| Buffer::from(bytes)))
    }

    #[napi]
    pub async fn vector_search(
        &self,
        collection: String,
        query: Buffer,
        dim: u32,
        element_type: Option<String>,
        metric: Option<String>,
        limit: Option<u32>,
    ) -> Result<Vec<NativeVectorHit>> {
        run_async(&self.inner, move |db| {
            let hits = db
                .vector_search(
                    &collection,
                    query.as_ref(),
                    dim,
                    parse_element_type(element_type.as_deref()),
                    parse_metric(metric.as_deref()),
                    limit.unwrap_or(10),
                )
                .map_err(napi_error)?;
            Ok(vector_hits_to_native(hits))
        })
        .await
    }

    #[napi]
    pub fn vector_search_sync(
        &self,
        collection: String,
        query: Buffer,
        dim: u32,
        element_type: Option<String>,
        metric: Option<String>,
        limit: Option<u32>,
    ) -> Result<Vec<NativeVectorHit>> {
        let hits = self
            .inner
            .vector_search(
                &collection,
                query.as_ref(),
                dim,
                parse_element_type(element_type.as_deref()),
                parse_metric(metric.as_deref()),
                limit.unwrap_or(10),
            )
            .map_err(napi_error)?;
        Ok(vector_hits_to_native(hits))
    }

    #[napi]
    pub async fn vector_search_text(
        &self,
        collection: String,
        text: String,
        limit: Option<u32>,
    ) -> Result<Vec<NativeVectorHit>> {
        run_async(&self.inner, move |db| {
            let hits = db
                .vector_search_text(&collection, &text, limit.unwrap_or(10))
                .map_err(napi_error)?;
            Ok(vector_hits_to_native(hits))
        })
        .await
    }

    #[napi]
    pub fn vector_search_text_sync(
        &self,
        collection: String,
        text: String,
        limit: Option<u32>,
    ) -> Result<Vec<NativeVectorHit>> {
        let hits = self
            .inner
            .vector_search_text(&collection, &text, limit.unwrap_or(10))
            .map_err(napi_error)?;
        Ok(vector_hits_to_native(hits))
    }

    #[napi]
    pub async fn wait_for_embeddings(&self, timeout_ms: Option<u32>) -> Result<()> {
        let duration = std::time::Duration::from_millis(u64::from(timeout_ms.unwrap_or(60_000)));
        run_async(&self.inner, move |db| {
            db.wait_for_embeddings(duration).map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn wait_for_embeddings_sync(&self, timeout_ms: Option<u32>) -> Result<()> {
        let duration = std::time::Duration::from_millis(u64::from(timeout_ms.unwrap_or(60_000)));
        self.inner.wait_for_embeddings(duration).map_err(napi_error)
    }

    #[napi]
    pub fn json_set_auto_embed_sync(
        &self,
        collection: String,
        config_json: Option<String>,
    ) -> Result<()> {
        let config: Option<lilia_core::AutoEmbedConfig> = match config_json {
            Some(ref s) if !s.trim().is_empty() => {
                Some(serde_json::from_str(s).map_err(|e| napi::Error::from_reason(e.to_string()))?)
            }
            _ => None,
        };
        self.inner
            .apply_json_auto_embed(&collection, config.as_ref())
            .map_err(napi_error)
    }

    #[napi]
    pub fn json_embed_status_sync(&self, collection: String) -> Result<String> {
        let status = self.inner.auto_embed_status(&collection).map_err(napi_error)?;
        serde_json::to_string(&status).map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    #[napi]
    pub async fn vector_create_index(
        &self,
        collection: String,
        config_json: String,
    ) -> Result<()> {
        run_async(&self.inner, move |db| {
            helpers::vector_create_index(db, &collection, &config_json)
        })
        .await
    }

    #[napi]
    pub fn vector_create_index_sync(
        &self,
        collection: String,
        config_json: String,
    ) -> Result<()> {
        helpers::vector_create_index(&self.inner, &collection, &config_json)
    }

    #[napi]
    pub async fn vector_get_index_info(&self, collection: String) -> Result<Option<String>> {
        run_async(&self.inner, move |db| {
            let info = db.vector_get_index_info(&collection).map_err(napi_error)?;
            opt_json_string(info.as_ref())
        })
        .await
    }

    #[napi]
    pub fn vector_get_index_info_sync(&self, collection: String) -> Result<Option<String>> {
        let info = self
            .inner
            .vector_get_index_info(&collection)
            .map_err(napi_error)?;
        opt_json_string(info.as_ref())
    }

    #[napi]
    pub async fn vector_drop_index(&self, collection: String) -> Result<bool> {
        run_async(&self.inner, move |db| {
            db.vector_drop_index(&collection).map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn vector_drop_index_sync(&self, collection: String) -> Result<bool> {
        self.inner
            .vector_drop_index(&collection)
            .map_err(napi_error)
    }

    #[napi]
    pub async fn vector_search_filtered(
        &self,
        collection: String,
        query: Buffer,
        dim: u32,
        element_type: Option<String>,
        metric: Option<String>,
        candidate_ids: Vec<String>,
        limit: Option<u32>,
    ) -> Result<Vec<NativeVectorHit>> {
        run_async(&self.inner, move |db| {
            let refs: Vec<&str> = candidate_ids.iter().map(String::as_str).collect();
            let hits = db
                .vector_search_filtered(
                    &collection,
                    query.as_ref(),
                    dim,
                    parse_element_type(element_type.as_deref()),
                    parse_metric(metric.as_deref()),
                    &refs,
                    limit.unwrap_or(10),
                )
                .map_err(napi_error)?;
            Ok(vector_hits_to_native(hits))
        })
        .await
    }

    #[napi]
    pub fn vector_search_filtered_sync(
        &self,
        collection: String,
        query: Buffer,
        dim: u32,
        element_type: Option<String>,
        metric: Option<String>,
        candidate_ids: Vec<String>,
        limit: Option<u32>,
    ) -> Result<Vec<NativeVectorHit>> {
        let refs: Vec<&str> = candidate_ids.iter().map(String::as_str).collect();
        let hits = self
            .inner
            .vector_search_filtered(
                &collection,
                query.as_ref(),
                dim,
                parse_element_type(element_type.as_deref()),
                parse_metric(metric.as_deref()),
                &refs,
                limit.unwrap_or(10),
            )
            .map_err(napi_error)?;
        Ok(vector_hits_to_native(hits))
    }

    #[napi]
    pub async fn vector_search_in_graph_neighbors(
        &self,
        collection: String,
        graph: String,
        start_node: String,
        direction: Option<String>,
        query: Buffer,
        dim: u32,
        element_type: Option<String>,
        metric: Option<String>,
        limit: Option<u32>,
    ) -> Result<Vec<NativeVectorHit>> {
        run_async(&self.inner, move |db| {
            let hits = db
                .vector_search_in_graph_neighbors(
                    &collection,
                    &graph,
                    &start_node,
                    parse_direction(direction.as_deref()),
                    query.as_ref(),
                    dim,
                    parse_element_type(element_type.as_deref()),
                    parse_metric(metric.as_deref()),
                    limit.unwrap_or(10),
                )
                .map_err(napi_error)?;
            Ok(vector_hits_to_native(hits))
        })
        .await
    }

    #[napi]
    pub fn vector_search_in_graph_neighbors_sync(
        &self,
        collection: String,
        graph: String,
        start_node: String,
        direction: Option<String>,
        query: Buffer,
        dim: u32,
        element_type: Option<String>,
        metric: Option<String>,
        limit: Option<u32>,
    ) -> Result<Vec<NativeVectorHit>> {
        let hits = self
            .inner
            .vector_search_in_graph_neighbors(
                &collection,
                &graph,
                &start_node,
                parse_direction(direction.as_deref()),
                query.as_ref(),
                dim,
                parse_element_type(element_type.as_deref()),
                parse_metric(metric.as_deref()),
                limit.unwrap_or(10),
            )
            .map_err(napi_error)?;
        Ok(vector_hits_to_native(hits))
    }

    #[napi]
    pub async fn vector_search_by_json_index(
        &self,
        collection: String,
        space: String,
        index_name: String,
        index_value: String,
        query: Buffer,
        dim: u32,
        element_type: Option<String>,
        metric: Option<String>,
        limit: Option<u32>,
    ) -> Result<Vec<NativeVectorHit>> {
        run_async(&self.inner, move |db| {
            let hits = db
                .vector_search_by_json_index(
                    &collection,
                    &space,
                    &index_name,
                    &index_value,
                    query.as_ref(),
                    dim,
                    parse_element_type(element_type.as_deref()),
                    parse_metric(metric.as_deref()),
                    limit.unwrap_or(10),
                )
                .map_err(napi_error)?;
            Ok(vector_hits_to_native(hits))
        })
        .await
    }

    #[napi]
    pub fn vector_search_by_json_index_sync(
        &self,
        collection: String,
        space: String,
        index_name: String,
        index_value: String,
        query: Buffer,
        dim: u32,
        element_type: Option<String>,
        metric: Option<String>,
        limit: Option<u32>,
    ) -> Result<Vec<NativeVectorHit>> {
        let hits = self
            .inner
            .vector_search_by_json_index(
                &collection,
                &space,
                &index_name,
                &index_value,
                query.as_ref(),
                dim,
                parse_element_type(element_type.as_deref()),
                parse_metric(metric.as_deref()),
                limit.unwrap_or(10),
            )
            .map_err(napi_error)?;
        Ok(vector_hits_to_native(hits))
    }

    #[napi]
    pub async fn hybrid_search(
        &self,
        collection: String,
        fts_query: Option<String>,
        vector_query: Option<Buffer>,
        dim: Option<u32>,
        element_type: Option<String>,
        metric: Option<String>,
        rrf_k: Option<u32>,
        fts_weight: Option<f64>,
        vector_weight: Option<f64>,
        limit: Option<u32>,
    ) -> Result<Vec<NativeHybridHit>> {
        run_async(&self.inner, move |db| {
            helpers::hybrid_search(
                db,
                &collection,
                fts_query.as_deref(),
                vector_query.as_deref(),
                dim,
                element_type.as_deref(),
                metric.as_deref(),
                rrf_k,
                fts_weight,
                vector_weight,
                limit,
            )
        })
        .await
    }

    #[napi]
    pub fn hybrid_search_sync(
        &self,
        collection: String,
        fts_query: Option<String>,
        vector_query: Option<Buffer>,
        dim: Option<u32>,
        element_type: Option<String>,
        metric: Option<String>,
        rrf_k: Option<u32>,
        fts_weight: Option<f64>,
        vector_weight: Option<f64>,
        limit: Option<u32>,
    ) -> Result<Vec<NativeHybridHit>> {
        helpers::hybrid_search(
            &self.inner,
            &collection,
            fts_query.as_deref(),
            vector_query.as_deref(),
            dim,
            element_type.as_deref(),
            metric.as_deref(),
            rrf_k,
            fts_weight,
            vector_weight,
            limit,
        )
    }

    // -----------------------------------------------------------------------
    // Time-Series
    // -----------------------------------------------------------------------

    #[napi]
    pub async fn ts_append_points(
        &self,
        series_id: String,
        points_json: String,
        resolution_ms: Option<i64>,
    ) -> Result<()> {
        run_async(&self.inner, move |db| {
            helpers::ts_append_points(db, &series_id, &points_json, resolution_ms)
        })
        .await
    }

    #[napi]
    pub fn ts_append_points_sync(
        &self,
        series_id: String,
        points_json: String,
        resolution_ms: Option<i64>,
    ) -> Result<()> {
        helpers::ts_append_points(&self.inner, &series_id, &points_json, resolution_ms)
    }

    #[napi]
    pub async fn ts_query_range(
        &self,
        series_id: String,
        start_time: i64,
        end_time: i64,
        resolution_ms: Option<i64>,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            let pts = match resolution_ms {
                Some(res) => {
                    db.ts_query_range_with_resolution(&series_id, res, start_time, end_time)
                }
                None => db.ts_query_range(&series_id, start_time, end_time),
            }
            .map_err(napi_error)?;
            to_json_string(&pts)
        })
        .await
    }

    #[napi]
    pub fn ts_query_range_sync(
        &self,
        series_id: String,
        start_time: i64,
        end_time: i64,
        resolution_ms: Option<i64>,
    ) -> Result<String> {
        let pts = match resolution_ms {
            Some(res) => {
                self.inner
                    .ts_query_range_with_resolution(&series_id, res, start_time, end_time)
            }
            None => self.inner.ts_query_range(&series_id, start_time, end_time),
        }
        .map_err(napi_error)?;
        to_json_string(&pts)
    }

    #[napi]
    pub async fn ts_query_auto(
        &self,
        series_id: String,
        start_time: i64,
        end_time: i64,
        max_points: u32,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(
                &db.ts_query_auto(&series_id, start_time, end_time, max_points as usize)
                    .map_err(napi_error)?,
            )
        })
        .await
    }

    #[napi]
    pub fn ts_query_auto_sync(
        &self,
        series_id: String,
        start_time: i64,
        end_time: i64,
        max_points: u32,
    ) -> Result<String> {
        to_json_string(
            &self
                .inner
                .ts_query_auto(&series_id, start_time, end_time, max_points as usize)
                .map_err(napi_error)?,
        )
    }

    #[napi]
    pub async fn ts_query_stats(
        &self,
        series_id: String,
        start_time: i64,
        end_time: i64,
        resolution_ms: Option<i64>,
    ) -> Result<Option<String>> {
        run_async(&self.inner, move |db| {
            let stats = match resolution_ms {
                Some(res) => {
                    db.ts_query_stats_with_resolution(&series_id, res, start_time, end_time)
                }
                None => db.ts_query_stats(&series_id, start_time, end_time),
            }
            .map_err(napi_error)?;
            opt_json_string(stats.as_ref())
        })
        .await
    }

    #[napi]
    pub fn ts_query_stats_sync(
        &self,
        series_id: String,
        start_time: i64,
        end_time: i64,
        resolution_ms: Option<i64>,
    ) -> Result<Option<String>> {
        let stats = match resolution_ms {
            Some(res) => {
                self.inner
                    .ts_query_stats_with_resolution(&series_id, res, start_time, end_time)
            }
            None => self.inner.ts_query_stats(&series_id, start_time, end_time),
        }
        .map_err(napi_error)?;
        opt_json_string(stats.as_ref())
    }

    #[napi]
    pub async fn ts_set_policy(&self, policy_json: String) -> Result<()> {
        run_async(&self.inner, move |db| {
            let policy: TimeSeriesLifecyclePolicy = from_json_str(&policy_json)?;
            db.ts_set_policy(&policy).map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn ts_set_policy_sync(&self, policy_json: String) -> Result<()> {
        let policy: TimeSeriesLifecyclePolicy = from_json_str(&policy_json)?;
        self.inner.ts_set_policy(&policy).map_err(napi_error)
    }

    #[napi]
    pub async fn ts_get_policy(&self, series_pattern: String) -> Result<Option<String>> {
        run_async(&self.inner, move |db| {
            let policy = db.ts_get_policy(&series_pattern).map_err(napi_error)?;
            opt_json_string(policy.as_ref())
        })
        .await
    }

    #[napi]
    pub fn ts_get_policy_sync(&self, series_pattern: String) -> Result<Option<String>> {
        let policy = self
            .inner
            .ts_get_policy(&series_pattern)
            .map_err(napi_error)?;
        opt_json_string(policy.as_ref())
    }

    #[napi]
    pub async fn ts_list_policies(&self) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(&db.ts_list_policies().map_err(napi_error)?)
        })
        .await
    }

    #[napi]
    pub fn ts_list_policies_sync(&self) -> Result<String> {
        to_json_string(&self.inner.ts_list_policies().map_err(napi_error)?)
    }

    #[napi]
    pub async fn ts_delete_policy(&self, series_pattern: String) -> Result<bool> {
        run_async(&self.inner, move |db| {
            db.ts_delete_policy(&series_pattern).map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn ts_delete_policy_sync(&self, series_pattern: String) -> Result<bool> {
        self.inner
            .ts_delete_policy(&series_pattern)
            .map_err(napi_error)
    }

    #[napi]
    pub async fn ts_apply_retention(
        &self,
        series_id: Option<String>,
        policy_json: Option<String>,
        now_ms: Option<i64>,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            helpers::ts_apply_retention(db, series_id.as_deref(), policy_json.as_deref(), now_ms)
        })
        .await
    }

    #[napi]
    pub fn ts_apply_retention_sync(
        &self,
        series_id: Option<String>,
        policy_json: Option<String>,
        now_ms: Option<i64>,
    ) -> Result<String> {
        helpers::ts_apply_retention(
            &self.inner,
            series_id.as_deref(),
            policy_json.as_deref(),
            now_ms,
        )
    }

    // -----------------------------------------------------------------------
    // Geofencing & GPS
    // -----------------------------------------------------------------------

    #[napi]
    pub async fn geo_add_fence(
        &self,
        id: String,
        name: String,
        category: String,
        polygon_json: String,
        metadata: Option<String>,
    ) -> Result<()> {
        run_async(&self.inner, move |db| {
            db.geo_add_fence(&id, &name, &category, &polygon_json, metadata.as_deref())
                .map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn geo_add_fence_sync(
        &self,
        id: String,
        name: String,
        category: String,
        polygon_json: String,
        metadata: Option<String>,
    ) -> Result<()> {
        self.inner
            .geo_add_fence(&id, &name, &category, &polygon_json, metadata.as_deref())
            .map_err(napi_error)
    }

    #[napi]
    pub async fn geo_remove_fence(&self, id: String) -> Result<bool> {
        run_async(&self.inner, move |db| {
            db.geo_remove_fence(&id).map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn geo_remove_fence_sync(&self, id: String) -> Result<bool> {
        self.inner.geo_remove_fence(&id).map_err(napi_error)
    }

    #[napi]
    pub async fn geo_contains_point(
        &self,
        polygon_json: String,
        lon: f64,
        lat: f64,
    ) -> Result<bool> {
        run_async(&self.inner, move |db| {
            db.geo_contains_point(&polygon_json, lon, lat)
                .map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn geo_contains_point_sync(&self, polygon_json: String, lon: f64, lat: f64) -> Result<bool> {
        self.inner
            .geo_contains_point(&polygon_json, lon, lat)
            .map_err(napi_error)
    }

    #[napi]
    pub async fn geo_find_fences_for_point(&self, lon: f64, lat: f64) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(&db.geo_find_fences_for_point(lon, lat).map_err(napi_error)?)
        })
        .await
    }

    #[napi]
    pub fn geo_find_fences_for_point_sync(&self, lon: f64, lat: f64) -> Result<String> {
        to_json_string(
            &self
                .inner
                .geo_find_fences_for_point(lon, lat)
                .map_err(napi_error)?,
        )
    }

    #[napi]
    pub async fn geo_record_gps_points(
        &self,
        track_id: String,
        points_json: String,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            helpers::geo_record_gps_points(db, &track_id, &points_json)
        })
        .await
    }

    #[napi]
    pub fn geo_record_gps_points_sync(
        &self,
        track_id: String,
        points_json: String,
    ) -> Result<String> {
        helpers::geo_record_gps_points(&self.inner, &track_id, &points_json)
    }

    #[napi]
    pub async fn geo_get_track_points(
        &self,
        track_id: String,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(
                &db.geo_get_track_points(&track_id, start_time, end_time)
                    .map_err(napi_error)?,
            )
        })
        .await
    }

    #[napi]
    pub fn geo_get_track_points_sync(
        &self,
        track_id: String,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<String> {
        to_json_string(
            &self
                .inner
                .geo_get_track_points(&track_id, start_time, end_time)
                .map_err(napi_error)?,
        )
    }

    #[napi]
    pub async fn geo_track_distance(&self, track_id: String) -> Result<f64> {
        run_async(&self.inner, move |db| {
            db.geo_track_distance_meters(&track_id).map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn geo_track_distance_sync(&self, track_id: String) -> Result<f64> {
        self.inner
            .geo_track_distance_meters(&track_id)
            .map_err(napi_error)
    }

    // -----------------------------------------------------------------------
    // CRDT
    // -----------------------------------------------------------------------

    #[napi]
    pub async fn crdt_counter_get(&self, space: String, id: String) -> Result<i64> {
        run_async(&self.inner, move |db| {
            db.crdt().counter(&space, &id).get().map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn crdt_counter_get_sync(&self, space: String, id: String) -> Result<i64> {
        self.inner
            .crdt()
            .counter(&space, &id)
            .get()
            .map_err(napi_error)
    }

    #[napi]
    pub async fn crdt_counter_state(&self, space: String, id: String) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(&db.crdt().counter(&space, &id).state().map_err(napi_error)?)
        })
        .await
    }

    #[napi]
    pub fn crdt_counter_state_sync(&self, space: String, id: String) -> Result<String> {
        to_json_string(
            &self
                .inner
                .crdt()
                .counter(&space, &id)
                .state()
                .map_err(napi_error)?,
        )
    }

    #[napi]
    pub async fn crdt_counter_merge(
        &self,
        space: String,
        id: String,
        remote_json: String,
    ) -> Result<i64> {
        run_async(&self.inner, move |db| {
            helpers::crdt_counter_merge(db, &space, &id, &remote_json)
        })
        .await
    }

    #[napi]
    pub fn crdt_counter_merge_sync(
        &self,
        space: String,
        id: String,
        remote_json: String,
    ) -> Result<i64> {
        helpers::crdt_counter_merge(&self.inner, &space, &id, &remote_json)
    }

    #[napi]
    pub async fn crdt_set_read(&self, space: String, id: String) -> Result<Vec<String>> {
        run_async(&self.inner, move |db| {
            let elems = db.crdt().set(&space, &id).read().map_err(napi_error)?;
            Ok(elems.into_iter().collect())
        })
        .await
    }

    #[napi]
    pub fn crdt_set_read_sync(&self, space: String, id: String) -> Result<Vec<String>> {
        let elems = self
            .inner
            .crdt()
            .set(&space, &id)
            .read()
            .map_err(napi_error)?;
        Ok(elems.into_iter().collect())
    }

    #[napi]
    pub async fn crdt_set_contains(
        &self,
        space: String,
        id: String,
        element: String,
    ) -> Result<bool> {
        run_async(&self.inner, move |db| {
            db.crdt()
                .set(&space, &id)
                .contains(&element)
                .map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn crdt_set_contains_sync(
        &self,
        space: String,
        id: String,
        element: String,
    ) -> Result<bool> {
        self.inner
            .crdt()
            .set(&space, &id)
            .contains(&element)
            .map_err(napi_error)
    }

    #[napi]
    pub async fn crdt_set_state(&self, space: String, id: String) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(&db.crdt().set(&space, &id).state().map_err(napi_error)?)
        })
        .await
    }

    #[napi]
    pub fn crdt_set_state_sync(&self, space: String, id: String) -> Result<String> {
        to_json_string(
            &self
                .inner
                .crdt()
                .set(&space, &id)
                .state()
                .map_err(napi_error)?,
        )
    }

    #[napi]
    pub async fn crdt_set_prune(&self, space: String, id: String, cutoff_ms: i64) -> Result<u32> {
        run_async(&self.inner, move |db| {
            let pruned = db
                .crdt()
                .set(&space, &id)
                .prune(cutoff_ms)
                .map_err(napi_error)?;
            Ok(u32::try_from(pruned).unwrap_or(u32::MAX))
        })
        .await
    }

    #[napi]
    pub fn crdt_set_prune_sync(&self, space: String, id: String, cutoff_ms: i64) -> Result<u32> {
        let pruned = self
            .inner
            .crdt()
            .set(&space, &id)
            .prune(cutoff_ms)
            .map_err(napi_error)?;
        Ok(u32::try_from(pruned).unwrap_or(u32::MAX))
    }

    #[napi]
    pub async fn crdt_set_merge(
        &self,
        space: String,
        id: String,
        remote_json: String,
    ) -> Result<()> {
        run_async(&self.inner, move |db| {
            helpers::crdt_set_merge(db, &space, &id, &remote_json)
        })
        .await
    }

    #[napi]
    pub fn crdt_set_merge_sync(&self, space: String, id: String, remote_json: String) -> Result<()> {
        helpers::crdt_set_merge(&self.inner, &space, &id, &remote_json)
    }

    #[napi]
    pub async fn crdt_json_get(&self, space: String, id: String) -> Result<Option<String>> {
        run_async(&self.inner, move |db| {
            let val = db.crdt().doc(&space, &id).get().map_err(napi_error)?;
            opt_json_string(val.as_ref())
        })
        .await
    }

    #[napi]
    pub fn crdt_json_get_sync(&self, space: String, id: String) -> Result<Option<String>> {
        let val = self
            .inner
            .crdt()
            .doc(&space, &id)
            .get()
            .map_err(napi_error)?;
        opt_json_string(val.as_ref())
    }

    #[napi]
    pub async fn crdt_json_state(&self, space: String, id: String) -> Result<Option<String>> {
        run_async(&self.inner, move |db| {
            let doc = db.crdt().doc(&space, &id).state().map_err(napi_error)?;
            opt_json_string(doc.as_ref())
        })
        .await
    }

    #[napi]
    pub fn crdt_json_state_sync(&self, space: String, id: String) -> Result<Option<String>> {
        let doc = self
            .inner
            .crdt()
            .doc(&space, &id)
            .state()
            .map_err(napi_error)?;
        opt_json_string(doc.as_ref())
    }

    #[napi]
    pub async fn crdt_json_merge(
        &self,
        space: String,
        id: String,
        remote_json: String,
    ) -> Result<String> {
        run_async(&self.inner, move |db| {
            helpers::crdt_json_merge(db, &space, &id, &remote_json)
        })
        .await
    }

    #[napi]
    pub fn crdt_json_merge_sync(
        &self,
        space: String,
        id: String,
        remote_json: String,
    ) -> Result<String> {
        helpers::crdt_json_merge(&self.inner, &space, &id, &remote_json)
    }

    // -----------------------------------------------------------------------
    // HLC & Conflicts
    // -----------------------------------------------------------------------

    #[napi]
    pub async fn hlc_next(&self) -> Result<String> {
        run_async(&self.inner, move |db| Ok(db.next_hlc().to_string())).await
    }

    #[napi]
    #[must_use]
    pub fn hlc_next_sync(&self) -> String {
        self.inner.next_hlc().to_string()
    }

    #[napi]
    pub async fn hlc_observe(&self, remote: String) -> Result<String> {
        run_async(&self.inner, move |db| {
            let parsed = parse_hlc(&remote)?;
            Ok(db.observe_hlc(&parsed).map_err(napi_error)?.to_string())
        })
        .await
    }

    #[napi]
    pub fn hlc_observe_sync(&self, remote: String) -> Result<String> {
        let parsed = parse_hlc(&remote)?;
        Ok(self
            .inner
            .observe_hlc(&parsed)
            .map_err(napi_error)?
            .to_string())
    }

    #[napi]
    pub async fn list_conflicts(&self, space: Option<String>) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(&db.list_conflicts(space.as_deref()).map_err(napi_error)?)
        })
        .await
    }

    #[napi]
    pub fn list_conflicts_sync(&self, space: Option<String>) -> Result<String> {
        to_json_string(
            &self
                .inner
                .list_conflicts(space.as_deref())
                .map_err(napi_error)?,
        )
    }

    #[napi]
    pub async fn resolve_conflict(
        &self,
        conflict_id: i64,
        resolved_value_json: String,
    ) -> Result<()> {
        run_async(&self.inner, move |db| {
            let val: serde_json::Value = from_json_str(&resolved_value_json)?;
            db.resolve_conflict(conflict_id, val).map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn resolve_conflict_sync(
        &self,
        conflict_id: i64,
        resolved_value_json: String,
    ) -> Result<()> {
        let val: serde_json::Value = from_json_str(&resolved_value_json)?;
        self.inner
            .resolve_conflict(conflict_id, val)
            .map_err(napi_error)
    }

    // -----------------------------------------------------------------------
    // Undo / Redo
    // -----------------------------------------------------------------------

    #[napi]
    pub async fn undo(&self) -> Result<Option<String>> {
        run_async(&self.inner, move |db| {
            opt_json_string(db.undo().map_err(napi_error)?.as_ref())
        })
        .await
    }

    #[napi]
    pub fn undo_sync(&self) -> Result<Option<String>> {
        opt_json_string(self.inner.undo().map_err(napi_error)?.as_ref())
    }

    #[napi]
    pub async fn redo(&self) -> Result<Option<String>> {
        run_async(&self.inner, move |db| {
            opt_json_string(db.redo().map_err(napi_error)?.as_ref())
        })
        .await
    }

    #[napi]
    pub fn redo_sync(&self) -> Result<Option<String>> {
        opt_json_string(self.inner.redo().map_err(napi_error)?.as_ref())
    }

    #[napi]
    pub async fn undo_history(&self) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(&db.undo_history().map_err(napi_error)?)
        })
        .await
    }

    #[napi]
    pub fn undo_history_sync(&self) -> Result<String> {
        to_json_string(&self.inner.undo_history().map_err(napi_error)?)
    }

    // -----------------------------------------------------------------------
    // CDC Watch, Live Query & Incremental Materialized Views
    // -----------------------------------------------------------------------

    #[napi(ts_args_type = "callback: (event: string) => void, namespace?: string")]
    pub async fn watch(
        &self,
        callback: ThreadsafeFunction<String>,
        namespace: Option<String>,
    ) -> Result<NativeSubscription> {
        let (tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let cancel_tx = Arc::new(Mutex::new(Some(tx)));
        let mut rx = self.inner.subscribe();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut cancel_rx => break,
                    evt = rx.recv() => {
                        match evt {
                            Ok(event) => {
                                if event.matches_filter(namespace.as_deref()) {
                                    if let Ok(json) = serde_json::to_string(&event) {
                                        callback.call(Ok(json), ThreadsafeFunctionCallMode::NonBlocking);
                                    }
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });

        Ok(NativeSubscription { cancel_tx })
    }

    #[napi(ts_args_type = "sql: string, callback: (diff: string) => void, paramsJson?: string")]
    pub async fn live_query(
        &self,
        sql: String,
        callback: ThreadsafeFunction<String>,
        params_json: Option<String>,
    ) -> Result<NativeLiveQuerySubscription> {
        let params = parse_params(params_json.as_deref())?;
        let callback = Arc::new(callback);
        run_async(&self.inner, move |db| {
            let cb_func = Arc::clone(&callback);
            let handle = db
                .live_query(&sql, &params, move |diff| {
                    if let Ok(json_str) = serde_json::to_string(&diff) {
                        cb_func.call(Ok(json_str), ThreadsafeFunctionCallMode::NonBlocking);
                    }
                })
                .map_err(napi_error)?;
            Ok(NativeLiveQuerySubscription {
                handle: Arc::new(Mutex::new(Some(handle))),
            })
        })
        .await
    }

    #[napi]
    pub async fn create_materialized_view(&self, name: String, query: String) -> Result<()> {
        run_async(&self.inner, move |db| {
            db.create_materialized_view(&name, &query)
                .map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn create_materialized_view_sync(&self, name: String, query: String) -> Result<()> {
        self.inner
            .create_materialized_view(&name, &query)
            .map_err(napi_error)
    }

    #[napi]
    pub async fn drop_materialized_view(&self, name: String) -> Result<()> {
        run_async(&self.inner, move |db| {
            db.drop_materialized_view(&name).map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn drop_materialized_view_sync(&self, name: String) -> Result<()> {
        self.inner
            .drop_materialized_view(&name)
            .map_err(napi_error)
    }

    #[napi]
    pub async fn refresh_materialized_view(&self, name: String) -> Result<()> {
        run_async(&self.inner, move |db| {
            db.refresh_materialized_view(&name).map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn refresh_materialized_view_sync(&self, name: String) -> Result<()> {
        self.inner
            .refresh_materialized_view(&name)
            .map_err(napi_error)
    }

    #[napi]
    pub async fn list_materialized_views(&self) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(&db.list_materialized_views().map_err(napi_error)?)
        })
        .await
    }

    #[napi]
    pub fn list_materialized_views_sync(&self) -> Result<String> {
        to_json_string(&self.inner.list_materialized_views().map_err(napi_error)?)
    }

    // -----------------------------------------------------------------------
    // Diagnostics & Maintenance
    // -----------------------------------------------------------------------

    #[napi]
    pub async fn quick_check(&self) -> Result<bool> {
        run_async(&self.inner, move |db| db.quick_check().map_err(napi_error)).await
    }

    #[napi]
    pub fn quick_check_sync(&self) -> Result<bool> {
        self.inner.quick_check().map_err(napi_error)
    }

    #[napi]
    pub async fn integrity_check(&self) -> Result<bool> {
        run_async(&self.inner, move |db| {
            db.integrity_check().map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn integrity_check_sync(&self) -> Result<bool> {
        self.inner.integrity_check().map_err(napi_error)
    }

    #[napi]
    pub async fn checkpoint(&self) -> Result<()> {
        run_async(&self.inner, move |db| db.checkpoint().map_err(napi_error)).await
    }

    #[napi]
    pub fn checkpoint_sync(&self) -> Result<()> {
        self.inner.checkpoint().map_err(napi_error)
    }

    #[napi]
    pub async fn checkpoint_stats(&self) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(&db.checkpoint_stats())
        })
        .await
    }

    #[napi]
    pub fn checkpoint_stats_sync(&self) -> Result<String> {
        to_json_string(&self.inner.checkpoint_stats())
    }

    #[napi]
    pub async fn storage_stats(&self) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(&db.storage_stats().map_err(napi_error)?)
        })
        .await
    }

    #[napi]
    pub fn storage_stats_sync(&self) -> Result<String> {
        to_json_string(&self.inner.storage_stats().map_err(napi_error)?)
    }

    #[napi]
    pub async fn backup(&self, destination: String) -> Result<()> {
        run_async(&self.inner, move |db| {
            db.backup(destination).map_err(napi_error)
        })
        .await
    }

    #[napi]
    pub fn backup_sync(&self, destination: String) -> Result<()> {
        self.inner.backup(destination).map_err(napi_error)
    }

    // -----------------------------------------------------------------------
    // CAS / BLOB
    // -----------------------------------------------------------------------

    #[napi]
    pub async fn blob_store(&self, bytes: Buffer, mime_type: Option<String>) -> Result<String> {
        run_async(&self.inner, move |db| {
            let desc = db.blob_store(&bytes).map_err(napi_error)?;
            let desc = if let Some(mime) = mime_type {
                desc.with_mime_type(mime)
            } else {
                desc
            };
            to_json_string(&desc)
        })
        .await
    }

    #[napi]
    pub fn blob_store_sync(&self, bytes: Buffer, mime_type: Option<String>) -> Result<String> {
        let desc = self.inner.blob_store(&bytes).map_err(napi_error)?;
        let desc = if let Some(mime) = mime_type {
            desc.with_mime_type(mime)
        } else {
            desc
        };
        to_json_string(&desc)
    }

    #[napi]
    pub async fn blob_get(&self, hash: String) -> Result<Option<Buffer>> {
        run_async(&self.inner, move |db| {
            let data = db.blob_get(&hash).map_err(napi_error)?;
            Ok(data.map(Buffer::from))
        })
        .await
    }

    #[napi]
    pub fn blob_get_sync(&self, hash: String) -> Result<Option<Buffer>> {
        let data = self.inner.blob_get(&hash).map_err(napi_error)?;
        Ok(data.map(Buffer::from))
    }

    #[napi]
    pub fn blob_path(&self, hash: String) -> String {
        self.inner.blob_path(&hash).to_string_lossy().to_string()
    }

    #[napi]
    pub fn blob_exists(&self, hash: String) -> bool {
        self.inner.blob_exists(&hash)
    }

    #[napi]
    pub async fn blob_gc(&self) -> Result<String> {
        run_async(&self.inner, move |db| {
            let stats = db.blob_gc().map_err(napi_error)?;
            to_json_string(&stats)
        })
        .await
    }

    #[napi]
    pub fn blob_gc_sync(&self) -> Result<String> {
        let stats = self.inner.blob_gc().map_err(napi_error)?;
        to_json_string(&stats)
    }

    // -----------------------------------------------------------------------
    // Temporal sharding
    // -----------------------------------------------------------------------

    #[napi]
    pub async fn sharding_list_shards(&self) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(&db.list_shards().map_err(napi_error)?)
        })
        .await
    }

    #[napi]
    pub fn sharding_list_shards_sync(&self) -> Result<String> {
        to_json_string(&self.inner.list_shards().map_err(napi_error)?)
    }

    #[napi]
    pub async fn sharding_active_shard(&self) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(&db.active_shard().map_err(napi_error)?)
        })
        .await
    }

    #[napi]
    pub fn sharding_active_shard_sync(&self) -> Result<String> {
        to_json_string(&self.inner.active_shard().map_err(napi_error)?)
    }

    #[napi]
    pub async fn sharding_rollover(&self) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(&db.rollover_shard().map_err(napi_error)?)
        })
        .await
    }

    #[napi]
    pub fn sharding_rollover_sync(&self) -> Result<String> {
        to_json_string(&self.inner.rollover_shard().map_err(napi_error)?)
    }

    #[napi]
    pub async fn sharding_detach_and_compress(&self, shard_id: String) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(&db.detach_and_compress(&shard_id).map_err(napi_error)?)
        })
        .await
    }

    #[napi]
    pub fn sharding_detach_and_compress_sync(&self, shard_id: String) -> Result<String> {
        to_json_string(&self.inner.detach_and_compress(&shard_id).map_err(napi_error)?)
    }

    #[napi]
    pub async fn sharding_decompress_and_attach(&self, shard_id: String) -> Result<String> {
        run_async(&self.inner, move |db| {
            to_json_string(&db.decompress_and_attach(&shard_id).map_err(napi_error)?)
        })
        .await
    }

    #[napi]
    pub fn sharding_decompress_and_attach_sync(&self, shard_id: String) -> Result<String> {
        to_json_string(&self.inner.decompress_and_attach(&shard_id).map_err(napi_error)?)
    }

    #[napi]
    pub async fn sharding_detach(&self, shard_id: String) -> Result<()> {
        run_async(&self.inner, move |db| db.detach_shard(&shard_id).map_err(napi_error)).await
    }

    #[napi]
    pub fn sharding_detach_sync(&self, shard_id: String) -> Result<()> {
        self.inner.detach_shard(&shard_id).map_err(napi_error)
    }

    #[napi]
    pub async fn sharding_attach(&self, shard_id: String, path: String) -> Result<()> {
        run_async(&self.inner, move |db| db.attach_shard(&shard_id, &path).map_err(napi_error)).await
    }

    #[napi]
    pub fn sharding_attach_sync(&self, shard_id: String, path: String) -> Result<()> {
        self.inner.attach_shard(&shard_id, &path).map_err(napi_error)
    }

    #[napi]
    pub async fn close(&self, timeout_ms: Option<u32>) -> Result<()> {
        let started = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(u64::from(timeout_ms.unwrap_or(5_000)));
        run_async(&self.inner, move |db| {
            db.close(timeout.saturating_sub(started.elapsed()))
                .map_err(napi_error)
        })
        .await
    }
}
