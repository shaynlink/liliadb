use lilia_core::{DiffWeight, Result, ViewDiff};
use lilia_sqlite_native::NativeConnection;
use parking_lot::RwLock;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// Callback type for listening to real-time differential view updates.
pub type ViewSubscriptionCallback = Arc<dyn Fn(ViewDiff) + Send + Sync + 'static>;

/// A reactive materialized view maintained differentially in O(1) time.
#[derive(Clone)]
pub struct DataflowView {
    pub(crate) name: String,
    pub(crate) source_name: String,
    pub(crate) query_definition: String,
    pub(crate) entries: Arc<RwLock<HashMap<String, f64>>>,
    pub(crate) listeners: Arc<RwLock<Vec<ViewSubscriptionCallback>>>,
    pub(crate) downstream_cascades: Arc<RwLock<Vec<ViewSubscriptionCallback>>>,
}

impl DataflowView {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        source_name: impl Into<String>,
        query_definition: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            source_name: source_name.into(),
            query_definition: query_definition.into(),
            entries: Arc::new(RwLock::new(HashMap::new())),
            listeners: Arc::new(RwLock::new(Vec::new())),
            downstream_cascades: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// View name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Source collection or parent view name.
    #[must_use]
    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    /// Definition string for introspection.
    #[must_use]
    pub fn definition(&self) -> &str {
        &self.query_definition
    }

    /// Point lookup for an aggregated key in O(1) constant time.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<f64> {
        let guard = self.entries.read();
        guard.get(key).copied()
    }

    /// Number of active aggregated keys in the view.
    #[must_use]
    pub fn len(&self) -> usize {
        let guard = self.entries.read();
        guard.len()
    }

    /// Checks if the view has zero entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns a full snapshot of all keys and their aggregated values.
    #[must_use]
    pub fn all(&self) -> HashMap<String, f64> {
        let guard = self.entries.read();
        guard.clone()
    }

    /// Converts the current view state into a JSON object.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        let guard = self.entries.read();
        json!(*guard)
    }

    /// Subscribes a reactive callback to receive differential changes in real time.
    pub fn subscribe<F>(&self, callback: F)
    where
        F: Fn(ViewDiff) + Send + Sync + 'static,
    {
        let mut guard = self.listeners.write();
        guard.push(Arc::new(callback));
    }

    /// Registers a downstream callback (used by cascading functional views).
    pub(crate) fn register_downstream<F>(&self, callback: F)
    where
        F: Fn(ViewDiff) + Send + Sync + 'static,
    {
        let mut guard = self.downstream_cascades.write();
        guard.push(Arc::new(callback));
    }

    /// Applies a differential update in O(1) constant time, then dispatches to listeners and downstream cascades.
    pub fn apply_diff(
        &self,
        key: &str,
        old_val: Option<f64>,
        new_val: Option<f64>,
        weight: DiffWeight,
    ) {
        {
            let mut guard = self.entries.write();
            match new_val {
                Some(val) => {
                    guard.insert(key.to_string(), val);
                }
                None => {
                    guard.remove(key);
                }
            }
        }

        let diff = ViewDiff {
            view_name: self.name.clone(),
            key: key.to_string(),
            old_value: old_val,
            new_value: new_val,
            weight,
        };

        // Notify user subscribers
        let listeners: Vec<ViewSubscriptionCallback> = {
            let guard = self.listeners.read();
            guard.clone()
        };
        for listener in listeners {
            listener(diff.clone());
        }

        // Notify downstream cascade pipelines
        let cascades: Vec<ViewSubscriptionCallback> = {
            let guard = self.downstream_cascades.read();
            guard.clone()
        };
        for cascade in cascades {
            cascade(diff.clone());
        }
    }

    /// Persists current view state into the database for instant warm start upon restart.
    ///
    /// # Errors
    /// Propagates SQL errors.
    pub fn persist_state(&self, conn: &NativeConnection) -> Result<()> {
        let state_json = serde_json::to_string(&self.all()).unwrap_or_else(|_| "{}".to_string());
        let now_ms = crate::storage::now_ms();
        let mut stmt = conn.prepare(
            "INSERT INTO _lilia_dataflow_views (name, source_name, definition, state_json, updated_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(name) DO UPDATE SET \
             source_name = excluded.source_name, \
             definition = excluded.definition, \
             state_json = excluded.state_json, \
             updated_at_ms = excluded.updated_at_ms;",
        )?;
        stmt.bind_text_static(1, &self.name)?;
        stmt.bind_text_static(2, &self.source_name)?;
        stmt.bind_text_static(3, &self.query_definition)?;
        stmt.bind_text_static(4, &state_json)?;
        stmt.bind_int64(5, now_ms)?;
        stmt.step()?;
        Ok(())
    }

    /// Loads persisted state into memory.
    pub fn load_state_from_json(&self, json_str: &str) {
        if let Ok(map) = serde_json::from_str::<HashMap<String, f64>>(json_str) {
            let mut guard = self.entries.write();
            *guard = map;
        }
    }
}

impl std::fmt::Debug for DataflowView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataflowView")
            .field("name", &self.name)
            .field("source_name", &self.source_name)
            .field("entries_count", &self.len())
            .finish()
    }
}
