use lilia_core::{DataflowAggKind, Diff, ErrorCode, LiliaError, Result};
use lilia_sqlite_native::NativeConnection;
use serde::de::DeserializeOwned;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::operators::{
    AggregateStage, DataflowSource, FilterStage, JoinGraphSpec, JoinGraphStage,
};
use super::record::DataflowDoc;
use super::view::DataflowView;
use crate::Database;

static NEXT_PIPELINE_ID: AtomicU64 = AtomicU64::new(1);

/// Helper for configuring aggregation operations on streams.
#[derive(Debug, Clone, Copy, Default)]
pub struct DataflowAggregator;

impl DataflowAggregator {
    #[must_use]
    pub fn sum(self, field: impl Into<String>) -> DataflowAggKind {
        DataflowAggKind::Sum(field.into())
    }

    #[must_use]
    pub fn count(self) -> DataflowAggKind {
        DataflowAggKind::Count
    }

    #[must_use]
    pub fn avg(self, field: impl Into<String>) -> DataflowAggKind {
        DataflowAggKind::Avg(field.into())
    }

    #[must_use]
    pub fn min(self, field: impl Into<String>) -> DataflowAggKind {
        DataflowAggKind::Min(field.into())
    }

    #[must_use]
    pub fn max(self, field: impl Into<String>) -> DataflowAggKind {
        DataflowAggKind::Max(field.into())
    }
}

/// A compiled reactive differential pipeline.
#[derive(Clone)]
pub struct DataflowPipeline {
    pub id: u64,
    pub source: DataflowSource,
    pub filter: Option<FilterStage>,
    pub join_graph: Option<JoinGraphStage>,
    pub aggregate: Option<AggregateStage>,
    pub view: Arc<DataflowView>,
}

impl DataflowPipeline {
    /// Ingests an incoming differential record (+Δ or -Δ), running it through all pipeline stages in O(1).
    pub fn process_diff(&self, diff: Diff<DataflowDoc>, conn: Option<&NativeConnection>) {
        // 1. Filter stage
        let filtered = if let Some(ref stage) = self.filter {
            match stage.process(diff) {
                Some(d) => d,
                None => return,
            }
        } else {
            diff
        };

        // 2. Join graph stage
        let joined = if let Some(ref stage) = self.join_graph {
            stage.process(filtered, conn)
        } else {
            filtered
        };

        // 3. Aggregate stage
        if let Some(ref stage) = self.aggregate {
            if let Some((key, old_val, new_val, weight)) = stage.process(&joined) {
                self.view.apply_diff(&key, old_val, new_val, weight);
            }
        } else {
            let key = joined.item.id.clone();
            self.view.apply_diff(&key, None, Some(1.0), joined.weight);
        }
    }
}

impl std::fmt::Debug for DataflowPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataflowPipeline")
            .field("id", &self.id)
            .field("source", &self.source)
            .field("has_filter", &self.filter.is_some())
            .field("join_graph", &self.join_graph)
            .field("aggregate", &self.aggregate)
            .field("view_name", &self.view.name())
            .finish()
    }
}

/// Fluent builder for declarative continuous dataflow pipelines.
pub struct DataflowBuilder<'a> {
    db: &'a Database,
    source: Option<DataflowSource>,
    filter: Option<FilterStage>,
    join_graph: Option<JoinGraphStage>,
    aggregate: Option<AggregateStage>,
}

impl<'a> std::fmt::Debug for DataflowBuilder<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataflowBuilder")
            .field("source", &self.source)
            .field("has_filter", &self.filter.is_some())
            .field("join_graph", &self.join_graph)
            .field("aggregate", &self.aggregate)
            .finish()
    }
}

impl<'a> DataflowBuilder<'a> {
    #[must_use]
    pub(crate) fn new(db: &'a Database) -> Self {
        Self {
            db,
            source: None,
            filter: None,
            join_graph: None,
            aggregate: None,
        }
    }

    /// Sets the source of the dataflow pipeline to a document collection.
    #[must_use]
    pub fn from_collection(mut self, collection: impl Into<String>) -> Self {
        self.source = Some(DataflowSource::Collection(collection.into()));
        self
    }

    /// Sets the source of the dataflow pipeline to a parent reactive view, enabling functional cascades.
    #[must_use]
    pub fn from_view(mut self, view_name: impl Into<String>) -> Self {
        self.source = Some(DataflowSource::View(view_name.into()));
        self
    }

