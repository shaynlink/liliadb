pub mod aggregation;
pub mod autotune;
pub mod calendar;
pub mod cas;
pub mod cjk;
mod codec;
pub mod compression;
pub mod crdt;
pub mod dataflow;
pub mod embed;
pub mod energy;
pub mod envelope;
mod error;
pub mod fle;
pub mod geo;
pub mod h3;
pub mod hlc;
pub mod hnsw;
pub mod ibf;
pub mod integrity;
pub mod json;
pub mod json_patch;
pub mod ledger;
pub mod levenshtein;
pub mod markov;
pub mod parity;
pub mod phonetic;
pub mod quantization;
pub mod rules;
pub mod security;
pub mod sharding;
pub mod temporal;
pub mod tier;
pub mod timeseries;
mod types;
mod version;
pub mod wear;
pub mod zero_copy;
#[cfg(feature = "zk")]
pub mod zk;

pub use aggregation::{
    accumulators::{
        AnomalyAccumulator, Centroid, CountAccumulator, HomomorphicSumAccumulator, HyperLogLog,
        MaxAccumulator, MinAccumulator, SumAccumulator, TDigest, WelfordAccumulator,
    },
    filter::FilterExpr,
    types::{Aggregate, Sort, SortDirection},
};
pub use autotune::{
    composite_expression, index_fields, index_name, normalize_sql, plan as plan_autotune,
    AutotuneCollector, AutotuneOptions, AutotuneReport, CycleReport, IndexLookups, IndexOrigin,
    IndexPhase, IndexRef, IndexUsageView, JsonQueryReport, PruneScope, QueryFingerprint,
    QueryObservation, QueryStats, Recommendation, SqlObservation, SqlQueryReport, SqlQueryStats,
    TrackedIndex,
};
pub use cas::{
    compute_hash, decode_kv_pointer, encode_kv_pointer, extract_blob_hashes_from_binary_doc,
    extract_blob_hashes_from_value, is_kv_pointer, BlobDescriptor, BlobGcStats, CasHashAlgorithm,
    CasHasher, CasOptions, CAS_KV_MAGIC, CAS_KV_POINTER_LEN,
};
pub use cjk::{
    cjk_segment, contains_cjk, is_cjk, is_cjk_ideograph, is_hangul, is_hiragana, is_katakana,
    prepare_cjk_for_indexing, prepare_cjk_for_search,
};
pub use codec::{Aes256GcmCodec, IdentityCodec, ValueCodec};
pub use compression::dict::{
    compress_with_dict, decompress_with_dict, extract_symbol_signature, hash_token_to_mask,
    train_dictionary, CollectionDictionary, DictionaryRegistry, DictionarySymbolTable,
    SymbolSignature,
};
pub use compression::{compress_bytes, decompress_bytes};
pub use crdt::{
    CvRdt, LogicalClock, LwwClock, LwwField, LwwJsonDocument, LwwRegister, OrSet, PNCounter,
};
pub use dataflow::{
    DataflowAggKind, Diff, DiffAvg, DiffCount, DiffMax, DiffMin, DiffSum, DiffWeight, OrderedFloat,
    ViewDiff,
};
pub use embed::{AutoEmbedConfig, BuiltinModel, EmbedOptions, EmbedStatus, Embedder};
pub use energy::{
    DeviceEnergyProfile, EnergyCoefficients, EnergyDiagnosis, QueryEnergyDiagnostic,
    QueryEnergyMetrics,
};
pub use envelope::{
    CipherAlgorithm, CompressionAlgorithm, CompressionProfile, DualHeaderEnvelope,
    EncryptionMetadata, EnvelopeConfig, EnvelopeHeader, EnvelopeRecoveryOutcome, HeaderRole,
    KdfAlgorithm, CHECKSUM_OFFSET, ENVELOPE_FORMAT_VERSION, ENVELOPE_HEADER_SIZE, ENVELOPE_MAGIC,
    MIN_ENVELOPE_FILE_SIZE,
};
pub use error::{ConstraintKind, ErrorCode, LiliaError, Result};
pub use fle::{
    decrypt_gcm, decrypt_json_fields, decrypt_paillier_f64, decrypt_paillier_i64, decrypt_siv,
    encrypt_gcm, encrypt_json_fields, encrypt_paillier_f64, encrypt_paillier_i64, encrypt_siv,
    paillier_ops, CompartmentInfo, DecryptionMode, EncryptedEnvelope, FieldCipherAlgorithm,
    FieldEncryptionRule, FlePolicy, KeyMaterial, KeyRegistry, PaillierCiphertext, PaillierKeypair,
    PaillierPrivateKey, PaillierPublicKey, ENC_PREFIX,
};
pub use geo::{
    circle_to_geopoly_json, coords_to_geopoly_json, haversine_distance_meters, GeofenceMatch,
    GpsPoint, EARTH_RADIUS_METERS,
};
pub use h3::SpatioTemporalPoint;
pub use hlc::{
    detect_causality, three_way_merge, CausalRelation, ConflictRecord, ConflictResolutionStrategy,
    FieldConflict, Hlc, HlcError, HybridClock, ThreeWayMergeResult, DEFAULT_MAX_DRIFT_MS,
};
pub use hnsw::{HnswIndex, HnswNode};
pub use ibf::{
    compute_checksum, get_hash_buckets, stratum_for_key, IbfCell, IbfDiff, IbfError,
    InvertibleBloomFilter, P2pApplyStats, P2pDocumentPayload, ReconciliationItem,
    ReconciliationSummary, StrataEstimateResult, StrataEstimator, DEFAULT_CELLS_PER_STRATUM,
    DEFAULT_IBF_SEED, DEFAULT_NUM_HASHES, DEFAULT_NUM_STRATA,
};
pub use integrity::{
    paths_overlap, CollectionOptions, KeyMapping, RefAction, Reference, ReferenceSource,
};
pub use json::{
    can_jit_expr, compile_predicate, eval_check, eval_expr, eval_filter_predicate,
    eval_filter_predicate_jit, parse_expr, parse_json_bytes_to_binary, parse_json_slice_to_binary,
    referenced_paths, validate_check_expr, BinaryArrayRef, BinaryDoc, BinaryDocBuilder,
    BinaryObjectRef, BinaryValueRef, CheckOutcome, CompiledPredicate, EvaluatedValue, Expr,
    JitPredicateCache, LiteralValue, MAGIC, MAGIC_ZSTD,
};
pub use ledger::{
    compute_commit_hash, hash_internal_node, verify_audit_path, AuditHop, InclusionProof,
    LedgerAccumulator, LedgerAction, LedgerCommit, LedgerDeltaEntry, LedgerVerificationReport,
    LedgerVerifyMode, MerkleTree, DOMAIN_COMMIT, DOMAIN_LEAF, DOMAIN_NODE,
};
pub use levenshtein::{LevenshteinAutomaton, LevenshteinState, TermTrie};
pub use markov::{AccessTarget, MarkovConfig, MarkovModel, MarkovStats};
pub use parity::{
    compute_page_checksum, Gf256, PageChecksum, PageRepairOutcome, ParityConfig, ReedSolomon,
    RepairSummary, GF,
};
pub use phonetic::{
    double_metaphone, is_phonetic_match, remove_accents, soundex, DoubleMetaphoneResult,
    PhoneticCandidate, PhoneticVocabulary,
};
pub use rules::{
    eval_rule_condition_json, eval_rule_condition_kv, eval_rule_condition_value,
    infer_target_collection, interpolate_string, interpolate_value, Action, Rule, RuleBuilder,
    RuleContext, SlidingWindow, TriggerMode, WindowCondition,
};
pub use security::{
    eval_exemption_condition, eval_security_condition, eval_security_condition_kv,
    eval_security_condition_str, mask_credit_card, mask_document, mask_email, mask_string,
    mask_value, MaskFunction, MaskingRule, NamedPolicy, Policy, PolicyAction, SecurityContext,
};
pub use sharding::{ShardArchiveResult, ShardGranularity, ShardInfo, ShardStatus, ShardingConfig};
pub use temporal::{
    format_ms_to_date, format_ms_to_iso, parse_iso_to_ms, BitemporalInterval, BitemporalPoint,
    BITEMPORAL_INFINITY, BITEMPORAL_NEG_INFINITY,
};
pub use tier::{
    CollectionKind, CollectionSpec, CollectionTierEntry, CommitMode, EvictionPolicy,
    MemoryTierStats, OverflowPolicy, RingBufferConfig, RingBufferStats, StorageTier,
};
pub use timeseries::{
    compress_timeseries, decompress_timeseries, downsample_points, pattern_matches, BitReader,
    BitWriter, DownsampleAggregation, TimeSeriesLifecyclePolicy, TimeSeriesPoint,
    TimeSeriesRetentionRule, TimeSeriesRetentionStats, TimeSeriesStats,
};
pub use types::{
    BatchOperation, BitemporalEntry, BranchDiff, CentralityType, CheckpointStats, ComponentType,
    CypherResult, DatabaseEvent, DictStats, Direction, Durability, FtsFuzzyHit, FtsFuzzyOptions,
    FtsHit, FtsPhoneticHit, FtsSuggestion, GraphEdge, GraphNode, HlcMutationOutcome, HlcRecord,
    HnswConfig, HybridHit, ImpactAnalysis, IndexOptions, JsonEntry, JsonIndexInfo,
    JsonRevisionInfo, KvEntry, LiveQueryDiff, MaterializedViewInfo, MergeResult, MergeStrategy,
    MutationResult, NodeScore, PageRankConfig, PathEdge, PhoneticAlgorithm, RedoResult,
    ResourceProfile, SqlQueryResult, TableDiff, TableStorageStat, UndoEntry, UndoResult,
    VectorElementType, VectorHit, VectorIndexInfo, VectorMetric, Version, WeightedPath,
};
pub use wear::{FlashWearTelemetry, FlashWearTier, FlashWearTracker, WearLevelingPolicy};
pub use zero_copy::{
    access_archived, access_archived_unchecked, archive_to_bytes, RelJsonEntry, RelKvEntry,
};

/// Ergonomic macro to construct document-style JSON values for updates, queries, and filters.

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
