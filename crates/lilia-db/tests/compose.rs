//! Searches composed from several models, storage statistics, integrity checks.

mod common;

use common::small_options;
use lilia_db::compose::{self, Fusion};
use lilia_db::{Database, Direction, IndexOptions, Policy, SecurityContext, Vector, VectorMetric};
use lilia_engine::Durability;
use serde_json::json;

fn open(dir: &std::path::Path, ledger: bool) -> Database {
    Database::open(dir, small_options(Durability::Performance, ledger)).unwrap()
}

fn v(x: f32, y: f32) -> Vector {
    Vector::from_f32(&[x, y]).unwrap()
}

fn ids(hits: &[lilia_db::VectorHit]) -> Vec<&str> {
    hits.iter().map(|h| h.id.as_str()).collect()
}

#[test]
fn vector_searches_are_restricted_to_a_graph_or_an_index() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();
    for (id, x) in [("a", 0.0), ("b", 1.0), ("c", 2.0), ("d", 0.1)] {
        s.vectors("emb").set(id, &v(x, 0.0)).unwrap();
    }
    let graph = s.graph("g");
    graph.add_edge("a", "b", "rel", 1.0).unwrap();
    graph.add_edge("b", "c", "rel", 1.0).unwrap();
    let near = compose::vectors_in_graph(
        &s,
        "emb",
        ("g", "a", Direction::Outbound),
        &v(0.0, 0.0),
        VectorMetric::L2,
        10,
    )
    .unwrap();
    assert_eq!(ids(&near), ["a", "b", "c"], "d is outside the graph");

    db.create_index("docs", "by_tag", "$.tag", &IndexOptions::default())
        .unwrap();
    for (id, tag, visibility) in [
        ("a", "x", "public"),
        ("b", "y", "public"),
        ("c", "x", "public"),
        ("d", "x", "private"),
    ] {
        s.json_put("docs", id, &json!({"tag": tag, "visibility": visibility}))
            .unwrap();
    }
    let tagged = compose::vectors_by_index(
        &s,
        "emb",
        ("docs", "by_tag", "x"),
        &v(0.0, 0.0),
        VectorMetric::L2,
        10,
    )
    .unwrap();
    assert_eq!(ids(&tagged), ["a", "d", "c"]);
    db.create_policy("public", Policy::new("docs", "$.visibility == 'public'"))
        .unwrap();
    let reader = db.session(SecurityContext::from_pair("role", "reader"));
    let visible = compose::vectors_by_index(
        &reader,
        "emb",
        ("docs", "by_tag", "x"),
        &v(0.0, 0.0),
        VectorMetric::L2,
        10,
    )
    .unwrap();
    assert_eq!(ids(&visible), ["a", "c"], "the policy hides d's document");
}

#[test]
fn hybrid_search_fuses_text_and_vector_rankings() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();
    for (id, text, x) in [
        ("both", "rust storage engine", 0.0),
        ("text", "rust storage engine design notes", 5.0),
        ("vector", "gardening", 0.1),
    ] {
        s.fts("kb").index(id, text).unwrap();
        s.vectors("kb").set(id, &v(x, 0.0)).unwrap();
    }
    let hits = compose::hybrid(
        &s,
        "kb",
        Some("storage"),
        Some((&v(0.0, 0.0), VectorMetric::L2)),
        Fusion::default(),
        10,
    )
    .unwrap();
    assert_eq!(hits[0].id, "both", "first in both rankings");
    assert!(hits[0].fts_rank.is_some() && hits[0].vector_rank.is_some());
    assert_eq!(hits.len(), 3);
    let text_only = compose::hybrid(&s, "kb", Some("storage"), None, Fusion::default(), 1).unwrap();
    assert_eq!(text_only.len(), 1);
    assert!(text_only[0].vector_rank.is_none());
    let vector_heavy = Fusion {
        fts_weight: 0.0,
        ..Fusion::default()
    };
    let hits = compose::hybrid(
        &s,
        "kb",
        Some("storage"),
        Some((&v(0.0, 0.0), VectorMetric::L2)),
        vector_heavy,
        10,
    )
    .unwrap();
    assert_eq!(hits[0].id, "both");
    assert_eq!(hits[1].id, "vector");
}

#[test]
fn statistics_and_integrity_checks_describe_the_storage() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), true);
    let s = db.system();
    for i in 0..500u32 {
        s.kv_put("ns", &i.to_be_bytes(), &[7u8; 64]).unwrap();
    }
    db.flush().unwrap();
    let stats = db.storage_stats();
    assert!(stats.levels.iter().any(|level| level.runs > 0), "{stats:?}");
    assert!(stats.version > 500 && stats.oldest_version <= stats.version);

    let full = db.check_integrity(false).unwrap();
    assert!(full.ok, "{full:?}");
    assert!(full.entries >= 500);
    assert!(full.ledger.unwrap().is_valid);
    let quick = db.check_integrity(true).unwrap();
    assert!(quick.ok && quick.entries == 0 && quick.ledger.is_some());

    let plain_dir = tempfile::tempdir().unwrap();
    let plain = open(plain_dir.path(), false);
    let report = plain.check_integrity(false).unwrap();
    assert!(report.ok && report.ledger.is_none());
}
