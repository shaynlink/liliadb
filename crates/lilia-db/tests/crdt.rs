//! CRDTs: PN-counters, OR-sets and LWW documents, replicated between databases by their
//! states; batches, transactions, security, ledger, concurrency.

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::small_options;
use lilia_core::crdt::{LwwClock, LwwJsonDocument};
use lilia_db::{
    Batch, CrdtWrite, Database, Error, LedgerVerifyMode, Policy, SecurityContext, Write,
};
use lilia_engine::Durability;
use serde_json::json;

fn open(dir: &std::path::Path, ledger: bool) -> Database {
    Database::open(dir, small_options(Durability::Performance, ledger)).unwrap()
}

fn strings(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|&s| s.to_owned()).collect()
}

#[test]
fn counters_count_per_actor_and_merge_replicas() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (db_a, db_b) = (open(dir_a.path(), false), open(dir_b.path(), false));
    let (s_a, s_b) = (db_a.system(), db_b.system());
    let a = s_a.crdt("metrics").counter("hits");
    let b = s_b.crdt("metrics").counter("hits");
    assert_eq!(a.value().unwrap(), 0);

    a.increment("device-1", 10).unwrap();
    a.decrement("device-1", 2).unwrap();
    a.increment("device-2", 15).unwrap();
    a.decrement("device-2", 5).unwrap();
    assert_eq!(a.value().unwrap(), 18);
    assert_eq!(
        a.increment("device-1", 0).unwrap().version,
        None,
        "no change"
    );
    let state = a.state().unwrap();
    assert_eq!(
        (
            state.p["device-1"],
            state.n["device-1"],
            state.p["device-2"]
        ),
        (10, 2, 15)
    );

    // Replica B learns A's state, then both count offline.
    b.merge(&state).unwrap();
    b.increment("device-3", 100).unwrap();
    b.decrement("device-1", 5).unwrap();
    a.increment("device-2", 1).unwrap();
    b.merge(&a.state().unwrap()).unwrap();
    a.merge(&b.state().unwrap()).unwrap();
    assert_eq!(a.state().unwrap(), b.state().unwrap());
    assert_eq!(a.value().unwrap(), 10 - 7 + 16 - 5 + 100);
    let again = a.merge(&b.state().unwrap()).unwrap();
    assert_eq!(again.version, None, "merging a known state changes nothing");

    let other = s_a.crdt("metrics").counter("misses");
    assert_eq!(other.value().unwrap(), 0, "objects are separate");
    assert!(matches!(
        a.increment("bad\nactor", 1),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn sets_keep_concurrent_adds_over_removals() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (db_a, db_b) = (open(dir_a.path(), false), open(dir_b.path(), false));
    let (s_a, s_b) = (db_a.system(), db_b.system());
    let a = s_a.crdt("inventory").set("items");
    let b = s_b.crdt("inventory").set("items");

    a.add("scanner-1", "widget-A").unwrap();
    a.add("scanner-1", "widget-B").unwrap();
    a.add("scanner-1", "").unwrap();
    a.add("scanner-1", "with\0nul").unwrap();
    assert!(a.contains("widget-A").unwrap());
    assert!(a.contains("with\0nul").unwrap() && !a.contains("with").unwrap());
    assert_eq!(
        a.elements().unwrap(),
        strings(&["", "widget-A", "widget-B", "with\0nul"])
    );
    assert!(a.remove("").unwrap());
    assert!(!a.remove("").unwrap(), "already removed");
    assert!(!a.remove("never-added").unwrap());

    b.merge(&a.state().unwrap()).unwrap();
    assert_eq!(b.elements().unwrap(), a.elements().unwrap());

    // A removes widget-A while B adds it again: the add wins on both sides.
    assert!(a.remove_at("widget-A", 1_000).unwrap());
    assert!(!a.contains("widget-A").unwrap());
    b.add("scanner-2", "widget-A").unwrap();
    a.merge(&b.state().unwrap()).unwrap();
    b.merge(&a.state().unwrap()).unwrap();
    assert!(a.contains("widget-A").unwrap());
    assert_eq!(a.state().unwrap(), b.state().unwrap());
    assert_eq!(
        a.elements().unwrap(),
        strings(&["widget-A", "widget-B", "with\0nul"])
    );

    // A removal after the merge sees both tags.
    assert!(a.remove_at("widget-A", 2_000).unwrap());
    b.merge(&a.state().unwrap()).unwrap();
    assert!(!b.contains("widget-A").unwrap());

    let before = a.state().unwrap().tombstones.len();
    assert_eq!(before, 3, "two tags of widget-A, one of the empty element");
    assert!(
        !a.prune(1_500).unwrap(),
        "removing again kept the later time, 2000"
    );
    assert!(a.prune(2_500).unwrap());
    assert_eq!(
        a.state().unwrap().tombstones.len(),
        1,
        "the empty element was removed now"
    );
    assert!(a.prune(i64::MAX).unwrap());
    assert!(a.state().unwrap().tombstones.is_empty());
    assert!(!a.prune(i64::MAX).unwrap());
    assert_eq!(a.elements().unwrap(), strings(&["widget-B", "with\0nul"]));
}

#[test]
fn documents_merge_concurrent_field_edits() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (db_a, db_b) = (open(dir_a.path(), false), open(dir_b.path(), false));
    let (s_a, s_b) = (db_a.system(), db_b.system());
    let a = s_a.crdt("users").doc("user-42");
    let b = s_b.crdt("users").doc("user-42");
    assert_eq!(a.get().unwrap(), None);

    a.set_field_at("system", "/username", &json!("johndoe"), 100)
        .unwrap();
    a.set_field_at("system", "profile/bio", &json!("Initial bio"), 100)
        .unwrap();
    a.set_field_at("system", "/profile/theme", &json!("dark"), 100)
        .unwrap();
    assert_eq!(
        a.get().unwrap(),
        Some(json!({"username": "johndoe", "profile": {"bio": "Initial bio", "theme": "dark"}}))
    );
    assert_eq!(a.field("/profile/theme").unwrap(), Some(json!("dark")));
    assert_eq!(
        a.field("profile").unwrap(),
        Some(json!({"bio": "Initial bio", "theme": "dark"}))
    );
    assert_eq!(a.field("/missing").unwrap(), None);
    b.merge(&a.state().unwrap().unwrap()).unwrap();

    // Offline: A edits the bio, B the theme and the avatar.
    a.set_field_at("device-a", "/profile/bio", &json!("Updated on mobile"), 150)
        .unwrap();
    b.set_field_at("device-b", "/profile/theme", &json!("light"), 160)
        .unwrap();
    b.set_field_at("device-b", "/profile/avatar", &json!("avatar.png"), 160)
        .unwrap();
    a.merge(&b.state().unwrap().unwrap()).unwrap();
    b.merge(&a.state().unwrap().unwrap()).unwrap();
    let merged = json!({
        "username": "johndoe",
        "profile": {"bio": "Updated on mobile", "theme": "light", "avatar": "avatar.png"}
    });
    assert_eq!(a.get().unwrap(), Some(merged.clone()));
    assert_eq!(b.get().unwrap(), Some(merged));

    // Concurrent writes of one field: the later clock wins on both sides.
    a.set_field_at("device-a", "/username", &json!("a-name"), 300)
        .unwrap();
    b.set_field_at("device-b", "/username", &json!("b-name"), 200)
        .unwrap();
    a.merge(&b.state().unwrap().unwrap()).unwrap();
    b.merge(&a.state().unwrap().unwrap()).unwrap();
    assert_eq!(a.field("/username").unwrap(), Some(json!("a-name")));
    assert_eq!(b.field("/username").unwrap(), Some(json!("a-name")));

    // A local write replaces what it sees, whatever its own time.
    a.set_field_at("device-c", "/username", &json!("late"), 1)
        .unwrap();
    assert_eq!(a.field("/username").unwrap(), Some(json!("late")));

    // A deletion takes the fields below; a newer remote field comes back, an older one not.
    assert!(a.delete_field_at("device-a", "/profile", 400).unwrap());
    assert!(!a.delete_field_at("device-a", "/profile", 401).unwrap());
    assert_eq!(a.get().unwrap(), Some(json!({"username": "late"})));
    let mut remote = LwwJsonDocument::new();
    remote.set_field("/profile/bio", json!("newer"), LwwClock::at(500, "z"));
    remote.set_field("/profile/theme", json!("older"), LwwClock::at(399, "z"));
    a.merge(&remote).unwrap();
    assert_eq!(
        a.get().unwrap(),
        Some(json!({"username": "late", "profile": {"bio": "newer"}}))
    );
    assert!(a.delete_field("device-a", "/").unwrap());
    assert_eq!(a.get().unwrap(), Some(json!({})), "every field deleted");
    assert!(a
        .state()
        .unwrap()
        .unwrap()
        .fields
        .values()
        .all(|f| f.is_deleted));

    assert!(matches!(
        a.set_field("device-a", "/a//b", &json!(1)),
        Err(Error::InvalidArgument(_))
    ));
    let mut bad = LwwJsonDocument::new();
    bad.fields
        .insert("no-slash".into(), remote.fields["/profile/bio"].clone());
    assert!(matches!(a.merge(&bad), Err(Error::InvalidArgument(_))));
}

