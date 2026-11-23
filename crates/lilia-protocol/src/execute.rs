//! Runs an [`Operation`] on a database through its system session. The daemon, its HTTP
//! gateway and the Node binding all call [`execute`], so an operation means the same thing in
//! each of them.

use lilia_core::{ErrorCode, LiliaError, VectorElementType, VectorMetric};
use lilia_db::compose::{self, Fusion};
use lilia_db::{wire, Database, Fence, Polygon, Scan, Vector};
use serde_json::{Map, Value};

use crate::{Operation, ResponseValue, StoredVector, PROTOCOL_MAJOR, PROTOCOL_MINOR};

/// The geo collection of the bindings, whose API names no collection.
pub const GEO_COLLECTION: &str = "geo";

/// What the daemon and the bindings can do, as the handshake reports it.
const CAPABILITIES: [&str; 13] = [
    "kv",
    "json",
    "batch",
    "sql",
    "graph",
    "fts",
    "vector",
    "hybrid",
    "crdt",
    "timeseries",
    "geo",
    "undo",
    "diagnostics",
];

mod graph;

use graph::execute_graph;

type Result<T> = std::result::Result<T, LiliaError>;

/// A page of a scan: after `after`, at most `limit` records, every record when `limit` is 0.
fn page(after: Option<impl Into<Vec<u8>>>, limit: u32) -> Scan {
    let mut scan = Scan::all();
    if let Some(after) = after {
        scan = scan.after(after);
    }
    if limit > 0 {
        scan = scan.limit(limit as usize);
    }
    scan
}

/// A list limit: `None` (everything) when 0.
fn bound(limit: u32) -> Option<usize> {
    (limit > 0).then_some(limit as usize)
}

fn invalid(message: impl Into<String>) -> LiliaError {
    LiliaError::new(ErrorCode::InvalidInput, message, false)
}

fn vector(element_type: VectorElementType, dim: u32, data: Vec<u8>) -> Result<Vector> {
    Ok(Vector::new(element_type, dim, data)?)
}

