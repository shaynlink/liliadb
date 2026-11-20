//! `LiliaQL`: Unified, cross-model fluid query API for `LiliaDB`.

pub mod builder;
pub mod exec;
pub mod macros;
pub mod plan;
pub mod projection;
pub mod types;

pub use builder::{LiliaQuery, QueryBuilder};
pub use macros::doc;
pub use projection::Projection;
pub use types::{IntoVectorBytes, QueryPlanExplanation, QueryResult, QueryRow, RrfConfig};
