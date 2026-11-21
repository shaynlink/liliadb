//! The batch format of the bindings: [`BatchOperation`]s (JSON or `MessagePack` on the wire) as
//! the [`Write`]s of a [`Batch`], and [`Outcome`]s as [`MutationResult`]s. Every binding goes
//! through these conversions, so a batch means the same thing in each of them.

use lilia_core::crdt::LwwJsonDocument;
use lilia_core::{BatchOperation, BinaryDoc, BitemporalInterval, MutationResult, StorageTier};

use crate::batch::{Batch, Outcome, Write};
use crate::bitemporal::BitemporalWrite;
use crate::crdt::CrdtWrite;
use crate::error::{Error, Result};
use crate::fts::FtsWrite;
use crate::graph::GraphWrite;
use crate::record::now_ms;
use crate::vectors::{Vector, VectorWrite};

/// The writes of `operations`, in order.
///
/// # Errors
///
/// [`Error::Unsupported`] for an operation `lilia-db` does not have (`JsonPutHlc`);
/// [`Error::Document`] for a document that cannot be stored; [`Error::InvalidArgument`] for a
/// vector, a merged state or an interval that does not decode.
pub fn batch(operations: &[BatchOperation]) -> Result<Batch> {
    let now = now_ms();
    let writes = operations
        .iter()
        .map(|operation| write(operation, now))
        .collect::<Result<Vec<_>>>()?;
    Ok(Batch::from(writes))
}

/// The results of a committed batch, one per operation.
#[must_use]
pub fn results(outcomes: &[Outcome]) -> Vec<MutationResult> {
    outcomes
        .iter()
        .map(|outcome| MutationResult {
            version: outcome.version,
            deleted: outcome.deleted,
            tier: StorageTier::Disk,
            buffered: false,
            hlc: None,
            conflict_detected: None,
        })
        .collect()
}

fn document(value: &serde_json::Value) -> Result<BinaryDoc> {
    BinaryDoc::from_json_value(value).map_err(|e| Error::Document(e.to_string()))
}

