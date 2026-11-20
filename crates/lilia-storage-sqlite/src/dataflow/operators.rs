use parking_lot::RwLock;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use lilia_core::{
    DataflowAggKind, Diff, DiffAvg, DiffCount, DiffMax, DiffMin, DiffSum, DiffWeight,
};
use lilia_sqlite_native::NativeConnection;

use super::record::DataflowDoc;

/// Dataflow pipeline source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataflowSource {
    Collection(String),
    View(String),
}

/// Filter stage applying a functional predicate to incoming differential records.
#[derive(Clone)]
pub struct FilterStage {
    predicate: Arc<dyn Fn(&DataflowDoc) -> bool + Send + Sync + 'static>,
}

impl std::fmt::Debug for FilterStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FilterStage")
    }
}

impl FilterStage {
    #[must_use]
    pub fn new<F>(predicate: F) -> Self
    where
        F: Fn(&DataflowDoc) -> bool + Send + Sync + 'static,
    {
        Self {
            predicate: Arc::new(predicate),
        }
    }

    /// Evaluates the predicate in O(1) time.
    #[must_use]
    pub fn process(&self, diff: Diff<DataflowDoc>) -> Option<Diff<DataflowDoc>> {
        if (self.predicate)(&diff.item) {
            Some(diff)
        } else {
            None
        }
    }
}

/// Specifications for joining documents with graph edges and nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinGraphSpec {
    pub foreign_key: String,
    pub edge_kind: String,
    pub target_graph: String,
}

/// Join stage connecting document deltas with graph structure in O(1) time.
#[derive(Debug, Clone)]
pub struct JoinGraphStage {
    pub spec: JoinGraphSpec,
    /// Fast in-memory cache mapping foreign_key -> (target_id, target_metadata)
    cache: Arc<RwLock<HashMap<String, (String, Value)>>>,
    /// Reverse mapping: target_node_id -> set of foreign_keys
    reverse_edges: Arc<RwLock<HashMap<String, Vec<String>>>>,
}

