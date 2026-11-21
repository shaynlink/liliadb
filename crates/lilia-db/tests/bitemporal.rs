//! Bitemporal entities: what was known when, about which valid time; slicing, joining,
//! retractions, scans, batches, transactions, security, ledger.

mod common;

use std::time::Duration;

use common::small_options;
use lilia_core::{parse_iso_to_ms, BitemporalInterval, BitemporalPoint, BITEMPORAL_INFINITY};
use lilia_db::{
    Batch, BitemporalWrite, Database, Error, LedgerVerifyMode, Options, Policy, Scan,
    SecurityContext, Write,
};
use lilia_engine::Durability;
use serde_json::json;

fn open(dir: &std::path::Path, ledger: bool) -> Database {
    let options = Options {
        history_retention: Duration::ZERO,
        ..small_options(Durability::Performance, ledger)
    };
    Database::open(dir, options).unwrap()
}

fn at(iso: &str) -> i64 {
    parse_iso_to_ms(iso).unwrap()
}

fn point(iso: &str) -> BitemporalPoint {
    BitemporalPoint::at(iso).unwrap()
}

#[test]
fn corrections_keep_what_was_known_before_them() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();
    let contracts = s.bitemporal("contracts");
    let coverage = BitemporalInterval::inclusive_days("2027-01-01", "2027-12-31").unwrap();
    let signed = contracts
        .put_recorded_at(
            "pol_881",
            &json!({"amount": 1000, "status": "signed"}),
            coverage,
            at("2026-11-10T10:00:00Z"),
        )
        .unwrap();
    let amended = contracts
        .put_recorded_at(
            "pol_881",
            &json!({"amount": 1050, "status": "rectified"}),
            coverage,
            at("2026-12-15T14:30:00Z"),
        )
        .unwrap();

    let nov = contracts
        .get_as_of("pol_881", point("2026-11-20"), point("2027-02-01"))
        .unwrap()
        .unwrap();
    assert_eq!(nov.value["amount"], 1000);
    assert_eq!(
        (nov.tt_from, nov.tt_to),
        (at("2026-11-10T10:00:00Z"), at("2026-12-15T14:30:00Z"))
    );
    assert!(
        nov.version > signed.version.unwrap(),
        "kept as a past slice by the amendment"
    );
    let dec = contracts
        .get_as_of("pol_881", point("2026-12-20"), point("2027-02-01"))
        .unwrap()
        .unwrap();
    assert_eq!(dec.value["amount"], 1050);
    assert_eq!(
        (dec.version, dec.tt_to),
        (amended.version.unwrap(), BITEMPORAL_INFINITY)
    );
    assert_eq!(
        contracts
            .get("pol_881", point("2027-02-01"))
            .unwrap()
            .unwrap()
            .value["amount"],
        1050
    );
    assert!(
        contracts
            .get("pol_881", point("2028-02-01"))
            .unwrap()
            .is_none(),
        "not covered"
    );
    assert!(
        contracts
            .get_as_of("pol_881", point("2026-11-01"), point("2027-02-01"))
            .unwrap()
            .is_none(),
        "not known yet"
    );
    let history = contracts.history("pol_881").unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].value["status"], "signed");

    // Transaction time never goes back for an entity.
    assert!(matches!(
        contracts.put_recorded_at("pol_881", &json!({}), coverage, at("2026-12-01")),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        contracts.put("pol_881", &json!({}), BitemporalInterval { from: 5, to: 5 }),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn intervals_are_cut_joined_and_retracted() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();
    let policies = s.bitemporal("policies");
    let year = BitemporalInterval::inclusive_days("2027-01-01", "2027-12-31").unwrap();
    let t1 = at("2026-11-10T12:00:00Z");
    let t2 = at("2026-12-01T12:00:00Z");
    policies
        .put_recorded_at("pol", &json!({"rate": 1000}), year, t1)
        .unwrap();
    let q3 = BitemporalInterval::inclusive_days("2027-07-01", "2027-09-30").unwrap();
    policies
        .put_recorded_at("pol", &json!({"rate": 1400}), q3, t2)
        .unwrap();
    let timeline = policies.timeline_as_of("pol", t2 + 1_000).unwrap();
    let rates: Vec<_> = timeline.iter().map(|e| e.value["rate"].clone()).collect();
    assert_eq!(rates, [json!(1000), json!(1400), json!(1000)]);
    assert_eq!(timeline[1].vt_from, q3.from);
    let rate = |known: &str, valid: &str| {
        policies
            .get_as_of("pol", point(known), point(valid))
            .unwrap()
            .unwrap()
            .value["rate"]
            .clone()
    };
    assert_eq!(rate("2026-12-05", "2027-05-01"), 1000);
    assert_eq!(rate("2026-12-05", "2027-08-15"), 1400);
    assert_eq!(rate("2026-12-05", "2027-11-01"), 1000);
    assert_eq!(rate("2026-11-20", "2027-08-15"), 1000);

    // Equal adjacent values join into one interval.
    let tariffs = s.bitemporal("tariffs");
    let h1 = BitemporalInterval::between("2027-01-01", "2027-07-01").unwrap();
    let h2 = BitemporalInterval::between("2027-07-01", "2028-01-01").unwrap();
    tariffs.put("power", &json!({"kwh": 0.25}), h1).unwrap();
    tariffs.put("power", &json!({"kwh": 0.25}), h2).unwrap();
    let joined = tariffs.timeline("power").unwrap();
    assert_eq!(joined.len(), 1);
    assert_eq!((joined[0].vt_from, joined[0].vt_to), (h1.from, h2.to));

    // Retractions cut around what they retract, and the past still answers.
    let t3 = at("2027-01-15T00:00:00Z");
    assert!(policies.delete_recorded_at("pol", Some(q3), t3).unwrap());
    assert!(policies.get("pol", point("2027-08-15")).unwrap().is_none());
    assert_eq!(policies.timeline("pol").unwrap().len(), 2);
    assert_eq!(rate("2027-01-01", "2027-08-15"), 1400);
    assert!(!policies.delete("pol", Some(q3)).unwrap());
    // Recorded now, before the last change: recorded at the last change instead.
    assert!(policies.delete("pol", None).unwrap());
    assert!(policies.timeline("pol").unwrap().is_empty());
    assert_eq!(
        policies
            .history("pol")
            .unwrap()
            .iter()
            .filter(|e| e.tt_to == t3)
            .count(),
        3
    );
    assert_eq!(rate("2027-01-01", "2027-05-01"), 1000);
    assert_eq!(
        policies.history("pol").unwrap().len(),
        4,
        "the year, then its three pieces"
    );
}

#[test]
fn scans_list_entities_as_known_then_and_now() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();
    let people = s.bitemporal("people");
    let always = BitemporalInterval { from: 0, to: 1_000 };
    for (id, t) in [("ann", 10), ("bob", 10), ("cid", 10), ("dan", 10)] {
        people
            .put_recorded_at(id, &json!({"name": id}), always, t)
            .unwrap();
    }
    people.delete_recorded_at("bob", None, 20).unwrap();
    let later = BitemporalInterval {
        from: 100,
        to: 1_000,
    };
    assert!(people.delete_recorded_at("cid", Some(later), 20).unwrap());
    s.bitemporal("other")
        .put("zed", &json!({}), always)
        .unwrap();

    let ids = |entries: Vec<lilia_core::BitemporalEntry>| -> Vec<String> {
        entries.into_iter().map(|e| e.id).collect()
    };
    assert_eq!(
        ids(people.scan(500, &Scan::all()).unwrap()),
        ["ann", "dan"],
        "cid ends at 100"
    );
    assert_eq!(
        ids(people.scan(50, &Scan::all()).unwrap()),
        ["ann", "cid", "dan"]
    );
    assert_eq!(ids(people.scan(150, &Scan::all()).unwrap()), ["ann", "dan"]);
    assert_eq!(
        ids(people.scan_as_of(15, 500, &Scan::all()).unwrap()),
        ["ann", "bob", "cid", "dan"]
    );
    let page = people.scan_as_of(15, 500, &Scan::all().limit(2)).unwrap();
    assert_eq!(ids(page), ["ann", "bob"]);
    assert_eq!(
        ids(people
            .scan_as_of(15, 500, &Scan::all().after("bob").limit(5))
            .unwrap()),
        ["cid", "dan"]
    );
    assert_eq!(
        ids(people.scan(500, &Scan::all().prefix("d")).unwrap()),
        ["dan"]
    );
}

