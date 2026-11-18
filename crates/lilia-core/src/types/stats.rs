//! System resource, checkpoint, and storage compression statistics.

use serde::{Deserialize, Serialize};

/// Resource footprint profile configuring SQLite cache, mmap, WAL checkpoints, and retention defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceProfile {
    #[default]
    Server,
    Mobile,
    Constrained,
}

/// Background WAL checkpoint worker counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointStats {
    pub attempts: u64,
    pub incomplete: u64,
    pub log_frames: i64,
    pub checkpointed_frames: i64,
    pub last_error: Option<crate::ErrorCode>,
}

/// Physical page and payload storage statistics for a single SQLite B-tree/table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableStorageStat {
    pub name: String,
    pub page_count: i64,
    pub payload_bytes: i64,
    pub unused_bytes: i64,
}

/// Metadata and compression stats for a trained Zstandard JSON dictionary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictStats {
    pub space: String,
    pub dict_id: u32,
    pub sample_count: usize,
    pub dict_size: usize,
    pub created_at_ms: i64,
}

/// Transitive graph dependency impact summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImpactAnalysis {
    pub direct: Vec<String>,
    pub dependants: Vec<String>,
    pub total_affected: usize,
}

/// Conflict resolution and merge strategy when merging a branch into the main database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    /// Only merge if the main database has not had conflicting modifications
    /// since the branch was created. If conflicts are detected, the merge is rejected.
    #[default]
    FastForward,
    /// Overwrite destination rows in the main database with the branch's modifications.
    Replace,
    /// Keep destination rows in the main database in case of conflict, only applying non-conflicting changes.
    Omit,
    /// Abort changeset application immediately upon encountering any conflict.
    AbortOnConflict,
}

/// Result of merging a branch into the main database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeResult {
    pub branch_name: String,
    pub rows_merged: usize,
    pub conflicts_detected: usize,
    pub strategy_used: MergeStrategy,
}

/// Statistics on table-level changes recorded in a branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableDiff {
    pub table_name: String,
    pub inserts: usize,
    pub updates: usize,
    pub deletes: usize,
}

/// Summary of all changes recorded in a branch compared to its base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchDiff {
    pub branch_name: String,
    pub total_changes: usize,
    pub tables: Vec<TableDiff>,
}