/// The write of one operation; `now` dates CRDT removals and fields without a time.
// reason: one arm per operation, each moving its fields into the write of its model.
#[allow(clippy::too_many_lines)]
fn write(operation: &BatchOperation, now: i64) -> Result<Write> {
    Ok(match operation.clone() {
        BatchOperation::KvSet {
            namespace,
            key,
            value,
            if_version,
            expires_at_ms,
        } => Write::KvPut {
            namespace,
            key,
            value,
            expires_at_ms,
            if_version,
        },
        BatchOperation::KvDelete {
            namespace,
            key,
            if_version,
        } => Write::KvDelete {
            namespace,
            key,
            if_version,
        },
        BatchOperation::JsonPut {
            space,
            id,
            value,
            if_version,
        } => Write::JsonPut {
            space,
            id,
            doc: document(&value)?,
            if_version,
        },
        BatchOperation::JsonPutRaw {
            space,
            id,
            mut data,
            if_version,
        } => Write::JsonPut {
            space,
            id,
            doc: lilia_core::json::simd::parse_json_bytes_to_binary(&mut data)
                .map_err(|e| Error::Document(e.to_string()))?,
            if_version,
        },
        BatchOperation::JsonDelete {
            space,
            id,
            if_version,
        } => Write::JsonDelete {
            space,
            id,
            if_version,
        },
        BatchOperation::GraphSetNode {
            graph,
            id,
            kind,
            metadata,
            doc_space,
            doc_id,
        } => {
            let document = doc_space.map(|space| (space, doc_id.unwrap_or_else(|| id.clone())));
            Write::Graph(GraphWrite::SetNode {
                graph,
                id,
                kind,
                metadata,
                document,
            })
        }
        BatchOperation::GraphDeleteNode { graph, id } => {
            Write::Graph(GraphWrite::DeleteNode { graph, id })
        }
        BatchOperation::GraphAddEdge {
            graph,
            source,
            target,
            kind,
            weight,
        } => Write::Graph(GraphWrite::AddEdge {
            graph,
            source,
            target,
            kind,
            weight,
        }),
        BatchOperation::GraphRemoveEdge {
            graph,
            source,
            target,
            kind,
        } => Write::Graph(GraphWrite::RemoveEdge {
            graph,
            source,
            target,
            kind,
        }),
        BatchOperation::FtsIndex {
            collection,
            doc_id,
            content,
        } => Write::Fts(FtsWrite::Index {
            collection,
            doc_id,
            content,
        }),
        BatchOperation::FtsDelete { collection, doc_id } => {
            Write::Fts(FtsWrite::Delete { collection, doc_id })
        }
        BatchOperation::VectorSet {
            collection,
            id,
            dim,
            element_type,
            vector,
        } => Write::Vector(VectorWrite::Set {
            collection,
            id,
            vector: Vector::new(element_type, dim, vector)?,
        }),
        BatchOperation::VectorDelete { collection, id } => {
            Write::Vector(VectorWrite::Delete { collection, id })
        }
        BatchOperation::CrdtPnCounterIncrement {
            space,
            id,
            actor_id,
            amount,
        } => Write::Crdt(CrdtWrite::Count {
            space,
            id,
            actor: actor_id,
            increment: amount,
            decrement: 0,
        }),
        BatchOperation::CrdtPnCounterDecrement {
            space,
            id,
            actor_id,
            amount,
        } => Write::Crdt(CrdtWrite::Count {
            space,
            id,
            actor: actor_id,
            increment: 0,
            decrement: amount,
        }),
        BatchOperation::CrdtOrSetAdd {
            space,
            id,
            actor_id,
            element,
        } => Write::Crdt(CrdtWrite::set_add(&space, &id, &actor_id, &element)),
        BatchOperation::CrdtOrSetRemove {
            space,
            id,
            element,
            timestamp_ms,
        } => Write::Crdt(CrdtWrite::SetRemove {
            space,
            id,
            element,
            timestamp_ms: timestamp_ms.unwrap_or(now),
        }),
        BatchOperation::CrdtJsonSetField {
            space,
            id,
            actor_id,
            path,
            value,
            timestamp_ms,
        } => Write::Crdt(CrdtWrite::DocSet {
            space,
            id,
            actor: actor_id,
            path,
            value,
            timestamp_ms: timestamp_ms.unwrap_or(now),
        }),
        BatchOperation::CrdtJsonDeleteField {
            space,
            id,
            actor_id,
            path,
            timestamp_ms,
        } => Write::Crdt(CrdtWrite::DocDelete {
            space,
            id,
            actor: actor_id,
            path,
            timestamp_ms: timestamp_ms.unwrap_or(now),
        }),
        BatchOperation::CrdtJsonMerge { space, id, state } => {
            let state = lilia_core::decompress_bytes(&state)
                .map_err(|e| Error::InvalidArgument(format!("merged CRDT state: {e}")))?;
            let state: LwwJsonDocument = serde_json::from_slice(&state)
                .map_err(|e| Error::InvalidArgument(format!("merged CRDT state: {e}")))?;
            Write::Crdt(CrdtWrite::DocMerge { space, id, state })
        }
        BatchOperation::JsonPutHlc { .. } => {
            return Err(Error::Unsupported(
                "hybrid logical clocks and their conflicts are not available in v0.2".into(),
            ))
        }
        BatchOperation::BitemporalPut {
            space,
            id,
            value,
            vt_from,
            vt_to,
            system_time,
        } => Write::Bitemporal(BitemporalWrite::Put {
            space,
            id,
            value,
            valid: BitemporalInterval {
                from: vt_from,
                to: vt_to,
            },
            recorded_at: system_time,
        }),
        BatchOperation::BitemporalDelete {
            space,
            id,
            vt_from,
            vt_to,
            system_time,
        } => {
            let valid = match (vt_from, vt_to) {
                (Some(from), Some(to)) => Some(BitemporalInterval { from, to }),
                (None, None) => None,
                _ => {
                    return Err(Error::InvalidArgument(
                        "a bitemporal deletion gives both ends of its interval, or neither".into(),
                    ))
                }
            };
            Write::Bitemporal(BitemporalWrite::Delete {
                space,
                id,
                valid,
                recorded_at: system_time,
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn operations_become_the_writes_of_their_model() {
        let operations = vec![
            BatchOperation::KvSet {
                namespace: "ns".into(),
                key: b"k".to_vec(),
                value: b"v".to_vec(),
                if_version: Some(0),
                expires_at_ms: None,
            },
            BatchOperation::JsonPutRaw {
                space: "docs".into(),
                id: "a".into(),
                data: br#"{"n": 1}"#.to_vec(),
                if_version: None,
            },
            BatchOperation::GraphSetNode {
                graph: "g".into(),
                id: "n".into(),
                kind: "k".into(),
                metadata: json!({}),
                doc_space: Some("docs".into()),
                doc_id: None,
            },
            BatchOperation::CrdtOrSetRemove {
                space: "c".into(),
                id: "s".into(),
                element: "e".into(),
                timestamp_ms: None,
            },
        ];
        let converted = batch(&operations).unwrap();
        let writes = converted.writes();
        assert_eq!(writes.len(), 4);
        assert!(matches!(
            &writes[0],
            Write::KvPut {
                if_version: Some(0),
                ..
            }
        ));
        assert!(
            matches!(&writes[1], Write::JsonPut { doc, .. } if doc.to_json_value() == json!({"n": 1}))
        );
        assert!(matches!(
            &writes[2],
            Write::Graph(GraphWrite::SetNode { document: Some((space, doc)), .. }) if space == "docs" && doc == "n"
        ));
        assert!(
            matches!(&writes[3], Write::Crdt(CrdtWrite::SetRemove { timestamp_ms, .. }) if *timestamp_ms > 0)
        );

        let refused = BatchOperation::BitemporalDelete {
            space: "b".into(),
            id: "x".into(),
            vt_from: Some(1),
            vt_to: None,
            system_time: None,
        };
        assert!(matches!(batch(&[refused]), Err(Error::InvalidArgument(_))));
        let hlc = BatchOperation::JsonPutHlc {
            space: "s".into(),
            id: "i".into(),
            value: json!({}),
            hlc: None,
            parent_hlc: None,
            strategy: lilia_core::hlc::ConflictResolutionStrategy::default(),
        };
        let error = batch(&[hlc]).unwrap_err();
        assert!(matches!(error, Error::Unsupported(_)));
        assert_eq!(
            lilia_core::LiliaError::from(error).code,
            lilia_core::ErrorCode::Unsupported
        );
    }

    #[test]
    fn outcomes_become_results() {
        let results = results(&[Outcome {
            version: Some(7),
            deleted: true,
        }]);
        assert_eq!((results[0].version, results[0].deleted), (Some(7), true));
    }
}
