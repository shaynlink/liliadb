//! Planning of graph writes, and their effect on a transaction's view.

use std::ops::Bound;

use bytes::Bytes;

use crate::error::{corruption, invalid, Result};
use crate::keys::{self, validate_name};
use crate::read::Source;
use crate::txn::View;
use crate::write::overlay::Changes;
use crate::write::{Model, Planner};

use super::store::{
    adjacency, edge_id, edge_identity, edge_key, encode_node, encode_weight, incident, node_key,
    prefix_bounds, validate_edge,
};
use super::GraphWrite;

impl Planner<'_> {
    /// Plans one graph write.
    ///
    /// # Errors
    ///
    /// Invalid names or weight, engine read errors.
    pub(crate) fn graph(&mut self, write: &GraphWrite) -> Result<()> {
        match write {
            GraphWrite::SetNode {
                graph,
                id,
                kind,
                metadata,
                document,
            } => {
                validate_name("graph", graph)?;
                validate_name("node id", id)?;
                validate_name("node kind", kind)?;
                if let Some((space, doc_id)) = document {
                    validate_name("document space", space)?;
                    validate_name("document id", doc_id)?;
                }
                let record = encode_node(kind, metadata, document.as_ref())?;
                let op = self.write_record(
                    &node_key(graph, id),
                    Some(record),
                    (Model::GraphNode, graph, id.clone()),
                );
                self.result(Some(op), false);
            }
            GraphWrite::DeleteNode { graph, id } => {
                validate_name("graph", graph)?;
                validate_name("node id", id)?;
                let key = node_key(graph, id);
                let existed = self.current(&key)?.is_some();
                let (out_start, out_end) = prefix_bounds(&adjacency(keys::EDGE_OUT, graph, id));
                let (in_start, in_end) = prefix_bounds(&adjacency(keys::EDGE_IN, graph, id));
                let outbound: Vec<Bytes> = self
                    .live_range(&out_start, &out_end)?
                    .into_iter()
                    .map(|(k, _)| k)
                    .collect();
                let inbound: Vec<Bytes> = self
                    .live_range(&in_start, &in_end)?
                    .into_iter()
                    .map(|(k, _)| k)
                    .collect();
                for (out, mirror) in incident(graph, id, &outbound, &inbound)? {
                    let rest =
                        keys::suffix(&out, &keys::collection(keys::EDGE_OUT, graph)).to_vec();
                    let edge =
                        edge_identity(&rest).ok_or_else(|| corruption("unreadable edge key"))?;
                    self.write_record(&out, None, (Model::GraphEdge, graph, edge));
                    self.op(mirror, None);
                }
                let op = existed
                    .then(|| self.write_record(&key, None, (Model::GraphNode, graph, id.clone())));
                self.result(op, existed);
            }
            GraphWrite::AddEdge {
                graph,
                source,
                target,
                kind,
                weight,
            } => {
                validate_edge(graph, source, target, kind)?;
                if !weight.is_finite() {
                    return Err(invalid("edge weights must be finite"));
                }
                let record = encode_weight(*weight);
                let op = self.write_record(
                    &edge_key(keys::EDGE_OUT, graph, source, kind, target),
                    Some(record.clone()),
                    (Model::GraphEdge, graph, edge_id(source, kind, target)),
                );
                self.op(
                    edge_key(keys::EDGE_IN, graph, target, kind, source),
                    Some(record),
                );
                self.result(Some(op), false);
            }
            GraphWrite::RemoveEdge {
                graph,
                source,
                target,
                kind,
            } => {
                validate_edge(graph, source, target, kind)?;
                let out = edge_key(keys::EDGE_OUT, graph, source, kind, target);
                if self.current(&out)?.is_some() {
                    let op = self.write_record(
                        &out,
                        None,
                        (Model::GraphEdge, graph, edge_id(source, kind, target)),
                    );
                    self.op(edge_key(keys::EDGE_IN, graph, target, kind, source), None);
                    self.result(Some(op), true);
                } else {
                    self.result(None, false);
                }
            }
        }
        Ok(())
    }
}

/// The records a graph write changes in a transaction's view; see [`crate::write::overlay`].
///
/// # Errors
///
/// Invalid names or weight, engine read errors.
pub(crate) fn overlay(write: &GraphWrite, view: &View<'_>) -> Result<(Changes, bool)> {
    match write {
        GraphWrite::SetNode {
            graph,
            id,
            kind,
            metadata,
            document,
        } => {
            validate_name("graph", graph)?;
            validate_name("node id", id)?;
            validate_name("node kind", kind)?;
            Ok((
                vec![(
                    node_key(graph, id),
                    Some(encode_node(kind, metadata, document.as_ref())?),
                )],
                false,
            ))
        }
        GraphWrite::DeleteNode { graph, id } => {
            validate_name("graph", graph)?;
            validate_name("node id", id)?;
            let key = node_key(graph, id);
            let existed = view.peek(&key)?.is_some();
            let scan = |prefix: Vec<u8>| -> Result<Vec<Bytes>> {
                let (start, end) = prefix_bounds(&prefix);
                view.range(Bound::Included(&start), Bound::Excluded(&end))?
                    .map(|item| Ok(item?.key))
                    .collect()
            };
            let outbound = scan(adjacency(keys::EDGE_OUT, graph, id))?;
            let inbound = scan(adjacency(keys::EDGE_IN, graph, id))?;
            let mut changes: Changes = vec![(key, None)];
            for (out, mirror) in incident(graph, id, &outbound, &inbound)? {
                changes.push((out, None));
                changes.push((mirror, None));
            }
            Ok((changes, existed))
        }
        GraphWrite::AddEdge {
            graph,
            source,
            target,
            kind,
            weight,
        } => {
            validate_edge(graph, source, target, kind)?;
            if !weight.is_finite() {
                return Err(invalid("edge weights must be finite"));
            }
            let record = encode_weight(*weight);
            Ok((
                vec![
                    (
                        edge_key(keys::EDGE_OUT, graph, source, kind, target),
                        Some(record.clone()),
                    ),
                    (
                        edge_key(keys::EDGE_IN, graph, target, kind, source),
                        Some(record),
                    ),
                ],
                false,
            ))
        }
        GraphWrite::RemoveEdge {
            graph,
            source,
            target,
            kind,
        } => {
            validate_edge(graph, source, target, kind)?;
            let out = edge_key(keys::EDGE_OUT, graph, source, kind, target);
            let existed = view.peek(&out)?.is_some();
            Ok((
                vec![
                    (out, None),
                    (edge_key(keys::EDGE_IN, graph, target, kind, source), None),
                ],
                existed,
            ))
        }
    }
}
