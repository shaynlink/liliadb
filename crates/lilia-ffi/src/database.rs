//! The typed `UniFFI` API (Swift, Kotlin): a [`LiliaDatabase`] whose methods run the operations of
//! [`lilia_protocol`] through the database's system session, each in a blocking form (`*_sync`)
//! and an asynchronous one, which runs on tokio's blocking pool so that it never blocks the
//! caller's executor.

use std::sync::Arc;
use std::time::Duration;

use lilia_core::{BatchOperation, JsonEntry, KvEntry, MutationResult};
use lilia_db::Database;
use lilia_protocol::{Operation, ResponseValue, SharedDatabase};

use crate::records::{
    FfiDirection, FfiFtsFuzzyHit, FfiFtsHit, FfiFtsPhoneticHit, FfiFtsSuggestion, FfiGeofenceMatch,
    FfiGpsPoint, FfiGraphNode, FfiJsonEntry, FfiJsonIndexInfo, FfiKvEntry, FfiOpenOptions,
    FfiSqlQueryResult, FfiTimeSeriesLifecyclePolicy, FfiTimeSeriesPoint,
    FfiTimeSeriesRetentionStats, FfiTimeSeriesStats, LiliaFfiError,
};

/// The inner value of a response of the expected variant; an error otherwise.
macro_rules! answer {
    ($value:expr, $variant:path) => {
        match $value {
            $variant(inner) => inner,
            other => {
                return Err(LiliaFfiError::Generic(format!(
                    "unexpected response: {}",
                    other.name()
                )))
            }
        }
    };
}

