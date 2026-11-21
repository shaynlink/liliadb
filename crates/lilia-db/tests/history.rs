//! History: reads in the past by version and by time, revisions of records, the window.

mod common;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::small_options;
use lilia_db::{Database, Error, Options, Policy, SecurityContext};
use lilia_engine::Durability;
use serde_json::json;

fn open(dir: &std::path::Path, retention: Duration) -> Database {
    let options = Options {
        history_retention: retention,
        ..small_options(Durability::Performance, false)
    };
    Database::open(dir, options).unwrap()
}

fn now_ms() -> i64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    i64::try_from(since.as_millis()).unwrap()
}

/// Waits until the clock has moved past the current millisecond.
fn next_millisecond() -> i64 {
    let start = now_ms();
    loop {
        std::thread::sleep(Duration::from_millis(1));
        let now = now_ms();
        if now > start {
            return now;
        }
    }
}

#[test]
fn documents_keep_their_revisions_and_the_past_reads_like_the_present() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), Duration::from_secs(3600));
    let s = db.system();
    let v1 = s
        .json_put(
            "docs",
            "contract",
            &json!({"title": "Agreement", "amount": 1000, "status": "draft"}),
        )
        .unwrap();
    s.kv_put("settings", b"theme", b"dark").unwrap();
    s.graph("g").set_node("a", "person", &json!({})).unwrap();
    let v2 = s
        .json_put(
            "docs",
            "contract",
            &json!({"title": "Agreement", "amount": 1250, "status": "in_review"}),
        )
        .unwrap();
    s.kv_put("settings", b"theme", b"light").unwrap();
    s.graph("g").set_node("b", "person", &json!({})).unwrap();
    let v3 = s
        .json_put(
            "docs",
            "contract",
            &json!({"title": "Agreement", "amount": 1250, "status": "approved", "by": "Bob"}),
        )
        .unwrap();
    assert!(s.json_delete("docs", "contract").unwrap());

    let past = s.at_version(v1).unwrap();
    assert_eq!(past.version(), Some(v1));
    let doc = past.json_get("docs", "contract").unwrap().unwrap();
    assert_eq!(
        (doc.value()["status"].clone(), doc.version),
        (json!("draft"), v1)
    );
    assert!(
        past.kv_get("settings", b"theme").unwrap().is_none(),
        "not written yet"
    );
    let at_v2 = s.at_version(v2).unwrap();
    assert_eq!(
        at_v2.kv_get("settings", b"theme").unwrap().unwrap().value,
        b"dark"
    );
    assert!(at_v2.graph("g").node("b").unwrap().is_none());
    assert!(at_v2.graph("g").node("a").unwrap().is_some());
    assert_eq!(
        s.at_version(v3)
            .unwrap()
            .json_scan("docs", &lilia_db::Scan::all())
            .unwrap()
            .len(),
        1
    );
    assert!(s.json_get("docs", "contract").unwrap().is_none());

    let history = s.json_history("docs", "contract").unwrap();
    let versions: Vec<u64> = history.iter().map(|r| r.version).collect();
    assert_eq!(versions.len(), 4);
    assert_eq!(&versions[..3], [v1, v2, v3]);
    assert!(history[3].doc.is_none(), "deleted last");
    assert_eq!(history[1].doc.as_ref().unwrap().value()["amount"], 1250);
    assert_eq!(
        past.json_history("docs", "contract").unwrap().len(),
        1,
        "up to its version"
    );
    let themes: Vec<Vec<u8>> = s
        .kv_history("settings", b"theme")
        .unwrap()
        .into_iter()
        .map(|r| r.entry.unwrap().value)
        .collect();
    assert_eq!(themes, [b"dark".to_vec(), b"light".to_vec()]);

    let patch = s.json_diff("docs", "contract", v1, v3).unwrap();
    let rebuilt = lilia_core::json_patch::apply(
        past.json_get("docs", "contract").unwrap().unwrap().value(),
        &patch,
    )
    .unwrap();
    assert_eq!(rebuilt["status"], "approved");
    assert_eq!(rebuilt["by"], "Bob");

    // A session in the past refuses writes; a transaction from it conflicts with what changed.
    assert!(matches!(
        past.kv_put("settings", b"x", b"1"),
        Err(Error::ReadOnly)
    ));
    assert!(matches!(
        past.graph("g").set_node("c", "person", &json!({})),
        Err(Error::ReadOnly)
    ));
    let tx = past.transaction();
    assert!(tx.json_get("docs", "contract").unwrap().is_some());
    tx.kv_put("settings", b"x", b"1").unwrap();
    assert!(matches!(tx.commit(), Err(Error::Conflict { .. })));
    assert!(matches!(
        s.at_version(db.oldest_version().max(1) + 1_000_000),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn reads_at_a_time_see_the_commits_before_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), Duration::from_secs(3600));
    let s = db.system();
    let before = now_ms();
    next_millisecond();
    s.kv_put("ns", b"k", b"1").unwrap();
    let between = next_millisecond();
    next_millisecond();
    s.kv_put("ns", b"k", b"2").unwrap();
    s.kv_put("ns", b"other", b"x").unwrap();
    let after = next_millisecond();

    assert!(s
        .at_time(before)
        .unwrap()
        .kv_get("ns", b"k")
        .unwrap()
        .is_none());
    assert_eq!(
        s.at_time(between)
            .unwrap()
            .kv_get("ns", b"k")
            .unwrap()
            .unwrap()
            .value,
        b"1"
    );
    let latest = s.at_time(after).unwrap();
    assert_eq!(latest.kv_get("ns", b"k").unwrap().unwrap().value, b"2");
    assert!(latest.kv_get("ns", b"other").unwrap().is_some());
    assert_eq!(
        db.version_at(after + 60_000).unwrap(),
        latest.version().unwrap()
    );

    // Expiry is judged at the time read.
    let expires = after + 50;
    s.write(&lilia_db::Write::KvPut {
        namespace: "ns".into(),
        key: b"short".to_vec(),
        value: b"v".to_vec(),
        expires_at_ms: Some(expires),
        if_version: None,
    })
    .unwrap();
    while now_ms() <= expires {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(s.kv_get("ns", b"short").unwrap().is_none());
    let then = s.at_time(expires - 1).unwrap();
    assert!(then.kv_get("ns", b"short").unwrap().is_some());
}

