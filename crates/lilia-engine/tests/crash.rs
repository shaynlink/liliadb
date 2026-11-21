//! Power-cut simulation: a writer commits batches while flushes and compactions run; the power
//! is cut at a random moment (or I/O errors are injected); the database is reopened on what a
//! real drive would keep. The recovered state must be exactly the state after some prefix of the
//! submitted batches: with `Durability::Durable`, every acknowledged batch, plus possibly the one
//! in flight; with weaker modes, any prefix (never a partial batch, never a gap).
//!
//! `LILIA_CRASH_ITERATIONS` overrides the number of simulated crashes (default 1,000).

mod common;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::{key, small_options, Rng};
use lilia_engine::{Db, Durability, Options, WriteBatch};
use lilia_io::{CrashOptions, MemVfs, Vfs};

type State = BTreeMap<Vec<u8>, Vec<u8>>;

const KEYS: u64 = 64;

/// The batches a writer will submit, and the state after each prefix of them.
fn plan(rng: &mut Rng, count: usize) -> (Vec<WriteBatch>, Vec<State>) {
    let mut states = vec![State::new()];
    let mut batches = Vec::with_capacity(count);
    for b in 0..count {
        let mut batch = WriteBatch::new();
        let mut state = states[b].clone();
        for _ in 0..=rng.below(5) {
            let k = key(rng.below(KEYS));
            if rng.below(6) == 0 {
                batch.delete(k.clone());
                state.remove(&k);
            } else {
                let value = format!("b{b}-{}", "x".repeat(usize::try_from(rng.below(200)).unwrap())).into_bytes();
                batch.put(k.clone(), value.clone());
                state.insert(k, value);
            }
        }
        batches.push(batch);
        states.push(state);
    }
    (batches, states)
}

fn read_state(db: &Db) -> State {
    (0..KEYS).filter_map(|i| db.get(&key(i)).unwrap().map(|v| (key(i), v.to_vec()))).collect()
}

fn iteration(seed: u64, background_threads: bool) {
    let mut rng = Rng(seed);
    let durability = match rng.below(4) {
        0 => Durability::Balanced,
        1 => Durability::Performance,
        _ => Durability::Durable,
    };
    let options = Options { background_threads, ..small_options(durability) };
    let memvfs = MemVfs::new(seed);
    let vfs: Arc<dyn Vfs> = Arc::new(memvfs.clone());
    let count = 40 + usize::try_from(rng.below(400)).unwrap();
    let (batches, states) = plan(&mut rng, count);

    // Some history first, flushed and compacted, so the crash also hits a non-empty database.
    let preload = usize::try_from(rng.below(40)).unwrap();
    let db = Db::open_with(Arc::clone(&vfs), "/db", options.clone()).unwrap();
    for batch in &batches[..preload] {
        db.write(batch.clone()).unwrap();
    }
    if rng.below(2) == 0 {
        db.flush().unwrap();
    }
    db.close().unwrap();

    let db = Arc::new(Db::open_with(Arc::clone(&vfs), "/db", options.clone()).unwrap());
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
                if stop.load(Ordering::Relaxed) || db.write(batch).is_err() {
                    break;
                }
                acknowledged += 1;
            }
            acknowledged
        })
    };
    std::thread::sleep(Duration::from_micros(rng.below(4_000)));
    memvfs.crash();
    stop.store(true, Ordering::Relaxed);
    let acknowledged = preload + writer.join().unwrap();
    drop(db);

    memvfs.set_fault_ppm(0);
    memvfs.restart(if rng.below(2) == 0 { CrashOptions::TORN } else { CrashOptions::CLEAN });
    let mut files_after_crash = vfs.list_dir(std::path::Path::new("/db")).unwrap();
    files_after_crash.sort();
    let db = Db::open_with(Arc::clone(&vfs), "/db", options.clone())
        .unwrap_or_else(|e| panic!("seed {seed}: reopening after the crash failed: {e}"));
    let recovered = read_state(&db);
    let candidates: Vec<usize> = match durability {
        Durability::Durable => vec![acknowledged, acknowledged + 1],
        Durability::Balanced | Durability::Performance => (preload..=acknowledged + 1).collect(),
    };
    let matched = candidates.iter().any(|&n| states.get(n) == Some(&recovered));
    if !matched {
        let prefix = states.iter().position(|s| s == &recovered);
        let short = |v: Option<&Vec<u8>>| v.map(|v| String::from_utf8_lossy(&v[..v.len().min(12)]).into_owned());
        let acknowledged_state = &states[acknowledged.min(states.len() - 1)];
        let differing: Vec<String> = (0..KEYS)
            .map(key)
            .filter(|k| acknowledged_state.get(k) != recovered.get(k))
            .map(|k| format!("{}: want {:?} got {:?}", String::from_utf8_lossy(&k), short(acknowledged_state.get(&k)), short(recovered.get(&k))))
            .collect();
        panic!(
            "seed {seed} ({durability:?}): recovered state matches no allowed prefix (preload {preload}, acknowledged {acknowledged}, matches prefix {prefix:?}); files after the crash {files_after_crash:?}; stats after reopen {:?}; differences from the acknowledged state: {differing:?}",
            db.stats()
        );
    }

    // The recovered database keeps working.
    let mut batch = WriteBatch::new();
    batch.put("after-crash", "ok");
    db.write(batch).unwrap();
    db.close().unwrap();
    let db = Db::open_with(vfs, "/db", options).unwrap();
    assert_eq!(db.get(b"after-crash").unwrap().unwrap(), "ok");
    assert_eq!(read_state(&db), recovered, "seed {seed}: state changed across a clean reopen");
}

#[test]
fn recovered_state_is_always_a_prefix_of_the_submitted_batches() {
    let iterations: u64 = std::env::var("LILIA_CRASH_ITERATIONS").ok().and_then(|v| v.parse().ok()).unwrap_or(1_000);
    let base: u64 = std::env::var("LILIA_CRASH_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    for seed in base..base + iterations {
        iteration(seed, true);
    }
}

#[test]
fn without_background_threads_too() {
    let iterations: u64 = std::env::var("LILIA_CRASH_ITERATIONS").ok().and_then(|v| v.parse().ok()).unwrap_or(1_000);
    let base: u64 = std::env::var("LILIA_CRASH_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    for seed in base..base + iterations {
        iteration(seed, false);
    }
}
