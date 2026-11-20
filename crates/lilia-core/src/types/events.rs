//! Real-time database mutation events and HLC tracking outcomes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DatabaseEvent {
    KvMutated {
        namespace: String,
        #[serde(with = "serde_bytes")]
        key: Vec<u8>,
        version: Option<u64>,
        deleted: bool,
    },
    JsonMutated {
        space: String,
        id: String,
        version: Option<u64>,
        deleted: bool,
    },
    BitemporalMutated {
        space: String,
        id: String,
        vt_from: Option<i64>,
        vt_to: Option<i64>,
        deleted: bool,
    },
    GraphNodeMutated {
        graph: String,
        id: String,
        deleted: bool,
    },
    GraphEdgeMutated {
        graph: String,
        source: String,
        target: String,
        kind: String,
        deleted: bool,
    },
    FtsMutated {
        collection: String,
        doc_id: String,
        deleted: bool,
    },
    VectorMutated {
        collection: String,
        id: String,
        deleted: bool,
    },
    CrdtCounterMutated {
        space: String,
        id: String,
        value: i64,
    },
    CrdtSetMutated {
        space: String,
        id: String,
        element: String,
        deleted: bool,
    },
    CrdtJsonMutated {
        space: String,
        id: String,
        version: Option<u64>,
    },
    ConflictDetected {
        model: String,
        space: String,
        id: String,
        base_hlc: Option<String>,
        ours_hlc: String,
        theirs_hlc: String,
        resolution_strategy: String,
    },
    TierChanged {
        kind: crate::CollectionKind,
        name: String,
        tier: crate::StorageTier,
    },
    RingBufferFlushed {
        ops_committed: u64,
        coalesced_ts_points: u64,
        latency_us: u64,
    },
}

impl DatabaseEvent {
    #[must_use]
    pub fn matches_filter(&self, filter: Option<&str>) -> bool {
        let Some(filter) = filter else { return true };
        if filter.is_empty() || filter == "*" {
            return true;
        }
        match self {
            Self::KvMutated { namespace, .. } => namespace == filter,
            Self::JsonMutated { space, .. }
            | Self::BitemporalMutated { space, .. }
            | Self::CrdtCounterMutated { space, .. }
            | Self::CrdtSetMutated { space, .. }
            | Self::CrdtJsonMutated { space, .. }
            | Self::ConflictDetected { space, .. } => space == filter,
            Self::GraphNodeMutated { graph, .. } | Self::GraphEdgeMutated { graph, .. } => {
                graph == filter
            }
            Self::FtsMutated { collection, .. } | Self::VectorMutated { collection, .. } => {
                collection == filter
            }
            Self::TierChanged { name, .. } => name == filter,
            Self::RingBufferFlushed { .. } => true,
        }
    }
}

/// Metadata representing an HLC-tracked record in persistent storage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq))]
pub struct HlcRecord {
    pub model: String,
    pub space: String,
    pub id: String,
    pub hlc: crate::hlc::Hlc,
    pub parent_hlc: Option<crate::hlc::Hlc>,
    pub node_id: String,
    pub updated_at_ms: i64,
}

/// Detailed outcome of an HLC-tracked mutation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct HlcMutationOutcome {
    pub hlc: crate::hlc::Hlc,
    pub version: u64,
    pub causal_relation: crate::hlc::CausalRelation,
    pub conflict_detected: bool,
    pub conflict_id: Option<i64>,
    pub resolved_by: Option<crate::hlc::ConflictResolutionStrategy>,
}
