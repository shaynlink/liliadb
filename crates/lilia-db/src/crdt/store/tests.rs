use std::collections::BTreeMap;

use lilia_core::crdt::{CvRdt, LwwJsonDocument};
use serde_json::json;

use super::*;

const OBJECT: (&str, &str) = ("space", "x");

#[derive(Default)]
struct Map(BTreeMap<Bytes, Bytes>);

impl RecordSource for Map {
    fn read(&mut self, key: &Bytes) -> Result<Option<Bytes>> {
        Ok(self.0.get(key).cloned())
    }

    fn live(&mut self, start: &[u8], end: &[u8]) -> Result<Vec<(Bytes, Bytes)>> {
        Ok(self
            .0
            .iter()
            .filter(|(k, _)| k.as_ref() >= start && k.as_ref() < end)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }
}

impl Map {
    fn apply(&mut self, effect: Effect) -> bool {
        for (key, value) in effect.changes {
            match value {
                Some(value) => self.0.insert(key, value),
                None => self.0.remove(&key),
            };
        }
        effect.deleted
    }

    fn counter(&self) -> PNCounter {
        let start = object_prefix(keys::CRDT_COUNTER, OBJECT.0, OBJECT.1);
        let mut counter = PNCounter::new();
        for (key, raw) in &self.0 {
            if let Some(actor) = key.strip_prefix(start.as_slice()) {
                let actor = String::from_utf8(actor.to_vec()).unwrap();
                let (p, n) = decode_counter(raw).unwrap();
                counter.p.insert(actor.clone(), p);
                counter.n.insert(actor, n);
            }
        }
        counter
    }

    fn set(&self) -> OrSet<String> {
        let start = object_prefix(keys::CRDT_SET, OBJECT.0, OBJECT.1);
        let mut set = OrSet::new();
        for (key, raw) in &self.0 {
            if let Some(rest) = key.strip_prefix(start.as_slice()) {
                let (element, tag) = entry_parts(rest).unwrap();
                if let Some(at) = decode_entry(raw).unwrap() {
                    set.tombstones.insert(tag.clone(), at);
                }
                set.add_with_tag(element, tag);
            }
        }
        set
    }

