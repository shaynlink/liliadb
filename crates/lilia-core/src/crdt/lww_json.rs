use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{CvRdt, LwwClock};

/// Represents an individual field within an LWW JSON document.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
    rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(crate = rkyv)]
pub struct LwwField {
    /// The stored JSON value of this field.
    #[rkyv(with = crate::zero_copy::AsJsonString)]
    pub value: Value,
    /// The logical clock of the last write.
    pub clock: LwwClock,
    /// Whether this field has been marked as deleted (tombstone).
    pub is_deleted: bool,
}

/// A Conflict-Free Replicated JSON Document using Last-Write-Wins registers at the field level.
///
/// Rather than resolving conflicts at the coarse whole-document level (which results in silent
/// overwrites of unrelated changes), `LwwJsonDocument` tracks logical clocks per field/path.
/// Concurrent offline edits to distinct fields merge seamlessly without data loss.
#[derive(
    Debug, Clone, PartialEq, Default, Serialize, Deserialize,
    rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(crate = rkyv)]
pub struct LwwJsonDocument {
    /// Maps normalized JSON Pointer paths (e.g. `/author/name`) to their LWW field state.
    pub fields: BTreeMap<String, LwwField>,
}

fn normalize_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    }
}

impl LwwJsonDocument {
    /// Creates a new, empty `LwwJsonDocument`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            fields: BTreeMap::new(),
        }
    }

    /// Sets a field at `path` with `value` and `clock`.
    ///
    /// The update only takes effect if `clock` is strictly greater than the existing field's clock.
    pub fn set_field(&mut self, path: &str, value: Value, clock: LwwClock) {
        let norm = normalize_path(path);
        match self.fields.get_mut(&norm) {
            Some(existing) => {
                if clock > existing.clock {
                    existing.value = value;
                    existing.clock = clock;
                    existing.is_deleted = false;
                }
            }
            None => {
                self.fields.insert(
                    norm,
                    LwwField {
                        value,
                        clock,
                        is_deleted: false,
                    },
                );
            }
        }
    }

    /// Marks a field at `path` as deleted using `clock`.
    ///
    /// Also marks any descendant fields (e.g. `/author/name` when deleting `/author`) as deleted
    /// if `clock` is greater than their current clock.
    pub fn delete_field(&mut self, path: &str, clock: &LwwClock) {
        let norm = normalize_path(path);
        let prefix = format!("{norm}/");

        // Tombstone the target field itself
        match self.fields.get_mut(&norm) {
            Some(existing) => {
                if clock > &existing.clock {
                    existing.is_deleted = true;
                    existing.clock = clock.clone();
                }
            }
            None => {
                self.fields.insert(
                    norm,
                    LwwField {
                        value: Value::Null,
                        clock: clock.clone(),
                        is_deleted: true,
                    },
                );
            }
        }

        // Tombstone any descendant fields under this path prefix
        for (f_path, f_state) in &mut self.fields {
            if f_path.starts_with(&prefix) && clock > &f_state.clock {
                f_state.is_deleted = true;
                f_state.clock = clock.clone();
            }
        }
    }

    /// Retrieves the value of a field if it exists and is not deleted.
    #[must_use]
    pub fn get_field(&self, path: &str) -> Option<&Value> {
        let norm = normalize_path(path);
        self.fields
            .get(&norm)
            .and_then(|f| if f.is_deleted { None } else { Some(&f.value) })
    }

    /// Flattens a `serde_json::Value` into an `LwwJsonDocument`, associating each leaf path with `clock`.
    #[must_use]
    pub fn from_value(value: &Value, clock: &LwwClock) -> Self {
        let mut doc = Self::new();
        flatten_into(&mut doc, "", value, clock);
        doc
    }

    /// Materializes the LWW fields into a standard hierarchical `serde_json::Value` object.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut root = Value::Object(Map::new());

        for (path, field) in &self.fields {
            if field.is_deleted {
                continue;
            }

            let tokens: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            if tokens.is_empty() {
                // Root assignment
                return field.value.clone();
            }

            insert_at_tokens(&mut root, &tokens, field.value.clone());
        }

        root
    }
}

fn flatten_into(doc: &mut LwwJsonDocument, current_path: &str, val: &Value, clock: &LwwClock) {
    match val {
        Value::Object(map) if !map.is_empty() => {
            for (key, sub_val) in map {
                let next_path = format!("{current_path}/{key}");
                flatten_into(doc, &next_path, sub_val, clock);
            }
        }
        _ => {
            let path = if current_path.is_empty() {
                "/"
            } else {
                current_path
            };
            doc.set_field(path, val.clone(), clock.clone());
        }
    }
}

