//! Vectors: storage, exact and HNSW search, index maintenance, quantization, transactions,
//! security, ledger, concurrency.

mod common;

use std::collections::HashSet;
use std::sync::Arc;

use common::{small_options, Rng};
use lilia_db::{
    Database, Error, HnswConfig, LedgerVerifyMode, Policy, SecurityContext, Vector,
    VectorElementType, VectorMetric,
};
use lilia_engine::Durability;
use serde_json::json;

fn open(dir: &std::path::Path, ledger: bool) -> Database {
    Database::open(dir, small_options(Durability::Performance, ledger)).unwrap()
}

fn random(rng: &mut Rng, dim: usize) -> Vec<f32> {
    #[allow(clippy::cast_precision_loss)] // reason: values below 2^20, exact in f32
    (0..dim)
        .map(|_| (rng.below(1 << 20) as f32 / (1 << 19) as f32) - 1.0)
        .collect()
}

fn v(values: &[f32]) -> Vector {
    Vector::from_f32(values).unwrap()
}

fn config(metric: VectorMetric, element_type: VectorElementType) -> HnswConfig {
    HnswConfig {
        m: 16,
        ef_construction: 100,
        ef_search: 64,
        metric,
        element_type,
        dim: 0,
    }
}

fn ids(hits: &[lilia_db::VectorHit]) -> Vec<&str> {
    hits.iter().map(|h| h.id.as_str()).collect()
}

#[test]
fn vectors_are_stored_validated_and_searched_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();
    let c = s.vectors("points");
    c.set("x", &v(&[1.0, 0.0])).unwrap();
    c.set("y", &v(&[0.0, 1.0])).unwrap();
    c.set("xy", &v(&[0.7, 0.7])).unwrap();
    c.set("far", &v(&[-5.0, -5.0])).unwrap();
    c.set("i8", &Vector::from_i8(&[100, 0]).unwrap()).unwrap();

    assert_eq!(c.get("x").unwrap(), Some(v(&[1.0, 0.0])));
    assert!(c.get("missing").unwrap().is_none());
    let near = c.search(&v(&[1.0, 0.1]), VectorMetric::L2, 3).unwrap();
    assert_eq!(
        ids(&near),
        ["x", "xy", "y"],
        "the int8 vector is compared after quantizing the query"
    );
    let cosine = c.search(&v(&[2.0, 0.0]), VectorMetric::Cosine, 2).unwrap();
    assert!(
        cosine[0].distance.abs() < 1e-6
            && cosine.iter().map(|h| h.id.as_str()).collect::<HashSet<_>>()
                == HashSet::from(["x", "i8"])
    );
    let within = c
        .search_within(&v(&[1.0, 0.0]), VectorMetric::L2, &["y", "far", "nope"], 5)
        .unwrap();
    assert_eq!(ids(&within), ["y", "far"]);
    assert!(
        c.search(&v(&[1.0, 0.0, 0.0]), VectorMetric::L2, 5)
            .unwrap()
            .is_empty(),
        "no vector of dimension 3"
    );
    assert!(c
        .search(&v(&[1.0, 0.0]), VectorMetric::L2, 0)
        .unwrap()
        .is_empty());

    assert!(c.delete("far").unwrap());
    assert!(!c.delete("far").unwrap());
    assert!(matches!(
        Vector::from_f32(&[f32::INFINITY]),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        c.set("bad\0", &v(&[1.0, 0.0])),
        Err(Error::InvalidArgument(_))
    ));
}

