//! Property graphs: nodes with a kind, JSON metadata and an optional link to a document, and
//! directed, weighted edges between node ids.
//!
//! Each edge is stored twice in one batch: under its source (`source\0kind\0target`) for
//! outbound walks and under its target (`target\0kind\0source`) for inbound ones, so neighbors
//! in either direction are one prefix scan. Edges do not require their nodes to exist; deleting
//! a node deletes the edges it is an end of.

mod handle;
mod plan;
mod store;

use serde_json::Value;

pub use handle::Graph;
pub(crate) use plan::overlay;
pub(crate) use store::{edge_identity, edge_key, node_key};

/// A graph write, for a [`crate::Batch`]; see [`Graph`] for each operation.
#[derive(Debug, Clone, PartialEq)]
pub enum GraphWrite {
    SetNode {
        graph: String,
        id: String,
        kind: String,
        metadata: Value,
        document: Option<(String, String)>,
    },
    DeleteNode {
        graph: String,
        id: String,
    },
    AddEdge {
        graph: String,
        source: String,
        target: String,
        kind: String,
        weight: f64,
    },
    RemoveEdge {
        graph: String,
        source: String,
        target: String,
        kind: String,
    },
}