fn insert_at_tokens(current: &mut Value, tokens: &[&str], value: Value) {
    if tokens.is_empty() {
        *current = value;
        return;
    }

    let token = tokens[0];
    let is_last = tokens.len() == 1;

    if !current.is_object() {
        *current = Value::Object(Map::new());
    }

    let map = current.as_object_mut().expect("must be object");

    if is_last {
        map.insert(token.to_owned(), value);
    } else {
        let entry = map
            .entry(token.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        insert_at_tokens(entry, &tokens[1..], value);
    }
}

impl CvRdt for LwwJsonDocument {
    fn merge(&mut self, other: &Self) {
        for (path, other_field) in &other.fields {
            match self.fields.get_mut(path) {
                Some(self_field) => {
                    if other_field.clock > self_field.clock {
                        *self_field = other_field.clone();
                    }
                }
                None => {
                    self.fields.insert(path.clone(), other_field.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_lww_json_flatten_and_materialize() {
        let initial = json!({
            "title": "Document Title",
            "author": {
                "name": "Ada Lovelace",
                "email": "ada@example.com"
            },
            "views": 42
        });

        let clock = LwwClock::at(100, "dev-1");
        let doc = LwwJsonDocument::from_value(&initial, &clock);

        assert_eq!(doc.get_field("/title"), Some(&json!("Document Title")));
        assert_eq!(doc.get_field("/author/name"), Some(&json!("Ada Lovelace")));
        assert_eq!(doc.get_field("/views"), Some(&json!(42)));

        let materialized = doc.to_value();
        assert_eq!(materialized, initial);
    }

    #[test]
    fn test_lww_json_concurrent_non_conflicting_field_edits() {
        let initial = json!({
            "title": "Notes",
            "author": {
                "name": "Alice",
                "avatar": "default.png"
            }
        });

        let clock0 = LwwClock::at(100, "dev-0");
        let mut replica1 = LwwJsonDocument::from_value(&initial, &clock0);
        let mut replica2 = replica1.clone();

        // Replica 1 modifies author.name offline at t=150
        replica1.set_field(
            "/author/name",
            json!("Alice Cooper"),
            LwwClock::at(150, "dev-1"),
        );

        // Replica 2 modifies author.avatar offline at t=160
        replica2.set_field(
            "/author/avatar",
            json!("guitar.png"),
            LwwClock::at(160, "dev-2"),
        );

        // Merge Replica 2 into Replica 1
        replica1.merge(&replica2);

        let final_val = replica1.to_value();
        assert_eq!(final_val["title"], "Notes");
        assert_eq!(final_val["author"]["name"], "Alice Cooper");
        assert_eq!(final_val["author"]["avatar"], "guitar.png");

        // Merge Replica 1 into Replica 2 (commutativity)
        replica2.merge(&replica1);
        assert_eq!(replica1, replica2);
    }

    #[test]
    fn test_lww_json_conflicting_field_edit_resolves_to_highest_clock() {
        let mut replica1 = LwwJsonDocument::new();
        let mut replica2 = LwwJsonDocument::new();

        // Both modify the same field "/status"
        replica1.set_field("/status", json!("pending"), LwwClock::at(100, "dev-1"));
        replica2.set_field("/status", json!("approved"), LwwClock::at(150, "dev-2"));

        replica1.merge(&replica2);
        assert_eq!(replica1.get_field("/status"), Some(&json!("approved")));

        // Equal timestamp, tie-break on actor_id
        let mut r_a = LwwJsonDocument::new();
        let mut r_b = LwwJsonDocument::new();
        r_a.set_field("/color", json!("red"), LwwClock::at(200, "actor-a"));
        r_b.set_field("/color", json!("blue"), LwwClock::at(200, "actor-b"));

        r_a.merge(&r_b);
        assert_eq!(r_a.get_field("/color"), Some(&json!("blue"))); // "actor-b" > "actor-a"
    }

    #[test]
    fn test_lww_json_field_deletion_and_recreation() {
        let mut doc = LwwJsonDocument::new();
        doc.set_field(
            "/author/bio",
            json!("Short bio"),
            LwwClock::at(100, "dev-1"),
        );
        assert!(doc.get_field("/author/bio").is_some());

        // Delete field at t=150
        doc.delete_field("/author/bio", &LwwClock::at(150, "dev-1"));
        assert_eq!(doc.get_field("/author/bio"), None);
        assert_eq!(doc.to_value(), json!({}));

        // Re-create field at t=200
        doc.set_field("/author/bio", json!("New bio"), LwwClock::at(200, "dev-1"));
        assert_eq!(doc.get_field("/author/bio"), Some(&json!("New bio")));
        assert_eq!(doc.to_value(), json!({"author": {"bio": "New bio"}}));
    }

    #[test]
    fn test_lww_json_semilattice_properties() {
        let mut a = LwwJsonDocument::new();
        a.set_field("/x", json!(1), LwwClock::at(100, "a"));
        a.set_field("/y", json!(2), LwwClock::at(100, "a"));

        let mut b = LwwJsonDocument::new();
        b.set_field("/y", json!(20), LwwClock::at(120, "b"));
        b.set_field("/z", json!(30), LwwClock::at(120, "b"));

        let mut c = LwwJsonDocument::new();
        c.set_field("/x", json!(100), LwwClock::at(150, "c"));

        // Idempotence
        let mut a_copy = a.clone();
        a_copy.merge(&a);
        assert_eq!(a_copy, a);

        // Commutativity
        let mut ab = a.clone();
        ab.merge(&b);
        let mut ba = b.clone();
        ba.merge(&a);
        assert_eq!(ab, ba);

        // Associativity
        let mut ab_c = ab.clone();
        ab_c.merge(&c);

        let mut bc = b.clone();
        bc.merge(&c);
        let mut a_bc = a.clone();
        a_bc.merge(&bc);

        assert_eq!(ab_c, a_bc);
    }
}
