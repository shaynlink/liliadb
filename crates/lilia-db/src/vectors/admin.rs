//! Creating, dropping and describing the HNSW index of a vector collection.

use std::ops::Bound;

use lilia_core::{HnswConfig, VectorIndexInfo};

use super::{fits, Vector};
use crate::db::Database;
use crate::error::{invalid, Result};
use crate::keys::{self, validate_name};

impl Database {
    /// Creates (or reconfigures) the HNSW index of vector collection `collection`, and returns
    /// once it is built. A dimension of 0 takes the dimension of the stored vectors.
    ///
    /// # Errors
    ///
    /// [`crate::Error::InvalidArgument`] for an invalid name or configuration, a dimension of 0
    /// on an empty collection, or a stored vector that does not fit the index; engine errors.
    pub fn create_vector_index(&self, collection: &str, mut config: HnswConfig) -> Result<()> {
        validate_name("collection", collection)?;
        if config.m < 2 || config.ef_construction == 0 || config.ef_search == 0 {
            return Err(invalid(
                "an HNSW index needs m >= 2, ef_construction >= 1 and ef_search >= 1",
            ));
        }
        {
            let mut catalog = self.catalog_mut();
            // Writers plan under the catalog's read lock: no vector lands between this check and
            // the new configuration, and every later vector is checked against it.
            let base = keys::collection(keys::VECTOR, collection);
            let end = keys::prefix_end(&base).unwrap_or_default();
            for entry in self
                .engine()
                .range(Bound::Included(&base), Bound::Excluded(&end))?
            {
                let entry = entry?;
                let vector = Vector::decode(&entry.value)?;
                if config.dim == 0 {
                    config.dim = vector.dim();
                }
                fits(&config, &vector).map_err(|e| {
                    invalid(format!(
                        "vector {} cannot be indexed: {e}",
                        String::from_utf8_lossy(keys::suffix(&entry.key, &base))
                    ))
                })?;
            }
            if config.dim == 0 {
                return Err(invalid("an index of an empty collection needs a dimension"));
            }
            let mut schema = catalog.schema_or_default(collection);
            schema.vector = Some(config.clone());
            self.store_schema(&mut catalog, collection, schema)?;
            self.vector_indexes().evict(collection);
        }
        self.vector_indexes()
            .get(self.engine(), collection, &config)
            .map(drop)
    }

    /// Drops the HNSW index of `collection`; `false` if it has none. Its vectors stay.
    ///
    /// # Errors
    ///
    /// Engine errors.
    pub fn drop_vector_index(&self, collection: &str) -> Result<bool> {
        validate_name("collection", collection)?;
        let mut catalog = self.catalog_mut();
        let mut schema = catalog.schema_or_default(collection);
        if schema.vector.take().is_none() {
            return Ok(false);
        }
        self.store_schema(&mut catalog, collection, schema)?;
        self.vector_indexes().evict(collection);
        Ok(true)
    }

    /// The HNSW index of `collection`, built if it was not yet.
    ///
    /// # Errors
    ///
    /// Engine errors building the index.
    pub fn vector_index(&self, collection: &str) -> Result<Option<VectorIndexInfo>> {
        validate_name("collection", collection)?;
        let Some(config) = self.schema(collection).and_then(|s| s.vector.clone()) else {
            return Ok(None);
        };
        let loaded = self
            .vector_indexes()
            .get(self.engine(), collection, &config)?;
        let index = &loaded.read().index;
        Ok(Some(VectorIndexInfo {
            collection: collection.to_owned(),
            node_count: index.len(),
            max_level: index.max_level(),
            m: config.m,
            ef_construction: config.ef_construction,
            ef_search: config.ef_search,
            metric: config.metric,
            element_type: config.element_type,
            dim: config.dim,
        }))
    }
}
