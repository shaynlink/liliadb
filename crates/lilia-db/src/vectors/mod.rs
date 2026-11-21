//! Vector collections: vectors by id, exact search, and an optional HNSW index.
//!
//! Each vector is a record (`collection\0id`: element type, dimension, bytes) and a ledger
//! record. A collection's HNSW configuration is part of its catalog schema; once a collection
//! has one, every vector written to it must have the index's dimension and be either `f32`
//! (quantized for an `i8` or bit index) or of the index's element type. The graph itself is kept
//! in memory ([`index`]); persisting it is the subject of alpha.4.

mod admin;
mod handle;
mod index;
mod vector;

use bytes::Bytes;
use lilia_core::{HnswConfig, VectorElementType};

pub use handle::Vectors;
pub(crate) use index::{VectorDelta, VectorIndexes};
pub use vector::Vector;

use crate::error::{invalid, Result};
use crate::keys::{self, validate_name};
use crate::txn::View;
use crate::write::overlay::Changes;
use crate::write::{Model, Planner};

/// A vector write, for a [`crate::Batch`]; see [`Vectors`] for each operation.
#[derive(Debug, Clone, PartialEq)]
pub enum VectorWrite {
    Set {
        collection: String,
        id: String,
        vector: Vector,
    },
    Delete {
        collection: String,
        id: String,
    },
}

pub(crate) fn vector_key(collection: &str, id: &str) -> Bytes {
    let mut key = keys::collection(keys::VECTOR, collection);
    key.extend_from_slice(id.as_bytes());
    key.into()
}

/// Whether `vector` may be stored in a collection indexed under `config`.
///
/// # Errors
///
/// [`crate::Error::InvalidArgument`] naming what does not fit.
pub(crate) fn fits(config: &HnswConfig, vector: &Vector) -> Result<()> {
    if vector.dim() != config.dim {
        return Err(invalid(format!(
            "the index of this collection holds vectors of dimension {}, got {}",
            config.dim,
            vector.dim()
        )));
    }
    let element_type = vector.element_type();
    if element_type != VectorElementType::Float32 && element_type != config.element_type {
        return Err(invalid(format!(
            "the index of this collection holds {} vectors (or float32 ones, quantized), got {}",
            config.element_type.as_str(),
            element_type.as_str()
        )));
    }
    Ok(())
}

fn validate(write: &VectorWrite) -> Result<()> {
    match write {
        VectorWrite::Set { collection, id, .. } | VectorWrite::Delete { collection, id } => {
            validate_name("collection", collection)?;
            validate_name("vector id", id)
        }
    }
}

impl Planner<'_> {
    /// Plans one vector write.
    ///
    /// # Errors
    ///
    /// Invalid names, a vector that does not fit the collection's index, engine read errors.
    pub(crate) fn vector(&mut self, write: &VectorWrite) -> Result<()> {
        validate(write)?;
        match write {
            VectorWrite::Set {
                collection,
                id,
                vector,
            } => {
                if let Some(config) = self.schema(collection).and_then(|s| s.vector.clone()) {
                    fits(&config, vector)?;
                }
                let op = self.write_record(
                    &vector_key(collection, id),
                    Some(vector.encode()),
                    (Model::Vector, collection, id.clone()),
                );
                self.vector_change(VectorDelta {
                    collection: collection.clone(),
                    id: id.clone(),
                    op,
                    vector: Some(vector.clone()),
                });
                self.result(Some(op), false);
            }
            VectorWrite::Delete { collection, id } => {
                let key = vector_key(collection, id);
                if self.current(&key)?.is_some() {
                    let op = self.write_record(&key, None, (Model::Vector, collection, id.clone()));
                    self.vector_change(VectorDelta {
                        collection: collection.clone(),
                        id: id.clone(),
                        op,
                        vector: None,
                    });
                    self.result(Some(op), true);
                } else {
                    self.result(None, false);
                }
            }
        }
        Ok(())
    }
}

/// The records a vector write changes in a transaction's view; see
/// [`crate::write::overlay`]. The index check happens at commit.
///
/// # Errors
///
/// Invalid names, engine read errors.
pub(crate) fn overlay(write: &VectorWrite, view: &View<'_>) -> Result<(Changes, bool)> {
    validate(write)?;
    match write {
        VectorWrite::Set {
            collection,
            id,
            vector,
        } => Ok((
            vec![(vector_key(collection, id), Some(vector.encode()))],
            false,
        )),
        VectorWrite::Delete { collection, id } => {
            let key = vector_key(collection, id);
            let existed = view.peek(&key)?.is_some();
            Ok((vec![(key, None)], existed))
        }
    }
}
