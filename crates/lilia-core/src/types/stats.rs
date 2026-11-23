//! The resource profile of a database and graph impact summaries.

use serde::{Deserialize, Serialize};

/// The memory and cache budget a database is sized for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq))]
#[serde(rename_all = "snake_case")]
pub enum ResourceProfile {
    #[default]
    Server,
    Mobile,
    Constrained,
}

/// Transitive graph dependency impact summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq))]
pub struct ImpactAnalysis {
    pub direct: Vec<String>,
    pub dependants: Vec<String>,
    pub total_affected: usize,
}

