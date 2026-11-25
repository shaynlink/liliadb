//! Analyses, states and undo/redo, in the shapes the bindings read.

use lilia_core::{BatchOperation, Direction, VectorMetric};
use lilia_protocol::{Operation, ResponseValue};
use serde_json::json;

use super::{batch, open, put, run};

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one pass over the analyses and states of each model"
)]
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
