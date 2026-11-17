#![allow(
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use lilia_core::hnsw::HnswIndex;
use lilia_core::quantization::{
    cosine_distance_f32, cosine_distance_i8, dot_distance_f32, hamming_distance_bits,
    l2_distance_f32,
};
use lilia_core::{
    BatchOperation, CollectionKind, ErrorCode, HnswConfig, LiliaError, Result, StorageTier,
    ValueCodec, VectorElementType, VectorHit, VectorIndexInfo, VectorMetric,
};
use lilia_sqlite_native::{NativeConnection, NativeTransaction, StmtSlot};

use crate::storage::{normalize_limit, now_ms, validate_name};
use crate::Database;

impl Database {
    /// Creates or updates an HNSW vector index for a collection.
    ///
    /// If vectors already exist in the collection, they are automatically indexed into the HNSW graph.
    #[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
    pub fn vector_create_index(&self, collection: &str, mut config: HnswConfig) -> Result<()> {
        struct ExistingVector {
            id: String,
            dim: u32,
            element_type: VectorElementType,
            bytes: Vec<u8>,
        }

        validate_name("collection", collection)?;
        let tier = self.catalog.tier_for(CollectionKind::Vector, collection);

        if tier == StorageTier::Memory {
            let scanned =
                self.mem_engine
                    .vectors
                    .scan(Some(collection), None, usize::MAX, now_ms());
            if config.dim == 0 {
                if let Some((_, _, first)) = scanned.first() {
                    config.dim = first.value.0;
                }
            }
            let mut index = HnswIndex::new(config.clone());
            for (_, key_bytes, snap) in &scanned {
                if let Ok(id) = String::from_utf8(key_bytes.clone()) {
                    let (_dim, elem_type, bytes) = &snap.value;
                    let vec_data = prepare_vector_for_index(bytes, *elem_type, config.element_type);
                    index.insert(&id, &vec_data)?;
                }
            }
            self.mem_engine
                .hnsw_meta
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(collection.to_owned(), config);
            self.mem_engine
                .hnsw_indices
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(collection.to_owned(), Arc::new(RwLock::new(index)));
            return Ok(());
        }

        if tier == StorageTier::Buffered {
            self.ring_core.flush()?;
        }

        // 1. Fetch all existing vectors for this collection
        let conn = self.read_lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, dim, element_type, vector FROM _lilia_vectors WHERE collection = ?1;",
        )?;
        stmt.bind_text_static(1, collection)?;

        let mut existing = Vec::new();
        while stmt.step()? {
            let id = stmt.column_text(0).to_string();
            let dim = u32::try_from(stmt.column_int64(1)).unwrap_or_default();
            let elem_code = stmt.column_int64(2);
            let element_type = VectorElementType::try_from(elem_code)?;
            let blob = stmt.column_blob(3);
            let decrypted = self.codec.decode(blob)?;
            existing.push(ExistingVector {
                id,
                dim,
                element_type,
                bytes: decrypted,
            });
        }
        drop(stmt);
        drop(conn);

        if config.dim == 0 && !existing.is_empty() {
            config.dim = existing[0].dim;
        }

        // 2. Build in-memory HNSW index
        let mut index = HnswIndex::new(config.clone());
        for item in &existing {
            let vec_data =
                prepare_vector_for_index(&item.bytes, item.element_type, config.element_type);
            index.insert(&item.id, &vec_data)?;
        }

        // 3. Persist metadata and graph edges in a writer transaction
        let mut writer = self.lock_writer()?;
        writer.exec("BEGIN IMMEDIATE;")?;

        // Persist meta
        let meta_stmt = writer.slot(StmtSlot::HnswMetaUpsert)?;
        meta_stmt.reset()?;
        meta_stmt.bind_text_static(1, collection)?;
        if let Some(entry_id) = index.entry_point_id() {
            meta_stmt.bind_text_static(2, entry_id)?;
        } else {
            meta_stmt.bind_null(2)?;
        }
        meta_stmt.bind_int64(3, crate::storage::sql_int(index.max_level()))?;
        meta_stmt.bind_int64(4, crate::storage::sql_int(config.m))?;
        meta_stmt.bind_int64(5, crate::storage::sql_int(config.ef_construction))?;
        meta_stmt.bind_int64(6, crate::storage::sql_int(config.ef_search))?;
        meta_stmt.bind_text_static(7, config.metric.as_str())?;
        meta_stmt.bind_int64(8, i64::from(config.element_type as u8))?;
        meta_stmt.bind_int64(9, i64::from(config.dim))?;
        meta_stmt.step()?;
        meta_stmt.reset()?;

