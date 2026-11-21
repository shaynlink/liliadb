//! Operations run on a database through `execute`, the path every binding takes.

use lilia_core::{
    BatchOperation, Direction, ErrorCode, TimeSeriesPoint, VectorElementType, VectorMetric,
};
use lilia_db::{Database, Durability, Options, ResourceProfile};
use lilia_protocol::{execute, Operation, ResponseValue, PROTOCOL_MAJOR};
use serde_json::json;

fn open(dir: &std::path::Path) -> Database {
    let mut options = Options::for_profile(ResourceProfile::Constrained);
    options.engine.durability = Durability::Performance;
    options.undo_depth = 8;
    Database::open(dir, options).unwrap()
}

fn run(db: &Database, operation: Operation) -> ResponseValue {
    execute(db, operation).unwrap()
}

fn floats(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn batch(db: &Database, operations: Vec<BatchOperation>) -> Vec<u64> {
    let ResponseValue::Mutations(results) = run(db, Operation::Batch { operations }) else {
        panic!("a batch answers with its mutations");
    };
    results
        .iter()
        .map(|r| r.version.unwrap_or_default())
        .collect()
}

fn put(space: &str, id: &str, value: serde_json::Value) -> BatchOperation {
    BatchOperation::JsonPut {
        space: space.into(),
        id: id.into(),
        value,
        if_version: None,
    }
}

#[test]
#[allow(clippy::too_many_lines)] // reason: one pass over the operations of each model.
fn documents_round_trip_with_indexes_history_and_undo() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    assert!(matches!(
        run(
            &db,
            Operation::Handshake {
                major: PROTOCOL_MAJOR,
                minor: 0
            }
        ),
        ResponseValue::Handshake { .. }
    ));
    let refused = execute(&db, Operation::Handshake { major: 1, minor: 2 }).unwrap_err();
    assert_eq!(refused.code, ErrorCode::Unsupported);

    run(
        &db,
        Operation::JsonCreateIndex {
            space: "users".into(),
            name: "by_city".into(),
            path: "$.city".into(),
            filter: None,
        },
    );
    let versions = batch(
        &db,
        vec![
            put("users", "u1", json!({"name": "Ada", "city": "Paris"})),
            put("users", "u2", json!({"name": "Bob", "city": "Lyon"})),
            put("users", "x9", json!({"name": "Cy", "city": "Paris"})),
            BatchOperation::KvSet {
                namespace: "ns".into(),
                key: b"k".to_vec(),
                value: b"v".to_vec(),
                if_version: None,
                expires_at_ms: None,
            },
        ],
    );
    assert!(versions.iter().all(|&v| v > 0));

    let ResponseValue::Kv(Some(entry)) = run(
        &db,
        Operation::KvGet {
            namespace: "ns".into(),
            key: b"k".to_vec(),
        },
    ) else {
        panic!("the key was written");
    };
    assert_eq!(entry.value, b"v");

    let ResponseValue::JsonPage(paris) = run(
        &db,
        Operation::JsonScanByIndex {
            space: "users".into(),
            index_name: "by_city".into(),
            value: "Paris".into(),
            after: None,
            limit: 0,
        },
    ) else {
        panic!("an index scan answers with documents");
    };
    let ids: Vec<&str> = paris.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, ["u1", "x9"]);

    let ResponseValue::JsonPage(prefixed) = run(
        &db,
        Operation::JsonScanPrefix {
            space: "users".into(),
            prefix: "u".into(),
            after_id: Some("u1".into()),
            limit: 0,
        },
    ) else {
        panic!("a prefix scan answers with documents");
    };
    assert_eq!(prefixed.len(), 1);
    assert_eq!(prefixed[0].id, "u2");

    let ResponseValue::JsonExtracted(name) = run(
        &db,
        Operation::JsonExtract {
            space: "users".into(),
            id: "u2".into(),
            path: "$.name".into(),
        },
    ) else {
        panic!("an extraction answers with a value");
    };
    assert_eq!(name, Some(json!("Bob")));

    let before = versions[0];
    batch(
        &db,
        vec![put("users", "u1", json!({"name": "Ada", "city": "Nice"}))],
    );
    let ResponseValue::Json(Some(old)) = run(
        &db,
        Operation::JsonGetAtVersion {
            space: "users".into(),
            id: "u1".into(),
            version: before,
        },
    ) else {
        panic!("the old version is in the history");
    };
    assert_eq!(old.value()["city"], "Paris");
    let ResponseValue::JsonRevisions(revisions) = run(
        &db,
        Operation::JsonHistory {
            space: "users".into(),
            id: "u1".into(),
        },
    ) else {
        panic!("a history answers with revisions");
    };
    assert_eq!(revisions.len(), 2);

    let ResponseValue::UndoStep(Some(step)) = run(&db, Operation::Undo) else {
        panic!("the last commit can be undone");
    };
    assert!(step.keys > 0);
    let ResponseValue::Json(Some(restored)) = run(
        &db,
        Operation::JsonGet {
            space: "users".into(),
            id: "u1".into(),
        },
    ) else {
        panic!("the document is back");
    };
    assert_eq!(restored.value()["city"], "Paris");

    let ResponseValue::Sql(result) = run(
        &db,
        Operation::SqlQuery {
            sql: "SELECT count(*) FROM lilia_json WHERE space = $1".into(),
            params: vec![json!("users")],
        },
    ) else {
        panic!("SQL answers with rows");
    };
    assert_eq!(result.scalar_f64(), Some(3.0));

    assert!(matches!(
        run(&db, Operation::IntegrityCheck),
        ResponseValue::Bool(true)
    ));
    assert!(matches!(
        run(&db, Operation::Checkpoint),
        ResponseValue::Ack
    ));
    let ResponseValue::StorageStats(stats) = run(&db, Operation::StorageStats) else {
        panic!("storage statistics");
    };
    assert!(stats.levels.iter().any(|level| level.runs > 0));
}