impl JoinGraphStage {
    #[must_use]
    pub fn new(spec: JoinGraphSpec) -> Self {
        Self {
            spec,
            cache: Arc::new(RwLock::new(HashMap::new())),
            reverse_edges: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Joins a document with the target graph node.
    pub fn process(
        &self,
        mut diff: Diff<DataflowDoc>,
        conn: Option<&NativeConnection>,
    ) -> Diff<DataflowDoc> {
        let fk_val = diff.item.get_str(&self.spec.foreign_key);
        let Some(fk) = fk_val else {
            return diff;
        };

        // Check L1 fast cache
        let cached = {
            let guard = self.cache.read();
            guard.get(fk).cloned()
        };

        let (_target_id, metadata) = if let Some(found) = cached {
            found
        } else {
            // Resolve from SQLite graph tables: _lilia_graph_edges and _lilia_graph_nodes
            let resolved = self.resolve_graph_node(fk, conn);
            if let Some(ref res) = resolved {
                let mut guard = self.cache.write();
                guard.insert(fk.to_string(), res.clone());
                let mut rev_guard = self.reverse_edges.write();
                rev_guard
                    .entry(res.0.clone())
                    .or_default()
                    .push(fk.to_string());
            }
            resolved.unwrap_or_else(|| (String::new(), Value::Object(serde_json::Map::new())))
        };

        let singular = self.spec.target_graph.trim_end_matches('s');
        diff.item.set_field(singular, metadata.clone());
        if let Value::Object(ref map) = metadata {
            for (k, v) in map {
                diff.item.set_field(&format!("{singular}.{k}"), v.clone());
            }
        }

        diff
    }

    /// Updates stage index when a graph edge or node changes.
    pub fn on_graph_edge_set(
        &self,
        source: &str,
        target: &str,
        kind: &str,
        conn: &NativeConnection,
    ) {
        if kind == self.spec.edge_kind {
            if let Some((_, meta)) = self.lookup_node_metadata(target, conn) {
                let mut guard = self.cache.write();
                guard.insert(source.to_string(), (target.to_string(), meta));
                let mut rev_guard = self.reverse_edges.write();
                rev_guard
                    .entry(target.to_string())
                    .or_default()
                    .push(source.to_string());
            }
        }
    }

    /// Evicts or updates when an edge is removed.
    pub fn on_graph_edge_remove(&self, source: &str, kind: &str) {
        if kind == self.spec.edge_kind {
            let mut guard = self.cache.write();
            guard.remove(source);
        }
    }

    fn resolve_graph_node(
        &self,
        source: &str,
        conn: Option<&NativeConnection>,
    ) -> Option<(String, Value)> {
        let conn = conn?;
        let edge_query =
            "SELECT target FROM _lilia_graph_edges WHERE source = ?1 AND kind = ?2 LIMIT 1;";
        let target_id = if let Ok(mut stmt) = conn.prepare(edge_query) {
            let _ = stmt.bind_text_static(1, source);
            let _ = stmt.bind_text_static(2, &self.spec.edge_kind);
            if let Ok(true) = stmt.step() {
                stmt.column_text(0).to_string()
            } else {
                return None;
            }
        } else {
            return None;
        };

        self.lookup_node_metadata(&target_id, conn)
    }

    fn lookup_node_metadata(
        &self,
        node_id: &str,
        conn: &NativeConnection,
    ) -> Option<(String, Value)> {
        let node_query = "SELECT metadata FROM _lilia_graph_nodes WHERE id = ?1 LIMIT 1;";
        if let Ok(mut stmt) = conn.prepare(node_query) {
            let _ = stmt.bind_text_static(1, node_id);
            if let Ok(true) = stmt.step() {
                let meta_str = stmt.column_text(0);
                let meta: Value = serde_json::from_str(meta_str).unwrap_or(Value::Null);
                return Some((node_id.to_string(), meta));
            }
        }
        None
    }
}

/// Internal state per aggregation group key.
#[derive(Debug, Clone, Default)]
pub struct GroupAggregateState {
    pub sum: DiffSum,
    pub count: DiffCount,
    pub avg: DiffAvg,
    pub min: DiffMin,
    pub max: DiffMax,
}

impl GroupAggregateState {
    #[must_use]
    pub fn current_value(&self, kind: &DataflowAggKind) -> Option<f64> {
        if self.count.value() <= 0 {
            return None;
        }
        match kind {
            DataflowAggKind::Sum(_) => Some(self.sum.value()),
            DataflowAggKind::Count =>
            {
                #[allow(clippy::cast_precision_loss)]
                Some(self.count.value() as f64)
            }
            DataflowAggKind::Avg(_) => self.avg.value(),
            DataflowAggKind::Min(_) => self.min.value(),
            DataflowAggKind::Max(_) => self.max.value(),
        }
    }

    pub fn update(&mut self, kind: &DataflowAggKind, val: f64, weight: DiffWeight) {
        self.count.update(weight);
        match kind {
            DataflowAggKind::Sum(_) => self.sum.update(val, weight),
            DataflowAggKind::Count => {}
            DataflowAggKind::Avg(_) => self.avg.update(val, weight),
            DataflowAggKind::Min(_) => self.min.update(val, weight),
            DataflowAggKind::Max(_) => self.max.update(val, weight),
        }
    }
}

/// Aggregate stage computing group-by aggregations differentially in O(1) constant time.
#[derive(Debug, Clone)]
pub struct AggregateStage {
    pub group_by_field: String,
    pub agg_kind: DataflowAggKind,
    pub groups: Arc<RwLock<HashMap<String, GroupAggregateState>>>,
}

impl AggregateStage {
    #[must_use]
    pub fn new(group_by_field: impl Into<String>, agg_kind: DataflowAggKind) -> Self {
        Self {
            group_by_field: group_by_field.into(),
            agg_kind,
            groups: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Processes a differential record in O(1) constant time.
    /// Returns: (group_key, old_aggregated_value, new_aggregated_value, weight)
    pub fn process(
        &self,
        diff: &Diff<DataflowDoc>,
    ) -> Option<(String, Option<f64>, Option<f64>, DiffWeight)> {
        let key_str = diff.item.get_str(&self.group_by_field)?;
        let key = key_str.to_string();

        let val = match &self.agg_kind {
            DataflowAggKind::Sum(field)
            | DataflowAggKind::Avg(field)
            | DataflowAggKind::Min(field)
            | DataflowAggKind::Max(field) => diff.item.get_f64(field).unwrap_or(0.0),
            DataflowAggKind::Count => 1.0,
        };

        let mut guard = self.groups.write();
        let state = guard.entry(key.clone()).or_default();
        let old_val = state.current_value(&self.agg_kind);

        state.update(&self.agg_kind, val, diff.weight);

        let new_val = state.current_value(&self.agg_kind);

        if state.count.value() <= 0 {
            guard.remove(&key);
        }

        Some((key, old_val, new_val, diff.weight))
    }
}
