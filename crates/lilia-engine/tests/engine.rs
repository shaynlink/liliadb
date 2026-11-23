//! Behaviour of the public API on the native file system.

mod common;

use std::ops::Bound;
use std::sync::Arc;

use common::{key, scan, small_options};
use lilia_engine::{Db, Durability, Error, Options, WriteBatch};

#[test]
fn point_writes_reads_versions_and_conditional_batches() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), small_options(Durability::Durable)).unwrap();
    let v1 = db.put("a", "1").unwrap();
    let v2 = db.put("a", "2").unwrap();
    assert!(v2 > v1);
    let got = db.get_versioned(b"a").unwrap().unwrap();
    assert_eq!((got.value.as_ref(), got.version), (&b"2"[..], v2));

    let mut stale = WriteBatch::new();
    stale.expect("a", Some(v1)).put("a", "3").put("b", "x");
    match db.write(stale) {
        Err(Error::Conflict {
            expected, actual, ..
        }) => assert_eq!((expected, actual), (Some(v1), Some(v2))),
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(
        db.get(b"b").unwrap(),
        None,
        "a conflicting batch writes nothing"
    );

    let mut create = WriteBatch::new();
    create.expect("b", None).put("b", "created");
    db.write(create).unwrap();
    let mut again = WriteBatch::new();
    again.expect("b", None).put("b", "twice");
    assert!(matches!(db.write(again), Err(Error::Conflict { .. })));

    db.delete("a").unwrap();
    assert_eq!(db.get(b"a").unwrap(), None);
    let mut recreate = WriteBatch::new();
    recreate.expect("a", None).put("a", "back");
    db.write(recreate).unwrap();

    let next = db.last_sequence() + 1;
    let mut stale_position = WriteBatch::new();
    stale_position.at_sequence(next + 1).put("c", "late");
    match db.write(stale_position) {
        Err(Error::SequenceMoved { expected, actual }) => {
            assert_eq!((expected, actual), (next + 1, next));
        }
        other => panic!("expected a moved sequence, got {other:?}"),
    }
    let mut pinned = WriteBatch::new();
    pinned.at_sequence(next).put("c", "now").put("d", "too");
    assert_eq!(db.write(pinned).unwrap(), next);
    assert_eq!(db.last_sequence(), next + 1);
    assert_eq!(db.get_versioned(b"d").unwrap().unwrap().version, next + 1);
    db.close().unwrap();
}

#[test]
fn data_survives_flushes_compactions_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let n = 6_000u64;
    {
        let db = Db::open(dir.path(), small_options(Durability::Performance)).unwrap();
        for chunk in (0..n).collect::<Vec<_>>().chunks(50) {
            let mut batch = WriteBatch::new();
            for &i in chunk {
                batch.put(key(i), format!("value-{i}-{}", "x".repeat(40)));
            }
            db.write(batch).unwrap();
        }
        for i in (0..n).step_by(3) {
            db.delete(key(i)).unwrap();
        }
        db.flush().unwrap();
        db.wait_for_compactions().unwrap();
        let stats = db.stats();
        assert!(
            stats.levels.iter().skip(1).any(|&(runs, _)| runs > 0),
            "compactions moved data below level 0: {stats:?}"
        );
        db.close().unwrap();
    }
    let db = Db::open(dir.path(), small_options(Durability::Performance)).unwrap();
    for i in 0..n {
        let expected = (i % 3 != 0).then(|| format!("value-{i}-{}", "x".repeat(40)));
        assert_eq!(
            db.get(&key(i))
                .unwrap()
                .map(|v| String::from_utf8(v.to_vec()).unwrap()),
            expected,
            "key {i}"
        );
    }
    let all = scan(&db);
    assert_eq!(all.len() as u64, n - n.div_ceil(3));
    let window: Vec<_> = db
        .range(Bound::Excluded(&key(10)[..]), Bound::Included(&key(20)[..]))
        .unwrap()
        .map(|e| e.unwrap().key.to_vec())
        .collect();
    assert_eq!(window, [11, 13, 14, 16, 17, 19, 20].map(key));
}

#[test]
fn snapshots_see_a_fixed_state_across_flushes_and_compactions() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), small_options(Durability::Balanced)).unwrap();
    for i in 0..500 {
        db.put(key(i), "before").unwrap();
    }
    let snapshot = db.snapshot();
    for i in 0..500 {
        if i % 2 == 0 {
            db.delete(key(i)).unwrap();
        } else {
            db.put(key(i), "after").unwrap();
        }
    }
    for i in 500..3_000 {
        db.put(key(i), "x".repeat(64)).unwrap();
    }
    db.flush().unwrap();
    db.wait_for_compactions().unwrap();
    for i in 0..500 {
        assert_eq!(
            db.get_at(&snapshot, &key(i)).unwrap().unwrap().value,
            "before"
        );
    }
    let old: Vec<_> = db
        .range_at(&snapshot, Bound::Unbounded, Bound::Unbounded)
        .unwrap()
        .map(|e| e.unwrap())
        .collect();
    assert_eq!(old.len(), 500);
    assert!(old.iter().all(|e| e.value == "before"));
    drop(snapshot);
    assert_eq!(db.get(&key(0)).unwrap(), None);
    assert_eq!(db.get(&key(1)).unwrap().unwrap(), "after");
}