        // Clean any old edges for this collection
        let mut del_edges =
            writer.prepare("DELETE FROM _lilia_hnsw_edges WHERE collection = ?1;")?;
        del_edges.bind_text_static(1, collection)?;
        del_edges.step()?;
        drop(del_edges);

        // Store edges for existing items
        let edge_stmt = writer.slot(StmtSlot::HnswEdgesUpsert)?;
        for item in &existing {
            let query_bytes =
                prepare_vector_for_index(&item.bytes, item.element_type, config.element_type);
            let hits = index.search(collection, &query_bytes, 1, None, None);
            if !hits.is_empty() {
                let neighbors_json = serde_json::to_vec(&vec![item.id.clone()])
                    .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
                edge_stmt.reset()?;
                edge_stmt.bind_text_static(1, collection)?;
                edge_stmt.bind_text_static(2, &item.id)?;
                edge_stmt.bind_int64(3, 0)?;
                edge_stmt.bind_blob_static(4, &neighbors_json)?;
                edge_stmt.step()?;
                edge_stmt.reset()?;
            }
        }

        writer.exec("COMMIT;")?;

        // 4. Update in-memory cache
        let arc_index = Arc::new(std::sync::RwLock::new(index));
        self.hnsw_cache
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(collection.to_string(), arc_index);