#[test]
fn the_window_moves_and_drops_older_history() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), Duration::from_millis(200));
    let s = db.system();
    let v1 = s.kv_put("ns", b"k", b"1").unwrap();
    let v2 = s.kv_put("ns", b"k", b"2").unwrap();
    assert_eq!(s.kv_history("ns", b"k").unwrap().len(), 2);
    std::thread::sleep(Duration::from_millis(300));
    let v3 = s.kv_put("ns", b"k", b"3").unwrap();
    db.trim_history().unwrap();
    assert!(db.oldest_version() >= v2 && db.oldest_version() < v3);
    assert!(matches!(
        s.at_version(v1),
        Err(Error::HistoryUnavailable(_))
    ));
    assert_eq!(
        s.at_version(v2)
            .unwrap()
            .kv_get("ns", b"k")
            .unwrap()
            .unwrap()
            .value,
        b"2"
    );
    let kept: Vec<u64> = s
        .kv_history("ns", b"k")
        .unwrap()
        .iter()
        .map(|r| r.version)
        .collect();
    assert_eq!(
        kept,
        [v2, v3],
        "the version current at the start of the window, and later ones"
    );
    assert!(matches!(
        s.at_time(now_ms() - 10_000),
        Err(Error::HistoryUnavailable(_))
    ));

    // Reopening keeps the window: its start is set again before any read.
    drop(s);
    db.close().unwrap();
    let db = open(dir.path(), Duration::from_millis(200));
    assert!(matches!(
        db.system().at_version(v1),
        Err(Error::HistoryUnavailable(_))
    ));
    assert_eq!(db.system().kv_get("ns", b"k").unwrap().unwrap().value, b"3");
}

#[test]
fn without_history_only_the_present_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), Duration::ZERO);
    let s = db.system();
    let v1 = s.kv_put("ns", b"k", b"1").unwrap();
    s.kv_put("ns", b"k", b"2").unwrap();
    assert!(matches!(
        s.at_version(v1),
        Err(Error::HistoryUnavailable(_))
    ));
    assert!(matches!(
        s.at_time(now_ms()),
        Err(Error::HistoryUnavailable(_))
    ));
    assert!(matches!(
        db.version_at(now_ms()),
        Err(Error::HistoryUnavailable(_))
    ));
    assert_eq!(s.kv_history("ns", b"k").unwrap().len(), 1);
    db.trim_history().unwrap();
}

#[test]
fn revisions_pass_the_callers_policies() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), Duration::from_secs(3600));
    let s = db.system();
    let public = s
        .json_put("posts", "p", &json!({"visibility": "public", "n": 1}))
        .unwrap();
    let private = s
        .json_put("posts", "p", &json!({"visibility": "private", "n": 2}))
        .unwrap();
    let again = s
        .json_put("posts", "p", &json!({"visibility": "public", "n": 3}))
        .unwrap();
    s.json_delete("posts", "p").unwrap();
    db.create_policy("public", Policy::new("posts", "$.visibility == 'public'"))
        .unwrap();
    let reader = db.session(SecurityContext::from_pair("role", "reader"));
    let seen: Vec<(u64, bool)> = reader
        .json_history("posts", "p")
        .unwrap()
        .iter()
        .map(|r| (r.version, r.doc.is_some()))
        .collect();
    assert_eq!(seen.len(), 3);
    assert_eq!(&seen[..2], [(public, true), (again, true)]);
    assert!(!seen[2].1, "the deletion after a readable version");
    assert!(seen.iter().all(|&(v, _)| v != private));
    assert!(reader
        .at_version(private)
        .unwrap()
        .json_get("posts", "p")
        .unwrap()
        .is_none());
}
