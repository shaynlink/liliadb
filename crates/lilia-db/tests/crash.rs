//! Power-cut simulation over the data models: a writer commits batches that write documents of
//! an indexed space, key-value records, graph nodes and edges, full-text documents, vectors,
//! time-series points, GPS points, CRDTs and a bitemporal entity, into a database that keeps a
//! ledger and undo entries, while an index may be being built; the power is cut at a random
//! moment (or I/O errors are injected) and the database is reopened on what a real drive would
//! keep. With `Durability::Durable`:
//!
//! - every model holds exactly the state after every acknowledged batch, plus possibly the one
//!   in flight;
//! - the ledger holds one commit per applied batch and passes strict verification;
//! - every index answers exactly like a scan, including an index whose build was interrupted;
//! - the bitemporal entity answers as known after each applied batch;
//! - undoing the last batch (when its undo entry was not cleared by an index build) leaves the
//!   state of the batch before, and redoing it the state after.
//!
//! `LILIA_CRASH_ITERATIONS` overrides the number of simulated crashes (default 200).

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::{small_options, Rng};
use lilia_core::{BitemporalInterval, GpsPoint, TimeSeriesPoint};
use lilia_db::{
    Batch, BitemporalWrite, CrdtWrite, Database, FtsWrite, GeoWrite, GraphWrite, IndexOptions, LedgerVerifyMode, Options, Scan,
    TimeSeriesWrite, Vector, VectorWrite, Write,
};
use lilia_engine::Durability;
use lilia_io::{CrashOptions, MemVfs, Vfs};
use serde_json::{json, Map, Value};

#[derive(Debug, Clone, Default, PartialEq)]
struct State {
    docs: BTreeMap<String, Value>,
    kv: BTreeMap<Vec<u8>, Vec<u8>>,
    /// Graph nodes by id: the batch that set them.
    nodes: BTreeMap<String, u64>,
    edges: BTreeSet<(String, String)>,
    /// Full-text documents by id: the number of their distinct word.
    texts: BTreeMap<String, u64>,
    vectors: BTreeMap<String, Vector>,
    /// Series points and GPS points by time: the batch that wrote them.
    points: BTreeMap<i64, u64>,
    track: BTreeMap<i64, u64>,
    counter: i64,
    elements: BTreeSet<String>,
    /// The value of the bitemporal entity, as known now.
    fact: Option<u64>,
}

const VALID: BitemporalInterval = BitemporalInterval { from: 0, to: 100 };

