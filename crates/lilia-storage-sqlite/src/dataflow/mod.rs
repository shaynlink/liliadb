//! Differential Dataflow & Functional Cascading Materialized Views for `LiliaDB`.

pub mod engine;
pub mod operators;
pub mod pipeline;
pub mod record;
pub mod view;

pub use engine::DataflowEngine;
pub use operators::{DataflowSource, JoinGraphSpec};
pub use pipeline::{DataflowAggregator, DataflowBuilder, DataflowPipeline, TypedDataflowBuilder};
pub use record::DataflowDoc;
pub use view::{DataflowView, ViewSubscriptionCallback};
