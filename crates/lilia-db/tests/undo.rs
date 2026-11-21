//! Undo and redo of commits: every model, the ledger, the depth, catalog changes, concurrency.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::small_options;
use lilia_db::{
    Batch, Database, Error, HnswConfig, IndexOptions, LedgerVerifyMode, Options, Scan, Vector,
    VectorElementType, VectorMetric,
};
use lilia_engine::Durability;
use serde_json::json;

fn open(dir: &std::path::Path, ledger: bool, depth: usize) -> Database {
    let options = Options {
        undo_depth: depth,
        history_retention: Duration::from_secs(3600),
        ..small_options(Durability::Performance, ledger)
    };
    Database::open(dir, options).unwrap()
}

#[test]
fn commits_are_undone_and_redone_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), true, 10);
    let s = db.system();
    let mut first = Batch::new();
    first
        .json_put("users", "u1", &json!({"name": "Ada", "role": "admin"}))
        .unwrap();
    first.kv_put("settings", "theme", "dark");
    s.commit(&first).unwrap();
    let mut second = Batch::new();
    second
        .json_put("users", "u1", &json!({"name": "Ada", "role": "superadmin"}))
        .unwrap();
    second.kv_put("settings", "theme", "light");
    let second_version = s.commit(&second).unwrap()[0].version.unwrap();

    let history = db.undo_history().unwrap();
    assert_eq!(history.len(), 2);
    assert!(history[0].version <= second_version && history[0].keys == 2);

    let step = db.undo().unwrap().unwrap();
    assert_eq!(step, history[0]);
    let role = || {
        s.json_get("users", "u1")
            .unwrap()
            .map(|d| d.value()["role"].clone())
    };
    let theme = || s.kv_get("settings", b"theme").unwrap().map(|e| e.value);
    assert_eq!(role(), Some(json!("admin")));
    assert_eq!(theme(), Some(b"dark".to_vec()));

    assert!(db.redo().unwrap().is_some());
    assert_eq!(role(), Some(json!("superadmin")));
    assert_eq!(theme(), Some(b"light".to_vec()));
    assert!(db.redo().unwrap().is_none(), "nothing left to redo");

    assert!(db.undo().unwrap().is_some());
    assert!(db.undo().unwrap().is_some());
    assert_eq!(role(), None);
    assert_eq!(theme(), None);
    assert!(db.undo().unwrap().is_none());

    // A new commit clears what could be redone.
    assert!(db.redo().unwrap().is_some());
    s.kv_put("settings", b"other", b"x").unwrap();
    assert!(db.redo().unwrap().is_none());
    assert_eq!(role(), Some(json!("admin")));

    let report = db.verify_ledger(LedgerVerifyMode::Strict).unwrap();
    assert!(report.is_valid, "{:?}", report.error);
}

#[test]
fn undo_restores_every_model_with_its_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false, 10);
    let s = db.system();
    db.create_index("docs", "by_tag", "$.tag", &IndexOptions::default())
        .unwrap();
    let hnsw = HnswConfig {
        m: 16,
        ef_construction: 100,
        ef_search: 64,
        metric: VectorMetric::L2,
        element_type: VectorElementType::Float32,
        dim: 2,
    };
    db.create_vector_index("embeddings", hnsw).unwrap();
    s.json_put("docs", "d1", &json!({"tag": "a"})).unwrap();
    s.fts("articles").index("a1", "storage engines").unwrap();
    s.vectors("embeddings")
        .set("v1", &Vector::from_f32(&[1.0, 0.0]).unwrap())
        .unwrap();
    let find = |tag: &str| {
        let mut query = serde_json::Map::new();
        query.insert("tag".into(), json!(tag));
        s.json_find("docs", &query, &Scan::all()).unwrap().len()
    };
    let hits = |term: &str| s.fts("articles").search(term, 10).unwrap().len();
    let nearest = || {
        s.vectors("embeddings")
            .search(&Vector::from_f32(&[0.0, 1.0]).unwrap(), VectorMetric::L2, 1)
            .unwrap()
            .into_iter()
            .map(|h| h.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        (find("a"), hits("storage"), nearest()),
        (1, 1, vec!["v1".to_owned()])
    );

    let mut batch = Batch::new();
    batch.json_put("docs", "d1", &json!({"tag": "b"})).unwrap();
    s.commit(&batch).unwrap();
    s.fts("articles").index("a1", "graph databases").unwrap();
    s.vectors("embeddings")
        .set("v2", &Vector::from_f32(&[0.0, 1.0]).unwrap())
        .unwrap();
    assert_eq!((find("a"), find("b"), hits("graph")), (0, 1, 1));
    assert_eq!(nearest(), ["v2"]);

    for _ in 0..3 {
        db.undo().unwrap().unwrap();
    }
    assert_eq!(
        (find("a"), find("b")),
        (1, 0),
        "index entries restored with the document"
    );
    assert_eq!((hits("storage"), hits("graph")), (1, 0));
    assert_eq!(nearest(), ["v1"], "the vector index follows the undo");
    assert!(s
        .fts("articles")
        .suggest("gra", 0, 5)
        .unwrap()
        .iter()
        .all(|suggestion| suggestion.term != "graph"));
}

#[test]
fn depth_window_and_catalog_changes_bound_the_entries() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false, 2);
    let s = db.system();
    for i in 0..4u8 {
        s.kv_put("ns", &[i], b"v").unwrap();
    }
    std::thread::sleep(Duration::from_millis(1_100));
    s.kv_put("ns", b"last", b"v").unwrap();
    assert_eq!(
        db.undo_history().unwrap().len(),
        2 + 1,
        "trimmed to the depth, then one more"
    );
    db.create_index("ns_docs", "by_x", "$.x", &IndexOptions::default())
        .unwrap();
    assert!(
        db.undo_history().unwrap().is_empty(),
        "a catalog change clears the entries"
    );
    assert!(db.undo().unwrap().is_none());

    let no_history = tempfile::tempdir().unwrap();
    let refused = Database::open(
        no_history.path(),
        Options {
            undo_depth: 1,
            history_retention: Duration::ZERO,
            ..small_options(Durability::Performance, false)
        },
    );
    assert!(matches!(refused, Err(Error::InvalidArgument(_))));
    let off = open(tempfile::tempdir().unwrap().path(), false, 0);
    off.system().kv_put("ns", b"k", b"v").unwrap();
    assert_eq!(off.undo_history().unwrap(), []);
    assert!(off.undo().unwrap().is_none());
}

#[test]
fn concurrent_writers_make_undo_conflict_never_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(open(dir.path(), true, 1_000));
    let writer = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            let s = db.system();
            for i in 0..300u32 {
                s.kv_put("shared", b"k", &i.to_be_bytes()).unwrap();
            }
        })
    };
    while db.undo_history().unwrap().len() < 10 {
        std::thread::yield_now();
    }
    let (mut undone, mut conflicts) = (0, 0);
    loop {
        match db.undo() {
            Ok(Some(_)) => undone += 1,
            Ok(None) => {}
            Err(Error::Conflict { .. }) => conflicts += 1,
            Err(other) => panic!("unexpected error {other:?}"),
        }
        if writer.is_finished() {
            break;
        }
    }
    writer.join().unwrap();
    assert!(undone + conflicts > 0);
    while db.undo().unwrap().is_some() {}
    assert!(
        db.system().kv_get("shared", b"k").unwrap().is_none(),
        "every commit undone"
    );
    let report = db.verify_ledger(LedgerVerifyMode::Strict).unwrap();
    assert!(report.is_valid, "{:?}", report.error);
}