#[test]
fn batches_transactions_policies_and_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), true);
    let s = db.system();
    let valid = BitemporalInterval { from: 100, to: 200 };
    let mut batch = Batch::new();
    batch.push(Write::Bitemporal(BitemporalWrite::Put {
        space: "facts".into(),
        id: "public".into(),
        value: json!({"n": 1}),
        valid,
        recorded_at: Some(10),
    }));
    batch.push(Write::Bitemporal(BitemporalWrite::Put {
        space: "facts".into(),
        id: "secret".into(),
        value: json!({"n": 2}),
        valid,
        recorded_at: None,
    }));
    let outcomes = s.commit(&batch).unwrap();
    assert!(outcomes.iter().all(|o| o.version.is_some()));

    let tx = s.transaction();
    let facts = tx.bitemporal("facts");
    facts
        .put(
            "public",
            &json!({"n": 3}),
            BitemporalInterval { from: 150, to: 300 },
        )
        .unwrap();
    assert_eq!(facts.timeline("public").unwrap().len(), 2);
    assert_eq!(
        s.bitemporal("facts").timeline("public").unwrap().len(),
        1,
        "not committed"
    );
    tx.commit().unwrap();
    assert_eq!(
        s.bitemporal("facts")
            .get("public", 250)
            .unwrap()
            .unwrap()
            .value["n"],
        3
    );

    db.create_policy("public", Policy::new("facts", "$.id == 'public'"))
        .unwrap();
    let guest = db.session(SecurityContext::from_pair("role", "guest"));
    assert!(guest
        .bitemporal("facts")
        .get("secret", 150)
        .unwrap()
        .is_none());
    assert!(guest
        .bitemporal("facts")
        .history("secret")
        .unwrap()
        .is_empty());
    assert_eq!(
        guest.bitemporal("facts").timeline("public").unwrap().len(),
        2
    );
    assert_eq!(
        guest
            .bitemporal("facts")
            .scan(150, &Scan::all())
            .unwrap()
            .len(),
        1
    );

    let proof = db.prove_bitemporal("facts", "public", 100).unwrap();
    assert_eq!(
        (proof.delta.model.as_str(), proof.delta.id.as_str()),
        ("bitemporal", "public\u{1f}100")
    );
    let report = db.verify_ledger(LedgerVerifyMode::Strict).unwrap();
    assert!(report.is_valid, "{:?}", report.error);
}
