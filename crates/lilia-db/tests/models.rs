//! Key-value and document models, conditional batches, expiry and secondary indexes.

mod common;

use common::{small_options, Rng};
use lilia_db::{Batch, Database, Error, IndexOptions, Scan, Write};
use lilia_engine::Durability;
use serde_json::{json, Map, Value};

fn open(dir: &std::path::Path) -> Database {
    Database::open(dir, small_options(Durability::Performance, false)).unwrap()
}

fn query(value: &Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn ids(entries: &[lilia_db::JsonEntry]) -> Vec<String> {
    entries.iter().map(|e| e.id.clone()).collect()
}

#[test]
fn key_value_reads_writes_versions_and_scans() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let s = db.system();
    let v1 = s.kv_put("ns", b"a", b"1").unwrap();
    let v2 = s.kv_put("ns", b"a", b"2").unwrap();
    assert!(v2 > v1);
    let got = s.kv_get("ns", b"a").unwrap().unwrap();
    assert_eq!(
        (got.value.as_slice(), got.version, got.namespace.as_str()),
        (&b"2"[..], v2, "ns")
    );
    assert!(
        s.kv_get("other", b"a").unwrap().is_none(),
        "namespaces are separate"
    );

    for i in 0..20u8 {
        s.kv_put("ns", format!("p{i:02}").as_bytes(), &[i]).unwrap();
    }
    s.kv_put("ns2", b"p00", b"elsewhere").unwrap();
    let page = s.kv_scan("ns", &Scan::all().prefix("p").limit(5)).unwrap();
    assert_eq!(
        page.iter().map(|e| e.key.clone()).collect::<Vec<_>>(),
        (0..5)
            .map(|i| format!("p{i:02}").into_bytes())
            .collect::<Vec<_>>()
    );
    let next = s
        .kv_scan(
            "ns",
            &Scan::all()
                .prefix("p")
                .after(page[4].key.clone())
                .limit(100),
        )
        .unwrap();
    assert_eq!(next.len(), 15);
    assert_eq!(
        s.kv_scan("ns", &Scan::all()).unwrap().len(),
        21,
        "the namespace holds a and p00..p19 only"
    );
    assert!(s.kv_scan("ns", &Scan::all().limit(0)).unwrap().is_empty());

    assert!(s.kv_delete("ns", b"a").unwrap());
    assert!(
        !s.kv_delete("ns", b"a").unwrap(),
        "deleting an absent key deletes nothing"
    );
    assert_eq!(s.kv_get("ns", b"a").unwrap().map(|e| e.version), None);
    assert!(matches!(
        s.kv_get("bad\0name", b"a"),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn conditional_batches_are_all_or_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let s = db.system();
    let v = s.json_put("users", "u1", &json!({"name": "Ada"})).unwrap();

    let mut stale = Batch::new();
    stale
        .kv_put("ns", "k", "v")
        .json_put("users", "u2", &json!({"name": "Bob"}))
        .unwrap();
    stale.push(Write::JsonPut {
        space: "users".into(),
        id: "u1".into(),
        doc: lilia_db::BinaryDoc::from_json_value(&json!({"name": "Ada Lovelace"})).unwrap(),
        if_version: Some(v + 100),
    });
    match s.commit(&stale) {
        Err(Error::Conflict {
            target,
            expected,
            actual,
        }) => {
            assert_eq!(
                (target.as_str(), expected, actual),
                ("json:users/u1", Some(v + 100), Some(v))
            );
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert!(s.kv_get("ns", b"k").unwrap().is_none());
    assert!(s.json_get("users", "u2").unwrap().is_none());

    let create_only = Write::KvPut {
        namespace: "ns".into(),
        key: b"once".to_vec(),
        value: b"1".to_vec(),
        expires_at_ms: None,
        if_version: Some(0),
    };
    let first = s.write(&create_only).unwrap();
    assert!(matches!(
        s.write(&create_only),
        Err(Error::Conflict { expected: None, .. })
    ));

    let mut ok = Batch::new();
    ok.push(Write::KvPut {
        namespace: "ns".into(),
        key: b"once".to_vec(),
        value: b"2".to_vec(),
        expires_at_ms: None,
        if_version: first.version,
    })
    .json_delete("users", "u1")
    .json_delete("users", "missing");
    let outcomes = s.commit(&ok).unwrap();
    assert!(outcomes[0].version > first.version);
    assert!(outcomes[1].deleted && outcomes[1].version.is_some());
    assert!(!outcomes[2].deleted && outcomes[2].version.is_none());

    let mut twice = Batch::new();
    twice.kv_put("ns", "t", "1");
    twice.push(Write::KvPut {
        namespace: "ns".into(),
        key: b"t".to_vec(),
        value: b"2".to_vec(),
        expires_at_ms: None,
        if_version: Some(0),
    });
    assert!(
        matches!(s.commit(&twice), Err(Error::InvalidArgument(_))),
        "a condition on a record the batch already wrote is rejected"
    );

    let mut ordered = Batch::new();
    ordered
        .kv_put("ns", "o", "1")
        .kv_delete("ns", "o")
        .kv_put("ns", "o", "3");
    let outcomes = s.commit(&ordered).unwrap();
    assert!(!outcomes[1].deleted || outcomes[1].version.is_some());
    assert_eq!(s.kv_get("ns", b"o").unwrap().unwrap().value, b"3");
}

#[test]
fn expired_records_are_invisible_absent_for_conditions_and_purged() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let s = db.system();
    let past = Some(1_000);
    let future = Some(i64::MAX);
    for (key, expires) in [("gone", past), ("kept", future), ("plain", None)] {
        s.write(&Write::KvPut {
            namespace: "ttl".into(),
            key: key.into(),
            value: b"v".to_vec(),
            expires_at_ms: expires,
            if_version: None,
        })
        .unwrap();
    }
    assert!(s.kv_get("ttl", b"gone").unwrap().is_none());
    assert_eq!(
        s.kv_get("ttl", b"kept").unwrap().unwrap().expires_at_ms,
        future
    );
    assert_eq!(s.kv_scan("ttl", &Scan::all()).unwrap().len(), 2);
    assert!(
        !s.kv_delete("ttl", b"gone").unwrap(),
        "an expired record is not live"
    );

    s.write(&Write::KvPut {
        namespace: "ttl".into(),
        key: b"again".to_vec(),
        value: b"v".to_vec(),
        expires_at_ms: past,
        if_version: None,
    })
    .unwrap();
    let recreate = Write::KvPut {
        namespace: "ttl".into(),
        key: b"again".to_vec(),
        value: b"new".to_vec(),
        expires_at_ms: None,
        if_version: Some(0),
    };
    s.write(&recreate).unwrap();
    assert_eq!(s.kv_get("ttl", b"again").unwrap().unwrap().value, b"new");

    s.write(&Write::KvPut {
        namespace: "ttl".into(),
        key: b"x1".to_vec(),
        value: b"v".to_vec(),
        expires_at_ms: past,
        if_version: None,
    })
    .unwrap();
    s.write(&Write::KvPut {
        namespace: "ttl2".into(),
        key: b"x2".to_vec(),
        value: b"v".to_vec(),
        expires_at_ms: past,
        if_version: None,
    })
    .unwrap();
    assert_eq!(db.purge_expired().unwrap(), 2);
    assert_eq!(db.purge_expired().unwrap(), 0);
    assert_eq!(s.kv_scan("ttl", &Scan::all()).unwrap().len(), 3);
}

#[test]
fn documents_scan_and_find_without_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let s = db.system();
    for i in 0..30 {
        s.json_put(
            "items",
            &format!("i{i:02}"),
            &json!({"n": i, "even": i % 2 == 0, "tags": [format!("t{}", i % 3)]}),
        )
        .unwrap();
    }
    s.json_put("other", "i00", &json!({"n": 0})).unwrap();
    let all = s.json_scan("items", &Scan::all()).unwrap();
    assert_eq!(all.len(), 30);
    assert_eq!(all[3].extract_i64("n"), Some(3));
    let page = s.json_scan("items", &Scan::all().after("i27")).unwrap();
    assert_eq!(ids(&page), ["i28", "i29"]);
    let found = s
        .json_find(
            "items",
            &query(&json!({"even": true, "tags": "t0"})),
            &Scan::all(),
        )
        .unwrap();
    assert_eq!(ids(&found), ["i00", "i06", "i12", "i18", "i24"]);
    let limited = s
        .json_find(
            "items",
            &query(&json!({"even": true})),
            &Scan::all().limit(2).after("i10"),
        )
        .unwrap();
    assert_eq!(ids(&limited), ["i12", "i14"]);
    assert!(matches!(
        s.json_put("items", "", &json!({})),
        Err(Error::InvalidArgument(_))
    ));
}

/// Finds through an index return exactly what a scan filtered by the query returns, as the
/// documents change and across a reopen.
#[test]
fn indexes_are_built_maintained_used_and_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = Rng(7);
    let statuses = ["new", "open", "closed"];
    let random_doc = |rng: &mut Rng, i: u64| {
        json!({
            "status": statuses[usize::try_from(rng.below(3)).unwrap()],
            "email": format!("User{}@Example.com", rng.below(20)),
            "deleted": rng.below(4) == 0,
            "seq": i,
        })
    };
    {
        let db = open(dir.path());
        let s = db.system();
        for i in 0..300u64 {
            s.json_put(
                "docs",
                &format!("d{:03}", rng.below(200)),
                &random_doc(&mut rng, i),
            )
            .unwrap();
        }
        db.create_index("docs", "by_status", "$.status", &IndexOptions::default())
            .unwrap();
        db.create_index(
            "docs",
            "by_email",
            "lower($.email)",
            &IndexOptions::default(),
        )
        .unwrap();
        db.create_index(
            "docs",
            "live_status",
            "$.status",
            &IndexOptions::new().filter("$.deleted == false"),
        )
        .unwrap();
        assert!(matches!(
            db.create_index("docs", "by_status", "$.x", &IndexOptions::default()),
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            db.create_index("docs", "broken", "lower(", &IndexOptions::default()),
            Err(Error::Expression { .. })
        ));
        for i in 300..600u64 {
            let id = format!("d{:03}", rng.below(200));
            if rng.below(5) == 0 {
                s.json_delete("docs", &id).unwrap();
            } else {
                s.json_put("docs", &id, &random_doc(&mut rng, i)).unwrap();
            }
        }
        db.close().unwrap();
    }

    let db = open(dir.path());
    let s = db.system();
    assert_eq!(db.indexes("docs").len(), 3);
    let everything = s.json_scan("docs", &Scan::all()).unwrap();
    let check = |q: Value| {
        let q = query(&q);
        let expected: Vec<String> = everything
            .iter()
            .filter(|e| {
                q.iter().all(|(k, v)| match k.as_str() {
                    "email" => {
                        e.extract_str("email").map(str::to_lowercase).as_deref()
                            == v.as_str().map(str::to_lowercase).as_deref()
                            && e.extract_str("email") == v.as_str()
                    }
                    _ => e.to_json_value().get(k) == Some(v),
                })
            })
            .map(|e| e.id.clone())
            .collect();
        assert_eq!(
            ids(&s.json_find("docs", &q, &Scan::all()).unwrap()),
            expected,
            "query {q:?}"
        );
    };
    for status in statuses {
        check(json!({"status": status}));
        check(json!({"status": status, "deleted": false}));
        check(json!({"status": status, "deleted": true}));
    }
    for n in 0..20 {
        check(json!({"email": format!("User{n}@Example.com")}));
    }
    let by_name = s
        .json_find(
            "docs",
            &query(&json!({"by_email": "user3@example.com"})),
            &Scan::all(),
        )
        .unwrap();
    assert!(by_name.iter().all(|e| e
        .extract_str("email")
        .unwrap()
        .eq_ignore_ascii_case("user3@example.com")));
    let paged = s
        .json_find(
            "docs",
            &query(&json!({"status": "open"})),
            &Scan::all().limit(3),
        )
        .unwrap();
    let rest = s
        .json_find(
            "docs",
            &query(&json!({"status": "open"})),
            &Scan::all().after(paged[2].id.clone()),
        )
        .unwrap();
    let all_open = s
        .json_find("docs", &query(&json!({"status": "open"})), &Scan::all())
        .unwrap();
    assert_eq!([ids(&paged), ids(&rest)].concat(), ids(&all_open));

    assert!(db.drop_index("docs", "by_status").unwrap());
    assert!(!db.drop_index("docs", "by_status").unwrap());
    db.create_index("docs", "by_status", "$.status", &IndexOptions::default())
        .unwrap();
    for status in statuses {
        check(json!({"status": status}));
    }
}
