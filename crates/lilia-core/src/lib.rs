mod codec;
pub mod compression;
mod error;
pub mod geo;
pub mod h3;
pub mod hnsw;
pub mod json_patch;
pub mod markov;
pub mod quantization;
pub mod temporal;
pub mod timeseries;
mod types;
mod version;

pub use codec::{Aes256GcmCodec, IdentityCodec, ValueCodec};
pub use compression::{compress_bytes, decompress_bytes};
pub use error::{ErrorCode, LiliaError, Result};
pub use geo::{
    circle_to_geopoly_json, coords_to_geopoly_json, haversine_distance_meters, GeofenceMatch,
    GpsPoint, EARTH_RADIUS_METERS,
};
pub use h3::SpatioTemporalPoint;
pub use hnsw::{HnswIndex, HnswNode};
pub use markov::{AccessTarget, MarkovConfig, MarkovModel, MarkovStats};
pub use temporal::{
    format_ms_to_date, format_ms_to_iso, parse_iso_to_ms, BitemporalInterval, BitemporalPoint,
    BITEMPORAL_INFINITY, BITEMPORAL_NEG_INFINITY,
};
pub use timeseries::{
    compress_timeseries, decompress_timeseries, downsample_points, pattern_matches, BitReader,
    BitWriter, DownsampleAggregation, TimeSeriesLifecyclePolicy, TimeSeriesPoint,
    TimeSeriesRetentionRule, TimeSeriesRetentionStats, TimeSeriesStats,
};
pub use types::{
    BatchOperation, BitemporalEntry, BranchDiff, CentralityType, ComponentType, DatabaseEvent, Direction,
    Durability, FtsHit, GraphEdge, GraphNode, HnswConfig, HybridHit, JsonEntry, JsonIndexInfo,
    JsonRevisionInfo, KvEntry, MergeResult, MergeStrategy, MutationResult, NodeScore, PathEdge,
    RedoResult, SqlQueryResult, TableDiff, UndoEntry, UndoResult, VectorElementType, VectorHit,
    VectorIndexInfo, VectorMetric, Version, WeightedPath,
};

/// Ergonomic macro to construct document-style JSON values for updates, queries, and filters.
///
/// # Example
/// ```rust
/// use lilia_core::doc;
///
/// let update = doc! { "$mul": { "price": 0.8 } };
/// ```
#[macro_export]
macro_rules! doc {
    ({ $($json:tt)* }) => {
        serde_json::json!({ $($json)* })
    };
    ($($json:tt)+) => {
        serde_json::json!({ $($json)+ })
    };
}

#[cfg(windows)]
pub mod windows_security;

/// Synchronizes directory metadata to persistent storage.
///
/// On Unix, opens the directory with `File::open` and invokes `fsync` via `sync_all`.
/// On Windows, opens the directory handle via `CreateFileW` with `FILE_FLAG_BACKUP_SEMANTICS`
/// and flushes it using `FlushFileBuffers`.
///
/// # Errors
///
/// Returns an [`std::io::Error`] if the directory cannot be opened or if the filesystem
/// metadata flush operation fails.
pub fn sync_directory(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(path)?.sync_all()
    }
    #[cfg(windows)]
    {
        windows_security::sync_directory(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_directory_lifecycle() {
        let temp = std::env::temp_dir().join(format!("lilia-sync-core-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp).unwrap();
        assert!(sync_directory(&temp).is_ok());

        let nonexistent = temp.join("does_not_exist");
        assert!(sync_directory(&nonexistent).is_err());

        std::fs::remove_dir_all(&temp).unwrap();
    }
}
