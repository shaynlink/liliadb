pub mod lww_json;
pub mod lww_register;
pub mod or_set;
pub mod pn_counter;

pub use lww_json::{LwwField, LwwJsonDocument};
pub use lww_register::{LogicalClock, LwwClock, LwwRegister};
pub use or_set::OrSet;
pub use pn_counter::PNCounter;

/// State-based Conflict-Free Replicated Data Type (`CvRDT`).
///
/// Implementors form a join-semilattice with a merge function that is:
/// - Commutative: `a.merge(b) == b.merge(a)`
/// - Associative: `(a.merge(b)).merge(c) == a.merge(b.merge(c))`
/// - Idempotent: `a.merge(a) == a`
pub trait CvRdt {
    /// Merges another replica's state into `self`.
    fn merge(&mut self, other: &Self);
}