#[test]
#[allow(clippy::too_many_lines)] // reason: one pass over the operations of each model.
fn graphs_searches_series_geo_and_crdts_go_through_their_models() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let edge = |source: &str, target: &str| BatchOperation::GraphAddEdge {
        graph: "g".into(),
        source: source.into(),
        target: target.into(),
        kind: "rel".into(),
        weight: 1.0,
    };
    let vector = |id: &str, x: f32| BatchOperation::VectorSet {
        collection: "kb".into(),
        id: id.into(),
        dim: 2,
        element_type: VectorElementType::Float32,
        vector: floats(&[x, 0.0]),
    };
    let text = |id: &str, content: &str| BatchOperation::FtsIndex {
        collection: "kb".into(),
        doc_id: id.into(),
        content: content.into(),
    };
    batch(
        &db,
        vec![
            edge("a", "b"),
            edge("b", "c"),
            vector("a", 0.0),
            vector("b", 1.0),
            vector("c", 2.0),
            vector("d", 0.1),
            text("a", "rust storage engine"),
            text("d", "gardening notes"),
            BatchOperation::CrdtPnCounterIncrement {
                space: "c".into(),
                id: "hits".into(),
                actor_id: "n1".into(),
                amount: 5,
            },
            BatchOperation::CrdtOrSetAdd {
                space: "c".into(),
                id: "tags".into(),
                actor_id: "n1".into(),
                element: "x".into(),
            },
        ],
    );

    let ResponseValue::WeightedPath(Some(path)) = run(
        &db,
        Operation::GraphShortestPath {
            graph: "g".into(),
            start: "a".into(),
            target: "c".into(),
            direction: Direction::Outbound,
            kind_filter: None,
        },
    ) else {
        panic!("c is reachable from a");
    };
    assert_eq!(path.nodes, ["a", "b", "c"]);
    let ResponseValue::Ids(reachable) = run(
        &db,
        Operation::GraphReachability {
            graph: "g".into(),
            start_nodes: vec!["b".into()],
            direction: Direction::Outbound,
        },
    ) else {
        panic!("reachable nodes");
    };
    assert!(reachable.contains(&"c".to_owned()));

    let ResponseValue::VectorHits(near) = run(
        &db,
        Operation::VectorSearchInGraphNeighbors {
            collection: "kb".into(),
            graph: "g".into(),
            start_node: "a".into(),
            direction: Direction::Outbound,
            query_vector: floats(&[0.0, 0.0]),
            dim: 2,
            element_type: VectorElementType::Float32,
            metric: VectorMetric::L2,
            limit: 10,
        },
    ) else {
        panic!("vector hits");
    };
    let ids: Vec<&str> = near.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(ids, ["a", "b", "c"], "d is outside the graph");

    let ResponseValue::HybridHits(hits) = run(
        &db,
        Operation::HybridSearch {
            collection: "kb".into(),
            fts_query: Some("storage".into()),
            vector: Some(floats(&[0.0, 0.0])),
            dim: Some(2),
            element_type: None,
            metric: Some(VectorMetric::L2),
            rrf_k: 0,
            fts_weight: 1.0,
            vector_weight: 1.0,
            limit: 3,
        },
    ) else {
        panic!("hybrid hits");
    };
    assert_eq!(hits[0].id, "a", "first in both rankings");

    let ResponseValue::FtsSuggestions(suggestions) = run(
        &db,
        Operation::FtsSuggest {
            collection: "kb".into(),
            prefix: "stor".into(),
            max_distance: 1,
            limit: 5,
        },
    ) else {
        panic!("suggestions");
    };
    assert!(suggestions.iter().any(|s| s.term == "storage"));

    run(
        &db,
        Operation::TsAppendPoints {
            series_id: "cpu".into(),
            points: (0..10i32)
                .map(|i| TimeSeriesPoint::new(i64::from(i) * 1_000, f64::from(i)))
                .collect(),
            resolution_ms: None,
        },
    );
    let ResponseValue::TsStats(Some(stats)) = run(
        &db,
        Operation::TsQueryStats {
            series_id: "cpu".into(),
            start_time: 0,
            end_time: 10_000,
            resolution_ms: None,
        },
    ) else {
        panic!("series statistics");
    };
    assert_eq!(stats.count, 10);

    let square = "[[2.0,48.0],[3.0,48.0],[3.0,49.0],[2.0,49.0],[2.0,48.0]]";
    run(
        &db,
        Operation::GeoAddFence {
            id: "zone".into(),
            name: "Zone".into(),
            category: "test".into(),
            polygon_json: square.into(),
            metadata: None,
        },
    );
    let ResponseValue::GeofenceMatches(inside) = run(
        &db,
        Operation::GeoFindFencesForPoint {
            lon: 2.5,
            lat: 48.5,
        },
    ) else {
        panic!("fences");
    };
    assert_eq!(inside.len(), 1);
    assert!(matches!(
        run(
            &db,
            Operation::GeoContainsPoint {
                polygon_json: square.into(),
                lon: 5.0,
                lat: 48.5,
            },
        ),
        ResponseValue::Bool(false)
    ));

    assert!(matches!(
        run(
            &db,
            Operation::CrdtCounterGet {
                space: "c".into(),
                id: "hits".into(),
            },
        ),
        ResponseValue::CrdtCounter(5)
    ));
    let ResponseValue::Ids(tags) = run(
        &db,
        Operation::CrdtSetGet {
            space: "c".into(),
            id: "tags".into(),
        },
    ) else {
        panic!("set elements");
    };
    assert_eq!(tags, ["x"]);
}

