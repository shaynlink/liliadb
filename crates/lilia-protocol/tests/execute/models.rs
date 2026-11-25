//! Graphs, searches, series, geo and CRDTs, each through its model.

use lilia_core::{BatchOperation, Direction, TimeSeriesPoint, VectorElementType, VectorMetric};
use lilia_protocol::{Operation, ResponseValue};

use super::{batch, floats, open, run};

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one pass over the operations of each model"
)]
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
