use lilia_core::aggregation::{Aggregate, FilterExpr, Sort};
use lilia_core::Result;
use serde_json::Value;

use super::cursor::{open_document_stream, StreamDocument};
use super::stages::{apply_full_sort, apply_top_k, execute_group_by, FilterStream, UnwindStream};
use crate::Database;

/// A fluent pull-based aggregation pipeline over JSON documents with constant O(1) RAM.
pub struct AggregationPipeline<'a> {
    db: &'a Database,
    space: String,
    filter_expr: Option<FilterExpr>,
    filter_fn: Option<Box<dyn Fn(&StreamDocument) -> bool + Send + Sync>>,
    unwind_field: Option<String>,
    group_by_field: Option<String>,
    aggregates: Vec<Aggregate>,
    sort: Option<Sort>,
    limit: Option<usize>,
    skip: Option<usize>,
}

impl std::fmt::Debug for AggregationPipeline<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AggregationPipeline")
            .field("space", &self.space)
            .field("filter_expr", &self.filter_expr)
            .field("unwind_field", &self.unwind_field)
            .field("group_by_field", &self.group_by_field)
            .field("aggregates", &self.aggregates)
            .field("sort", &self.sort)
            .field("limit", &self.limit)
            .field("skip", &self.skip)
            .finish()
    }
}

impl<'a> AggregationPipeline<'a> {
    #[must_use]
    pub fn new(db: &'a Database, space: impl Into<String>) -> Self {
        Self {
            db,
            space: space.into(),
            filter_expr: None,
            filter_fn: None,
            unwind_field: None,
            group_by_field: None,
            aggregates: Vec::new(),
            sort: None,
            limit: None,
            skip: None,
        }
    }

    /// Add a declarative filter stage using a [`FilterExpr`] or `doc! { ... }`.
    #[must_use]
    pub fn filter(mut self, expr: FilterExpr) -> Self {
        self.filter_expr = Some(expr);
        self
    }

    /// Add a custom closure predicate filter stage.
    #[must_use]
    pub fn filter_fn<F>(mut self, pred: F) -> Self
    where
        F: Fn(&StreamDocument) -> bool + Send + Sync + 'static,
    {
        self.filter_fn = Some(Box::new(pred));
        self
    }

    /// Flatten an array field, emitting a document for each element in the array lazily.
    #[must_use]
    pub fn unwind(mut self, field: impl Into<String>) -> Self {
        self.unwind_field = Some(field.into());
        self
    }

    /// Group documents by a key field and compute single-pass streaming aggregations.
    #[must_use]
    pub fn group_by(mut self, field: impl Into<String>, aggregates: Vec<Aggregate>) -> Self {
        self.group_by_field = Some(field.into());
        self.aggregates = aggregates;
        self
    }

    /// Compute single-pass streaming aggregations across all documents without grouping.
    #[must_use]
    pub fn aggregate_all(mut self, aggregates: Vec<Aggregate>) -> Self {
        self.group_by_field = None;
        self.aggregates = aggregates;
        self
    }

    /// Sort the aggregated or streamed documents by a field.
    #[must_use]
    pub fn sort(mut self, sort: Sort) -> Self {
        self.sort = Some(sort);
        self
    }

    /// Limit the maximum number of results yielded.
    #[must_use]
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Skip the first `skip` results.
    #[must_use]
    pub fn skip(mut self, skip: usize) -> Self {
        self.skip = Some(skip);
        self
    }

    /// Execute the pipeline eagerly and collect all resulting documents into a vector.
    pub fn execute(self) -> Result<Vec<Value>> {
        self.stream()?.collect()
    }

    /// Stream the pipeline results lazily as a pull-based iterator.
    pub fn stream(self) -> Result<Box<dyn Iterator<Item = Result<Value>> + 'a>> {
        let raw_stream = open_document_stream(self.db, &self.space)?;
        let filtered = FilterStream::new(raw_stream, self.filter_expr, self.filter_fn);

        let doc_stream: Box<dyn Iterator<Item = Result<StreamDocument>> + 'a> =
            if let Some(field) = self.unwind_field {
                Box::new(UnwindStream::new(filtered, field))
            } else {
                Box::new(filtered)
            };

        // If aggregations are requested:
        if !self.aggregates.is_empty() || self.group_by_field.is_some() {
            let mut grouped = execute_group_by(
                doc_stream,
                self.group_by_field.as_deref(),
                &self.aggregates,
            )?;

            // Sort and Top-K optimization
            if let Some(ref sort) = self.sort {
                if let Some(limit) = self.limit {
                    if self.skip.unwrap_or(0) == 0 {
                        // Top-K Bounded Heap optimization
                        grouped = apply_top_k(grouped, sort, limit);
                        return Ok(Box::new(grouped.into_iter().map(Ok)));
                    }
                }
                grouped = apply_full_sort(grouped, sort);
            }

            let mut iter: Box<dyn Iterator<Item = Value> + 'a> = Box::new(grouped.into_iter());
            if let Some(skip) = self.skip {
                iter = Box::new(iter.skip(skip));
            }
            if let Some(limit) = self.limit {
                iter = Box::new(iter.take(limit));
            }
            return Ok(Box::new(iter.map(Ok)));
        }

        // If no aggregation, streaming individual documents:
        if let Some(ref sort) = self.sort {
            let mut all_docs = Vec::new();
            for res in doc_stream {
                all_docs.push(res?.to_json_value());
            }
            if let Some(limit) = self.limit {
                if self.skip.unwrap_or(0) == 0 {
                    all_docs = apply_top_k(all_docs, sort, limit);
                    return Ok(Box::new(all_docs.into_iter().map(Ok)));
                }
            }
            all_docs = apply_full_sort(all_docs, sort);
            let mut iter: Box<dyn Iterator<Item = Value> + 'a> = Box::new(all_docs.into_iter());
            if let Some(skip) = self.skip {
                iter = Box::new(iter.skip(skip));
            }
            if let Some(limit) = self.limit {
                iter = Box::new(iter.take(limit));
            }
            return Ok(Box::new(iter.map(Ok)));
        }

        let mut iter: Box<dyn Iterator<Item = Result<Value>> + 'a> =
            Box::new(doc_stream.map(|res| res.map(|d| d.to_json_value())));

        if let Some(skip) = self.skip {
            iter = Box::new(iter.skip(skip));
        }
        if let Some(limit) = self.limit {
            iter = Box::new(iter.take(limit));
        }

        Ok(iter)
    }
}
