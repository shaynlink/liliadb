pub mod cursor;
pub mod pipeline;
pub mod stages;

pub use cursor::StreamDocument;
pub use lilia_core::aggregation::{
    doc, Aggregate, AnomalyAccumulator, CountAccumulator, FilterExpr, HyperLogLog, MaxAccumulator,
    MinAccumulator, Sort, SortDirection, SumAccumulator, TDigest, WelfordAccumulator,
};
pub use pipeline::AggregationPipeline;
