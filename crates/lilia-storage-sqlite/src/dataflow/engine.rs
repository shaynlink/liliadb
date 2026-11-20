use parking_lot::RwLock;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

use lilia_core::{BatchOperation, Diff, Result};
use lilia_sqlite_native::{ChangeOp, ChangesetIterator, NativeConnection};

use super::operators::DataflowSource;
use super::pipeline::DataflowPipeline;
use super::record::DataflowDoc;
use super::view::DataflowView;

/// Central engine managing reactive differential dataflow pipelines and views.
#[derive(Debug, Default)]
pub struct DataflowEngine {
    pub(crate) views: RwLock<HashMap<String, Arc<DataflowView>>>,
    pub(crate) pipelines: RwLock<Vec<Arc<DataflowPipeline>>>,
}

impl DataflowEngine {
    #[must_use]
    pub fn new() -> Self {
        Self {
            views: RwLock::new(HashMap::new()),
            pipelines: RwLock::new(Vec::new()),
        }
    }

    /// Checks if there are active registered dataflow pipelines.
    #[must_use]
    pub fn has_active_pipelines(&self) -> bool {
        let guard = self.pipelines.read();
        !guard.is_empty()
    }

    /// Returns an existing dataflow view by name.
    #[must_use]
    pub fn get_view(&self, name: &str) -> Option<Arc<DataflowView>> {
        let guard = self.views.read();
        guard.get(name).cloned()
    }

    /// Lists all registered dataflow view names.
    #[must_use]
    pub fn list_views(&self) -> Vec<String> {
        let guard = self.views.read();
        guard.keys().cloned().collect()
    }

    /// Unregisters and drops a dataflow view and its associated pipeline.
    pub fn drop_view(&self, name: &str, conn: Option<&NativeConnection>) -> bool {
        let mut views = self.views.write();
        let mut pipelines = self.pipelines.write();

        let removed_view = views.remove(name).is_some();
        pipelines.retain(|p| p.view.name() != name);

        if let Some(c) = conn {
            let _ = c.exec(&format!(
                "DELETE FROM _lilia_dataflow_views WHERE name = '{name}';"
            ));
        }

        removed_view
    }

