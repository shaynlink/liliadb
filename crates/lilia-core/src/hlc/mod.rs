//! Hybrid Logical Clocks (HLC) & Concurrency Detection without Wall Clock dependency.
//!
//! Based on Kulkarni et al. (2014): "Logical Physical Clocks and Consistent Snapshots
//! in Globally Distributed Databases".
//!
//! Guarantees:
//! 1. Absolute Causal Ordering: If event B was created after observing event A, then
//!    `HLC(B) > HLC(A)` mathematically, even if B's physical clock is 10 minutes behind.
//! 2. Deterministic Concurrency Detection: Exactly distinguishes whether two mutations
//!    are sequential (A -> B or B -> A) or concurrent (A || B).
//! 3. 3-Way Merge & Automatic Conflict Resolution without silent write loss.

mod causality;
mod clock;
mod merge;
mod timestamp;

#[cfg(test)]
mod tests;

pub use causality::{detect_causality, CausalRelation};
pub use clock::{HybridClock, DEFAULT_MAX_DRIFT_MS};
pub use merge::{
    three_way_merge, ConflictRecord, ConflictResolutionStrategy, FieldConflict, ThreeWayMergeResult,
};
pub use timestamp::{now_ms, Hlc, HlcError};