/// Runs `operation` on `database` through its system session.
///
/// # Errors
///
/// The operation's error: invalid input, a conflict, a storage failure. A handshake of another
/// protocol major is refused with [`ErrorCode::Unsupported`].
#[allow(
    clippy::too_many_lines,
    reason = "one arm per operation, each a call on the model the operation names"
)]
pub fn execute(database: &Database, operation: Operation) -> Result<ResponseValue> {
    let session = database.system();
    let s = &session;
    Ok(match operation {
        Operation::Handshake { major, .. } if major == PROTOCOL_MAJOR => ResponseValue::Handshake {
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
            capabilities: CAPABILITIES.iter().map(|c| (*c).to_owned()).collect(),
        },
        Operation::Handshake { major, .. } => {
            return Err(LiliaError::new(
                ErrorCode::Unsupported,
                format!(
                    "protocol major {major} is not supported; this daemon speaks {PROTOCOL_MAJOR}"
                ),
                false,
            ));
        }
        Operation::Shutdown => ResponseValue::Shutdown,
        Operation::IntegrityCheck => ResponseValue::Bool(database.check_integrity(false)?.ok),
        Operation::QuickCheck => ResponseValue::Bool(database.check_integrity(true)?.ok),
        Operation::Checkpoint => {
            database.flush()?;
            ResponseValue::Ack
        }
        Operation::StorageStats => ResponseValue::StorageStats(database.storage_stats()),
        Operation::KvGet { namespace, key } => ResponseValue::Kv(s.kv_get(&namespace, &key)?),
        Operation::KvScan {
            namespace,
            after,
            limit,
        } => ResponseValue::KvPage(s.kv_scan(&namespace, &page(after, limit))?),
        Operation::JsonGet { space, id } => ResponseValue::Json(s.json_get(&space, &id)?),
        Operation::JsonScan {
            space,
            after,
            limit,
        } => ResponseValue::JsonPage(s.json_scan(&space, &page(after, limit))?),
        Operation::JsonScanPrefix {
            space,
            prefix,
            after_id,
            limit,
        } => ResponseValue::JsonPage(s.json_scan(&space, &page(after_id, limit).prefix(prefix))?),
        Operation::JsonExtract { space, id, path } => ResponseValue::JsonExtracted(
            s.json_get(&space, &id)?
                .and_then(|entry| entry.doc.extract(&path).map(|v| v.to_json_value())),
        ),
        Operation::JsonFind {
            space,
            query,
            after,
            limit,
        } => {
            let Value::Object(terms) = query else {
                return Err(invalid(
                    "a query is a JSON object of field paths (or index names) and values",
                ));
            };
            ResponseValue::JsonPage(s.json_find(&space, &terms, &page(after, limit))?)
        }
        Operation::JsonCreateIndex {
            space,
            name,
            path,
            filter,
        } => {
            database.create_index(&space, &name, &path, &lilia_core::IndexOptions { filter })?;
            ResponseValue::Ack
        }
        Operation::JsonDropIndex { space, name } => {
            ResponseValue::Bool(database.drop_index(&space, &name)?)
        }
        Operation::JsonListIndexes { space } => {
            ResponseValue::JsonIndexes(database.indexes(&space))
        }
        Operation::JsonScanByIndex {
            space,
            index_name,
            value,
            after,
            limit,
        } => {
            let mut terms = Map::new();
            terms.insert(index_name, Value::String(value));
            ResponseValue::JsonPage(s.json_find(&space, &terms, &page(after, limit))?)
        }
        Operation::JsonGetAtVersion { space, id, version } => {
            ResponseValue::Json(s.at_version(version)?.json_get(&space, &id)?)
        }
        Operation::JsonGetAtTimestamp {
            space,
            id,
            timestamp_ms,
        } => ResponseValue::Json(s.at_time(timestamp_ms)?.json_get(&space, &id)?),
        Operation::JsonHistory { space, id } => {
            ResponseValue::JsonRevisions(s.json_history(&space, &id)?)
        }
        Operation::JsonDiff {
            space,
            id,
            from_version,
            to_version,
        } => ResponseValue::JsonValue(s.json_diff(&space, &id, from_version, to_version)?),
        Operation::Batch { operations } => {
            let outcomes = s.commit(&wire::batch(&operations)?)?;
            ResponseValue::Mutations(wire::results(&outcomes))
        }
        Operation::SqlQuery { sql, params } => ResponseValue::Sql(s.sql(&sql, &params)?),
        Operation::FtsSearch {
            collection,
            query,
            limit,
        } => ResponseValue::FtsHits(s.fts(&collection).search(&query, limit as usize)?),
        Operation::FtsDelete { collection, doc_id } => {
            ResponseValue::Bool(s.fts(&collection).delete(&doc_id)?)
        }
        Operation::FtsSuggest {
            collection,
            prefix,
            max_distance,
            limit,
        } => ResponseValue::FtsSuggestions(s.fts(&collection).suggest(
            &prefix,
            max_distance,
            limit as usize,
        )?),
        Operation::FtsFuzzySearch {
            collection,
            query,
            max_distance,
            limit,
        } => ResponseValue::FtsFuzzyHits(s.fts(&collection).fuzzy_search(
            &query,
            max_distance,
            limit as usize,
        )?),
        Operation::FtsPhoneticSearch {
            collection,
            query,
            limit,
        } => ResponseValue::FtsPhoneticHits(
            s.fts(&collection).phonetic_search(&query, limit as usize)?,
        ),
        Operation::FtsLoadDictionary { collection, words } => {
            let words: Vec<&str> = words.iter().map(String::as_str).collect();
            s.fts(&collection).load_dictionary(&words)?;
            ResponseValue::Ack
        }
        Operation::VectorGet { collection, id } => {
            ResponseValue::Vector(s.vectors(&collection).get(&id)?.map(|v| StoredVector {
                dim: v.dim(),
                element_type: v.element_type(),
                data: v.data().to_vec(),
            }))
        }
        Operation::VectorSearch {
            collection,
            vector: data,
            dim,
            element_type,
            metric,
            limit,
        } => ResponseValue::VectorHits(s.vectors(&collection).search(
            &vector(element_type, dim, data)?,
            metric,
            limit as usize,
        )?),
        Operation::VectorCreateIndex { collection, config } => {
            database.create_vector_index(&collection, config)?;
            ResponseValue::Ack
        }
        Operation::VectorGetIndexInfo { collection } => {
            ResponseValue::VectorIndexInfo(database.vector_index(&collection)?)
        }
        Operation::VectorDropIndex { collection } => {
            ResponseValue::Bool(database.drop_vector_index(&collection)?)
        }
        Operation::VectorSearchFiltered {
            collection,
            query_vector,
            dim,
            element_type,
            metric,
            candidate_ids,
            limit,
        } => {
            let ids: Vec<&str> = candidate_ids.iter().map(String::as_str).collect();
            ResponseValue::VectorHits(s.vectors(&collection).search_within(
                &vector(element_type, dim, query_vector)?,
                metric,
                &ids,
                limit as usize,
            )?)
        }
        Operation::VectorSearchInGraphNeighbors {
            collection,
            graph,
            start_node,
            direction,
            query_vector,
            dim,
            element_type,
            metric,
            limit,
        } => ResponseValue::VectorHits(compose::vectors_in_graph(
            s,
            &collection,
            (&graph, &start_node, direction),
            &vector(element_type, dim, query_vector)?,
            metric,
            limit as usize,
        )?),
        Operation::VectorSearchByJsonIndex {
            collection,
            space,
            index_name,
            index_value,
            query_vector,
            dim,
            element_type,
            metric,
            limit,
        } => ResponseValue::VectorHits(compose::vectors_by_index(
            s,
            &collection,
            (&space, &index_name, &index_value),
            &vector(element_type, dim, query_vector)?,
            metric,
            limit as usize,
        )?),
        Operation::HybridSearch {
            collection,
            fts_query,
            vector: data,
            dim,
            element_type,
            metric,
            rrf_k,
            fts_weight,
            vector_weight,
            limit,
        } => {
            let query = match (data, dim) {
                (Some(data), Some(dim)) => Some((
                    vector(
                        element_type.unwrap_or(VectorElementType::Float32),
                        dim,
                        data,
                    )?,
                    metric.unwrap_or(VectorMetric::Cosine),
                )),
                (None, _) => None,
                (Some(_), None) => {
                    return Err(invalid("a hybrid search vector needs its dimension"));
                }
            };
            let fusion = Fusion {
                k: rrf_k,
                fts_weight,
                vector_weight,
            };
            ResponseValue::HybridHits(compose::hybrid(
                s,
                &collection,
                fts_query.as_deref(),
                query.as_ref().map(|(v, m)| (v, *m)),
                fusion,
                limit as usize,
            )?)
        }
        Operation::TsAppendPoints {
            series_id,
            points,
            resolution_ms,
        } => {
            let series = s.series(&series_id);
            match resolution_ms {
                Some(resolution) => series.append_at(resolution, &points)?,
                None => series.append(&points)?,
            };
            ResponseValue::Ack
        }
        Operation::TsQueryRange {
            series_id,
            start_time,
            end_time,
            resolution_ms,
        } => {
            let series = s.series(&series_id);
            ResponseValue::TsPoints(match resolution_ms {
                Some(resolution) => series.range_at(resolution, start_time, end_time)?,
                None => series.range(start_time, end_time)?,
            })
        }
        Operation::TsQueryAuto {
            series_id,
            start_time,
            end_time,
            max_points,
        } => ResponseValue::TsPoints(
            s.series(&series_id)
                .auto(start_time, end_time, max_points)?,
        ),
        Operation::TsQueryStats {
            series_id,
            start_time,
            end_time,
            resolution_ms,
        } => {
            let series = s.series(&series_id);
            ResponseValue::TsStats(match resolution_ms {
                Some(resolution) => series.stats_at(resolution, start_time, end_time)?,
                None => series.stats(start_time, end_time)?,
            })
        }
        Operation::TsSetPolicy { policy } => {
            database.set_series_policy(&policy)?;
            ResponseValue::Ack
        }
        Operation::TsGetPolicy { series_pattern } => {
            ResponseValue::TsPolicy(database.series_policy(&series_pattern)?)
        }
        Operation::TsListPolicies => ResponseValue::TsPolicies(database.series_policies()?),
        Operation::TsDeletePolicy { series_pattern } => {
            ResponseValue::Bool(database.delete_series_policy(&series_pattern)?)
        }
        Operation::TsApplyRetention { now_ms } => ResponseValue::TsRetentionStats(match now_ms {
            Some(now) => database.apply_series_retention_at(now)?,
            None => database.apply_series_retention()?,
        }),
        Operation::GeoAddFence {
            id,
            name,
            category,
            polygon_json,
            metadata,
        } => {
            let fence = Fence {
                name,
                category,
                polygon: Polygon::parse(&polygon_json)?,
                metadata,
            };
            s.geo(GEO_COLLECTION).put_fence(&id, &fence)?;
            ResponseValue::Ack
        }
        Operation::GeoRemoveFence { id } => {
            ResponseValue::Bool(s.geo(GEO_COLLECTION).remove_fence(&id)?)
        }
        Operation::GeoContainsPoint {
            polygon_json,
            lon,
            lat,
        } => ResponseValue::Bool(Polygon::parse(&polygon_json)?.contains(lon, lat)),
        Operation::GeoFindFencesForPoint { lon, lat } => {
            ResponseValue::GeofenceMatches(s.geo(GEO_COLLECTION).fences_at(lon, lat)?)
        }
        Operation::GeoRecordGpsPoints { track_id, points } => {
            ResponseValue::GeofenceMatches(s.geo(GEO_COLLECTION).record(&track_id, &points)?)
        }
        Operation::GeoGetTrackPoints {
            track_id,
            start_time,
            end_time,
        } => ResponseValue::GpsPoints(s.geo(GEO_COLLECTION).track(
            &track_id,
            start_time.unwrap_or(i64::MIN),
            end_time.unwrap_or(i64::MAX),
        )?),
        Operation::GeoTrackDistance { track_id } => {
            ResponseValue::F64(s.geo(GEO_COLLECTION).track_distance(&track_id)?)
        }
        Operation::CrdtCounterGet { space, id } => {
            ResponseValue::CrdtCounter(s.crdt(&space).counter(&id).value()?)
        }
        Operation::CrdtCounterState { space, id } => {
            ResponseValue::CrdtCounterState(s.crdt(&space).counter(&id).state()?)
        }
        Operation::CrdtCounterMerge { space, id, remote } => {
            let counter = s.crdt(&space).counter(&id);
            counter.merge(&remote)?;
            ResponseValue::CrdtCounter(counter.value()?)
        }
        Operation::CrdtSetGet { space, id } => {
            ResponseValue::Ids(s.crdt(&space).set(&id).elements()?.into_iter().collect())
        }
        Operation::CrdtSetContains { space, id, element } => {
            ResponseValue::Bool(s.crdt(&space).set(&id).contains(&element)?)
        }
        Operation::CrdtSetState { space, id } => {
            ResponseValue::CrdtSetState(s.crdt(&space).set(&id).state()?)
        }
        Operation::CrdtSetPrune {
            space,
            id,
            cutoff_ms,
        } => ResponseValue::Bool(s.crdt(&space).set(&id).prune(cutoff_ms)?),
        Operation::CrdtSetMerge { space, id, remote } => {
            s.crdt(&space).set(&id).merge(&remote)?;
            ResponseValue::Ack
        }
        Operation::CrdtJsonGet { space, id } => {
            ResponseValue::CrdtJson(s.crdt(&space).doc(&id).get()?)
        }
        Operation::CrdtJsonState { space, id } => {
            ResponseValue::CrdtJsonState(s.crdt(&space).doc(&id).state()?)
        }
        Operation::CrdtJsonMerge { space, id, remote } => {
            let outcome = s.crdt(&space).doc(&id).merge(&remote)?;
            ResponseValue::Mutations(wire::results(&[outcome]))
        }
        Operation::Undo => ResponseValue::UndoStep(database.undo()?),
        Operation::Redo => ResponseValue::UndoStep(database.redo()?),
        Operation::UndoHistory => ResponseValue::UndoSteps(database.undo_history()?),
        graph => execute_graph(s, graph)?,
    })
}