/// The writes of batch `b` to the models other than documents and key-value records.
fn model_writes(rng: &mut Rng, b: u64, batch: &mut Batch, state: &mut State) {
    let node = format!("n{}", rng.below(12));
    batch.push(Write::Graph(GraphWrite::SetNode { graph: "g".into(), id: node.clone(), kind: "k".into(), metadata: json!({"b": b}), document: None }));
    state.nodes.insert(node.clone(), b);
    let target = format!("n{}", rng.below(12));
    batch.push(Write::Graph(GraphWrite::AddEdge { graph: "g".into(), source: node.clone(), target: target.clone(), kind: "link".into(), weight: 1.0 }));
    state.edges.insert((node, target));
    if rng.below(4) == 0 {
        let gone = format!("n{}", rng.below(12));
        batch.push(Write::Graph(GraphWrite::DeleteNode { graph: "g".into(), id: gone.clone() }));
        state.nodes.remove(&gone);
        state.edges.retain(|(s, t)| *s != gone && *t != gone);
    }

    let text = format!("t{}", rng.below(8));
    let word = rng.below(5);
    batch.push(Write::Fts(FtsWrite::Index { collection: "f".into(), doc_id: text.clone(), content: format!("w{word} shared") }));
    state.texts.insert(text, word);
    if rng.below(4) == 0 {
        let gone = format!("t{}", rng.below(8));
        batch.push(Write::Fts(FtsWrite::Delete { collection: "f".into(), doc_id: gone.clone() }));
        state.texts.remove(&gone);
    }

    let id = format!("v{}", rng.below(8));
    #[allow(clippy::cast_precision_loss)] // reason: batch numbers stay far below 2^24
    let vector = Vector::from_f32(&[b as f32, 1.0]).unwrap();
    batch.push(Write::Vector(VectorWrite::Set { collection: "vec".into(), id: id.clone(), vector: vector.clone() }));
    state.vectors.insert(id, vector);

    let time = i64::try_from(rng.below(500)).unwrap();
    #[allow(clippy::cast_precision_loss)] // reason: batch numbers stay far below 2^53
    let point = TimeSeriesPoint { timestamp: time, value: b as f64 };
    batch.push(Write::TimeSeries(TimeSeriesWrite::Append { series: "s".into(), resolution_ms: 0, points: vec![point] }));
    state.points.insert(time, b);

    let at = i64::try_from(rng.below(200)).unwrap();
    #[allow(clippy::cast_precision_loss)] // reason: below 80
    let latitude = (b % 80) as f64;
    let gps = GpsPoint { timestamp: at, latitude, longitude: 0.0, altitude: None, speed: None, accuracy: None, h3_index: None };
    batch.push(Write::Geo(GeoWrite::Record { collection: "geo".into(), track: "trk".into(), points: vec![gps] }));
    state.track.insert(at, b % 80);

    batch.push(Write::Crdt(CrdtWrite::Count { space: "c".into(), id: "hits".into(), actor: format!("a{}", b % 3), increment: 1, decrement: 0 }));
    state.counter += 1;
    let element = format!("e{}", rng.below(6));
    if rng.below(3) == 0 {
        batch.push(Write::Crdt(CrdtWrite::SetRemove { space: "c".into(), id: "set".into(), element: element.clone(), timestamp_ms: i64::try_from(b).unwrap() }));
        state.elements.remove(&element);
    } else {
        batch.push(Write::Crdt(CrdtWrite::set_add("c", "set", "w", &element)));
        state.elements.insert(element);
    }

    batch.push(Write::Bitemporal(BitemporalWrite::Put { space: "bt".into(), id: "x".into(), value: json!(b), valid: VALID, recorded_at: Some(recorded(b)) }));
    state.fact = Some(b);
}

/// The transaction time of the bitemporal write of batch `b`.
fn recorded(b: u64) -> i64 {
    i64::try_from(b).unwrap() + 1
}

const IDS: u64 = 40;
const COLORS: [&str; 4] = ["red", "green", "blue", "gray"];

fn plan(rng: &mut Rng, count: usize) -> (Vec<Batch>, Vec<State>) {
    let mut states = vec![State::default()];
    let mut batches = Vec::with_capacity(count);
    for b in 0..count {
        let mut batch = Batch::new();
        let mut state = states[b].clone();
        let id = format!("d{:02}", rng.below(IDS));
        let doc = json!({"color": COLORS[usize::try_from(rng.below(4)).unwrap()], "size": rng.below(3), "batch": b});
        batch.json_put("items", &id, &doc).unwrap();
        state.docs.insert(id, doc);
        for _ in 0..rng.below(3) {
            let id = format!("d{:02}", rng.below(IDS));
            batch.json_delete("items", &id);
            state.docs.remove(&id);
        }
        let key = format!("k{}", rng.below(16)).into_bytes();
        let value = format!("b{b}").into_bytes();
        batch.kv_put("meta", key.clone(), value.clone());
        state.kv.insert(key, value);
        model_writes(rng, b as u64, &mut batch, &mut state);
        batches.push(batch);
        states.push(state);
    }
    (batches, states)
}

