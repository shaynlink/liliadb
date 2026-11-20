//! Graph data types, edges, nodes, and traversal/analytics configurations.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const fn default_edge_weight() -> f64 {
    1.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq))]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Outbound,
    Inbound,
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct GraphNode {
    pub graph: String,
    pub id: String,
    pub kind: String,
    #[rkyv(with = crate::zero_copy::AsJsonString)]
    pub metadata: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_space: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct GraphEdge {
    pub graph: String,
    pub source: String,
    pub target: String,
    pub kind: String,
    #[serde(default = "default_edge_weight")]
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct PathEdge {
    pub source: String,
    pub target: String,
    pub kind: String,
    #[serde(default = "default_edge_weight")]
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct WeightedPath {
    pub nodes: Vec<String>,
    pub total_weight: f64,
    pub edges: Vec<PathEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct NodeScore {
    pub node: String,
    pub score: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq))]
#[serde(rename_all = "snake_case")]
pub enum CentralityType {
    Betweenness,
    Closeness,
    Degree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq))]
#[serde(rename_all = "snake_case")]
pub enum ComponentType {
    StronglyConnected,
    WeaklyConnected,
}

/// Tabular output returned by embedded Cypher queries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct CypherResult {
    pub columns: Vec<String>,
    #[rkyv(with = rkyv::with::Map<rkyv::with::Map<crate::zero_copy::AsJsonString>>)]
    pub rows: Vec<Vec<Value>>,
}

impl CypherResult {
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut list = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            let mut map = serde_json::Map::new();
            for (col, val) in self.columns.iter().zip(row.iter()) {
                map.insert(col.clone(), val.clone());
            }
            list.push(Value::Object(map));
        }
        Value::Array(list)
    }

    #[must_use]
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

const fn default_pagerank_damping() -> f64 {
    0.85
}

const fn default_pagerank_iterations() -> usize {
    50
}

const fn default_pagerank_tolerance() -> f64 {
    1e-6
}

const fn default_pagerank_direction() -> Direction {
    Direction::Outbound
}

/// Configuration for weighted / personalized `PageRank` graph computation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct PageRankConfig {
    #[serde(default = "default_pagerank_damping")]
    pub damping_factor: f64,
    #[serde(default = "default_pagerank_iterations")]
    pub max_iterations: usize,
    #[serde(default = "default_pagerank_tolerance")]
    pub tolerance: f64,
    pub seeds: Option<Vec<String>>,
    #[serde(default = "default_pagerank_direction")]
    pub direction: Direction,
    pub edge_kind: Option<String>,
}

impl Default for PageRankConfig {
    fn default() -> Self {
        Self {
            damping_factor: 0.85,
            max_iterations: 50,
            tolerance: 1e-6,
            seeds: None,
            direction: Direction::Outbound,
            edge_kind: None,
        }
    }
}
