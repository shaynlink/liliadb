//! Public data transfer objects, mutations, and domain types for Lilia.

mod entry;
mod fts;
mod graph;
mod index;
mod stats;
mod vector;

pub use entry::{
    ArchivedBatchOperation, ArchivedMutationResult, BatchOperation, BitemporalEntry, Durability,
    JsonEntry, KvEntry, MutationResult,
};
pub use fts::{FtsFuzzyHit, FtsHit, FtsPhoneticHit, FtsSuggestion};
pub use graph::{
    CentralityType, ComponentType, CypherResult, Direction, GraphEdge, GraphNode, NodeScore,
    PageRankConfig, PathEdge, WeightedPath,
};
pub use index::{IndexOptions, JsonIndexInfo, SqlQueryResult, Version};
pub use stats::{ImpactAnalysis, ResourceProfile};
pub use vector::{
    HnswConfig, HybridHit, VectorElementType, VectorHit, VectorIndexInfo, VectorMetric,
};