    /// Ensures the catalog table for dataflow views exists.
    ///
    /// # Errors
    /// Propagates SQL errors.
    pub fn ensure_catalog(conn: &NativeConnection) -> Result<()> {
        conn.exec(
            "CREATE TABLE IF NOT EXISTS _lilia_dataflow_views (
                name TEXT PRIMARY KEY,
                source_name TEXT NOT NULL,
                definition TEXT NOT NULL,
                state_json TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );",
        )
    }

    /// Loads persisted dataflow views from database on startup for warm start.
    pub fn load_persisted_views(&self, conn: &NativeConnection) {
        let _ = Self::ensure_catalog(conn);
        if let Ok(mut stmt) = conn
            .prepare("SELECT name, source_name, definition, state_json FROM _lilia_dataflow_views;")
        {
            while let Ok(true) = stmt.step() {
                let name = stmt.column_text(0).to_string();
                let source_name = stmt.column_text(1).to_string();
                let definition = stmt.column_text(2).to_string();
                let state_json = stmt.column_text(3);

                let view = Arc::new(DataflowView::new(&name, &source_name, &definition));
                view.load_state_from_json(state_json);

                let mut views = self.views.write();
                views.insert(name, view);
            }
        }
    }

    /// Registers a compiled pipeline, performs initial snapshot bootstrap, and links cascading listeners.
    ///
    /// # Errors
    /// Propagates SQL or bootstrap errors.
    pub fn register_pipeline(
        &self,
        pipeline: Arc<DataflowPipeline>,
        conn: &NativeConnection,
    ) -> Result<()> {
        Self::ensure_catalog(conn)?;

        let view_name = pipeline.view.name().to_string();

        // 1. Initial snapshot bootstrap
        match &pipeline.source {
            DataflowSource::Collection(collection) => {
                self.bootstrap_collection(&pipeline, collection, conn);
            }
            DataflowSource::View(parent_view_name) => {
                if let Some(parent_view) = self.get_view(parent_view_name) {
                    // Bootstrap from parent view's existing state
                    for (k, v) in parent_view.all() {
                        let doc = DataflowDoc::new(&k, json!({ "key": k.clone(), "value": v }));
                        pipeline.process_diff(Diff::insert(doc), Some(conn));
                    }

                    // Register downstream cascade callback
                    let child_pipeline = Arc::clone(&pipeline);
                    parent_view.register_downstream(move |view_diff| {
                        let doc = DataflowDoc::new(
                            &view_diff.key,
                            json!({ "key": view_diff.key.clone(), "value": view_diff.new_value.unwrap_or(0.0) }),
                        );
                        let diff = Diff::new(doc, view_diff.weight);
                        child_pipeline.process_diff(diff, None);
                    });
                }
            }
        }

        // Persist initial view state
        let _ = pipeline.view.persist_state(conn);

        // Store view and pipeline in engine registry
        {
            let mut views = self.views.write();
            views.insert(view_name, Arc::clone(&pipeline.view));
        }
        {
            let mut pipelines = self.pipelines.write();
            pipelines.push(pipeline);
        }

        Ok(())
    }

    fn bootstrap_collection(
        &self,
        pipeline: &Arc<DataflowPipeline>,
        collection: &str,
        conn: &NativeConnection,
    ) {
        let query = "SELECT id, value, format, dict_id FROM _lilia_json WHERE space = ?1;";
        let Ok(mut stmt) = conn.prepare(query) else {
            return;
        };
        let _ = stmt.bind_text_static(1, collection);

        while let Ok(true) = stmt.step() {
            let id = stmt.column_text(0).to_string();
            let val = match stmt.column_type(1) {
                lilia_sqlite_native::ffi::SQLITE_BLOB => {
                    let blob = stmt.column_blob(1);
                    if blob.starts_with(b"LBJ\x01") {
                        if let Ok(doc) = lilia_core::json::BinaryDoc::from_bytes(blob.to_vec()) {
                            doc.to_json_value()
                        } else {
                            serde_json::Value::Null
                        }
                    } else if let Ok(s) = std::str::from_utf8(blob) {
                        serde_json::from_str(s).unwrap_or(serde_json::Value::Null)
                    } else {
                        serde_json::Value::Null
                    }
                }
                _ => {
                    let text = stmt.column_text(1);
                    serde_json::from_str(text).unwrap_or(serde_json::Value::Null)
                }
            };

            let doc = DataflowDoc::new(id, val);
            pipeline.process_diff(Diff::insert(doc), Some(conn));
        }
    }

    /// Dispatches committed batch operations directly to active dataflow pipelines.
    pub fn process_operations(&self, operations: &[BatchOperation], conn: &NativeConnection) {
        if operations.is_empty() || !self.has_active_pipelines() {
            return;
        }

        let pipelines: Vec<Arc<DataflowPipeline>> = {
            let guard = self.pipelines.read();
            guard.clone()
        };

        for op in operations {
            match op {
                BatchOperation::JsonPut {
                    space, id, value, ..
                }
                | BatchOperation::JsonPutHlc {
                    space, id, value, ..
                } => {
                    let doc = DataflowDoc::new(id.clone(), value.clone());
                    for p in &pipelines {
                        if let DataflowSource::Collection(ref col) = p.source {
                            if col == space {
                                p.process_diff(Diff::insert(doc.clone()), Some(conn));
                                let _ = p.view.persist_state(conn);
                            }
                        }
                    }
                }
                BatchOperation::JsonDelete { space, id, .. } => {
                    let doc = DataflowDoc::new(id.clone(), serde_json::Value::Null);
                    for p in &pipelines {
                        if let DataflowSource::Collection(ref col) = p.source {
                            if col == space {
                                p.process_diff(Diff::delete(doc.clone()), Some(conn));
                                let _ = p.view.persist_state(conn);
                            }
                        }
                    }
                }
                BatchOperation::GraphAddEdge {
                    source,
                    target,
                    kind,
                    ..
                } => {
                    for p in &pipelines {
                        if let Some(ref jg) = p.join_graph {
                            jg.on_graph_edge_set(source, target, kind, conn);
                        }
                    }
                }
                BatchOperation::GraphRemoveEdge { source, kind, .. } => {
                    for p in &pipelines {
                        if let Some(ref jg) = p.join_graph {
                            jg.on_graph_edge_remove(source, kind);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Dispatches SQLite session changesets to active dataflow pipelines.
    pub fn process_changeset(&self, changeset: &[u8], conn: &NativeConnection) {
        if changeset.is_empty() || !self.has_active_pipelines() {
            return;
        }

        let Ok(mut iter) = ChangesetIterator::new(changeset) else {
            return;
        };

        let pipelines: Vec<Arc<DataflowPipeline>> = {
            let guard = self.pipelines.read();
            guard.clone()
        };

        while let Ok(Some(change)) = iter.next_change() {
            if change.table == "_lilia_json" {
                match change.op {
                    ChangeOp::Insert => {
                        let space = change
                            .new_values
                            .first()
                            .and_then(|v| v.as_ref())
                            .and_then(|v| v.as_str());
                        let id = change
                            .new_values
                            .get(1)
                            .and_then(|v| v.as_ref())
                            .and_then(|v| v.as_str());
                        let val = change.new_values.get(2).and_then(|v| v.as_ref());
                        if let (Some(space), Some(id), Some(val)) = (space, id, val) {
                            let doc = DataflowDoc::new(id, val.clone());
                            for p in &pipelines {
                                if let DataflowSource::Collection(ref col) = p.source {
                                    if col == space {
                                        p.process_diff(Diff::insert(doc.clone()), Some(conn));
                                        let _ = p.view.persist_state(conn);
                                    }
                                }
                            }
                        }
                    }
                    ChangeOp::Delete => {
                        let space = change
                            .old_values
                            .first()
                            .and_then(|v| v.as_ref())
                            .and_then(|v| v.as_str());
                        let id = change
                            .old_values
                            .get(1)
                            .and_then(|v| v.as_ref())
                            .and_then(|v| v.as_str());
                        let val = change
                            .old_values
                            .get(2)
                            .and_then(|v| v.as_ref())
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        if let (Some(space), Some(id)) = (space, id) {
                            let doc = DataflowDoc::new(id, val);
                            for p in &pipelines {
                                if let DataflowSource::Collection(ref col) = p.source {
                                    if col == space {
                                        p.process_diff(Diff::delete(doc.clone()), Some(conn));
                                        let _ = p.view.persist_state(conn);
                                    }
                                }
                            }
                        }
                    }
                    ChangeOp::Update => {
                        let space = change
                            .new_values
                            .first()
                            .and_then(|v| v.as_ref())
                            .and_then(|v| v.as_str())
                            .or_else(|| {
                                change
                                    .old_values
                                    .first()
                                    .and_then(|v| v.as_ref())
                                    .and_then(|v| v.as_str())
                            });
                        let id = change
                            .new_values
                            .get(1)
                            .and_then(|v| v.as_ref())
                            .and_then(|v| v.as_str())
                            .or_else(|| {
                                change
                                    .old_values
                                    .get(1)
                                    .and_then(|v| v.as_ref())
                                    .and_then(|v| v.as_str())
                            });
                        let old_val = change
                            .old_values
                            .get(2)
                            .and_then(|v| v.as_ref())
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        let new_val = change
                            .new_values
                            .get(2)
                            .and_then(|v| v.as_ref())
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);

                        if let (Some(space), Some(id)) = (space, id) {
                            let old_doc = DataflowDoc::new(id, old_val);
                            let new_doc = DataflowDoc::new(id, new_val);
                            for p in &pipelines {
                                if let DataflowSource::Collection(ref col) = p.source {
                                    if col == space {
                                        p.process_diff(Diff::delete(old_doc.clone()), Some(conn));
                                        p.process_diff(Diff::insert(new_doc.clone()), Some(conn));
                                        let _ = p.view.persist_state(conn);
                                    }
                                }
                            }
                        }
                    }
                }
            } else if change.table == "_lilia_graph_edges" {
                let source = change
                    .new_values
                    .get(1)
                    .and_then(|v| v.as_ref())
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        change
                            .old_values
                            .get(1)
                            .and_then(|v| v.as_ref())
                            .and_then(|v| v.as_str())
                    });
                let target = change
                    .new_values
                    .get(2)
                    .and_then(|v| v.as_ref())
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        change
                            .old_values
                            .get(2)
                            .and_then(|v| v.as_ref())
                            .and_then(|v| v.as_str())
                    });
                let kind = change
                    .new_values
                    .get(3)
                    .and_then(|v| v.as_ref())
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        change
                            .old_values
                            .get(3)
                            .and_then(|v| v.as_ref())
                            .and_then(|v| v.as_str())
                    });

                if let (Some(source), Some(target), Some(kind)) = (source, target, kind) {
                    for p in &pipelines {
                        if let Some(ref jg) = p.join_graph {
                            match change.op {
                                ChangeOp::Insert | ChangeOp::Update => {
                                    jg.on_graph_edge_set(source, target, kind, conn);
                                }
                                ChangeOp::Delete => {
                                    jg.on_graph_edge_remove(source, kind);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