        Ok(())
    }

    /// Retrieves index metadata for a collection if an HNSW index is configured.
    pub fn vector_get_index_info(&self, collection: &str) -> Result<Option<VectorIndexInfo>> {
        validate_name("collection", collection)?;
        if let Some(index_arc) = self.get_or_load_hnsw_index(collection)? {
            let index = index_arc
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let cfg = index.config();
            Ok(Some(VectorIndexInfo {
                collection: collection.to_string(),
                node_count: index.len(),
                max_level: index.max_level(),
                m: cfg.m,
                ef_construction: cfg.ef_construction,
                ef_search: cfg.ef_search,
                metric: cfg.metric,
                element_type: cfg.element_type,
                dim: cfg.dim,
            }))
        } else {
            Ok(None)
        }
    }

    /// Drops the HNSW index for a collection.
    pub fn vector_drop_index(&self, collection: &str) -> Result<bool> {
        validate_name("collection", collection)?;
        let tier = self.catalog.tier_for(CollectionKind::Vector, collection);

        if tier == StorageTier::Memory {
            let removed_idx = self
                .mem_engine
                .hnsw_indices
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(collection)
                .is_some();
            let removed_meta = self
                .mem_engine
                .hnsw_meta
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(collection)
                .is_some();
            return Ok(removed_idx || removed_meta);
        }

        if tier == StorageTier::Buffered {
            self.ring_core.flush()?;
        }

        self.hnsw_cache
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(collection);

        let writer = self.lock_writer()?;
        let mut del_meta = writer.prepare("DELETE FROM _lilia_hnsw_meta WHERE collection = ?1;")?;
        del_meta.bind_text_static(1, collection)?;
        del_meta.step()?;
        let changes = writer.changes();
        drop(del_meta);

        let mut del_edges =
            writer.prepare("DELETE FROM _lilia_hnsw_edges WHERE collection = ?1;")?;
        del_edges.bind_text_static(1, collection)?;
        del_edges.step()?;

        Ok(changes > 0)
    }

    /// Loads or retrieves the HNSW index for a collection from cache or SQLite persistence.
    pub(crate) fn get_or_load_hnsw_index(
        &self,
        collection: &str,
    ) -> Result<Option<Arc<std::sync::RwLock<HnswIndex>>>> {
        let tier = self.catalog.tier_for(CollectionKind::Vector, collection);
        if tier == StorageTier::Memory {
            let guard = self
                .mem_engine
                .hnsw_indices
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            return Ok(guard.get(collection).map(Arc::clone));
        }
        if tier == StorageTier::Buffered {
            self.ring_core.flush()?;
        }
        {
            let cache = self
                .hnsw_cache
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(index) = cache.get(collection) {
                return Ok(Some(Arc::clone(index)));
            }
        }
        let conn = self.read_lock()?;
        get_or_load_hnsw_index_on_conn(&conn, self.codec.as_ref(), &self.hnsw_cache, collection)
    }

    pub(crate) fn sync_hnsw_insert(
        &self,
        collection: &str,
        id: &str,
        dim: u32,
        element_type: VectorElementType,
        vector: &[u8],
        tx: &mut NativeTransaction<'_>,
    ) -> Result<()> {
        sync_hnsw_insert_with_cache(
            tx,
            &self.hnsw_cache,
            self.codec.as_ref(),
            collection,
            id,
            dim,
            element_type,
            vector,
        )
    }

    pub(crate) fn sync_hnsw_delete(
        &self,
        collection: &str,
        id: &str,
        tx: &mut NativeTransaction<'_>,
    ) -> Result<()> {
        sync_hnsw_delete_with_cache(tx, &self.hnsw_cache, self.codec.as_ref(), collection, id)
    }

    pub fn vector_set(
        &self,
        collection: &str,
        id: &str,
        dim: u32,
        element_type: VectorElementType,
        vector: &[u8],
    ) -> Result<()> {
        validate_name("collection", collection)?;
        validate_name("id", id)?;
        self.batch(&[BatchOperation::VectorSet {
            collection: collection.to_string(),
            id: id.to_string(),
            dim,
            element_type,
            vector: vector.to_vec(),
        }])?;
        Ok(())
    }

    pub fn vector_get(
        &self,
        collection: &str,
        id: &str,
    ) -> Result<Option<(u32, VectorElementType, Vec<u8>)>> {
        validate_name("collection", collection)?;
        validate_name("id", id)?;
        let tier = self.catalog.tier_for(CollectionKind::Vector, collection);
        if tier == StorageTier::Memory {
            return Ok(self
                .mem_engine
                .vectors
                .get(collection, id.as_bytes(), now_ms(), true)
                .map(|snap| snap.value));
        }
        if tier == StorageTier::Buffered {
            self.ring_core.flush()?;
        }
        let conn = self.read_lock()?;
        let mut stmt = conn.prepare(
            "SELECT dim, element_type, vector FROM _lilia_vectors WHERE collection = ?1 AND id = ?2;",
        )?;
        stmt.bind_text_static(1, collection)?;
        stmt.bind_text_static(2, id)?;

        if stmt.step()? {
            let dim = stmt.column_int64(0) as u32;
            let elem_code = stmt.column_int64(1);
            let elem_type = VectorElementType::try_from(elem_code)?;
            let blob = stmt.column_blob(2);
            let decrypted = self.codec.decode(blob)?;
            Ok(Some((dim, elem_type, decrypted)))
        } else {
            Ok(None)
        }
    }

    pub fn vector_delete(&self, collection: &str, id: &str) -> Result<bool> {
        validate_name("collection", collection)?;
        validate_name("id", id)?;
        if self.catalog.tier_for(CollectionKind::Vector, collection) == StorageTier::Buffered {
            self.ring_core.flush()?;
        }
        let results = self.batch(&[BatchOperation::VectorDelete {
            collection: collection.to_string(),
            id: id.to_string(),
        }])?;
        Ok(results.first().is_some_and(|r| r.deleted))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn vector_search(
        &self,
        collection: &str,
        query_vector: &[u8],
        dim: u32,
        element_type: VectorElementType,
        metric: VectorMetric,
        limit: u32,
    ) -> Result<Vec<VectorHit>> {
        validate_name("collection", collection)?;

        // 1. Try HNSW ANN search if an index is present
        if let Some(index_arc) = self.get_or_load_hnsw_index(collection)? {
            let index = index_arc
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let norm_limit = normalize_limit(limit) as usize;

            // Two-stage re-ranking: if index is quantized (Int8 / Bit) and caller asked for Float32
            if index.config().element_type != VectorElementType::Float32
                && element_type == VectorElementType::Float32
            {
                let q_f32 = bytes_to_f32_slice(query_vector);
                let q_quantized = prepare_vector_for_index(
                    query_vector,
                    element_type,
                    index.config().element_type,
                );
                let hits = index.search_and_rerank(
                    collection,
                    &q_f32,
                    &q_quantized,
                    norm_limit,
                    None,
                    None,
                    2,
                    |id| {
                        self.vector_get(collection, id)
                            .ok()
                            .flatten()
                            .map(|(_, _, bytes)| bytes_to_f32_slice(&bytes))
                    },
                );
                return Ok(hits);
            }

            // Normal HNSW search
            let q_prepared =
                prepare_vector_for_index(query_vector, element_type, index.config().element_type);
            let hits = index.search(collection, &q_prepared, norm_limit, None, None);
            return Ok(hits);
        }

        // 2. Fallback: Exact flat scan
        let norm_limit = normalize_limit(limit) as usize;
        if self.catalog.tier_for(CollectionKind::Vector, collection) == StorageTier::Memory {
            let mut hits = Vec::new();
            for (_, key_bytes, snap) in
                self.mem_engine
                    .vectors
                    .scan(Some(collection), None, usize::MAX, now_ms())
            {
                let Ok(id) = String::from_utf8(key_bytes) else {
                    continue;
                };
                let (_d, row_elem_type, bytes) = snap.value;
                if row_elem_type != element_type {
                    continue;
                }
                let dist = compute_distance(query_vector, &bytes, element_type, metric);
                hits.push(VectorHit {
                    collection: collection.to_string(),
                    id,
                    distance: dist,
                });
            }
            sort_and_truncate_hits(&mut hits, norm_limit);
            let _ = dim;
            return Ok(hits);
        }

        let conn = self.read_lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, element_type, vector FROM _lilia_vectors WHERE collection = ?1;",
        )?;
        stmt.bind_text_static(1, collection)?;

        let mut hits = Vec::new();
        while stmt.step()? {
            let id = stmt.column_text(0).to_string();
            let row_elem_code = stmt.column_int64(1);
            let row_elem_type = VectorElementType::try_from(row_elem_code)?;
            if row_elem_type != element_type {
                continue;
            }
            let blob = stmt.column_blob(2);
            let decrypted = self.codec.decode(blob)?;

            let dist = compute_distance(query_vector, &decrypted, element_type, metric);
            hits.push(VectorHit {
                collection: collection.to_string(),
                id,
                distance: dist,
            });
        }

        sort_and_truncate_hits(&mut hits, norm_limit);
        let _ = dim;
        Ok(hits)
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
    pub fn vector_search_filtered(
        &self,
        collection: &str,
        query_vector: &[u8],
        dim: u32,
        element_type: VectorElementType,
        metric: VectorMetric,
        candidate_ids: &[&str],
        limit: u32,
    ) -> Result<Vec<VectorHit>> {
        validate_name("collection", collection)?;
        if candidate_ids.is_empty() {
            return Ok(Vec::new());
        }

        let norm_limit = normalize_limit(limit) as usize;

        // 1. Try Pre-filtered HNSW traversal if index exists
        if let Some(index_arc) = self.get_or_load_hnsw_index(collection)? {
            let index = index_arc
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let allowed_set: HashSet<&str> = candidate_ids.iter().copied().collect();
            let filter = |id: &str| allowed_set.contains(id);

            if index.config().element_type != VectorElementType::Float32
                && element_type == VectorElementType::Float32
            {
                let q_f32 = bytes_to_f32_slice(query_vector);
                let q_quantized = prepare_vector_for_index(
                    query_vector,
                    element_type,
                    index.config().element_type,
                );
                let hits = index.search_and_rerank(
                    collection,
                    &q_f32,
                    &q_quantized,
                    norm_limit,
                    None,
                    Some(&filter),
                    2,
                    |id| {
                        self.vector_get(collection, id)
                            .ok()
                            .flatten()
                            .map(|(_, _, bytes)| bytes_to_f32_slice(&bytes))
                    },
                );
                return Ok(hits);
            }

            let q_prepared =
                prepare_vector_for_index(query_vector, element_type, index.config().element_type);
            let hits = index.search(collection, &q_prepared, norm_limit, None, Some(&filter));
            return Ok(hits);
        }

        // 2. Fallback: Exact flat scan
        if self.catalog.tier_for(CollectionKind::Vector, collection) == StorageTier::Memory {
            let allowed: HashSet<&str> = candidate_ids.iter().copied().collect();
            let mut hits = Vec::new();
            for (_, key_bytes, snap) in
                self.mem_engine
                    .vectors
                    .scan(Some(collection), None, usize::MAX, now_ms())
            {
                let Ok(id) = String::from_utf8(key_bytes) else {
                    continue;
                };
                if !allowed.contains(id.as_str()) {
                    continue;
                }
                let (_d, row_elem_type, bytes) = snap.value;
                if row_elem_type != element_type {
                    continue;
                }
                let dist = compute_distance(query_vector, &bytes, element_type, metric);
                hits.push(VectorHit {
                    collection: collection.to_string(),
                    id,
                    distance: dist,
                });
            }
            sort_and_truncate_hits(&mut hits, norm_limit);
            let _ = dim;
            return Ok(hits);
        }

        // SQLite json_each join
        let conn = self.read_lock()?;
        let candidates_json = serde_json::to_string(candidate_ids)
            .map_err(|e| LiliaError::new(ErrorCode::InvalidInput, e.to_string(), false))?;

        let mut stmt = conn.prepare(
            "SELECT v.id, v.element_type, v.vector \
             FROM _lilia_vectors v \
             JOIN json_each(?2) c ON v.id = c.value \
             WHERE v.collection = ?1;",
        )?;
        stmt.bind_text_static(1, collection)?;
        stmt.bind_text_static(2, &candidates_json)?;

        let mut hits = Vec::new();
        while stmt.step()? {
            let id = stmt.column_text(0).to_string();
            let row_elem_code = stmt.column_int64(1);
            let row_elem_type = VectorElementType::try_from(row_elem_code)?;
            if row_elem_type != element_type {
                continue;
            }
            let blob = stmt.column_blob(2);
            let decrypted = self.codec.decode(blob)?;

            let dist = compute_distance(query_vector, &decrypted, element_type, metric);
            hits.push(VectorHit {
                collection: collection.to_string(),
                id,
                distance: dist,
            });
        }

        sort_and_truncate_hits(&mut hits, norm_limit);
        let _ = dim;
        Ok(hits)
    }
}

