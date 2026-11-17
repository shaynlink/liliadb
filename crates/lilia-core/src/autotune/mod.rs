//! Auto-tuning engine building blocks: workload statistics, index synthesis and pruning.
//!
//! This module is storage agnostic. It records what queries do ([`AutotuneCollector`]), decides
//! which indexes should exist ([`plan`]) and describes the outcome ([`AutotuneReport`]). The
//! storage engine owns the scheduling and applies the recommendations.

mod collector;
mod fingerprint;
mod options;
mod planner;
mod report;
mod sql;
mod stats;

pub use collector::{AutotuneCollector, IndexLookups};
pub use fingerprint::{
    composite_expression, index_fields, index_name, index_name_hashed, is_indexable_field,
    normalize_field, QueryFingerprint, MAX_COMPOSITE_FIELDS,
};
pub use options::{AutotuneOptions, PruneScope};
pub use planner::{plan, IndexOrigin, IndexPhase, Recommendation, TrackedIndex};
pub use report::{
    AutotuneReport, CycleReport, IndexRef, IndexUsageView, JsonQueryReport, SqlQueryReport,
};
pub use sql::normalize_sql;
pub use stats::{QueryObservation, QueryStats, SqlObservation, SqlQueryStats};