#[test]
fn batches_and_transactions_see_their_own_writes() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();

    let mut batch = Batch::new();
    batch.push(Write::Crdt(CrdtWrite::Count {
        space: "analytics".into(),
        id: "views".into(),
        actor: "agent-1".into(),
        increment: 50,
        decrement: 8,
    }));
    batch.push(Write::Crdt(CrdtWrite::set_add(
        "tags",
        "article-1",
        "agent-1",
        "database",
    )));
    batch.push(Write::Crdt(CrdtWrite::SetRemove {
        space: "tags".into(),
        id: "article-1".into(),
        element: "database".into(),
        timestamp_ms: 10,
    }));
    batch.push(Write::Crdt(CrdtWrite::DocSet {
        space: "articles".into(),
        id: "art-1".into(),
        actor: "agent-1".into(),
        path: "/title".into(),
        value: json!("CRDTs in Storage"),
        timestamp_ms: 10,
    }));
    let outcomes = s.commit(&batch).unwrap();
    assert!(outcomes[0].version.is_some() && outcomes[2].deleted);
    assert_eq!(s.crdt("analytics").counter("views").value().unwrap(), 42);
    assert!(!s
        .crdt("tags")
        .set("article-1")
        .contains("database")
        .unwrap());
    assert_eq!(
        s.crdt("articles").doc("art-1").field("/title").unwrap(),
        Some(json!("CRDTs in Storage"))
    );

    let tx = s.transaction();
    let views = tx.crdt("analytics").counter("views");
    views.increment("agent-2", 8).unwrap();
    tx.crdt("tags")
        .set("article-1")
        .add("agent-2", "rust")
        .unwrap();
    assert_eq!(views.value().unwrap(), 50);
    assert!(tx.crdt("tags").set("article-1").contains("rust").unwrap());
    assert_eq!(
        s.crdt("analytics").counter("views").value().unwrap(),
        42,
        "not committed yet"
    );
    tx.commit().unwrap();
    assert_eq!(s.crdt("analytics").counter("views").value().unwrap(), 50);

    // Increments commute: transactions counting for one actor both commit, and both count.
    let first = s.transaction();
    let second = s.transaction();
    first
        .crdt("analytics")
        .counter("views")
        .increment("agent-1", 1)
        .unwrap();
    second
        .crdt("analytics")
        .counter("views")
        .increment("agent-1", 2)
        .unwrap();
    first.commit().unwrap();
    second.commit().unwrap();
    assert_eq!(s.crdt("analytics").counter("views").value().unwrap(), 53);

    // A transaction that read the counter conflicts with a writer that changed the totals it
    // read.
    let tx = s.transaction();
    assert_eq!(tx.crdt("analytics").counter("views").value().unwrap(), 53);
    s.crdt("analytics")
        .counter("views")
        .increment("agent-2", 1)
        .unwrap();
    tx.crdt("analytics")
        .counter("views")
        .increment("agent-1", 1)
        .unwrap();
    assert!(matches!(tx.commit(), Err(Error::Conflict { .. })));
}

