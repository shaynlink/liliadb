use std::time::Duration;

use lilia_core::{
    BatchOperation, CollectionKind, CollectionSpec, Direction, ErrorCode, EvictionPolicy,
    FtsFuzzyHit, FtsHit, FtsSuggestion, GraphEdge, GraphNode, HnswConfig, JsonEntry, KvEntry,
    LiliaError, MutationResult, Result, StorageTier, TimeSeriesPoint, VectorElementType, VectorHit,
};
use serde_json::Value;

use crate::crdt::{CounterStore, DocStore, SetStore};
use crate::engine::Database;
use crate::storage::{now_ms, validate_name};

/// Fluent configurator for opening or declaring a tier-routed `LiliaDB` collection.
#[derive(Debug, Clone)]
pub struct CollectionBuilder<'a> {
    db: &'a Database,
    name: String,
    tier: Option<StorageTier>,
    ttl_ms: Option<u64>,
    eviction: Option<EvictionPolicy>,
    compartment: Option<String>,
    fle_policy: Option<lilia_core::FlePolicy>,
}

impl<'a> CollectionBuilder<'a> {
    #[must_use]
    pub(crate) fn new(db: &'a Database, name: impl Into<String>) -> Self {
        Self {
            db,
            name: name.into(),
            tier: None,
            ttl_ms: None,
            eviction: None,
            compartment: None,
            fle_policy: None,
        }
    }

    #[must_use]
    pub const fn tier(mut self, tier: StorageTier) -> Self {
        self.tier = Some(tier);
        self
    }

    #[must_use]
    pub const fn memory(self) -> Self {
        self.tier(StorageTier::Memory)
    }

    #[must_use]
    pub const fn buffered(self) -> Self {
        self.tier(StorageTier::Buffered)
    }

    #[must_use]
    pub const fn disk(self) -> Self {
        self.tier(StorageTier::Disk)
    }

    #[must_use]
    pub const fn ttl_ms(mut self, ttl_ms: u64) -> Self {
        self.ttl_ms = Some(ttl_ms);
        self
    }

    #[must_use]
    pub fn ttl(mut self, duration: Duration) -> Self {
        self.ttl_ms = Some(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX));
        self
    }

    #[must_use]
    pub const fn eviction(mut self, eviction: EvictionPolicy) -> Self {
        self.eviction = Some(eviction);
        self
    }

    #[must_use]
    pub const fn lru(self, max_entries: usize) -> Self {
        self.eviction(EvictionPolicy::Lru { max_entries })
    }

    #[must_use]
    pub const fn drop_oldest(self, max_entries: usize) -> Self {
        self.eviction(EvictionPolicy::DropOldest { max_entries })
    }

    #[must_use]
    pub fn compartment(mut self, id: impl Into<String>) -> Self {
        self.compartment = Some(id.into());
        self
    }

    #[must_use]
    pub fn field_encryption(
        mut self,
        path: impl Into<String>,
        algo: lilia_core::FieldCipherAlgorithm,
        key_id: impl Into<String>,
    ) -> Self {
        let policy = self
            .fle_policy
            .take()
            .unwrap_or_default()
            .field(path, algo, key_id);
        self.fle_policy = Some(policy);
        self
    }

    #[must_use]
    pub fn fle_policy(mut self, policy: lilia_core::FlePolicy) -> Self {
        self.fle_policy = Some(policy);
        self
    }

    fn apply_spec(&self, kind: CollectionKind) -> Result<CollectionSpec> {
        validate_name("collection", &self.name)?;
        let existing = self.db.catalog.spec_for(kind, &self.name);
        if self.tier.is_none()
            && self.ttl_ms.is_none()
            && self.eviction.is_none()
            && self.compartment.is_none()
            && self.fle_policy.is_none()
        {
            return Ok(existing);
        }
        let mut spec = CollectionSpec::new(kind, &self.name, self.tier.unwrap_or(existing.tier));
        spec.ttl_ms = self.ttl_ms.or(existing.ttl_ms);
        spec.eviction = self.eviction.unwrap_or(existing.eviction);
        spec.durability = existing.durability;
        spec.compartment = self.compartment.clone().or(existing.compartment);
        spec.fle_policy = self.fle_policy.clone().or(existing.fle_policy);
        self.db.set_collection_spec(spec.clone())?;
        Ok(spec)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn configure(self, kind: CollectionKind) -> Result<CollectionSpec> {
        self.apply_spec(kind)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn kv(self) -> Result<KvCollection<'a>> {
        self.apply_spec(CollectionKind::Kv)?;
        Ok(KvCollection {
            db: self.db,
            name: self.name,
        })
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn json(self) -> Result<JsonCollection<'a>> {
        self.apply_spec(CollectionKind::Json)?;
        Ok(JsonCollection {
            db: self.db,
            name: self.name,
        })
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn timeseries(self) -> Result<TimeSeriesCollection<'a>> {
        self.apply_spec(CollectionKind::TimeSeries)?;
        Ok(TimeSeriesCollection {
            db: self.db,
            name: self.name,
        })
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn graph(self) -> Result<GraphCollection<'a>> {
        self.apply_spec(CollectionKind::Graph)?;
        Ok(GraphCollection {
            db: self.db,
            name: self.name,
        })
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn fts(self) -> Result<FtsCollection<'a>> {
        self.apply_spec(CollectionKind::Fts)?;
        Ok(FtsCollection {
            db: self.db,
            name: self.name,
        })
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn vector(self) -> Result<VectorCollection<'a>> {
        self.apply_spec(CollectionKind::Vector)?;
        Ok(VectorCollection {
            db: self.db,
            name: self.name,
        })
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn crdt(self) -> Result<CrdtCollection<'a>> {
        self.apply_spec(CollectionKind::Crdt)?;
        Ok(CrdtCollection {
            db: self.db,
            name: self.name,
        })
    }
}