/// A method in its two forms: `$sync` runs `$body` on the caller's thread, `$name` on tokio's
/// blocking pool. `$body` sees the database as `$db` and the arguments by name.
macro_rules! method {
    (
        $(#[$doc:meta])*
        $name:ident / $sync:ident ( $($arg:ident : $ty:ty),* $(,)? ) -> $out:ty,
        |$db:ident| $body:expr
    ) => {
        #[uniffi::export(async_runtime = "tokio")]
        impl LiliaDatabase {
            $(#[$doc])*
            pub fn $sync(&self, $($arg: $ty),*) -> Result<$out, LiliaFfiError> {
                let database = self.database()?;
                let $db: &Database = &database;
                $body
            }

            $(#[$doc])*
            pub async fn $name(&self, $($arg: $ty),*) -> Result<$out, LiliaFfiError> {
                let database = self.database()?;
                tokio::task::spawn_blocking(move || {
                    let $db: &Database = &database;
                    $body
                })
                .await
                .map_err(|e| LiliaFfiError::Generic(format!("worker failed: {e}")))?
            }
        }
    };
}

/// An open database. Calls share it; `close` closes it once they return.
#[derive(Debug, uniffi::Object)]
pub struct LiliaDatabase {
    database: SharedDatabase,
}

impl LiliaDatabase {
    fn database(&self) -> Result<Arc<Database>, LiliaFfiError> {
        Ok(self.database.get()?)
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl LiliaDatabase {
    /// Opens (or creates) the database of directory `path`.
    #[uniffi::constructor]
    pub fn open(path: String, options: FfiOpenOptions) -> Result<Arc<Self>, LiliaFfiError> {
        let database = lilia_protocol::open(path, &(&options).into())?;
        Ok(Arc::new(Self {
            database: SharedDatabase::new(database),
        }))
    }

    /// Closes the database once the calls running on it return, waiting at most `timeout_ms`.
    /// Later calls fail with `Closed`; closing twice does nothing.
    pub fn close_sync(&self, timeout_ms: u64) -> Result<(), LiliaFfiError> {
        Ok(self.database.close(Duration::from_millis(timeout_ms))?)
    }

    /// As [`LiliaDatabase::close_sync`], on tokio's blocking pool.
    pub async fn close(&self, timeout_ms: u64) -> Result<(), LiliaFfiError> {
        let Some(database) = self.database.take() else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || {
            lilia_protocol::close(database, Duration::from_millis(timeout_ms))
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(format!("worker failed: {e}")))??;
        Ok(())
    }
}

fn run(database: &Database, operation: Operation) -> Result<ResponseValue, LiliaFfiError> {
    Ok(lilia_protocol::execute(database, operation)?)
}

/// The result of one write.
fn write(database: &Database, operation: BatchOperation) -> Result<MutationResult, LiliaFfiError> {
    let operations = vec![operation];
    let results = answer!(
        run(database, Operation::Batch { operations })?,
        ResponseValue::Mutations
    );
    results
        .into_iter()
        .next()
        .ok_or_else(|| LiliaFfiError::Generic("a write without its result".into()))
}

fn kv_entry(entry: KvEntry) -> FfiKvEntry {
    FfiKvEntry {
        namespace: entry.namespace,
        key: entry.key,
        value: entry.value,
        version: entry.version,
        expires_at_ms: entry.expires_at_ms,
    }
}

fn json_entry(entry: JsonEntry) -> Result<FfiJsonEntry, LiliaFfiError> {
    Ok(FfiJsonEntry {
        value_json: serde_json::to_string(&entry.doc)?,
        space: entry.space,
        id: entry.id,
        version: entry.version,
    })
}

fn json_entries(entries: Vec<JsonEntry>) -> Result<Vec<FfiJsonEntry>, LiliaFfiError> {
    entries.into_iter().map(json_entry).collect()
}

fn graph_node(node: lilia_core::GraphNode) -> FfiGraphNode {
    FfiGraphNode {
        metadata_json: node.metadata.to_string(),
        graph: node.graph,
        id: node.id,
        kind: node.kind,
    }
}

fn fts_hit(hit: lilia_core::FtsHit) -> FfiFtsHit {
    FfiFtsHit {
        collection: hit.collection,
        doc_id: hit.doc_id,
        snippet: hit.snippet,
        rank: hit.rank,
    }
}

fn points(points: Vec<lilia_core::TimeSeriesPoint>) -> Vec<FfiTimeSeriesPoint> {
    points.into_iter().map(Into::into).collect()
}

fn matches(matches: Vec<lilia_core::GeofenceMatch>) -> Vec<FfiGeofenceMatch> {
    matches.into_iter().map(Into::into).collect()
}

method!(
    kv_get / kv_get_sync(namespace: String, key: Vec<u8>) -> Option<FfiKvEntry>,
    |db| Ok(answer!(run(db, Operation::KvGet { namespace, key })?, ResponseValue::Kv).map(kv_entry))
);

method!(
    /// Stores `value` under `key`; the version of the write.
    kv_set / kv_set_sync(namespace: String, key: Vec<u8>, value: Vec<u8>, expires_at_ms: Option<i64>) -> u64,
    |db| {
        let operation = BatchOperation::KvSet { namespace, key, value, if_version: None, expires_at_ms };
        Ok(write(db, operation)?.version.unwrap_or_default())
    }
);

method!(
    kv_delete / kv_delete_sync(namespace: String, key: Vec<u8>) -> bool,
    |db| Ok(write(db, BatchOperation::KvDelete { namespace, key, if_version: None })?.deleted)
);

method!(
    /// The records of `namespace` after `after`, at most `limit` (every one when 0).
    kv_scan / kv_scan_sync(namespace: String, after: Option<Vec<u8>>, limit: u32) -> Vec<FfiKvEntry>,
    |db| {
        let page = answer!(run(db, Operation::KvScan { namespace, after, limit })?, ResponseValue::KvPage);
        Ok(page.into_iter().map(kv_entry).collect())
    }
);

method!(
    json_get / json_get_sync(space: String, id: String) -> Option<FfiJsonEntry>,
    |db| answer!(run(db, Operation::JsonGet { space, id })?, ResponseValue::Json).map(json_entry).transpose()
);

method!(
    /// Stores the document `value_json`; the version of the write.
    json_put / json_put_sync(space: String, id: String, value_json: String) -> u64,
    |db| {
        let value = serde_json::from_str(&value_json)?;
        let operation = BatchOperation::JsonPut { space, id, value, if_version: None };
        Ok(write(db, operation)?.version.unwrap_or_default())
    }
);

method!(
    json_delete / json_delete_sync(space: String, id: String) -> bool,
    |db| Ok(write(db, BatchOperation::JsonDelete { space, id, if_version: None })?.deleted)
);

method!(
    /// The documents whose indexes hold the values of `query_json` (an object of index names and
    /// values), after `after_id`, at most `limit` (every one when 0).
    json_find / json_find_sync(space: String, query_json: String, after_id: Option<String>, limit: u32) -> Vec<FfiJsonEntry>,
    |db| {
        let query = serde_json::from_str(&query_json)?;
        let operation = Operation::JsonFind { space, query, after: after_id, limit };
        json_entries(answer!(run(db, operation)?, ResponseValue::JsonPage))
    }
);

method!(
    /// Creates index `name` of `space` over `path` (`$.field`, or an expression), backfilled
    /// from the stored documents; with `filter`, a partial index of the documents it selects.
    json_create_index / json_create_index_sync(space: String, name: String, path: String, filter: Option<String>) -> (),
    |db| run(db, Operation::JsonCreateIndex { space, name, path, filter }).map(drop)
);

method!(
    json_drop_index / json_drop_index_sync(space: String, name: String) -> bool,
    |db| Ok(answer!(run(db, Operation::JsonDropIndex { space, name })?, ResponseValue::Bool))
);

method!(
    json_list_indexes / json_list_indexes_sync(space: String) -> Vec<FfiJsonIndexInfo>,
    |db| Ok(answer!(run(db, Operation::JsonListIndexes { space })?, ResponseValue::JsonIndexes).into_iter().map(Into::into).collect())
);

method!(
    graph_set_node / graph_set_node_sync(graph: String, id: String, kind: String, metadata_json: String) -> (),
    |db| {
        let metadata = serde_json::from_str(&metadata_json)?;
        let operation = BatchOperation::GraphSetNode { graph, id, kind, metadata, doc_space: None, doc_id: None };
        write(db, operation).map(drop)
    }
);

method!(
    graph_get_node / graph_get_node_sync(graph: String, id: String) -> Option<FfiGraphNode>,
    |db| Ok(answer!(run(db, Operation::GraphGetNode { graph, id })?, ResponseValue::GraphNode).map(graph_node))
);

method!(
    graph_delete_node / graph_delete_node_sync(graph: String, id: String) -> bool,
    |db| Ok(answer!(run(db, Operation::GraphDeleteNode { graph, id })?, ResponseValue::Bool))
);

method!(
    graph_add_edge / graph_add_edge_sync(graph: String, source: String, target: String, kind: String) -> (),
    |db| write(db, BatchOperation::GraphAddEdge { graph, source, target, kind, weight: 1.0 }).map(drop)
);

method!(
    graph_remove_edge / graph_remove_edge_sync(graph: String, source: String, target: String, kind: String) -> bool,
    |db| Ok(answer!(run(db, Operation::GraphRemoveEdge { graph, source, target, kind })?, ResponseValue::Bool))
);

method!(
    graph_neighbors / graph_neighbors_sync(graph: String, node_id: String, direction: FfiDirection, kind_filter: Option<String>) -> Vec<String>,
    |db| {
        let operation = Operation::GraphNeighbors { graph, node: node_id, direction: direction.into(), kind: kind_filter };
        Ok(answer!(run(db, operation)?, ResponseValue::Ids))
    }
);

method!(
    graph_reachability / graph_reachability_sync(graph: String, start_nodes: Vec<String>, direction: FfiDirection) -> Vec<String>,
    |db| {
        let operation = Operation::GraphReachability { graph, start_nodes, direction: direction.into() };
        Ok(answer!(run(db, operation)?, ResponseValue::Ids))
    }
);

method!(
    fts_index / fts_index_sync(collection: String, doc_id: String, content: String) -> (),
    |db| write(db, BatchOperation::FtsIndex { collection, doc_id, content }).map(drop)
);

method!(
    fts_delete / fts_delete_sync(collection: String, doc_id: String) -> bool,
    |db| Ok(answer!(run(db, Operation::FtsDelete { collection, doc_id })?, ResponseValue::Bool))
);

method!(
    fts_search / fts_search_sync(collection: String, query: String, limit: u32) -> Vec<FfiFtsHit>,
    |db| {
        let hits = answer!(run(db, Operation::FtsSearch { collection, query, limit })?, ResponseValue::FtsHits);
        Ok(hits.into_iter().map(fts_hit).collect())
    }
);

method!(
    fts_suggest / fts_suggest_sync(collection: String, prefix: String, max_distance: u8, limit: u32) -> Vec<FfiFtsSuggestion>,
    |db| {
        let operation = Operation::FtsSuggest { collection, prefix, max_distance, limit };
        let suggestions = answer!(run(db, operation)?, ResponseValue::FtsSuggestions);
        Ok(suggestions
            .into_iter()
            .map(|s| FfiFtsSuggestion { term: s.term, distance: s.distance, doc_count: s.doc_count, exact_prefix: s.exact_prefix })
            .collect())
    }
);

method!(
    fts_fuzzy_search / fts_fuzzy_search_sync(collection: String, query: String, max_distance: u8, limit: u32) -> Vec<FfiFtsFuzzyHit>,
    |db| {
        let operation = Operation::FtsFuzzySearch { collection, query, max_distance, limit };
        let hits = answer!(run(db, operation)?, ResponseValue::FtsFuzzyHits);
        Ok(hits
            .into_iter()
            .map(|h| FfiFtsFuzzyHit {
                collection: h.collection,
                doc_id: h.doc_id,
                snippet: h.snippet,
                rank: h.rank,
                edit_distance: h.edit_distance,
                matched_terms: h.matched_terms,
            })
            .collect())
    }
);

method!(
    fts_phonetic_search / fts_phonetic_search_sync(collection: String, query: String, limit: u32) -> Vec<FfiFtsPhoneticHit>,
    |db| {
        let operation = Operation::FtsPhoneticSearch { collection, query, limit };
        let hits = answer!(run(db, operation)?, ResponseValue::FtsPhoneticHits);
        Ok(hits
            .into_iter()
            .map(|h| FfiFtsPhoneticHit {
                collection: h.collection,
                doc_id: h.doc_id,
                snippet: h.snippet,
                rank: h.rank,
                matched_terms: h.matched_terms,
                phonetic_codes: h.phonetic_codes,
            })
            .collect())
    }
);

method!(
    fts_load_dictionary / fts_load_dictionary_sync(collection: String, words: Vec<String>) -> (),
    |db| run(db, Operation::FtsLoadDictionary { collection, words }).map(drop)
);

method!(
    /// A read-only SQL query (`DataFusion` dialect) with `$1`, `$2`... bound to the elements of
    /// `params_json` (a JSON array); each row comes as a JSON array.
    sql_query / sql_query_sync(sql: String, params_json: Option<String>) -> FfiSqlQueryResult,
    |db| {
        let params = params_json.as_deref().map(serde_json::from_str).transpose()?.unwrap_or_default();
        let result = answer!(run(db, Operation::SqlQuery { sql, params })?, ResponseValue::Sql);
        Ok(FfiSqlQueryResult {
            rows_json: result.rows.iter().map(serde_json::to_string).collect::<Result<_, _>>()?,
            columns: result.columns,
            rows_affected: result.rows_affected,
        })
    }
);

method!(
    /// Writes the memtable to a sorted run: a point to call before the app is suspended.
    checkpoint / checkpoint_sync() -> (),
    |db| run(db, Operation::Checkpoint).map(drop)
);

method!(
    /// Verifies the ledger, fast; true without a ledger.
    quick_check / quick_check_sync() -> bool,
    |db| Ok(answer!(run(db, Operation::QuickCheck)?, ResponseValue::Bool))
);

method!(
    /// Reads every stored entry, verifying each block's checksum, and verifies the ledger.
    integrity_check / integrity_check_sync() -> bool,
    |db| Ok(answer!(run(db, Operation::IntegrityCheck)?, ResponseValue::Bool))
);

method!(
    ts_append_points / ts_append_points_sync(series_id: String, points: Vec<FfiTimeSeriesPoint>) -> (),
    |db| {
        let points = points.into_iter().map(Into::into).collect();
        run(db, Operation::TsAppendPoints { series_id, points, resolution_ms: None }).map(drop)
    }
);

method!(
    ts_query_range / ts_query_range_sync(series_id: String, start_time: i64, end_time: i64) -> Vec<FfiTimeSeriesPoint>,
    |db| {
        let operation = Operation::TsQueryRange { series_id, start_time, end_time, resolution_ms: None };
        Ok(points(answer!(run(db, operation)?, ResponseValue::TsPoints)))
    }
);

method!(
    ts_query_range_with_resolution / ts_query_range_with_resolution_sync(series_id: String, resolution_ms: i64, start_time: i64, end_time: i64) -> Vec<FfiTimeSeriesPoint>,
    |db| {
        let operation = Operation::TsQueryRange { series_id, start_time, end_time, resolution_ms: Some(resolution_ms) };
        Ok(points(answer!(run(db, operation)?, ResponseValue::TsPoints)))
    }
);

method!(
    ts_query_stats / ts_query_stats_sync(series_id: String, start_time: i64, end_time: i64) -> Option<FfiTimeSeriesStats>,
    |db| {
        let operation = Operation::TsQueryStats { series_id, start_time, end_time, resolution_ms: None };
        Ok(answer!(run(db, operation)?, ResponseValue::TsStats).map(Into::into))
    }
);

method!(
    ts_query_auto / ts_query_auto_sync(series_id: String, start_time: i64, end_time: i64, max_points: u32) -> Vec<FfiTimeSeriesPoint>,
    |db| {
        let operation = Operation::TsQueryAuto { series_id, start_time, end_time, max_points: max_points as usize };
        Ok(points(answer!(run(db, operation)?, ResponseValue::TsPoints)))
    }
);

method!(
    ts_set_policy / ts_set_policy_sync(policy: FfiTimeSeriesLifecyclePolicy) -> (),
    |db| run(db, Operation::TsSetPolicy { policy: policy.into() }).map(drop)
);

method!(
    ts_get_policy / ts_get_policy_sync(series_pattern: String) -> Option<FfiTimeSeriesLifecyclePolicy>,
    |db| Ok(answer!(run(db, Operation::TsGetPolicy { series_pattern })?, ResponseValue::TsPolicy).map(Into::into))
);

method!(
    ts_list_policies / ts_list_policies_sync() -> Vec<FfiTimeSeriesLifecyclePolicy>,
    |db| Ok(answer!(run(db, Operation::TsListPolicies)?, ResponseValue::TsPolicies).into_iter().map(Into::into).collect())
);

method!(
    ts_delete_policy / ts_delete_policy_sync(series_pattern: String) -> bool,
    |db| Ok(answer!(run(db, Operation::TsDeletePolicy { series_pattern })?, ResponseValue::Bool))
);

method!(
    /// Applies every series' lifecycle policy as of now.
    ts_apply_retention / ts_apply_retention_sync() -> FfiTimeSeriesRetentionStats,
    |db| Ok(answer!(run(db, Operation::TsApplyRetention { now_ms: None })?, ResponseValue::TsRetentionStats).into())
);

method!(
    geo_add_fence / geo_add_fence_sync(id: String, name: String, category: String, polygon_json: String, metadata: Option<String>) -> (),
    |db| run(db, Operation::GeoAddFence { id, name, category, polygon_json, metadata }).map(drop)
);

method!(
    geo_remove_fence / geo_remove_fence_sync(id: String) -> bool,
    |db| Ok(answer!(run(db, Operation::GeoRemoveFence { id })?, ResponseValue::Bool))
);

method!(
    geo_contains_point / geo_contains_point_sync(polygon_json: String, lon: f64, lat: f64) -> bool,
    |db| Ok(answer!(run(db, Operation::GeoContainsPoint { polygon_json, lon, lat })?, ResponseValue::Bool))
);

method!(
    geo_find_fences_for_point / geo_find_fences_for_point_sync(lon: f64, lat: f64) -> Vec<FfiGeofenceMatch>,
    |db| Ok(matches(answer!(run(db, Operation::GeoFindFencesForPoint { lon, lat })?, ResponseValue::GeofenceMatches)))
);

method!(
    /// Records the points of track `track_id`; the fences they enter.
    geo_record_gps_points / geo_record_gps_points_sync(track_id: String, points: Vec<FfiGpsPoint>) -> Vec<FfiGeofenceMatch>,
    |db| {
        let points = points.into_iter().map(Into::into).collect();
        Ok(matches(answer!(run(db, Operation::GeoRecordGpsPoints { track_id, points })?, ResponseValue::GeofenceMatches)))
    }
);

method!(
    geo_get_track_points / geo_get_track_points_sync(track_id: String, start_time: Option<i64>, end_time: Option<i64>) -> Vec<FfiGpsPoint>,
    |db| {
        let operation = Operation::GeoGetTrackPoints { track_id, start_time, end_time };
        Ok(answer!(run(db, operation)?, ResponseValue::GpsPoints).into_iter().map(Into::into).collect())
    }
);

method!(
    geo_track_distance / geo_track_distance_sync(track_id: String) -> f64,
    |db| Ok(answer!(run(db, Operation::GeoTrackDistance { track_id })?, ResponseValue::F64))
);