/// HNSW search finds what exact search finds (recall@10 >= 0.95), stays exact as vectors are
/// replaced and deleted, survives a reopen, and is rejected for vectors that do not fit it.
#[test]
fn hnsw_index_is_accurate_maintained_and_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = Rng(42);
    let dim = 16;
    let queries: Vec<Vec<f32>> = (0..30).map(|_| random(&mut rng, dim)).collect();
    {
        let db = open(dir.path(), false);
        let s = db.system();
        let c = s.vectors("emb");
        for i in 0..800 {
            c.set(&format!("e{i:04}"), &v(&random(&mut rng, dim)))
                .unwrap();
        }
        assert!(db.vector_index("emb").unwrap().is_none());
        let exact: Vec<Vec<String>> = queries
            .iter()
            .map(|q| {
                c.search(&v(q), VectorMetric::L2, 10)
                    .unwrap()
                    .into_iter()
                    .map(|h| h.id)
                    .collect()
            })
            .collect();

        db.create_vector_index("emb", config(VectorMetric::L2, VectorElementType::Float32))
            .unwrap();
        let info = db.vector_index("emb").unwrap().unwrap();
        assert_eq!((info.node_count, info.dim), (800, 16));
        let mut found = 0;
        for (q, expected) in queries.iter().zip(&exact) {
            let hits = c.search(&v(q), VectorMetric::L2, 10).unwrap();
            found += hits.iter().filter(|h| expected.contains(&h.id)).count();
        }
        assert!(found >= 285, "recall@10 = {found}/300");

        c.set("e0001", &v(&queries[0])).unwrap();
        assert_eq!(
            c.search(&v(&queries[0]), VectorMetric::L2, 1).unwrap()[0].id,
            "e0001",
            "a replaced vector is found at its new place"
        );
        c.delete("e0001").unwrap();
        assert_ne!(
            c.search(&v(&queries[0]), VectorMetric::L2, 1).unwrap()[0].id,
            "e0001",
            "a deleted vector is gone"
        );
        assert_eq!(db.vector_index("emb").unwrap().unwrap().node_count, 799);

        assert!(
            matches!(c.set("short", &v(&[1.0])), Err(Error::InvalidArgument(_))),
            "wrong dimension"
        );
        let bits = Vector::new(VectorElementType::Bit, 16, vec![0; 2]).unwrap();
        assert!(
            matches!(c.set("bits", &bits), Err(Error::InvalidArgument(_))),
            "wrong element type"
        );
        db.close().unwrap();
    }
    let db = open(dir.path(), false);
    let s = db.system();
    let c = s.vectors("emb");
    let hits = c.search(&v(&queries[1]), VectorMetric::L2, 5).unwrap();
    let exact = c
        .search_within(
            &v(&queries[1]),
            VectorMetric::L2,
            &hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
            5,
        )
        .unwrap();
    assert_eq!(
        ids(&hits),
        ids(&exact),
        "the rebuilt index returns exact distances"
    );
    assert_eq!(db.vector_index("emb").unwrap().unwrap().node_count, 799);
    assert!(db.drop_vector_index("emb").unwrap());
    assert!(!db.drop_vector_index("emb").unwrap());
    c.set("short", &v(&[1.0])).unwrap();
    assert!(
        matches!(
            db.create_vector_index("emb", config(VectorMetric::L2, VectorElementType::Float32)),
            Err(Error::InvalidArgument(_))
        ),
        "a stored vector does not fit"
    );
    assert!(matches!(
        db.create_vector_index(
            "empty",
            config(VectorMetric::L2, VectorElementType::Float32)
        ),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn quantized_indexes_rerank_with_exact_distances() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();
    let c = s.vectors("q");
    let mut rng = Rng(7);
    let points: Vec<Vec<f32>> = (0..400).map(|_| random(&mut rng, 16)).collect();
    for (i, p) in points.iter().enumerate() {
        c.set(&format!("p{i}"), &v(p)).unwrap();
    }
    let query = v(&points[17]);
    let exact = c.search(&query, VectorMetric::Cosine, 5).unwrap();
    db.create_vector_index("q", config(VectorMetric::Cosine, VectorElementType::Int8))
        .unwrap();
    let hits = c.search(&query, VectorMetric::Cosine, 5).unwrap();
    assert_eq!(hits[0].id, "p17");
    assert!(
        hits[0].distance.abs() < 1e-6,
        "distances are computed on the stored f32 vectors"
    );
    assert!(
        hits.iter()
            .filter(|h| exact.iter().any(|e| e.id == h.id))
            .count()
            >= 4
    );
    let l2 = c.search(&query, VectorMetric::L2, 1).unwrap();
    assert_eq!(l2[0].id, "p17", "another metric searches exactly");
}

#[test]
fn transactions_see_their_own_vectors_and_validate_reads() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();
    let c = s.vectors("t");
    for i in 0..50u8 {
        c.set(&format!("v{i}"), &v(&[f32::from(i), 0.0])).unwrap();
    }
    db.create_vector_index("t", config(VectorMetric::L2, VectorElementType::Float32))
        .unwrap();
    let tx = s.transaction();
    tx.vectors("t").set("mine", &v(&[100.0, 0.0])).unwrap();
    assert!(tx.vectors("t").delete("v49").unwrap());
    let hits = tx
        .vectors("t")
        .search(&v(&[99.0, 0.0]), VectorMetric::L2, 2)
        .unwrap();
    assert_eq!(
        ids(&hits),
        ["mine", "v48"],
        "own vectors found, own deletions hidden"
    );
    assert!(c.get("mine").unwrap().is_none());
    s.vectors("t").set("v48", &v(&[0.0, 9.0])).unwrap();
    assert!(
        matches!(tx.commit(), Err(Error::Conflict { .. })),
        "a hit read by the transaction changed"
    );

    let tx = s.transaction();
    tx.vectors("t").set("mine", &v(&[100.0, 0.0])).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        c.search(&v(&[99.0, 0.0]), VectorMetric::L2, 1).unwrap()[0].id,
        "mine",
        "committed transaction vectors reach the index"
    );
}

