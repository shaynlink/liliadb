pub mod calendar;
pub mod cjk;
pub mod crdt;
mod error;
pub mod geo;
pub mod h3;
pub mod hnsw;
pub mod json;
pub mod json_patch;
pub mod ledger;
pub mod levenshtein;
pub mod phonetic;
pub mod quantization;
pub mod security;
pub mod temporal;
pub mod timeseries;
mod types;
mod version;
pub mod zero_copy;

pub use cjk::{
    cjk_segment, contains_cjk, is_cjk, is_cjk_ideograph, is_hangul, is_hiragana, is_katakana,
    prepare_cjk_for_indexing, prepare_cjk_for_search,
};
pub use crdt::{
    CvRdt, LogicalClock, LwwClock, LwwField, LwwJsonDocument, LwwRegister, OrSet, PNCounter,
};
pub use error::{ErrorCode, LiliaError, Result};
pub use geo::{
    circle_to_geopoly_json, coords_to_geopoly_json, haversine_distance_meters, GeofenceMatch,
    GpsPoint, EARTH_RADIUS_METERS,
};
pub use h3::SpatioTemporalPoint;
pub use hnsw::{HnswIndex, HnswNode};
pub use json::{
    eval_expr, eval_filter_predicate, parse_expr, parse_json_bytes_to_binary,
    parse_json_slice_to_binary, referenced_paths, BinaryArrayRef, BinaryDoc, BinaryDocBuilder,
    BinaryObjectRef, BinaryValueRef, EvaluatedValue, Expr, LiteralValue, MAGIC,
};
pub use ledger::{
    compute_commit_hash, hash_internal_node, verify_audit_path, AuditHop, LedgerAction,
    LedgerCommit, LedgerDeltaEntry, LedgerVerificationReport, LedgerVerifyMode, MerkleTree,
    DOMAIN_COMMIT, DOMAIN_LEAF, DOMAIN_NODE,
};
pub use levenshtein::{LevenshteinAutomaton, LevenshteinState, TermTrie};
pub use phonetic::{
    double_metaphone, is_phonetic_match, remove_accents, soundex, DoubleMetaphoneResult,
    PhoneticCandidate, PhoneticVocabulary,
};
pub use security::{
    eval_exemption_condition, eval_security_condition, eval_security_condition_kv,
    eval_security_condition_str, mask_credit_card, mask_document, mask_email, mask_string,
    mask_value, MaskFunction, MaskingRule, NamedPolicy, Policy, PolicyAction, SecurityContext,
};
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
    ArchivedBatchOperation, ArchivedMutationResult, BatchOperation, BitemporalEntry,
    CentralityType, ComponentType, CypherResult, Direction, Durability, FtsFuzzyHit, FtsHit,
    FtsPhoneticHit, FtsSuggestion, GraphEdge, GraphNode, HnswConfig, HybridHit, ImpactAnalysis,
    IndexOptions, JsonEntry, JsonIndexInfo, KvEntry, MutationResult, NodeScore, PageRankConfig,
    PathEdge, ResourceProfile, SqlQueryResult, VectorElementType, VectorHit, VectorIndexInfo,
    VectorMetric, Version, WeightedPath,
};
pub use zero_copy::{access_archived, access_archived_unchecked, AsJsonString};

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
