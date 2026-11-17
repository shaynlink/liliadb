use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use super::CvRdt;

/// A Lamport / Hybrid Logical Clock counter for deterministic event ordering.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct LogicalClock {
    /// Physical or logical timestamp in milliseconds.
    pub timestamp_ms: i64,
    /// Monotonic sequence counter for disambiguating events within the same millisecond.
    pub counter: u32,
}

impl LogicalClock {
    /// Creates a new `LogicalClock` with the specified timestamp and counter.
    #[must_use]
    pub const fn new(timestamp_ms: i64, counter: u32) -> Self {
        Self {
            timestamp_ms,
            counter,
        }
    }

    /// Increments the counter for a new event occurring at the same timestamp.
    #[must_use]
    pub fn tick(&self, now_ms: i64) -> Self {
        if now_ms > self.timestamp_ms {
            Self::new(now_ms, 0)
        } else {
            Self::new(self.timestamp_ms, self.counter.saturating_add(1))
        }
    }
}

/// A composite clock including an actor identifier for deterministic total ordering.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LwwClock {
    /// The logical clock.
    pub clock: LogicalClock,
    /// The actor or replica identifier used for tie-breaking.
    pub actor_id: String,
}

impl LwwClock {
    /// Creates a new `LwwClock`.
    #[must_use]
    pub fn new(clock: LogicalClock, actor_id: impl Into<String>) -> Self {
        Self {
            clock,
            actor_id: actor_id.into(),
        }
    }

    /// Convenience helper to create a clock with a millisecond timestamp, 0 counter, and actor ID.
    #[must_use]
    pub fn at(timestamp_ms: i64, actor_id: impl Into<String>) -> Self {
        Self::new(LogicalClock::new(timestamp_ms, 0), actor_id)
    }
}

impl PartialOrd for LwwClock {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LwwClock {
    fn cmp(&self, other: &Self) -> Ordering {
        self.clock
            .cmp(&other.clock)
            .then_with(|| self.actor_id.cmp(&other.actor_id))
    }
}

/// A Last-Write-Wins Register (LWW-Register) CRDT.
///
/// Holds a single value and an associated logical clock. Merging resolves concurrently
/// written values deterministically in favor of the highest clock (using actor ID for ties).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LwwRegister<T> {
    /// The current stored value.
    pub value: T,
    /// The clock of the last winning write.
    pub clock: LwwClock,
}

impl<T: Clone> LwwRegister<T> {
    /// Creates a new `LwwRegister` with the given value and clock.
    #[must_use]
    pub fn new(value: T, clock: LwwClock) -> Self {
        Self { value, clock }
    }

    /// Sets a new value if the provided `clock` is strictly greater than the register's current clock.
    pub fn set(&mut self, value: T, clock: LwwClock) {
        if clock > self.clock {
            self.value = value;
            self.clock = clock;
        }
    }
}

impl<T: Clone> CvRdt for LwwRegister<T> {
    fn merge(&mut self, other: &Self) {
        if other.clock > self.clock {
            self.value = other.value.clone();
            self.clock = other.clock.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lww_clock_ordering() {
        let c1 = LwwClock::at(100, "node-a");
        let c2 = LwwClock::at(200, "node-a");
        assert!(c2 > c1);

        // Same timestamp, different counter
        let c3 = LwwClock::new(LogicalClock::new(100, 1), "node-a");
        assert!(c3 > c1);

        // Same timestamp and counter, tie-break on actor_id
        let c4 = LwwClock::at(100, "node-b");
        assert!(c4 > c1); // "node-b" > "node-a"
    }

    #[test]
    fn test_lww_register_set_and_merge() {
        let mut reg_a = LwwRegister::new("initial", LwwClock::at(100, "dev-1"));
        reg_a.set("update-1", LwwClock::at(150, "dev-1"));
        assert_eq!(reg_a.value, "update-1");

        // Older write is ignored
        reg_a.set("stale", LwwClock::at(120, "dev-1"));
        assert_eq!(reg_a.value, "update-1");

        // Another replica with a higher clock
        let reg_b = LwwRegister::new("winner", LwwClock::at(200, "dev-2"));
        reg_a.merge(&reg_b);
        assert_eq!(reg_a.value, "winner");
        assert_eq!(reg_a.clock.actor_id, "dev-2");
    }

    #[test]
    fn test_lww_register_semilattice_properties() {
        let a = LwwRegister::new(10, LwwClock::at(100, "a"));
        let b = LwwRegister::new(20, LwwClock::at(150, "b"));
        let c = LwwRegister::new(30, LwwClock::at(120, "c"));

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
        assert_eq!(ab.value, 20);

        // Associativity
        let mut ab_c = ab.clone();
        ab_c.merge(&c);

        let mut bc = b.clone();
        bc.merge(&c);
        let mut a_bc = a.clone();
        a_bc.merge(&bc);

        assert_eq!(ab_c, a_bc);
        assert_eq!(ab_c.value, 20);
    }
}
