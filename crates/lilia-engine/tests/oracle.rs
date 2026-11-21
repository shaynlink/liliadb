//! Random operations checked against an in-memory model, across flushes, compactions, snapshots
//! and reopenings.

mod common;

use std::collections::BTreeMap;
use std::ops::Bound;
use std::sync::Arc;

use common::{key, scan, small_options, Rng};
use lilia_engine::{Db, Durability, Error, Options, WriteBatch};
use lilia_io::{MemVfs, Vfs};

type Model = BTreeMap<Vec<u8>, (Vec<u8>, u64)>;

fn check_range(db: &Db, model: &Model, rng: &mut Rng) {
    let a = key(rng.below(400));
    let b = key(rng.below(400));
    let (low, high) = if a <= b { (a, b) } else { (b, a) };
    let start = if rng.below(2) == 0 { Bound::Included(&low[..]) } else { Bound::Excluded(&low[..]) };
    let end = if rng.below(2) == 0 { Bound::Included(&high[..]) } else { Bound::Excluded(&high[..]) };
    let got: Vec<(Vec<u8>, Vec<u8>, u64)> =
        db.range(start, end).unwrap().map(|e| e.unwrap()).map(|e| (e.key.to_vec(), e.value.to_vec(), e.version)).collect();
    let expected: Vec<(Vec<u8>, Vec<u8>, u64)> =
        model.range::<[u8], _>((start, end)).map(|(k, (v, version))| (k.clone(), v.clone(), *version)).collect();
    assert_eq!(got, expected, "range {start:?}..{end:?}");
}

fn run(seed: u64, operations: u64, background_threads: bool) {
    let vfs = MemVfs::new(seed);
    let vfs: Arc<dyn Vfs> = Arc::new(vfs);
    let options = Options { background_threads, ..small_options(Durability::Durable) };
    let mut db = Db::open_with(Arc::clone(&vfs), "/oracle", options.clone()).unwrap();
    let mut rng = Rng(seed);
    let mut model = Model::new();
    let mut snapshots = Vec::new();

    for op in 0..operations {
        match rng.below(100) {
            0..=54 => {
                let mut batch = WriteBatch::new();
                let mut staged = model.clone();
                for _ in 0..=rng.below(4) {
                    let k = key(rng.below(400));
                    if rng.below(5) == 0 {
                        batch.delete(k.clone());
                        staged.remove(&k);
                    } else {
                        let value = format!("{op}-{}", "v".repeat(usize::try_from(rng.below(120)).unwrap())).into_bytes();
                        batch.put(k.clone(), value.clone());
                        staged.insert(k, (value, 0));
                    }
                }
                let first = db.write(batch.clone()).unwrap();
                // Versions are first + index; later writes to the same key in a batch win.
                for (i, write) in batch.ops().iter().enumerate() {
                    if let Some(entry) = staged.get_mut(write.key().as_ref()) {
                        entry.1 = first + i as u64;
                    }
                }
                model = staged;
            }
            55..=64 => {
                let k = key(rng.below(400));
                let current = model.get(&k).map(|(_, v)| *v);
                let expected = if rng.below(3) == 0 { current.map(|v| v + 1) } else { current };
                let mut batch = WriteBatch::new();
                batch.expect(k.clone(), expected).put(k.clone(), format!("cas{op}"));
                match db.write(batch) {
                    Ok(version) => {
                        assert_eq!(expected, current, "a stale expectation was accepted");
                        model.insert(k, (format!("cas{op}").into_bytes(), version));
                    }
                    Err(Error::Conflict { actual, .. }) => {
                        assert_ne!(expected, current, "a valid expectation was refused");
                        assert_eq!(actual, current);
                    }
                    Err(other) => panic!("unexpected error {other}"),
                }
            }
            65..=84 => {
                let k = key(rng.below(400));
                let got = db.get_versioned(&k).unwrap().map(|v| (v.value.to_vec(), v.version));
                assert_eq!(got.as_ref(), model.get(&k), "get {}", String::from_utf8_lossy(&k));
            }
            85..=91 => check_range(&db, &model, &mut rng),
            92..=94 => {
                if snapshots.len() < 4 {
                    snapshots.push((db.snapshot(), model.clone()));
                } else {
                    let (snapshot, frozen) = snapshots.remove(usize::try_from(rng.below(4)).unwrap());
                    let got: Model = db
                        .range_at(&snapshot, Bound::Unbounded, Bound::Unbounded)
                        .unwrap()
                        .map(|e| e.unwrap())
                        .map(|e| (e.key.to_vec(), (e.value.to_vec(), e.version)))
                        .collect();
                    assert_eq!(got, frozen, "snapshot view changed");
                }
            }
            95..=97 => db.flush().unwrap(),
            _ => {
                snapshots.clear();
                db.close().unwrap();
                db = Db::open_with(Arc::clone(&vfs), "/oracle", options.clone()).unwrap();
            }
        }
    }
    db.wait_for_compactions().unwrap();
    let all = scan(&db);
    assert_eq!(all, model.iter().map(|(k, (v, _))| (k.clone(), v.clone())).collect::<BTreeMap<_, _>>());
}

#[test]
fn random_operations_match_the_model() {
    for seed in 1..=6 {
        run(seed, 4_000, true);
    }
}

#[test]
fn without_background_threads_writers_flush_and_compact_themselves() {
    for seed in 1..=6 {
        run(seed, 4_000, false);
    }
}