#[test]
fn reads_pass_the_callers_policies_and_entries_are_ledger_records() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), true);
    let s = db.system();
    let space = s.crdt("team");
    space.counter("public").increment("ann", 3).unwrap();
    space.counter("private").increment("ann", 4).unwrap();
    space.set("public").add("ann", "x").unwrap();
    space
        .doc("public")
        .set_field_at("ann", "/a", &json!(1), 5)
        .unwrap();
    db.create_policy(
        "public",
        Policy::new(
            "team",
            "$.id == 'public' || current_context('role') == 'admin'",
        ),
    )
    .unwrap();
    let admin = db.session(SecurityContext::from_pair("role", "admin"));
    let guest = db.session(SecurityContext::from_pair("role", "guest"));
    assert_eq!(admin.crdt("team").counter("private").value().unwrap(), 4);
    assert_eq!(guest.crdt("team").counter("private").value().unwrap(), 0);
    assert_eq!(guest.crdt("team").counter("public").value().unwrap(), 3);
    assert!(guest.crdt("team").set("public").contains("x").unwrap());
    assert_eq!(
        guest.crdt("team").doc("public").get().unwrap(),
        Some(json!({"a": 1}))
    );

    let counter = db.prove_crdt_counter("team", "public", "ann").unwrap();
    assert_eq!(
        (counter.delta.model.as_str(), counter.delta.id.as_str()),
        ("crdt_counter", "public\u{1f}ann")
    );
    let tag = s.crdt("team").set("public").state().unwrap().entries["x"]
        .first()
        .unwrap()
        .clone();
    let entry = db
        .prove_crdt_set_entry("team", "public", "x", &tag)
        .unwrap();
    assert_eq!(entry.delta.id, format!("public\u{1f}x\u{1f}{tag}"));
    let field = db.prove_crdt_field("team", "public", "a").unwrap();
    assert_eq!(
        (field.delta.model.as_str(), field.delta.id.as_str()),
        ("crdt_doc", "public\u{1f}/a")
    );
    assert!(matches!(
        db.prove_crdt_field("team", "public", "/b"),
        Err(Error::NotFound(_))
    ));
    s.crdt("team").set("public").remove("x").unwrap();
    assert!(s.crdt("team").set("public").prune(i64::MAX).unwrap());
    let report = db.verify_ledger(LedgerVerifyMode::Strict).unwrap();
    assert!(report.is_valid, "{:?}", report.error);
}