#[test]
#[allow(clippy::too_many_lines)] // reason: one pass over the analyses and states of each model.
fn analyses_states_and_undo_redo_answer_in_their_shapes() {
    use lilia_core::{CentralityType, ComponentType, GpsPoint, HnswConfig, PageRankConfig};

    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let node = |id: &str| BatchOperation::GraphSetNode {
        graph: "flow".into(),
        id: id.into(),
        kind: "step".into(),
        metadata: json!({}),
        doc_space: Some("steps".into()),
        doc_id: None,
    };
    let edge = |source: &str, target: &str| BatchOperation::GraphAddEdge {
        graph: "flow".into(),
        source: source.into(),
        target: target.into(),
        kind: "next".into(),
        weight: 2.0,
    };
    batch(
        &db,
        vec![
            put("steps", "s1", json!({"title": "start"})),
            node("s1"),
            node("s2"),
            node("s3"),
            edge("s1", "s2"),
            edge("s2", "s3"),
            edge("s3", "s1"),
            BatchOperation::FtsIndex {
                collection: "notes".into(),
                doc_id: "n1".into(),
                content: "ephemeral".into(),
            },
            BatchOperation::CrdtJsonSetField {
                space: "c".into(),
                id: "doc".into(),
                actor_id: "n1".into(),
                path: "title".into(),
                value: json!("draft"),
                timestamp_ms: Some(10),
            },
            BatchOperation::CrdtOrSetAdd {
                space: "c".into(),
                id: "tags".into(),
                actor_id: "n1".into(),
                element: "x".into(),
            },
            BatchOperation::CrdtPnCounterIncrement {
                space: "c".into(),
                id: "hits".into(),
                actor_id: "n1".into(),
                amount: 2,
            },
        ],
    );

    assert!(matches!(
        run(
            &db,
            Operation::GraphGetNodeDocument {
                graph: "flow".into(),
                id: "s1".into(),
            },
        ),
        ResponseValue::JsonExtracted(Some(document)) if document["title"] == "start"
    ));
    let ResponseValue::NodeScores(ranks) = run(
        &db,
        Operation::GraphPageRank {
            graph: "flow".into(),
            config: PageRankConfig::default(),
        },
    ) else {
        panic!("page ranks");
    };
    assert_eq!(ranks.len(), 3);
    let ResponseValue::NodeScores(degrees) = run(
        &db,
        Operation::GraphCentrality {
            graph: "flow".into(),
            centrality: CentralityType::Degree,
            direction: Direction::Both,
            kind_filter: None,
            normalized: false,
        },
    ) else {
        panic!("centralities");
    };
    assert_eq!(degrees.len(), 3);
    let ResponseValue::NodeGroups(components) = run(
        &db,
        Operation::GraphConnectedComponents {
            graph: "flow".into(),
            component_type: ComponentType::StronglyConnected,
            kind_filter: None,
        },
    ) else {
        panic!("components");
    };
    assert_eq!(components.len(), 1, "the three steps form a cycle");
    assert!(matches!(
        run(
            &db,
            Operation::GraphHasCycle {
                graph: "flow".into(),
                direction: Direction::Outbound,
                kind_filter: None,
            },
        ),
        ResponseValue::Bool(true)
    ));
    let ResponseValue::CypherResult(result) = run(
        &db,
        Operation::GraphCypher {
            graph: "flow".into(),
            query: "MATCH (s:step) RETURN s.id AS id".into(),
        },
    ) else {
        panic!("cypher rows");
    };
    assert_eq!(result.rows.len(), 3);

    run(
        &db,
        Operation::VectorCreateIndex {
            collection: "emb".into(),
            config: HnswConfig {
                dim: 2,
                metric: VectorMetric::L2,
                ..HnswConfig::default()
            },
        },
    );
    assert!(matches!(
        run(
            &db,
            Operation::VectorGetIndexInfo {
                collection: "emb".into()
            }
        ),
        ResponseValue::VectorIndexInfo(Some(_))
    ));
    assert!(matches!(
        run(
            &db,
            Operation::VectorDropIndex {
                collection: "emb".into()
            }
        ),
        ResponseValue::Bool(true)
    ));

    assert!(matches!(
        run(
            &db,
            Operation::FtsDelete {
                collection: "notes".into(),
                doc_id: "n1".into(),
            },
        ),
        ResponseValue::Bool(true)
    ));
    assert!(matches!(
        run(
            &db,
            Operation::CrdtJsonState {
                space: "c".into(),
                id: "doc".into(),
            },
        ),
        ResponseValue::CrdtJsonState(Some(_))
    ));
    assert!(matches!(
        run(
            &db,
            Operation::CrdtSetContains {
                space: "c".into(),
                id: "tags".into(),
                element: "x".into(),
            },
        ),
        ResponseValue::Bool(true)
    ));
    let ResponseValue::CrdtCounterState(state) = run(
        &db,
        Operation::CrdtCounterState {
            space: "c".into(),
            id: "hits".into(),
        },
    ) else {
        panic!("counter state");
    };
    assert!(matches!(
        run(
            &db,
            Operation::CrdtCounterMerge {
                space: "c".into(),
                id: "hits".into(),
                remote: state,
            },
        ),
        ResponseValue::CrdtCounter(2)
    ));

    let point = |timestamp: i64, longitude: f64| GpsPoint {
        timestamp,
        latitude: 48.85,
        longitude,
        altitude: None,
        speed: None,
        accuracy: None,
        h3_index: None,
    };
    run(
        &db,
        Operation::GeoRecordGpsPoints {
            track_id: "t".into(),
            points: vec![point(1_000, 2.35), point(2_000, 2.36)],
        },
    );
    let ResponseValue::GpsPoints(track) = run(
        &db,
        Operation::GeoGetTrackPoints {
            track_id: "t".into(),
            start_time: None,
            end_time: None,
        },
    ) else {
        panic!("track points");
    };
    assert_eq!(track.len(), 2);
    let ResponseValue::F64(distance) = run(
        &db,
        Operation::GeoTrackDistance {
            track_id: "t".into(),
        },
    ) else {
        panic!("track distance");
    };
    assert!(distance > 500.0, "{distance}");

    let first = batch(&db, vec![put("docs", "d", json!({"n": 1}))])[0];
    let second = batch(&db, vec![put("docs", "d", json!({"n": 2}))])[0];
    let ResponseValue::JsonValue(diff) = run(
        &db,
        Operation::JsonDiff {
            space: "docs".into(),
            id: "d".into(),
            from_version: first,
            to_version: second,
        },
    ) else {
        panic!("a diff");
    };
    assert_ne!(diff, json!(null));
    let ResponseValue::UndoSteps(steps) = run(&db, Operation::UndoHistory) else {
        panic!("undo history");
    };
    assert!(steps.len() >= 2);
    run(&db, Operation::Undo);
    assert!(matches!(
        run(&db, Operation::Redo),
        ResponseValue::UndoStep(Some(_))
    ));
    let ResponseValue::Json(Some(document)) = run(
        &db,
        Operation::JsonGet {
            space: "docs".into(),
            id: "d".into(),
        },
    ) else {
        panic!("the document");
    };
    assert_eq!(
        document.value()["n"],
        2,
        "the redo restored the undone write"
    );
    assert!(matches!(
        run(&db, Operation::QuickCheck),
        ResponseValue::Bool(true)
    ));
}