fn read_state(db: &Database) -> State {
    let s = db.system();
    let mut texts = BTreeMap::new();
    for word in 0..5 {
        for hit in s.fts("f").search(&format!("w{word}"), 100).unwrap() {
            texts.insert(hit.doc_id, word);
        }
    }
    assert_eq!(s.fts("f").search("shared", 100).unwrap().len(), texts.len(), "every document holds the shared word");
    let as_batch = |value: f64| {
        assert!(value.fract() == 0.0 && value >= 0.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // reason: whole and non-negative
        let batch = value as u64;
        batch
    };
    State {
        docs: s.json_scan("items", &Scan::all()).unwrap().into_iter().map(|e| (e.id.clone(), e.to_json_value())).collect(),
        kv: s.kv_scan("meta", &Scan::all()).unwrap().into_iter().map(|e| (e.key, e.value)).collect(),
        nodes: s.graph("g").nodes(None, &Scan::all()).unwrap().into_iter().map(|n| (n.id, n.metadata["b"].as_u64().unwrap())).collect(),
        edges: s.graph("g").edges(None, None, Some("link"), None).unwrap().into_iter().map(|e| (e.source, e.target)).collect(),
        texts,
        vectors: (0..8).filter_map(|i| s.vectors("vec").get(&format!("v{i}")).unwrap().map(|v| (format!("v{i}"), v))).collect(),
        points: s.series("s").range(i64::MIN, i64::MAX).unwrap().into_iter().map(|p| (p.timestamp, as_batch(p.value))).collect(),
        track: s.geo("geo").track("trk", i64::MIN, i64::MAX).unwrap().into_iter().map(|p| (p.timestamp, as_batch(p.latitude))).collect(),
        counter: s.crdt("c").counter("hits").value().unwrap(),
        elements: s.crdt("c").set("set").elements().unwrap(),
        fact: s.bitemporal("bt").get("x", 50).unwrap().map(|e| e.value.as_u64().unwrap()),
    }
}

/// The bitemporal entity answers, as known after each of the `applied` batches, what that batch
/// recorded.
fn check_audit(db: &Database, applied: usize, seed: u64) {
    let facts = db.system();
    let facts = facts.bitemporal("bt");
    for b in 0..applied as u64 {
        let known = facts.get_as_of("x", recorded(b), 50).unwrap().map(|e| e.value.as_u64().unwrap());
        assert_eq!(known, Some(b), "seed {seed}: the fact as known after batch {b}");
    }
    assert_eq!(facts.history("x").unwrap().len(), applied, "seed {seed}: one slice per applied batch");
}

/// Every index of `items` answers like a filtered scan.
fn check_indexes(db: &Database, state: &State, seed: u64) {
    let s = db.system();
    for color in COLORS {
        let mut query = Map::new();
        query.insert("color".into(), json!(color));
        let found: Vec<String> = s.json_find("items", &query, &Scan::all()).unwrap().into_iter().map(|e| e.id).collect();
        let expected: Vec<String> = state.docs.iter().filter(|(_, d)| d["color"] == color).map(|(id, _)| id.clone()).collect();
        assert_eq!(found, expected, "seed {seed}: color {color}");
        for size in 0..3 {
            let mut alone = Map::new();
            alone.insert("size".into(), json!(size));
            let found = s.json_find("items", &alone, &Scan::all()).unwrap().len();
            assert_eq!(found, state.docs.values().filter(|d| d["size"] == size).count(), "seed {seed}: size {size}");
            query.insert("size".into(), json!(size));
            let found = s.json_find("items", &query, &Scan::all()).unwrap().len();
            let expected = state.docs.values().filter(|d| d["color"] == color && d["size"] == size).count();
            assert_eq!(found, expected, "seed {seed}: color {color}, size {size}");
        }
    }
}

fn iteration(seed: u64) {
    let mut rng = Rng(seed);
    let options = Options { undo_depth: 4, ..small_options(Durability::Durable, true) };
    let memvfs = MemVfs::new(seed);
    let vfs: Arc<dyn Vfs> = Arc::new(memvfs.clone());
    let count = 30 + usize::try_from(rng.below(150)).unwrap();
    let (batches, states) = plan(&mut rng, count);

    let preload = usize::try_from(rng.below(30)).unwrap();
    let db = Database::open_with(Arc::clone(&vfs), "/db", options.clone()).unwrap();
    db.create_index("items", "by_color", "$.color", &IndexOptions::default()).unwrap();
    for batch in &batches[..preload] {
        db.system().commit(batch).unwrap();
    }
    db.close().unwrap();

    let db = Arc::new(Database::open_with(Arc::clone(&vfs), "/db", options.clone()).unwrap());
    if rng.below(3) == 0 {
        memvfs.set_fault_ppm(u32::try_from(rng.below(20_000)).unwrap());
    }
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (db, stop) = (Arc::clone(&db), Arc::clone(&stop));
        let batches = batches[preload..].to_vec();
        std::thread::spawn(move || {
            let mut acknowledged = 0;
            for batch in batches {
                if stop.load(Ordering::Relaxed) || db.system().commit(&batch).is_err() {
                    break;
                }
                acknowledged += 1;
            }
            acknowledged
        })
    };
    // A second index, built while the writer runs; the crash may interrupt the build.
    let builder = (rng.below(2) == 0).then(|| {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            drop(db.create_index("items", "by_size", "$.size", &IndexOptions::default()));
        })
    });
    std::thread::sleep(Duration::from_micros(rng.below(4_000)));
    memvfs.crash();
    stop.store(true, Ordering::Relaxed);
    let acknowledged = preload + writer.join().unwrap();
    if let Some(builder) = builder {
        builder.join().unwrap();
    }
    drop(db);

    memvfs.set_fault_ppm(0);
    memvfs.restart(if rng.below(2) == 0 { CrashOptions::TORN } else { CrashOptions::CLEAN });
    let db = Database::open_with(Arc::clone(&vfs), "/db", options.clone())
        .unwrap_or_else(|e| panic!("seed {seed}: reopening after the crash failed: {e}"));
    let recovered = read_state(&db);
    let applied = [acknowledged, acknowledged + 1]
        .into_iter()
        .find(|&n| states.get(n) == Some(&recovered))
        .unwrap_or_else(|| panic!("seed {seed}: recovered state matches neither {acknowledged} nor {} batches", acknowledged + 1));
    assert_eq!(db.ledger_len().unwrap(), applied as u64, "seed {seed}: one ledger commit per applied batch");
    let report = db.verify_ledger(LedgerVerifyMode::Strict).unwrap();
    assert!(report.is_valid, "seed {seed}: {report:?}");
    check_indexes(&db, &recovered, seed);
    check_audit(&db, applied, seed);

    // Undo the last batch, unless an index build cleared its entry, then redo it.
    if db.undo().unwrap().is_some() {
        assert_eq!(read_state(&db), states[applied - 1], "seed {seed}: undoing batch {applied}");
        check_indexes(&db, &states[applied - 1], seed);
        assert!(db.redo().unwrap().is_some());
        assert_eq!(read_state(&db), states[applied], "seed {seed}: redoing batch {applied}");
        let report = db.verify_ledger(LedgerVerifyMode::Strict).unwrap();
        assert!(report.is_valid, "seed {seed}: after undo and redo: {report:?}");
    }

    // The recovered database keeps working.
    db.system().json_put("items", "after", &json!({"color": "red", "size": 0})).unwrap();
    assert!(db.prove_doc("items", "after").unwrap().verify(&db.ledger_root().unwrap()));
    db.close().unwrap();
}

#[test]
fn recovered_records_ledger_and_indexes_agree_after_a_power_cut() {
    let iterations: u64 = std::env::var("LILIA_CRASH_ITERATIONS").ok().and_then(|v| v.parse().ok()).unwrap_or(200);
    let base: u64 = std::env::var("LILIA_CRASH_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    for seed in base..base + iterations {
        iteration(seed);
    }
}