    /// Convenience typed constructor starting from a collection and deserializing to `T`.
    #[must_use]
    pub fn from_collection_typed<T>(
        self,
        collection: impl Into<String>,
    ) -> TypedDataflowBuilder<'a, T>
    where
        T: DeserializeOwned + Send + Sync + 'static,
    {
        TypedDataflowBuilder {
            inner: self.from_collection(collection),
            _marker: std::marker::PhantomData,
        }
    }

    /// Adds a functional filter predicate to the pipeline.
    #[must_use]
    pub fn filter<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&DataflowDoc) -> bool + Send + Sync + 'static,
    {
        self.filter = Some(FilterStage::new(predicate));
        self
    }

    /// Joins incoming document records with a target graph in O(1) time.
    #[must_use]
    pub fn join_graph(
        mut self,
        foreign_key: impl Into<String>,
        edge_kind: impl Into<String>,
        target_graph: impl Into<String>,
    ) -> Self {
        self.join_graph = Some(JoinGraphStage::new(JoinGraphSpec {
            foreign_key: foreign_key.into(),
            edge_kind: edge_kind.into(),
            target_graph: target_graph.into(),
        }));
        self
    }

    /// Groups the stream by a field and applies an incremental accumulator in O(1) time.
    #[must_use]
    pub fn aggregate_by<F>(mut self, group_by_field: impl Into<String>, agg_fn: F) -> Self
    where
        F: FnOnce(DataflowAggregator) -> DataflowAggKind,
    {
        let kind = agg_fn(DataflowAggregator);
        self.aggregate = Some(AggregateStage::new(group_by_field, kind));
        self
    }

    /// Compiles the dataflow pipeline, performs initial snapshot bootstrap, and registers the continuous reactive view.
    ///
    /// # Errors
    /// Returns an error if the pipeline configuration is incomplete or view creation fails.
    pub fn into_view(self, view_name: impl Into<String>) -> Result<Arc<DataflowView>> {
        let name = view_name.into();
        let Some(source) = self.source else {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "dataflow pipeline requires a source (.from_collection or .from_view)",
                false,
            ));
        };

        let source_name = match &source {
            DataflowSource::Collection(c) => c.clone(),
            DataflowSource::View(v) => v.clone(),
        };

        let definition = format!(
            "DataflowPipeline: source={source_name}, has_filter={}, has_join={}, has_agg={}",
            self.filter.is_some(),
            self.join_graph.is_some(),
            self.aggregate.is_some()
        );

        let view = Arc::new(DataflowView::new(name, source_name, definition));
        let id = NEXT_PIPELINE_ID.fetch_add(1, Ordering::SeqCst);

        let pipeline = Arc::new(DataflowPipeline {
            id,
            source,
            filter: self.filter,
            join_graph: self.join_graph,
            aggregate: self.aggregate,
            view: Arc::clone(&view),
        });

        self.db.register_dataflow_pipeline(pipeline)?;

        Ok(view)
    }
}

/// Typed wrapper around `DataflowBuilder` providing clean closures over concrete structs `T`.
pub struct TypedDataflowBuilder<'a, T> {
    inner: DataflowBuilder<'a>,
    _marker: std::marker::PhantomData<T>,
}

impl<'a, T> std::fmt::Debug for TypedDataflowBuilder<'a, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypedDataflowBuilder")
            .field("inner", &self.inner)
            .finish()
    }
}

impl<'a, T> TypedDataflowBuilder<'a, T>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    #[must_use]
    pub fn filter<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&T) -> bool + Send + Sync + 'static,
    {
        self.inner = self.inner.filter(move |doc| {
            if let Ok(typed) = doc.to_typed::<T>() {
                predicate(&typed)
            } else {
                false
            }
        });
        self
    }

    #[must_use]
    pub fn join_graph(
        mut self,
        foreign_key: impl Into<String>,
        edge_kind: impl Into<String>,
        target_graph: impl Into<String>,
    ) -> Self {
        self.inner = self.inner.join_graph(foreign_key, edge_kind, target_graph);
        self
    }

    #[must_use]
    pub fn aggregate_by<F>(mut self, group_by_field: impl Into<String>, agg_fn: F) -> Self
    where
        F: FnOnce(DataflowAggregator) -> DataflowAggKind,
    {
        self.inner = self.inner.aggregate_by(group_by_field, agg_fn);
        self
    }

    /// Compiles into a continuous reactive view.
    ///
    /// # Errors
    /// Propagates registration errors.
    pub fn into_view(self, view_name: impl Into<String>) -> Result<Arc<DataflowView>> {
        self.inner.into_view(view_name)
    }
}
