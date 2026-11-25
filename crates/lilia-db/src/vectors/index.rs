//! HNSW indexes of the vector collections that have one, kept in memory.
//!
//! An index is built from a snapshot the first time its collection is searched, then kept
//! exact by the vector changes of every commit ([`VectorIndexes::apply`]). A change is applied
//! only if its version is newer than both the build's snapshot and the last change applied to
//! the same id, so commits that apply in a different order than they committed still leave each
//! id at its latest vector. Builds and the lookups of `apply` take the same lock, so a commit
//! that lands during a build is applied after it.

use std::collections::HashMap;
use std::ops::Bound;
use std::sync::Arc;

use lilia_core::hnsw::HnswIndex;
use lilia_core::HnswConfig;
use lilia_engine::Db;
use parking_lot::{Mutex, RwLock};

use super::vector::Vector;
use crate::error::{corruption, Result};
use crate::keys;

/// A vector change of a commit: the version of the record is the commit's first sequence plus
/// `op`; `None` for a deletion.
#[derive(Debug, Clone)]
pub struct VectorDelta {
    pub collection: String,
    pub id: String,
    pub op: usize,
    pub vector: Option<Vector>,
}

/// A built index.
#[derive(Debug)]
pub struct Loaded {
    pub index: HnswIndex,
    built_at: u64,
    /// Version of the last change applied to each id.
    applied: HashMap<String, u64>,
}

impl Loaded {
    fn insert(&mut self, id: &str, vector: &Vector) -> Result<()> {
        let prepared = vector
            .convert(self.index.config().element_type)
            .ok_or_else(|| corruption(format!("vector {id} does not fit its index")))?;
        self.index
            .insert(id, prepared.data())
            .map_err(|e| corruption(format!("vector index: {e}")))?;
        Ok(())
    }
}

/// Indexes built so far.
#[derive(Debug, Default)]
pub struct VectorIndexes {
    loaded: Mutex<HashMap<String, Arc<RwLock<Loaded>>>>,
}

fn build(engine: &Db, collection: &str, config: &HnswConfig) -> Result<Loaded> {
    let snapshot = engine.snapshot();
    let base = keys::collection(keys::VECTOR, collection);
    let end = keys::prefix_end(&base).unwrap_or_default();
    let mut loaded = Loaded {
        index: HnswIndex::new(config.clone()),
        built_at: snapshot.sequence(),
        applied: HashMap::new(),
    };
    for entry in engine.range_at(&snapshot, Bound::Included(&base), Bound::Excluded(&end))? {
        let entry = entry?;
        let id = std::str::from_utf8(keys::suffix(&entry.key, &base))
            .map_err(|_| corruption("vector id is not UTF-8"))?;
        loaded.insert(id, &Vector::decode(&entry.value)?)?;
    }
    Ok(loaded)
}

impl VectorIndexes {
    /// The index of `collection` under `config`, building it first if needed.
    ///
    /// # Errors
    ///
    /// Read errors and undecodable records of the build.
    pub fn get(
        &self,
        engine: &Db,
        collection: &str,
        config: &HnswConfig,
    ) -> Result<Arc<RwLock<Loaded>>> {
        let mut loaded = self.loaded.lock();
        if let Some(index) = loaded.get(collection) {
            return Ok(Arc::clone(index));
        }
        let index = Arc::new(RwLock::new(build(engine, collection, config)?));
        loaded.insert(collection.to_owned(), Arc::clone(&index));
        Ok(index)
    }

    /// Forgets the index of `collection`, after its configuration changed.
    pub fn evict(&self, collection: &str) {
        self.loaded.lock().remove(collection);
    }

    /// Applies the vector changes of a commit whose first sequence is `first`.
    ///
    /// # Errors
    ///
    /// A stored vector that does not fit its index.
    pub fn apply(&self, deltas: &[VectorDelta], first: u64) -> Result<()> {
        if deltas.is_empty() {
            return Ok(());
        }
        for delta in deltas {
            let Some(index) = self.loaded.lock().get(&delta.collection).cloned() else {
                continue;
            };
            let version = first + delta.op as u64;
            let mut index = index.write();
            if version <= index.built_at
                || index.applied.get(&delta.id).is_some_and(|&v| v >= version)
            {
                continue;
            }
            index.applied.insert(delta.id.clone(), version);
            match &delta.vector {
                Some(vector) => index.insert(&delta.id, vector)?,
                None => {
                    index.index.delete(&delta.id);
                }
            }
        }
        Ok(())
    }
}