fn sort_and_truncate_hits(hits: &mut Vec<VectorHit>, norm_limit: usize) {
    hits.sort_by(|a, b| {
        a.distance
            .partial_cmp(&b.distance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if hits.len() > norm_limit {
        hits.truncate(norm_limit);
    }
}

pub(crate) fn get_or_load_hnsw_index_on_conn(
    conn: &NativeConnection,
    codec: &dyn ValueCodec,
    hnsw_cache: &RwLock<HashMap<String, Arc<RwLock<HnswIndex>>>>,
    collection: &str,
) -> Result<Option<Arc<RwLock<HnswIndex>>>> {
    {
        let cache = hnsw_cache
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = cache.get(collection) {
            return Ok(Some(Arc::clone(index)));
        }
    }

    let mut meta_stmt = conn.prepare(
        "SELECT entry_id, max_level, m, ef_construction, ef_search, metric, element_type, dim \
         FROM _lilia_hnsw_meta WHERE collection = ?1;",
    )?;
    meta_stmt.bind_text_static(1, collection)?;
    if !meta_stmt.step()? {
        return Ok(None);
    }

    let max_level = meta_stmt.column_int64(1) as usize;
    let m = meta_stmt.column_int64(2) as usize;
    let ef_construction = meta_stmt.column_int64(3) as usize;
    let ef_search = meta_stmt.column_int64(4) as usize;
    let metric_str = meta_stmt.column_text(5);
    let metric = metric_str.parse::<VectorMetric>()?;
    let elem_code = meta_stmt.column_int64(6);
    let element_type = VectorElementType::try_from(elem_code)?;
    let dim = meta_stmt.column_int64(7) as u32;

    let config = HnswConfig {
        m,
        ef_construction,
        ef_search,
        metric,
        element_type,
        dim,
    };

    let mut index = HnswIndex::new(config);
    let mut vec_stmt =
        conn.prepare("SELECT id, element_type, vector FROM _lilia_vectors WHERE collection = ?1;")?;
    vec_stmt.bind_text_static(1, collection)?;

    while vec_stmt.step()? {
        let id = vec_stmt.column_text(0).to_string();
        let row_elem_code = vec_stmt.column_int64(1);
        let row_elem_type = VectorElementType::try_from(row_elem_code)?;
        let blob = vec_stmt.column_blob(2);
        let decrypted = codec.decode(blob)?;

        let index_vec = prepare_vector_for_index(&decrypted, row_elem_type, element_type);
        index.insert(&id, &index_vec)?;
    }
    let _ = max_level;

    let arc_index = Arc::new(RwLock::new(index));
    hnsw_cache
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(collection.to_string(), Arc::clone(&arc_index));

    Ok(Some(arc_index))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn sync_hnsw_insert_with_cache(
    tx: &mut NativeTransaction<'_>,
    hnsw_cache: &RwLock<HashMap<String, Arc<RwLock<HnswIndex>>>>,
    codec: &dyn ValueCodec,
    collection: &str,
    id: &str,
    dim: u32,
    element_type: VectorElementType,
    vector: &[u8],
) -> Result<()> {
    if let Some(index_arc) =
        get_or_load_hnsw_index_on_conn(tx.conn(), codec, hnsw_cache, collection)?
    {
        let mut index = index_arc
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let index_vec = prepare_vector_for_index(vector, element_type, index.config().element_type);
        let (_, modified_edges) = index.insert(id, &index_vec)?;

        let edge_stmt = tx.slot(StmtSlot::HnswEdgesUpsert)?;
        for (node_id, level, neighbors) in modified_edges {
            let neighbors_blob = serde_json::to_vec(&neighbors)
                .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
            edge_stmt.reset()?;
            edge_stmt.bind_text_static(1, collection)?;
            edge_stmt.bind_text_static(2, &node_id)?;
            edge_stmt.bind_int64(3, crate::storage::sql_int(level))?;
            edge_stmt.bind_blob_static(4, &neighbors_blob)?;
            edge_stmt.step()?;
            edge_stmt.reset()?;
        }

        let meta_stmt = tx.slot(StmtSlot::HnswMetaUpsert)?;
        meta_stmt.reset()?;
        meta_stmt.bind_text_static(1, collection)?;
        if let Some(ep) = index.entry_point_id() {
            meta_stmt.bind_text_static(2, ep)?;
        } else {
            meta_stmt.bind_null(2)?;
        }
        meta_stmt.bind_int64(3, crate::storage::sql_int(index.max_level()))?;
        meta_stmt.bind_int64(4, crate::storage::sql_int(index.config().m))?;
        meta_stmt.bind_int64(5, crate::storage::sql_int(index.config().ef_construction))?;
        meta_stmt.bind_int64(6, crate::storage::sql_int(index.config().ef_search))?;
        meta_stmt.bind_text_static(7, index.config().metric.as_str())?;
        meta_stmt.bind_int64(8, i64::from(index.config().element_type as u8))?;
        meta_stmt.bind_int64(9, i64::from(dim))?;
        meta_stmt.step()?;
        meta_stmt.reset()?;
    }
    Ok(())
}

pub(crate) fn sync_hnsw_delete_with_cache(
    tx: &mut NativeTransaction<'_>,
    hnsw_cache: &RwLock<HashMap<String, Arc<RwLock<HnswIndex>>>>,
    codec: &dyn ValueCodec,
    collection: &str,
    id: &str,
) -> Result<()> {
    if let Some(index_arc) =
        get_or_load_hnsw_index_on_conn(tx.conn(), codec, hnsw_cache, collection)?
    {
        let mut index = index_arc
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        index.delete(id);

        let del_stmt = tx.slot(StmtSlot::HnswEdgesDelete)?;
        del_stmt.reset()?;
        del_stmt.bind_text_static(1, collection)?;
        del_stmt.bind_text_static(2, id)?;
        del_stmt.step()?;
        del_stmt.reset()?;

        let meta_stmt = tx.slot(StmtSlot::HnswMetaUpsert)?;
        meta_stmt.reset()?;
        meta_stmt.bind_text_static(1, collection)?;
        if let Some(ep) = index.entry_point_id() {
            meta_stmt.bind_text_static(2, ep)?;
        } else {
            meta_stmt.bind_null(2)?;
        }
        meta_stmt.bind_int64(3, crate::storage::sql_int(index.max_level()))?;
        meta_stmt.bind_int64(4, crate::storage::sql_int(index.config().m))?;
        meta_stmt.bind_int64(5, crate::storage::sql_int(index.config().ef_construction))?;
        meta_stmt.bind_int64(6, crate::storage::sql_int(index.config().ef_search))?;
        meta_stmt.bind_text_static(7, index.config().metric.as_str())?;
        meta_stmt.bind_int64(8, i64::from(index.config().element_type as u8))?;
        meta_stmt.bind_int64(9, i64::from(index.config().dim))?;
        meta_stmt.step()?;
        meta_stmt.reset()?;
    }
    Ok(())
}

pub(crate) fn sync_hnsw_for_op_with_cache(
    tx: &mut NativeTransaction<'_>,
    hnsw_cache: &RwLock<HashMap<String, Arc<RwLock<HnswIndex>>>>,
    codec: &dyn ValueCodec,
    op: &BatchOperation,
) -> Result<()> {
    match op {
        BatchOperation::VectorSet {
            collection,
            id,
            dim,
            element_type,
            vector,
        } => sync_hnsw_insert_with_cache(
            tx,
            hnsw_cache,
            codec,
            collection,
            id,
            *dim,
            *element_type,
            vector,
        ),
        BatchOperation::VectorDelete { collection, id } => {
            sync_hnsw_delete_with_cache(tx, hnsw_cache, codec, collection, id)
        }
        _ => Ok(()),
    }
}

/// Converts or quantizes a vector slice into the target element representation.
pub(crate) fn prepare_vector_for_index(
    input: &[u8],
    source_type: VectorElementType,
    target_type: VectorElementType,
) -> Vec<u8> {
    match (source_type, target_type) {
        (VectorElementType::Float32, VectorElementType::Int8) => {
            let f = bytes_to_f32_slice(input);
            let (q, _) = lilia_core::quantization::quantize_f32_to_i8(&f);
            q.into_iter().map(|x| x as u8).collect()
        }
        (VectorElementType::Float32, VectorElementType::Bit) => {
            let f = bytes_to_f32_slice(input);
            lilia_core::quantization::quantize_f32_to_bits(&f)
        }
        _ => input.to_vec(),
    }
}

pub(crate) fn bytes_to_f32_slice(bytes: &[u8]) -> Vec<f32> {
    if !bytes.len().is_multiple_of(4) {
        return Vec::new();
    }
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

pub(crate) fn bytes_to_i8_slice(bytes: &[u8]) -> Vec<i8> {
    bytes.iter().map(|&b| b.cast_signed()).collect()
}

fn compute_distance(
    query: &[u8],
    target: &[u8],
    element_type: VectorElementType,
    metric: VectorMetric,
) -> f64 {
    match element_type {
        VectorElementType::Float32 => {
            let q = bytes_to_f32_slice(query);
            let t = bytes_to_f32_slice(target);
            match metric {
                VectorMetric::Cosine => cosine_distance_f32(&q, &t),
                VectorMetric::L2 => l2_distance_f32(&q, &t),
                VectorMetric::Dot => dot_distance_f32(&q, &t),
                VectorMetric::Hamming => f64::from(hamming_distance_bits(query, target)),
            }
        }
        VectorElementType::Int8 => {
            let q = bytes_to_i8_slice(query);
            let t = bytes_to_i8_slice(target);
            match metric {
                VectorMetric::Cosine => cosine_distance_i8(&q, &t),
                VectorMetric::Hamming => f64::from(hamming_distance_bits(query, target)),
                VectorMetric::L2 | VectorMetric::Dot => {
                    let q_f: Vec<f32> = q.iter().map(|&x| f32::from(x)).collect();
                    let t_f: Vec<f32> = t.iter().map(|&x| f32::from(x)).collect();
                    if metric == VectorMetric::L2 {
                        l2_distance_f32(&q_f, &t_f)
                    } else {
                        dot_distance_f32(&q_f, &t_f)
                    }
                }
            }
        }
        VectorElementType::Bit => f64::from(hamming_distance_bits(query, target)),
    }
}
