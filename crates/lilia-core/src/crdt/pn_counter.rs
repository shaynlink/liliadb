use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::CvRdt;

/// A Positive-Negative Counter (PN-Counter) CRDT.
///
/// Enables distributed counting across multiple offline actors where each actor
/// can increment and decrement independently. Merging is monotonic, commutative,
/// associative, and idempotent.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq))]
pub struct PNCounter {
    /// Cumulative increments per actor.
    pub p: BTreeMap<String, u64>,
    /// Cumulative decrements per actor.
    pub n: BTreeMap<String, u64>,
}

impl PNCounter {
    /// Creates a new, empty `PNCounter` with a value of 0.
    #[must_use]
    pub fn new() -> Self {
        Self {
            p: BTreeMap::new(),
            n: BTreeMap::new(),
        }
    }

    /// Increments the counter by `amount` for the specified `actor`.
    pub fn increment(&mut self, actor: &str, amount: u64) {
        if amount == 0 {
            return;
        }
        let entry = self.p.entry(actor.to_owned()).or_insert(0);
        *entry = entry.saturating_add(amount);
    }

    /// Decrements the counter by `amount` for the specified `actor`.
    pub fn decrement(&mut self, actor: &str, amount: u64) {
        if amount == 0 {
            return;
        }
        let entry = self.n.entry(actor.to_owned()).or_insert(0);
        *entry = entry.saturating_add(amount);
    }

    /// Computes the effective value of the counter (`sum(P) - sum(N)`).
    #[must_use]
    pub fn value(&self) -> i64 {
        let p_sum: i128 = self.p.values().copied().map(i128::from).sum();
        let n_sum: i128 = self.n.values().copied().map(i128::from).sum();
        let diff = p_sum - n_sum;
        i64::try_from(diff).unwrap_or(if diff > 0 { i64::MAX } else { i64::MIN })
    }
}

impl CvRdt for PNCounter {
    fn merge(&mut self, other: &Self) {
        for (actor, other_p) in &other.p {
            let self_p = self.p.entry(actor.clone()).or_insert(0);
            *self_p = (*self_p).max(*other_p);
        }
        for (actor, other_n) in &other.n {
            let self_n = self.n.entry(actor.clone()).or_insert(0);
            *self_n = (*self_n).max(*other_n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pn_counter_basic() {
        let mut counter = PNCounter::new();
        assert_eq!(counter.value(), 0);

        counter.increment("device-1", 10);
        assert_eq!(counter.value(), 10);

        counter.decrement("device-1", 3);
        assert_eq!(counter.value(), 7);

        counter.decrement("device-2", 12);
        assert_eq!(counter.value(), -5);
    }

    #[test]
    fn test_pn_counter_offline_merge() {
        let mut replica1 = PNCounter::new();
        let mut replica2 = PNCounter::new();

        replica1.increment("node-a", 15);
        replica1.decrement("node-a", 5);

        replica2.increment("node-b", 20);
        replica2.decrement("node-b", 8);

        // Before merge: replica1 has 10, replica2 has 12
        assert_eq!(replica1.value(), 10);
        assert_eq!(replica2.value(), 12);

        // Merge replica2 into replica1
        replica1.merge(&replica2);
        // Expected value: 10 + 12 = 22
        assert_eq!(replica1.value(), 22);

        // Further offline mutations
        replica1.decrement("node-a", 2); // 20
        replica2.increment("node-b", 5); // 17

        replica2.merge(&replica1);
        assert_eq!(replica2.value(), 25);
    }

    #[test]
    fn test_pn_counter_semilattice_properties() {
        let mut a = PNCounter::new();
        a.increment("x", 10);
        a.decrement("x", 2);

        let mut b = PNCounter::new();
        b.increment("y", 7);
        b.decrement("x", 4);

        let mut c = PNCounter::new();
        c.increment("z", 100);
        c.decrement("y", 3);

        // Idempotence: merge(A, A) == A
        let mut a_copy = a.clone();
        a_copy.merge(&a);
        assert_eq!(a_copy, a);

        // Commutativity: merge(A, B) == merge(B, A)
        let mut ab = a.clone();
        ab.merge(&b);

        let mut ba = b.clone();
        ba.merge(&a);

        assert_eq!(ab, ba);
        assert_eq!(ab.value(), ba.value());

        // Associativity: merge(merge(A, B), C) == merge(A, merge(B, C))
        let mut ab_c = ab.clone();
        ab_c.merge(&c);

        let mut bc = b.clone();
        bc.merge(&c);
        let mut a_bc = a.clone();
        a_bc.merge(&bc);

        assert_eq!(ab_c, a_bc);
        assert_eq!(ab_c.value(), a_bc.value());
    }
}
