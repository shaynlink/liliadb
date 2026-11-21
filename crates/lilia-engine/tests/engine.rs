//! Behaviour of the public API on the native file system.

mod common;

use std::ops::Bound;
use std::sync::Arc;

use common::{key, scan, small_options};
use lilia_engine::{Db, Durability, Error, WriteBatch};

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
        Err(Error::Conflict { expected, actual, .. }) => assert_eq!((expected, actual), (Some(v1), Some(v2))),
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(db.get(b"b").unwrap(), None, "a conflicting batch writes nothing");

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
        Err(Error::SequenceMoved { expected, actual }) => assert_eq!((expected, actual), (next + 1, next)),
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
        assert!(stats.levels.iter().skip(1).any(|&(runs, _)| runs > 0), "compactions moved data below level 0: {stats:?}");
        db.close().unwrap();
    }
    let db = Db::open(dir.path(), small_options(Durability::Performance)).unwrap();
    for i in 0..n {
        let expected = (i % 3 != 0).then(|| format!("value-{i}-{}", "x".repeat(40)));
        assert_eq!(db.get(&key(i)).unwrap().map(|v| String::from_utf8(v.to_vec()).unwrap()), expected, "key {i}");
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
        assert_eq!(db.get_at(&snapshot, &key(i)).unwrap().unwrap().value, "before");
    }
    let old: Vec<_> = db.range_at(&snapshot, Bound::Unbounded, Bound::Unbounded).unwrap().map(|e| e.unwrap()).collect();
    assert_eq!(old.len(), 500);
    assert!(old.iter().all(|e| e.value == "before"));
    drop(snapshot);
    assert_eq!(db.get(&key(0)).unwrap(), None);
    assert_eq!(db.get(&key(1)).unwrap().unwrap(), "after");
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
            assert_eq!(db.get(&key(t * 1_000 + i)).unwrap().unwrap(), format!("{t}:{i}"));
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
