use serde::{Deserialize, Serialize};

use super::planner::{IndexOrigin, IndexPhase, Recommendation};
use super::stats::{QueryStats, SqlQueryStats};

/// Reference to an index of a collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexRef {
    pub space: String,
    pub index_name: String,
}

/// Statistics of one JSON query shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonQueryReport {
    pub space: String,
    pub fields: Vec<String>,
    pub stats: QueryStats,
}

/// Statistics of one normalized SQL statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqlQueryReport {
    pub sql: String,
    pub stats: SqlQueryStats,
}

/// Usage and footprint of one JSON index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexUsageView {
    pub space: String,
    pub index_name: String,
    pub expression: String,
    pub origin: IndexOrigin,
    pub phase: IndexPhase,
    /// Entries stored for this index.
    pub entries: u64,
    /// Approximate storage taken by those entries.
    pub approx_bytes: u64,
    pub lookups: u64,
    pub last_used_ms: Option<i64>,
    pub first_seen_ms: i64,
    /// Idle for longer than the configured window.
    pub zombie: bool,
}

/// Snapshot of what the auto-tuner has observed and what it would do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutotuneReport {
    pub enabled: bool,
    pub generated_at_ms: i64,
    pub json_queries: Vec<JsonQueryReport>,
    pub sql_queries: Vec<SqlQueryReport>,
    pub indexes: Vec<IndexUsageView>,
    /// Bytes of payload held by the shared JSON index table (from `dbstat`).
    pub index_table_bytes: u64,
    pub recommendations: Vec<Recommendation>,
    /// Outcome of the most recent tuning cycle of this process, if any.
    pub last_cycle: Option<CycleReport>,
}

/// Outcome of one tuning cycle.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CycleReport {
    pub at_ms: i64,
    pub created: Vec<IndexRef>,
    pub dropped: Vec<IndexRef>,
    /// Recommendations that were not applied because the matching option is off.
    pub pending: Vec<Recommendation>,
    /// Actions that failed, with their error message.
    pub failures: Vec<String>,
}