#[test]
fn concurrent_writers_lose_nothing() {
    for ledger in [false, true] {
        concurrent_writers(ledger);
    }
}

fn concurrent_writers(ledger: bool) {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(open(dir.path(), ledger));
    let threads: Vec<_> = (0..8u64)
        .map(|w| {
            let db = Arc::clone(&db);
            std::thread::spawn(move || {
                let s = db.system();
                let space = s.crdt("shared");
                let own = format!("writer-{w}");
                for i in 0..100u64 {
                    // Each writer counts as itself and as one actor shared by all.
                    space.counter("c").increment(&own, 1).unwrap();
                    space.counter("c").increment("everyone", 1).unwrap();
                    space.counter("c").decrement("everyone", i % 2).unwrap();
                    let element = format!("{w}-{i}");
                    space.set("s").add(&own, &element).unwrap();
                    if i % 4 == 0 {
                        space.set("s").remove(&element).unwrap();
                    }
                    space
                        .doc("d")
                        .set_field(&own, &format!("/{own}/last"), &json!(i))
                        .unwrap();
                    space
                        .doc("d")
                        .set_field(&own, "/shared", &json!(w))
                        .unwrap();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let s = db.system();
    let space = s.crdt("shared");
    let counter = space.counter("c").state().unwrap();
    assert_eq!(counter.p["everyone"], 800);
    assert_eq!(counter.n["everyone"], 400);
    assert_eq!(counter.value(), 800 + 800 - 400);
    let elements = space.set("s").elements().unwrap();
    let expected: BTreeSet<String> = (0..8u64)
        .flat_map(|w| {
            (0..100u64)
                .filter(|i| i % 4 != 0)
                .map(move |i| format!("{w}-{i}"))
        })
        .collect();
    assert_eq!(elements, expected);
    let doc = space.doc("d").get().unwrap().unwrap();
    for w in 0..8 {
        assert_eq!(doc[format!("writer-{w}")]["last"], json!(99));
    }
    assert!(doc["shared"].as_u64().is_some_and(|w| w < 8));
    assert_eq!(space.doc("d").state().unwrap().unwrap().fields.len(), 9);
    if ledger {
        let report = db.verify_ledger(LedgerVerifyMode::Strict).unwrap();
        assert!(report.is_valid, "{:?}", report.error);
    }
}
