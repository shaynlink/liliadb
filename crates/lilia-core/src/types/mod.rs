//! Public data transfer objects, mutations, and domain types for Lilia.

mod entry;
mod events;
mod fts;
mod graph;
mod index;
mod stats;
mod vector;

pub use entry::{BatchOperation, BitemporalEntry, Durability, JsonEntry, KvEntry, MutationResult};
pub use events::{DatabaseEvent, HlcMutationOutcome, HlcRecord};
pub use fts::{FtsFuzzyHit, FtsFuzzyOptions, FtsHit, FtsSuggestion};
pub use graph::{
    CentralityType, ComponentType, CypherResult, Direction, GraphEdge, GraphNode, NodeScore,
    PageRankConfig, PathEdge, WeightedPath,
};
pub use index::{
    IndexOptions, JsonIndexInfo, JsonRevisionInfo, LiveQueryDiff, MaterializedViewInfo, RedoResult,
    SqlQueryResult, UndoEntry, UndoResult, Version,
};
pub use stats::{
    BranchDiff, CheckpointStats, DictStats, ImpactAnalysis, MergeResult, MergeStrategy,
    ResourceProfile, TableDiff, TableStorageStat,
};
pub use vector::{
    HnswConfig, HybridHit, VectorElementType, VectorHit, VectorIndexInfo, VectorMetric,
};
