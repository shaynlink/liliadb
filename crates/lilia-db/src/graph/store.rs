//! Keys and records of the graph keyspaces.

use std::collections::HashSet;

use bytes::Bytes;
use lilia_core::GraphNode;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{corruption, Result};
use crate::keys::{self, validate_name};

#[derive(Debug, Serialize, Deserialize)]
struct StoredNode {
    kind: String,
    metadata: Value,
    #[serde(default)]
    document: Option<(String, String)>,
}

pub(super) const NODE_FORMAT: u8 = 1;
pub(super) const EDGE_FORMAT: u8 = 1;

pub(crate) fn node_key(graph: &str, id: &str) -> Bytes {
    let mut key = keys::collection(keys::GRAPH_NODE, graph);
    key.extend_from_slice(id.as_bytes());
    key.into()
}

/// Prefix of the edges of `node` in one direction's keyspace: `tag graph\0 node\0`.
pub(super) fn adjacency(tag: u8, graph: &str, node: &str) -> Vec<u8> {
    let mut key = keys::collection(tag, graph);
    key.extend_from_slice(node.as_bytes());
    key.push(0);
    key
}

pub(crate) fn edge_key(tag: u8, graph: &str, from: &str, kind: &str, to: &str) -> Bytes {
    let mut key = adjacency(tag, graph, from);
    key.extend_from_slice(kind.as_bytes());
    key.push(0);
    key.extend_from_slice(to.as_bytes());
    key.into()
}

/// `(kind, other end)` of an edge key, after its adjacency prefix.
pub(super) fn split_edge(rest: &[u8]) -> Result<(String, String)> {
    let split = rest
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| corruption("edge key without a kind"))?;
    let text = |bytes: &[u8]| {
        String::from_utf8(bytes.to_vec()).map_err(|_| corruption("edge key is not UTF-8"))
    };
    Ok((text(&rest[..split])?, text(&rest[split + 1..])?))
}

/// Ledger id of an edge.
pub(super) fn edge_id(source: &str, kind: &str, target: &str) -> String {
    format!("{source}{sep}{kind}{sep}{target}", sep = keys::ID_SEPARATOR)
}

/// Ledger id of the record at `rest` (an outbound edge key after `graph\0`).
pub(crate) fn edge_identity(rest: &[u8]) -> Option<String> {
    let split = rest.iter().position(|&b| b == 0)?;
    let source = std::str::from_utf8(&rest[..split]).ok()?;
    let (kind, target) = split_edge(&rest[split + 1..]).ok()?;
    Some(edge_id(source, &kind, &target))
}

pub(super) fn encode_node(
    kind: &str,
    metadata: &Value,
    document: Option<&(String, String)>,
) -> Result<Bytes> {
    let stored = StoredNode {
        kind: kind.to_owned(),
        metadata: metadata.clone(),
        document: document.cloned(),
    };
    let mut out = vec![NODE_FORMAT];
    serde_json::to_writer(&mut out, &stored)
        .map_err(|e| corruption(format!("node encoding: {e}")))?;
    Ok(out.into())
}

pub(super) fn decode_node(graph: &str, id: &str, raw: &[u8]) -> Result<GraphNode> {
    let Some((&NODE_FORMAT, body)) = raw.split_first() else {
        return Err(corruption("graph node with an unknown format"));
    };
    let stored: StoredNode =
        serde_json::from_slice(body).map_err(|e| corruption(format!("graph node: {e}")))?;
    let (doc_space, doc_id) = stored
        .document
        .map_or((None, None), |(space, id)| (Some(space), Some(id)));
    Ok(GraphNode {
        graph: graph.to_owned(),
        id: id.to_owned(),
        kind: stored.kind,
        metadata: stored.metadata,
        doc_space,
        doc_id,
    })
}

pub(super) fn encode_weight(weight: f64) -> Bytes {
    let mut out = vec![EDGE_FORMAT];
    out.extend_from_slice(&weight.to_le_bytes());
    out.into()
}

pub(super) fn decode_weight(raw: &[u8]) -> Result<f64> {
    match raw {
        [EDGE_FORMAT, rest @ ..] => {
            let bytes: [u8; 8] = rest
                .try_into()
                .map_err(|_| corruption("edge weight is not 8 bytes"))?;
            Ok(f64::from_le_bytes(bytes))
        }
        _ => Err(corruption("graph edge with an unknown format")),
    }
}

pub(super) fn validate_edge(graph: &str, source: &str, target: &str, kind: &str) -> Result<()> {
    validate_name("graph", graph)?;
    validate_name("edge source", source)?;
    validate_name("edge target", target)?;
    validate_name("edge kind", kind)
}

/// The edges `node` is an end of, as the batch or transaction sees them: outbound keys, and the
/// inbound keys with their outbound mirrors. Each edge once, even a self-loop.
pub(super) fn incident(
    graph: &str,
    node: &str,
    outbound: &[Bytes],
    inbound: &[Bytes],
) -> Result<Vec<(Bytes, Bytes)>> {
    let mut seen = HashSet::new();
    let mut edges = Vec::new();
    let out_prefix = adjacency(keys::EDGE_OUT, graph, node);
    for key in outbound {
        let (kind, target) = split_edge(keys::suffix(key, &out_prefix))?;
        if seen.insert(key.clone()) {
            edges.push((
                key.clone(),
                edge_key(keys::EDGE_IN, graph, &target, &kind, node),
            ));
        }
    }
    let in_prefix = adjacency(keys::EDGE_IN, graph, node);
    for key in inbound {
        let (kind, source) = split_edge(keys::suffix(key, &in_prefix))?;
        let out = edge_key(keys::EDGE_OUT, graph, &source, &kind, node);
        if seen.insert(out.clone()) {
            edges.push((out, key.clone()));
        }
    }
    Ok(edges)
}

pub(super) fn prefix_bounds(prefix: &[u8]) -> (Vec<u8>, Vec<u8>) {
    (
        prefix.to_vec(),
        keys::prefix_end(prefix).unwrap_or_default(),
    )
}
