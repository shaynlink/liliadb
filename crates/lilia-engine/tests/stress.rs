//! Concurrent writes, flushes and compactions without crashes: the state must equal the model
//! after the writes, and stay equal across clean reopenings.
//!
//! `LILIA_STRESS_ITERATIONS` sets the number of seeds (default 200); `LILIA_STRESS_THREADS`
//! overrides the compaction thread count (concurrent compactions are where races hide).

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use common::{key, small_options, Rng};
use lilia_engine::{Db, Durability, WriteBatch};
use lilia_io::{MemVfs, Vfs};

fn read_all(db: &Db) -> BTreeMap<Vec<u8>, Vec<u8>> {
    (0..64)
        .filter_map(|i| db.get(&key(i)).unwrap().map(|v| (key(i), v.to_vec())))
        .collect()
}

fn run(seed: u64) {
    let vfs: Arc<dyn Vfs> = Arc::new(MemVfs::new(seed));
    let mut options = small_options(Durability::Performance);
    if let Some(threads) = std::env::var("LILIA_STRESS_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        options.compaction_threads = threads;
    }
    let mut rng = Rng(seed);
    let mut model = BTreeMap::new();
    for round in 0..3 {
        let db = Db::open_with(Arc::clone(&vfs), "/s", options.clone()).unwrap();
        assert_eq!(
            read_all(&db),
            model,
            "seed {seed} round {round}: state after reopen"
        );
        for b in 0..300 {
            let mut batch = WriteBatch::new();
            for _ in 0..=rng.below(5) {
                let k = key(rng.below(64));
                if rng.below(6) == 0 {
                    batch.delete(k.clone());
                    model.remove(&k);
                } else {
                    let v = format!(
                        "r{round}b{b}-{}",
                        "x".repeat(usize::try_from(rng.below(200)).unwrap())
                    )
                    .into_bytes();
                    batch.put(k.clone(), v.clone());
                    model.insert(k, v);
                }
            }
            db.write(batch).unwrap();
        }
        assert_eq!(
            read_all(&db),
            model,
            "seed {seed} round {round}: state after writes"
        );
        db.flush().unwrap();
        db.wait_for_compactions().unwrap();
        assert_eq!(
            read_all(&db),
            model,
            "seed {seed} round {round}: state after compactions"
        );
        db.close().unwrap();
    }
}

#[test]
fn concurrent_maintenance_never_changes_the_visible_state() {
    let iterations: u64 = std::env::var("LILIA_STRESS_ITERATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    for seed in 0..iterations {
        run(seed);
    }
}
