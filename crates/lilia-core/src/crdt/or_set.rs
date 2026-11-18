use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::CvRdt;

/// An Observed-Remove Set (OR-Set) CRDT with Add-Wins semantics.
///
/// Each element addition is tagged with a globally unique identifier. When an element
/// is removed, all tags currently observed locally for that element are recorded in
/// the tombstone set. Concurrent additions generate distinct tags that are not yet
/// observed by the removing replica, so the addition wins on merge.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OrSet<T: Ord + Clone> {
    /// Active and tombstoned entries mapping to their birth tags.
    pub entries: BTreeMap<T, BTreeSet<String>>,
    /// Tombstoned tags mapping to the timestamp (in ms) when they were tombstoned.
    pub tombstones: BTreeMap<String, i64>,
}

impl<T: Ord + Clone> OrSet<T> {
    /// Creates a new, empty `OrSet`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            tombstones: BTreeMap::new(),
        }
    }

    /// Adds an element for the given `actor` and returns the generated unique tag.
    pub fn add(&mut self, actor: &str, elem: T) -> String {
        let tag = format!("{actor}:{}", Uuid::new_v4());
        self.entries.entry(elem).or_default().insert(tag.clone());
        tag
    }

    /// Adds an element with an explicit tag (useful for replication / sync replays).
    pub fn add_with_tag(&mut self, elem: T, tag: String) {
        self.entries.entry(elem).or_default().insert(tag);
    }

    /// Removes an element by tombstoning all currently observed tags for it.
    ///
    /// If no tags exist for `elem`, this is a no-op.
    pub fn remove(&mut self, elem: &T, timestamp_ms: i64) {
        if let Some(tags) = self.entries.get(elem) {
            for tag in tags {
                self.tombstones
                    .entry(tag.clone())
                    .and_modify(|ts| *ts = (*ts).max(timestamp_ms))
                    .or_insert(timestamp_ms);
            }
        }
    }

    /// Checks if the element is currently present in the set.
    ///
    /// An element is present if it has at least one tag that is not tombstoned.
    #[must_use]
    pub fn contains(&self, elem: &T) -> bool {
        self.entries
            .get(elem)
            .is_some_and(|tags| tags.iter().any(|tag| !self.tombstones.contains_key(tag)))
    }

    /// Returns all elements currently present in the set.
    #[must_use]
    pub fn read(&self) -> BTreeSet<T> {
        self.entries
            .iter()
            .filter(|(_, tags)| tags.iter().any(|tag| !self.tombstones.contains_key(tag)))
            .map(|(elem, _)| elem.clone())
            .collect()
    }

    /// Prunes tombstones that were recorded strictly before `cutoff_ms`.
    ///
    /// This cleans up both the tombstone map and dead tags from `entries`.
    pub fn prune(&mut self, cutoff_ms: i64) {
        let expired_tags: BTreeSet<String> = self
            .tombstones
            .iter()
            .filter(|(_, &ts)| ts < cutoff_ms)
            .map(|(tag, _)| tag.clone())
            .collect();

        if expired_tags.is_empty() {
            return;
        }

        for tags in self.entries.values_mut() {
            tags.retain(|tag| !expired_tags.contains(tag));
        }
        self.entries.retain(|_, tags| !tags.is_empty());
        self.tombstones.retain(|tag, _| !expired_tags.contains(tag));
    }
}

impl<T: Ord + Clone> CvRdt for OrSet<T> {
    fn merge(&mut self, other: &Self) {
        for (elem, other_tags) in &other.entries {
            let self_tags = self.entries.entry(elem.clone()).or_default();
            self_tags.extend(other_tags.iter().cloned());
        }
        for (tag, other_ts) in &other.tombstones {
            self.tombstones
                .entry(tag.clone())
                .and_modify(|ts| *ts = (*ts).max(*other_ts))
                .or_insert(*other_ts);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_or_set_add_remove() {
        let mut set = OrSet::new();
        assert!(!set.contains(&"item-1"));

        set.add("device-1", "item-1");
        assert!(set.contains(&"item-1"));
        assert_eq!(set.read(), BTreeSet::from(["item-1"]));

        set.remove(&"item-1", 100);
        assert!(!set.contains(&"item-1"));
        assert!(set.read().is_empty());
    }

    #[test]
    fn test_or_set_add_wins_on_concurrent_remove() {
        let mut replica_a = OrSet::new();
        let mut replica_b = OrSet::new();

        // 1. Initial state: item-1 is added and synced to both replicas
        replica_a.add("node-a", "item-1");
        replica_b.merge(&replica_a);
        assert!(replica_b.contains(&"item-1"));

        // 2. Offline partition:
        // Replica A removes item-1 (observing only the tag from step 1)
        replica_a.remove(&"item-1", 200);
        assert!(!replica_a.contains(&"item-1"));

        // Concurrently, Replica B re-adds item-1 (generating a new tag)
        replica_b.add("node-b", "item-1");
        assert!(replica_b.contains(&"item-1"));

        // 3. Merging B into A -> Add-Wins: the newly generated tag on B was never observed by A's remove
        replica_a.merge(&replica_b);
        assert!(
            replica_a.contains(&"item-1"),
            "Add must win over concurrent remove"
        );
        assert!(replica_a.read().contains(&"item-1"));

        // Symmetric check
        replica_b.merge(&replica_a);
        assert!(replica_b.contains(&"item-1"));
        assert_eq!(replica_a, replica_b);
    }

    #[test]
    fn test_or_set_causal_remove() {
        let mut replica_a = OrSet::new();
        let mut replica_b = OrSet::new();

        // Replica A adds item-1
        replica_a.add("node-a", "item-1");

        // Sync to Replica B
        replica_b.merge(&replica_a);

        // Replica B removes item-1 causally AFTER receiving it
        replica_b.remove(&"item-1", 300);

        // Sync back to Replica A
        replica_a.merge(&replica_b);

        // Both must agree item-1 is removed
        assert!(!replica_a.contains(&"item-1"));
        assert!(!replica_b.contains(&"item-1"));
    }

    #[test]
    fn test_or_set_pruning() {
        let mut set = OrSet::new();
        set.add("node-a", "x");
        set.remove(&"x", 100);

        set.add("node-a", "y");
        set.remove(&"y", 200);

        set.add("node-a", "z");

        assert_eq!(set.tombstones.len(), 2);

        // Prune before 150 -> x tombstone removed, y tombstone kept
        set.prune(150);
        assert_eq!(set.tombstones.len(), 1);
        assert!(!set.contains(&"y"));
        assert!(set.contains(&"z"));

        // Prune before 250 -> all tombstones pruned
        set.prune(250);
        assert!(set.tombstones.is_empty());
        assert!(set.contains(&"z"));
    }

    #[test]
    fn test_or_set_semilattice_properties() {
        let mut a = OrSet::new();
        a.add("dev-1", "alpha");
        a.add("dev-1", "beta");

        let mut b = OrSet::new();
        b.add("dev-2", "beta");
        b.add("dev-2", "gamma");

        let mut c = OrSet::new();
        c.add("dev-3", "delta");

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
        assert_eq!(ab.read(), ba.read());

        // Associativity
        let mut ab_c = ab.clone();
        ab_c.merge(&c);

        let mut bc = b.clone();
        bc.merge(&c);
        let mut a_bc = a.clone();
        a_bc.merge(&bc);

        assert_eq!(ab_c, a_bc);
        assert_eq!(ab_c.read(), a_bc.read());
    }
}
