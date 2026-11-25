pub mod centrality;
pub mod shortest_path;
pub mod topology;

pub use centrality::*;
pub use shortest_path::*;
pub use topology::*;

/// A node or edge count as `f64`, for scores and averages.
#[allow(
    clippy::cast_precision_loss,
    reason = "exact below 2^53, far beyond any in-memory graph"
)]
pub(crate) fn count_f64(count: usize) -> f64 {
    count as f64
}