/// Typed handle for a tier-routed KV namespace.
#[derive(Debug, Clone)]
pub struct KvCollection<'a> {
    db: &'a Database,
    name: String,
}

impl KvCollection<'_> {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn tier(&self) -> StorageTier {
        self.db.catalog.tier_for(CollectionKind::Kv, &self.name)
    }

    #[must_use]
    pub fn spec(&self) -> CollectionSpec {
        self.db.catalog.spec_for(CollectionKind::Kv, &self.name)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn get(&self, key: &[u8]) -> Result<Option<KvEntry>> {
        self.db.kv_get(&self.name, key)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn set(&self, key: &[u8], value: &[u8]) -> Result<MutationResult> {
        self.set_op(key, value, None, None)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn set_with_ttl(&self, key: &[u8], value: &[u8], ttl_ms: u64) -> Result<MutationResult> {
        let expires_at_ms = now_ms().saturating_add(i64::try_from(ttl_ms).unwrap_or(i64::MAX));
        self.set_op(key, value, None, Some(expires_at_ms))
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn set_if_version(
        &self,
        key: &[u8],
        value: &[u8],
        expected_version: Option<u64>,
    ) -> Result<MutationResult> {
        self.set_op(key, value, expected_version, None)
    }

    fn set_op(
        &self,
        key: &[u8],
        value: &[u8],
        if_version: Option<u64>,
        expires_at_ms: Option<i64>,
    ) -> Result<MutationResult> {
        let spec = self.spec();
        if spec.tier == StorageTier::Memory && if_version.is_none() {
            let eff_exp = expires_at_ms.or_else(|| {
                spec.ttl_ms
                    .map(|ttl| now_ms().saturating_add(i64::try_from(ttl).unwrap_or(i64::MAX)))
            });
            let encoded = self.db.codec.encode(value)?.into_owned();
            let ver = self
                .db
                .mem_engine
                .kv_put_direct(&self.name, key, encoded, None, eff_exp)?;
            return Ok(MutationResult {
                version: Some(ver),
                deleted: false,
                hlc: None,
                conflict_detected: None,
                tier: StorageTier::Memory,
                buffered: false,
            });
        }
        let op = BatchOperation::KvSet {
            namespace: self.name.clone(),
            key: key.to_vec(),
            value: value.to_vec(),
            if_version,
            expires_at_ms,
        };
        let mut results = self.db.batch(&[op])?;
        Ok(results.remove(0))
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn delete(&self, key: &[u8]) -> Result<MutationResult> {
        self.delete_if_version(key, None)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn delete_if_version(
        &self,
        key: &[u8],
        expected_version: Option<u64>,
    ) -> Result<MutationResult> {
        let op = BatchOperation::KvDelete {
            namespace: self.name.clone(),
            key: key.to_vec(),
            if_version: expected_version,
        };
        let mut results = self.db.batch(&[op])?;
        Ok(results.remove(0))
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn scan(&self, after: Option<&[u8]>, limit: u32) -> Result<Vec<KvEntry>> {
        self.db.kv_scan(&self.name, after, limit)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn get_blob_path(&self, key: &[u8]) -> Result<Option<std::path::PathBuf>> {
        self.db.kv_get_blob_path(&self.name, key)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn get_blob_reader(&self, key: &[u8]) -> Result<Option<std::fs::File>> {
        self.db.kv_get_blob_reader(&self.name, key)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn promote(&self) -> Result<usize> {
        self.db.promote_collection(CollectionKind::Kv, &self.name)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn demote(&self, target: StorageTier) -> Result<usize> {
        self.db
            .demote_collection(CollectionKind::Kv, &self.name, target)
    }
}

/// Typed handle for a tier-routed JSON document space.
#[derive(Debug, Clone)]
pub struct JsonCollection<'a> {
    db: &'a Database,
    name: String,
}

impl<'a> JsonCollection<'a> {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn tier(&self) -> StorageTier {
        self.db.catalog.tier_for(CollectionKind::Json, &self.name)
    }

    #[must_use]
    pub fn spec(&self) -> CollectionSpec {
        self.db.catalog.spec_for(CollectionKind::Json, &self.name)
    }

    #[must_use]
    pub fn aggregate(&self) -> crate::aggregation::AggregationPipeline<'a> {
        crate::aggregation::AggregationPipeline::new(self.db, &self.name)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn get(&self, id: &str) -> Result<Option<JsonEntry>> {
        self.db.json_get(&self.name, id)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn store_blob(&self, data: &[u8]) -> Result<lilia_core::BlobDescriptor> {
        self.db.blob_store(data)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn put(&self, id: &str, value: Value) -> Result<MutationResult> {
        self.put_if_version(id, value, None)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn put_if_version(
        &self,
        id: &str,
        value: Value,
        expected_version: Option<u64>,
    ) -> Result<MutationResult> {
        if self.tier() == StorageTier::Memory && expected_version.is_none() {
            let ver = self
                .db
                .mem_engine
                .json_put_direct(&self.name, id, value, None)?;
            return Ok(MutationResult {
                version: Some(ver),
                deleted: false,
                hlc: None,
                conflict_detected: None,
                tier: StorageTier::Memory,
                buffered: false,
            });
        }
        let op = BatchOperation::JsonPut {
            space: self.name.clone(),
            id: id.to_owned(),
            value,
            if_version: expected_version,
        };
        let mut results = self.db.batch(&[op])?;
        Ok(results.remove(0))
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn delete(&self, id: &str) -> Result<MutationResult> {
        self.delete_if_version(id, None)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn delete_if_version(
        &self,
        id: &str,
        expected_version: Option<u64>,
    ) -> Result<MutationResult> {
        let op = BatchOperation::JsonDelete {
            space: self.name.clone(),
            id: id.to_owned(),
            if_version: expected_version,
        };
        let mut results = self.db.batch(&[op])?;
        Ok(results.remove(0))
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn scan(&self, after: Option<&str>, limit: u32) -> Result<Vec<JsonEntry>> {
        self.db.json_scan(&self.name, after, limit)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn create_index(&self, index_name: &str, json_path: &str) -> Result<()> {
        self.db.create_json_index(&self.name, index_name, json_path)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn create_index_with_options(
        &self,
        index_name: &str,
        expression: &str,
        options: impl Into<lilia_core::IndexOptions>,
    ) -> Result<()> {
        self.db
            .create_json_index_with_options(&self.name, index_name, expression, options.into())
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn find(
        &self,
        query: &serde_json::Map<String, Value>,
        after_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JsonEntry>> {
        self.db.json_find(&self.name, query, after_id, limit)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn promote(&self) -> Result<usize> {
        self.db.promote_collection(CollectionKind::Json, &self.name)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn demote(&self, target: StorageTier) -> Result<usize> {
        self.db
            .demote_collection(CollectionKind::Json, &self.name, target)
    }
}

/// Typed handle for a tier-routed `TimeSeries` stream.
#[derive(Debug, Clone)]
pub struct TimeSeriesCollection<'a> {
    db: &'a Database,
    name: String,
}

impl TimeSeriesCollection<'_> {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn tier(&self) -> StorageTier {
        self.db
            .catalog
            .tier_for(CollectionKind::TimeSeries, &self.name)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn append(&self, timestamp: i64, value: f64) -> Result<()> {
        self.append_points(&[TimeSeriesPoint { timestamp, value }])
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn append_points(&self, points: &[TimeSeriesPoint]) -> Result<()> {
        self.db.ts_append_points(&self.name, points)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn append_with_resolution(
        &self,
        resolution_ms: i64,
        points: &[TimeSeriesPoint],
    ) -> Result<()> {
        self.db.ts_append_chunk(&self.name, resolution_ms, points)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn query_range(&self, start_time: i64, end_time: i64) -> Result<Vec<TimeSeriesPoint>> {
        self.db.ts_query_range(&self.name, start_time, end_time)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn promote(&self) -> Result<usize> {
        self.db
            .promote_collection(CollectionKind::TimeSeries, &self.name)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn demote(&self, target: StorageTier) -> Result<usize> {
        self.db
            .demote_collection(CollectionKind::TimeSeries, &self.name, target)
    }
}

/// Typed handle for a tier-routed Property Graph.
#[derive(Debug, Clone)]
pub struct GraphCollection<'a> {
    db: &'a Database,
    name: String,
}

impl GraphCollection<'_> {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn tier(&self) -> StorageTier {
        self.db.catalog.tier_for(CollectionKind::Graph, &self.name)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn set_node(&self, id: &str, kind: &str, metadata: &Value) -> Result<()> {
        self.db.graph_set_node(&self.name, id, kind, metadata)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn get_node(&self, id: &str) -> Result<Option<GraphNode>> {
        self.db.graph_get_node(&self.name, id)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn delete_node(&self, id: &str) -> Result<bool> {
        self.db.graph_delete_node(&self.name, id)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn list_nodes(&self, after: Option<&str>, limit: u32) -> Result<Vec<GraphNode>> {
        self.db.graph_list_nodes(&self.name, after, limit)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn add_edge(&self, source: &str, target: &str, kind: &str) -> Result<()> {
        self.db.graph_add_edge(&self.name, source, target, kind)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn add_edge_weighted(
        &self,
        source: &str,
        target: &str,
        kind: &str,
        weight: f64,
    ) -> Result<()> {
        self.db
            .graph_add_edge_weighted(&self.name, source, target, kind, weight)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn remove_edge(&self, source: &str, target: &str, kind: &str) -> Result<bool> {
        self.db.graph_remove_edge(&self.name, source, target, kind)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn list_edges(
        &self,
        source: Option<&str>,
        target: Option<&str>,
        kind: Option<&str>,
        limit: u32,
    ) -> Result<Vec<GraphEdge>> {
        self.db
            .graph_list_edges(&self.name, source, target, kind, limit)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn reachability(
        &self,
        start_nodes: &[String],
        direction: Direction,
    ) -> Result<Vec<String>> {
        self.db
            .graph_reachability(&self.name, start_nodes, direction)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn neighbors(
        &self,
        id: &str,
        direction: Direction,
        kind: Option<&str>,
    ) -> Result<Vec<String>> {
        self.db.graph_neighbors(&self.name, id, direction, kind)
    }
}

/// Typed handle for a tier-routed FTS5 full-text collection.
#[derive(Debug, Clone)]
pub struct FtsCollection<'a> {
    db: &'a Database,
    name: String,
}

impl FtsCollection<'_> {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn tier(&self) -> StorageTier {
        self.db.catalog.tier_for(CollectionKind::Fts, &self.name)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn index(&self, doc_id: &str, content: &str) -> Result<()> {
        self.db.fts_index(&self.name, doc_id, content)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn delete(&self, doc_id: &str) -> Result<bool> {
        self.db.fts_delete(&self.name, doc_id)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<FtsHit>> {
        self.db.fts_search(&self.name, query, limit)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn suggest(
        &self,
        prefix: &str,
        max_distance: u8,
        limit: u32,
    ) -> Result<Vec<FtsSuggestion>> {
        self.db.fts_suggest(&self.name, prefix, max_distance, limit)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn fuzzy_search(
        &self,
        query: &str,
        max_distance: u8,
        limit: u32,
    ) -> Result<Vec<FtsFuzzyHit>> {
        self.db
            .fts_fuzzy_search(&self.name, query, max_distance, limit)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn load_dictionary(&self, words: &[&str]) -> Result<()> {
        self.db.fts_load_dictionary(&self.name, words)
    }
}

/// Typed handle for a tier-routed Vector collection.
#[derive(Debug, Clone)]
pub struct VectorCollection<'a> {
    db: &'a Database,
    name: String,
}

impl VectorCollection<'_> {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn tier(&self) -> StorageTier {
        self.db.catalog.tier_for(CollectionKind::Vector, &self.name)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn create_index(&self, config: HnswConfig) -> Result<()> {
        self.db.vector_create_index(&self.name, config)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn set(
        &self,
        id: &str,
        dim: u32,
        element_type: VectorElementType,
        vector: &[u8],
    ) -> Result<()> {
        self.db
            .vector_set(&self.name, id, dim, element_type, vector)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn get(&self, id: &str) -> Result<Option<(u32, VectorElementType, Vec<u8>)>> {
        self.db.vector_get(&self.name, id)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn delete(&self, id: &str) -> Result<bool> {
        self.db.vector_delete(&self.name, id)
    }

    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn search(
        &self,
        query_vector: &[u8],
        dim: u32,
        element_type: VectorElementType,
        metric: lilia_core::VectorMetric,
        limit: u32,
    ) -> Result<Vec<VectorHit>> {
        self.db
            .vector_search(&self.name, query_vector, dim, element_type, metric, limit)
    }
}

/// Typed handle for a tier-routed CRDT space.
#[derive(Debug, Clone)]
pub struct CrdtCollection<'a> {
    db: &'a Database,
    name: String,
}

impl<'a> CrdtCollection<'a> {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn tier(&self) -> StorageTier {
        self.db.catalog.tier_for(CollectionKind::Crdt, &self.name)
    }

    #[must_use]
    pub fn counter(&self, id: impl Into<String>) -> CounterStore<'a> {
        self.db.crdt().counter(&self.name, id)
    }

    #[must_use]
    pub fn set(&self, id: impl Into<String>) -> SetStore<'a> {
        self.db.crdt().set(&self.name, id)
    }

    #[must_use]
    pub fn doc(&self, id: impl Into<String>) -> DocStore<'a> {
        self.db.crdt().doc(&self.name, id)
    }
}

/// Builder / staged write-set for atomic cross-tier (`Disk` + `Memory`) transactions via [`Database::transaction`].
#[derive(Debug)]
pub struct CrossTierTransaction<'a> {
    db: &'a Database,
    ops: Vec<BatchOperation>,
    rolled_back: bool,
}

impl<'a> CrossTierTransaction<'a> {
    #[must_use]
    pub fn new(db: &'a Database) -> Self {
        Self {
            db,
            ops: Vec::new(),
            rolled_back: false,
        }
    }

    pub fn push(&mut self, op: BatchOperation) {
        self.ops.push(op);
    }

    pub fn set_rollback_only(&mut self) {
        self.rolled_back = true;
    }

    #[must_use]
    pub fn db(&self) -> &'a Database {
        self.db
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn kv_set(&mut self, namespace: &str, key: &[u8], value: &[u8]) {
        self.ops.push(BatchOperation::KvSet {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
            value: value.to_vec(),
            if_version: None,
            expires_at_ms: None,
        });
    }

    pub fn kv_set_if_version(
        &mut self,
        namespace: &str,
        key: &[u8],
        value: &[u8],
        if_version: Option<u64>,
    ) {
        self.ops.push(BatchOperation::KvSet {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
            value: value.to_vec(),
            if_version,
            expires_at_ms: None,
        });
    }

    pub fn kv_set_with_ttl(
        &mut self,
        namespace: &str,
        key: &[u8],
        value: &[u8],
        expires_at_ms: i64,
    ) {
        self.ops.push(BatchOperation::KvSet {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
            value: value.to_vec(),
            if_version: None,
            expires_at_ms: Some(expires_at_ms),
        });
    }

    pub fn kv_delete(&mut self, namespace: &str, key: &[u8], if_version: Option<u64>) {
        self.ops.push(BatchOperation::KvDelete {
            namespace: namespace.to_owned(),
            key: key.to_vec(),
            if_version,
        });
    }

    pub fn json_put(&mut self, space: &str, id: &str, value: Value, if_version: Option<u64>) {
        self.ops.push(BatchOperation::JsonPut {
            space: space.to_owned(),
            id: id.to_owned(),
            value,
            if_version,
        });
    }

    pub fn json_delete(&mut self, space: &str, id: &str, if_version: Option<u64>) {
        self.ops.push(BatchOperation::JsonDelete {
            space: space.to_owned(),
            id: id.to_owned(),
            if_version,
        });
    }

    pub fn graph_set_node(&mut self, graph: &str, id: &str, kind: &str, metadata: Value) {
        self.graph_set_node_doc(graph, id, kind, None, None, metadata);
    }

    /// Stages a graph node linked to the JSON document `(doc_space, doc_id)`; `doc_id` defaults
    /// to the node id. Declared references are verified when the transaction commits.
    pub fn graph_set_node_doc(
        &mut self,
        graph: &str,
        id: &str,
        kind: &str,
        doc_space: Option<&str>,
        doc_id: Option<&str>,
        metadata: Value,
    ) {
        self.ops.push(BatchOperation::GraphSetNode {
            graph: graph.to_owned(),
            id: id.to_owned(),
            kind: kind.to_owned(),
            metadata,
            doc_space: doc_space.map(ToOwned::to_owned),
            doc_id: doc_id.map(ToOwned::to_owned),
        });
    }

    pub fn graph_add_edge(
        &mut self,
        graph: &str,
        source: &str,
        target: &str,
        kind: &str,
        weight: f64,
    ) {
        self.ops.push(BatchOperation::GraphAddEdge {
            graph: graph.to_owned(),
            source: source.to_owned(),
            target: target.to_owned(),
            kind: kind.to_owned(),
            weight,
        });
    }

    pub fn fts_index(&mut self, collection: &str, doc_id: &str, content: &str) {
        self.ops.push(BatchOperation::FtsIndex {
            collection: collection.to_owned(),
            doc_id: doc_id.to_owned(),
            content: content.to_owned(),
        });
    }

    pub fn vector_set(
        &mut self,
        collection: &str,
        id: &str,
        dim: u32,
        element_type: VectorElementType,
        vector: &[u8],
    ) {
        self.ops.push(BatchOperation::VectorSet {
            collection: collection.to_owned(),
            id: id.to_owned(),
            dim,
            element_type,
            vector: vector.to_vec(),
        });
    }

    /// # Errors
    /// Returns `Conflict` errors.
    pub fn commit(self) -> Result<Vec<MutationResult>> {
        if self.rolled_back {
            return Err(LiliaError::new(
                ErrorCode::Conflict,
                "cross-tier transaction aborted via rollback",
                false,
            ));
        }
        if self.ops.is_empty() {
            return Ok(Vec::new());
        }
        self.db.batch(&self.ops)
    }
}
