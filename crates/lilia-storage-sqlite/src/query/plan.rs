//! Query execution plan definitions and stage representations for `LiliaQL`.

use lilia_core::aggregation::FilterExpr;
use lilia_core::{Direction, VectorElementType, VectorMetric};

use super::projection::Projection;
use super::types::RrfConfig;

/// Base data model source for a `LiliaQL` pipeline.
#[derive(Debug, Clone, PartialEq)]
pub enum QuerySource {
    /// Starting from JSON documents in a collection/space.
    Json { space: String },
    /// Starting from nodes in a graph.
    Graph { graph: String },
    /// Starting from full-text BM25 search.
    Fts { collection: String, query: String },
    /// Starting from dense vector similarity.
    Vector {
        collection: String,
        vector: Vec<u8>,
        dim: u32,
        element_type: VectorElementType,
    },
    /// Starting from raw SQL query.
    Sql { query: String },
    /// Starting from key-value namespace.
    Kv { namespace: String },
}

/// Stage representing graph edge traversal.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphTraversalStage {
    pub graph: Option<String>,
    pub kind: String,
    pub direction: Direction,
    pub max_hops: usize,
}

/// Stage representing full-text lexical search and ranking.
#[derive(Debug, Clone, PartialEq)]
pub struct FtsSearchStage {
    pub collection: String,
    pub query: String,
}

/// Stage representing dense vector KNN search and threshold filtering.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorSearchStage {
    pub collection: String,
    pub vector: Vec<u8>,
    pub dim: u32,
    pub element_type: VectorElementType,
    pub metric: VectorMetric,
    pub threshold: f64,
}

/// Logical plan holding the composed pipeline stages before optimization and execution.
#[derive(Debug, Clone, PartialEq)]
pub struct LogicalQueryPlan {
    pub source: QuerySource,
    pub filter: Option<FilterExpr>,
    pub graph_traversal: Option<GraphTraversalStage>,
    pub fts_search: Option<FtsSearchStage>,
    pub vector_search: Option<VectorSearchStage>,
    pub rrf: RrfConfig,
    pub projection: Projection,
    pub order_by: Option<(String, lilia_core::SortDirection)>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub distinct: bool,
}

impl LogicalQueryPlan {
    /// Creates a new logical plan starting from a given source.
    #[must_use]
    pub fn new(source: QuerySource) -> Self {
        Self {
            source,
            filter: None,
            graph_traversal: None,
            fts_search: None,
            vector_search: None,
            rrf: RrfConfig::default(),
            projection: Projection::new(),
            order_by: None,
            limit: None,
            offset: None,
            distinct: false,
        }
    }
}
