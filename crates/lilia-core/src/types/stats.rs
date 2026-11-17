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