/// Writes `rounds` versions of keys 0..100 (the last round deletes the even ones), with enough
/// filler between rounds that each lands in other runs; returns the sequence after each round.
fn versioned_rounds(db: &Db, rounds: u64) -> Vec<u64> {
    let mut after = Vec::new();
    for round in 0..rounds {
        for i in 0..100 {
            if round == rounds - 1 && i % 2 == 0 {
                db.delete(key(i)).unwrap();
            } else {
                db.put(key(i), format!("round {round}")).unwrap();
            }
        }
        after.push(db.last_sequence());
        for i in 0..400 {
            db.put(key(10_000 + round * 400 + i), "x".repeat(64))
                .unwrap();
        }
    }
    db.flush().unwrap();
    db.wait_for_compactions().unwrap();
    after
}

#[test]
fn history_is_kept_until_its_horizon_moves() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(
        dir.path(),
        Options {
            retain_history: true,
            ..small_options(Durability::Balanced)
        },
    )
    .unwrap();
    assert_eq!(db.oldest_sequence(), 0);
    let after = versioned_rounds(&db, 6);
    assert!(
        db.stats().levels.iter().skip(1).any(|&(runs, _)| runs > 0),
        "compactions ran"
    );

    for (round, &sequence) in after.iter().enumerate() {
        let past = db.snapshot_at(sequence).unwrap();
        let value = db.get_at(&past, &key(1)).unwrap().unwrap();
        assert_eq!(value.value, format!("round {round}"));
        let live = db
            .range_at(&past, Bound::Included(&key(0)), Bound::Excluded(&key(100)))
            .unwrap()
            .count();
        assert_eq!(live, if round == 5 { 50 } else { 100 });
    }
    let history = db.history(&key(0)).unwrap();
    assert_eq!(history.len(), 6, "every version, newest first");
    assert_eq!(history[0].value, None, "deleted last");
    assert_eq!(history[5].value.as_deref(), Some(&b"round 0"[..]));
    assert!(history.windows(2).all(|w| w[0].version > w[1].version));
    assert!(matches!(
        db.snapshot_at(db.last_sequence() + 1),
        Err(Error::InvalidArgument(_))
    ));

    // Past the horizon, compactions drop what only older reads would see.
    db.set_history_horizon(after[3]).unwrap();
    db.set_history_horizon(after[1]).unwrap();
    assert_eq!(db.oldest_sequence(), after[3], "a horizon never moves back");
    match db.snapshot_at(after[2]) {
        Err(Error::HistoryUnavailable { sequence, oldest }) => {
            assert_eq!((sequence, oldest), (after[2], after[3]));
        }
        other => panic!("expected unavailable history, got {other:?}"),
    }
    let at_horizon = db.snapshot_at(after[3]).unwrap();
    for i in 0..3_000 {
        db.put(key(20_000 + i), "y".repeat(64)).unwrap();
    }
    db.flush().unwrap();
    db.wait_for_compactions().unwrap();
    assert_eq!(
        db.get_at(&at_horizon, &key(1)).unwrap().unwrap().value,
        "round 3"
    );
    let kept = db.history(&key(1)).unwrap();
    assert_eq!(
        kept.last().unwrap().value.as_deref(),
        Some(&b"round 3"[..]),
        "the version current at the horizon is the oldest"
    );
    assert_eq!(kept.len(), 3);
}

#[test]
fn history_survives_the_replay_of_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let options = Options {
        retain_history: true,
        ..small_options(Durability::Durable)
    };
    let db = Db::open(dir.path(), options.clone()).unwrap();
    let versions: Vec<u64> = (0..5)
        .map(|i| db.put("k", format!("{i}")).unwrap())
        .collect();
    db.close().unwrap();
    let db = Db::open(dir.path(), options).unwrap();
    for (i, &version) in versions.iter().enumerate() {
        let past = db.snapshot_at(version).unwrap();
        assert_eq!(
            db.get_at(&past, b"k").unwrap().unwrap().value,
            format!("{i}")
        );
    }
    assert_eq!(db.history(b"k").unwrap().len(), 5);
}

#[test]
fn without_history_only_snapshots_keep_the_past() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), small_options(Durability::Balanced)).unwrap();
    let first = db.put("k", "1").unwrap();
    let pinned = db.snapshot();
    db.put("k", "2").unwrap();
    assert_eq!(
        db.get_at(&db.snapshot_at(first).unwrap(), b"k")
            .unwrap()
            .unwrap()
            .value,
        "1"
    );
    drop(pinned);
    assert!(matches!(
        db.snapshot_at(first),
        Err(Error::HistoryUnavailable { .. })
    ));
    assert!(matches!(
        db.set_history_horizon(first),
        Err(Error::InvalidArgument(_))
    ));
    assert_eq!(
        db.history(b"k").unwrap().len(),
        1,
        "the current version only"
    );
}

#[test]
fn concurrent_writers_are_all_committed() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open(dir.path(), small_options(Durability::Durable)).unwrap());
    let threads: Vec<_> = (0..8u64)
        .map(|t| {
            let db = Arc::clone(&db);
            std::thread::spawn(move || {
                for i in 0..200 {
                    db.put(key(t * 1_000 + i), format!("{t}:{i}")).unwrap();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    for t in 0..8u64 {
        for i in 0..200 {
            assert_eq!(
                db.get(&key(t * 1_000 + i)).unwrap().unwrap(),
                format!("{t}:{i}")
            );
        }
    }
    let db = Arc::into_inner(db).unwrap();
    db.close().unwrap();
    let db = Db::open(dir.path(), small_options(Durability::Durable)).unwrap();
    assert_eq!(scan(&db).len(), 1_600);
}

#[test]
fn a_directory_is_opened_by_one_handle_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), small_options(Durability::Durable)).unwrap();
    assert!(Db::open(dir.path(), small_options(Durability::Durable)).is_err());
    db.close().unwrap();
    Db::open(dir.path(), small_options(Durability::Durable)).unwrap();
}