    fn doc(&self) -> LwwJsonDocument {
        let start = object_prefix(keys::CRDT_FIELD, OBJECT.0, OBJECT.1);
        let mut doc = LwwJsonDocument::new();
        for (key, raw) in &self.0 {
            if let Some(path) = key.strip_prefix(start.as_slice()) {
                let path = String::from_utf8(path.to_vec()).unwrap();
                doc.fields.insert(path, decode_field(raw).unwrap());
            }
        }
        doc
    }
}

#[test]
fn counters_count_per_actor_and_merge_by_maximum() {
    let mut map = Map::default();
    let effect = count(&mut map, OBJECT, "a", 10, 2).unwrap();
    map.apply(effect);
    let effect = count(&mut map, OBJECT, "a", 5, 0).unwrap();
    map.apply(effect);
    assert_eq!(count(&mut map, OBJECT, "a", 0, 0).unwrap().changes, vec![]);
    assert_eq!(map.counter().value(), 13);

    let mut remote = PNCounter::new();
    remote.increment("a", 12);
    remote.decrement("a", 4);
    remote.increment("b", 100);
    let effect = merge_counter(&mut map, OBJECT, &remote).unwrap();
    map.apply(effect);
    assert_eq!(map.counter().value(), 15 - 4 + 100);
    let mut expected = map.counter();
    expected.merge(&remote);
    assert_eq!(map.counter(), expected);
    let again = merge_counter(&mut map, OBJECT, &remote).unwrap();
    assert!(again.changes.is_empty(), "merging is idempotent");
}

#[test]
fn sets_add_remove_merge_and_prune_like_the_or_set() {
    let mut map = Map::default();
    for (element, tag) in [("a", "n:1"), ("b", "n:2"), ("a\0z", "n:3")] {
        let effect = add(&mut map, OBJECT, element, tag).unwrap();
        map.apply(effect);
    }
    let removed = remove(&mut map, OBJECT, "a", 100).unwrap();
    assert!(map.apply(removed));
    let set = map.set();
    assert_eq!(
        set.read(),
        BTreeSet::from(["b".to_owned(), "a\0z".to_owned()])
    );
    assert!(!remove(&mut map, OBJECT, "a", 50).unwrap().deleted);
    let effect = add(&mut map, OBJECT, "a", "n:1").unwrap();
    assert!(
        effect.changes.is_empty(),
        "a tombstoned tag stays tombstoned"
    );

    // A concurrent add of `a` under a new tag wins over the removal.
    let mut remote = OrSet::new();
    remote.add_with_tag("a".to_owned(), "m:1".to_owned());
    remote.add_with_tag("b".to_owned(), "n:2".to_owned());
    remote.tombstones.insert("n:2".to_owned(), 200);
    let effect = merge_set(&mut map, OBJECT, &remote).unwrap();
    map.apply(effect);
    let mut expected = set.clone();
    expected.merge(&remote);
    assert_eq!(map.set(), expected);
    assert_eq!(
        map.set().read(),
        BTreeSet::from(["a".to_owned(), "a\0z".to_owned()])
    );

    // A tombstone without its entry applies to the local entry of its tag.
    let mut orphan = OrSet::new();
    orphan.tombstones.insert("n:3".to_owned(), 300);
    let effect = merge_set(&mut map, OBJECT, &orphan).unwrap();
    map.apply(effect);
    assert_eq!(map.set().read(), BTreeSet::from(["a".to_owned()]));

    let pruned = prune(&mut map, OBJECT, 250).unwrap();
    assert!(map.apply(pruned));
    assert_eq!(
        map.set().tombstones.len(),
        1,
        "the entry removed at 300 stays"
    );
    assert_eq!(map.set().read(), BTreeSet::from(["a".to_owned()]));
}

#[test]
fn documents_order_local_writes_after_what_they_replace() {
    let mut map = Map::default();
    let effect = set_field(&mut map, OBJECT, "/name", &json!("Ada"), ("b", 500)).unwrap();
    map.apply(effect);
    // An earlier clock from another actor still replaces the value it observed.
    let effect = set_field(&mut map, OBJECT, "/name", &json!("Grace"), ("a", 100)).unwrap();
    map.apply(effect);
    let doc = map.doc();
    assert_eq!(doc.get_field("/name"), Some(&json!("Grace")));
    assert_eq!(doc.fields["/name"].clock.clock, LogicalClock::new(500, 1));

    for (path, value) in [
        ("/profile/bio", json!("x")),
        ("/profile/theme", json!("dark")),
    ] {
        let effect = set_field(&mut map, OBJECT, path, &value, ("a", 600)).unwrap();
        map.apply(effect);
    }
    let effect = set_field(&mut map, OBJECT, "/profiles", &json!(1), ("a", 600)).unwrap();
    map.apply(effect);
    let deleted = delete_field(&mut map, OBJECT, "/profile", ("a", 10)).unwrap();
    assert!(map.apply(deleted));
    assert_eq!(
        map.doc().to_value(),
        json!({"name": "Grace", "profiles": 1})
    );
    let doc = map.doc();
    assert!(doc.fields["/profile"].is_deleted, "absent, marked deleted");
    assert!(doc.fields["/profile/bio"].clock.clock > LogicalClock::new(600, 0));

    // A remote value newer than the deletion comes back; an older one does not.
    let mut remote = LwwJsonDocument::new();
    remote.set_field("/profile/bio", json!("new"), LwwClock::at(700, "c"));
    remote.set_field("/profile/theme", json!("old"), LwwClock::at(600, "c"));
    let effect = merge_doc(&mut map, OBJECT, remote.fields.iter()).unwrap();
    map.apply(effect);
    assert_eq!(
        map.doc().to_value(),
        json!({"name": "Grace", "profile": {"bio": "new"}, "profiles": 1})
    );

    let deleted = delete_field(&mut map, OBJECT, "/", ("a", 0)).unwrap();
    assert!(map.apply(deleted));
    assert_eq!(map.doc().to_value(), json!({}));
}

#[test]
fn paths_are_normalized() {
    assert_eq!(normalize_path("a/b").unwrap(), "/a/b");
    assert_eq!(normalize_path("").unwrap(), "/");
    assert_eq!(normalize_path("/").unwrap(), "/");
    assert!(normalize_path("/a/").is_err());
    assert!(normalize_path("/a//b").is_err());
}
