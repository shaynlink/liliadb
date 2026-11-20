//! Power-cut simulation over the data models: a writer commits batches of documents and
//! key-value records into an indexed space of a database that keeps a ledger, while an index may
//! be being built; the power is cut at a random moment (or I/O errors are injected) and the
//! database is reopened on what a real drive would keep. With `Durability::Durable`:
//!
//! - the records are exactly the state after every acknowledged batch, plus possibly the one in
//!   flight;
//! - the ledger holds one commit per applied batch and passes strict verification;
//! - every index answers exactly like a scan, including an index whose build was interrupted.
//!
//! `LILIA_CRASH_ITERATIONS` overrides the number of simulated crashes (default 200).

mod common;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::{small_options, Rng};
use lilia_db::{Batch, Database, IndexOptions, LedgerVerifyMode, Scan};
use lilia_engine::Durability;
use lilia_io::{CrashOptions, MemVfs, Vfs};
use serde_json::{json, Map, Value};

#[derive(Debug, Clone, Default, PartialEq)]
struct State {
    docs: BTreeMap<String, Value>,
    kv: BTreeMap<Vec<u8>, Vec<u8>>,
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
        batches.push(batch);
        states.push(state);
    }
    (batches, states)
}

fn read_state(db: &Database) -> State {
    let s = db.system();
    State {
        docs: s.json_scan("items", &Scan::all()).unwrap().into_iter().map(|e| (e.id.clone(), e.to_json_value())).collect(),
        kv: s.kv_scan("meta", &Scan::all()).unwrap().into_iter().map(|e| (e.key, e.value)).collect(),
    }
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
    let options = small_options(Durability::Durable, true);
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