#[test]
fn hits_pass_the_callers_read_policies_and_vectors_are_ledger_records() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), true);
    let s = db.system();
    s.json_put("items", "a", &json!({"owner": "ann"})).unwrap();
    s.json_put("items", "b", &json!({"owner": "bob"})).unwrap();
    s.vectors("items").set("a", &v(&[1.0, 0.0])).unwrap();
    s.vectors("items").set("b", &v(&[1.0, 0.1])).unwrap();
    s.vectors("items").set("c", &v(&[1.0, 0.2])).unwrap();
    db.create_policy(
        "owner",
        Policy::new("items", "$.owner == current_context('user')"),
    )
    .unwrap();
    let bob = db.session(SecurityContext::from_pair("user", "bob"));
    assert_eq!(
        ids(&bob
            .vectors("items")
            .search(&v(&[1.0, 0.0]), VectorMetric::L2, 5)
            .unwrap()),
        ["b"]
    );
    assert!(bob.vectors("items").get("a").unwrap().is_none());
    db.create_vector_index(
        "items",
        config(VectorMetric::L2, VectorElementType::Float32),
    )
    .unwrap();
    assert_eq!(
        ids(&bob
            .vectors("items")
            .search(&v(&[1.0, 0.0]), VectorMetric::L2, 1)
            .unwrap()),
        ["b"],
        "denied hits do not use up the limit"
    );

    let report = db.verify_ledger(LedgerVerifyMode::Strict).unwrap();
    assert!(report.is_valid, "{report:?}");
    assert!(db
        .prove_vector("items", "c")
        .unwrap()
        .verify(&report.merkle_root));
}

/// Writers replace the same vectors concurrently: the index ends with each id at its last
/// committed vector.
#[test]
fn concurrent_writers_leave_the_index_exact() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(open(dir.path(), false));
    db.system()
        .vectors("c")
        .set("seed", &v(&[0.0, 0.0, 0.0]))
        .unwrap();
    db.create_vector_index("c", config(VectorMetric::L2, VectorElementType::Float32))
        .unwrap();
    let writers: Vec<_> = (0..8u8)
        .map(|t| {
            let db = Arc::clone(&db);
            std::thread::spawn(move || {
                for i in 0..60u8 {
                    let id = format!("k{}", i % 15);
                    db.system()
                        .vectors("c")
                        .set(&id, &v(&[f32::from(t), f32::from(i), 1.0]))
                        .unwrap();
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let s = db.system();
    let c = s.vectors("c");
    assert_eq!(db.vector_index("c").unwrap().unwrap().node_count, 16);
    for k in 0..15 {
        let id = format!("k{k}");
        let stored = c.get(&id).unwrap().unwrap();
        let hit = &c.search(&stored, VectorMetric::L2, 1).unwrap()[0];
        assert!(
            hit.distance.abs() < 1e-9,
            "{id} is indexed at its last vector"
        );
    }
}
