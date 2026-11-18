//! Invertible Bloom Filter (IBF) and Strata Estimator module for P2P Set Reconciliation.
//!
//! Provides probabilistic data structures for peer-to-peer decentralized database sync
//! with minimal bandwidth footprint.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::missing_panics_doc
)]

pub mod cell;
pub mod filter;
pub mod strata;

pub use cell::{compute_checksum, IbfCell};
pub use filter::{
    get_hash_buckets, IbfDiff, IbfError, InvertibleBloomFilter, DEFAULT_IBF_SEED,
    DEFAULT_NUM_HASHES,
};
pub use strata::{
    stratum_for_key, StrataEstimateResult, StrataEstimator, DEFAULT_CELLS_PER_STRATUM,
    DEFAULT_NUM_STRATA,
};

use serde::{Deserialize, Serialize};

/// Payload representing a document update or deletion transferred during P2P sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct P2pDocumentPayload {
    /// Data model ("json", "kv", etc.).
    pub model: String,
    /// Collection or namespace.
    pub space: String,
    /// Unique document ID.
    pub id: String,
    /// Document value (`None` denotes a tombstone / deletion).
    pub value: Option<serde_json::Value>,
    /// Hybrid Logical Clock timestamp of this mutation.
    pub hlc: crate::hlc::Hlc,
    /// Causal predecessor HLC (used for 3-way merge conflict detection).
    pub parent_hlc: Option<crate::hlc::Hlc>,
}

/// Statistics reported after applying incoming P2P changesets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct P2pApplyStats {
    /// Number of document mutations applied.
    pub applied_count: usize,
    /// Number of concurrent conflicts detected.
    pub conflicts_detected: usize,
    /// Number of conflicts successfully resolved.
    pub conflicts_resolved: usize,
}

/// Summary of an end-to-end P2P reconciliation execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconciliationSummary {
    /// Total communication rounds used (1 for Fast-Path, 2 for Normal-Path).
    pub rounds_used: u32,
    /// Whether the Fast-Path (direct decoding via Strata Estimator level 0) succeeded.
    pub fast_path: bool,
    /// Estimated number of differences between the two replicas.
    pub estimated_diff: usize,
    /// Number of document changesets sent from A to B.
    pub items_sent_a_to_b: usize,
    /// Number of document changesets sent from B to A.
    pub items_sent_b_to_a: usize,
    /// Mutations applied on Replica A.
    pub stats_a: P2pApplyStats,
    /// Mutations applied on Replica B.
    pub stats_b: P2pApplyStats,
}

/// A composite 128-bit item representation combining document/record identity and state fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReconciliationItem {
    /// 64-bit BLAKE3 hash of the document identifier (`model:space:id`).
    pub id_hash: u64,
    /// 64-bit BLAKE3 hash of the document state (HLC, commit leaf hash, or payload hash).
    pub state_hash: u64,
}

impl ReconciliationItem {
    /// Creates a new `ReconciliationItem`.
    #[inline]
    #[must_use]
    pub fn new(id_hash: u64, state_hash: u64) -> Self {
        Self {
            id_hash,
            state_hash,
        }
    }

    /// Packs into a single 128-bit unsigned integer for IBF operations.
    #[inline]
    #[must_use]
    pub fn to_u128(&self) -> u128 {
        (u128::from(self.id_hash) << 64) | u128::from(self.state_hash)
    }

    /// Unpacks from a 128-bit unsigned integer.
    #[inline]
    #[must_use]
    pub fn from_u128(val: u128) -> Self {
        let id_hash = (val >> 64) as u64;
        let state_hash = val as u64;
        Self {
            id_hash,
            state_hash,
        }
    }

    /// Constructs an item from model, space, `doc_id`, and state bytes.
    #[must_use]
    pub fn from_parts(model: &str, space: &str, id: &str, state_bytes: &[u8]) -> Self {
        let mut id_hasher = blake3::Hasher::new();
        id_hasher.update(model.as_bytes());
        id_hasher.update(b":");
        id_hasher.update(space.as_bytes());
        id_hasher.update(b":");
        id_hasher.update(id.as_bytes());
        let id_hash = u64::from_le_bytes(id_hasher.finalize().as_bytes()[0..8].try_into().unwrap());

        let mut state_hasher = blake3::Hasher::new();
        state_hasher.update(state_bytes);
        let state_hash =
            u64::from_le_bytes(state_hasher.finalize().as_bytes()[0..8].try_into().unwrap());

        Self {
            id_hash,
            state_hash,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ibf_insert_remove_and_purity() {
        let mut ibf = InvertibleBloomFilter::new(64, 3, DEFAULT_IBF_SEED).expect("create ibf");
        let item1 =
            ReconciliationItem::from_parts("json", "users", "user-1", b"version-1").to_u128();
        let item2 =
            ReconciliationItem::from_parts("json", "users", "user-2", b"version-1").to_u128();

        ibf.insert(item1);
        ibf.insert(item2);

        // Peeling without subtraction will extract item1 and item2 as (sign = +1)
        let diff = ibf.decode().expect("decode ibf");
        assert_eq!(diff.a_minus_b.len(), 2);
        assert!(diff.b_minus_a.is_empty());
        assert!(diff.a_minus_b.contains(&item1));
        assert!(diff.a_minus_b.contains(&item2));

        // Now remove item1 and item2: filter must become completely empty
        ibf.remove(item1);
        ibf.remove(item2);
        assert!(ibf.cells.iter().all(IbfCell::is_empty));
    }

    #[test]
    fn test_ibf_subtraction_and_symmetric_difference() {
        let mut ibf_a = InvertibleBloomFilter::new(100, 3, DEFAULT_IBF_SEED).expect("ibf a");
        let mut ibf_b = InvertibleBloomFilter::new(100, 3, DEFAULT_IBF_SEED).expect("ibf b");

        // 50 common items
        for i in 0..50 {
            let item = ReconciliationItem::new(i, 1000).to_u128();
            ibf_a.insert(item);
            ibf_b.insert(item);
        }

        // Node A has 3 unique items
        let a_only_1 = ReconciliationItem::new(101, 1).to_u128();
        let a_only_2 = ReconciliationItem::new(102, 1).to_u128();
        let a_only_3 = ReconciliationItem::new(103, 1).to_u128();
        ibf_a.insert(a_only_1);
        ibf_a.insert(a_only_2);
        ibf_a.insert(a_only_3);

        // Node B has 2 unique items
        let b_only_1 = ReconciliationItem::new(201, 2).to_u128();
        let b_only_2 = ReconciliationItem::new(202, 2).to_u128();
        ibf_b.insert(b_only_1);
        ibf_b.insert(b_only_2);

        // Subtract IBF_A - IBF_B
        ibf_a.subtract(&ibf_b).expect("subtract");

        // Decode differences
        let diff = ibf_a.decode().expect("peel diff");

        assert_eq!(diff.a_minus_b.len(), 3);
        assert_eq!(diff.b_minus_a.len(), 2);
        assert!(diff.a_minus_b.contains(&a_only_1));
        assert!(diff.a_minus_b.contains(&a_only_2));
        assert!(diff.a_minus_b.contains(&a_only_3));
        assert!(diff.b_minus_a.contains(&b_only_1));
        assert!(diff.b_minus_a.contains(&b_only_2));
    }

    #[test]
    fn test_ibf_concurrent_modification_detection() {
        // Document "user-1" modified differently on Node A and Node B
        let doc_id = "user-1";
        let item_a =
            ReconciliationItem::from_parts("json", "users", doc_id, b"hlc:100.1@node_a").to_u128();
        let item_b =
            ReconciliationItem::from_parts("json", "users", doc_id, b"hlc:100.2@node_b").to_u128();

        let mut ibf_a = InvertibleBloomFilter::new(50, 3, DEFAULT_IBF_SEED).expect("ibf a");
        let mut ibf_b = InvertibleBloomFilter::new(50, 3, DEFAULT_IBF_SEED).expect("ibf b");

        ibf_a.insert(item_a);
        ibf_b.insert(item_b);

        ibf_a.subtract(&ibf_b).expect("subtract");
        let diff = ibf_a.decode().expect("decode");

        assert_eq!(diff.a_minus_b.len(), 1);
        assert_eq!(diff.b_minus_a.len(), 1);

        let unp_a = ReconciliationItem::from_u128(diff.a_minus_b[0]);
        let unp_b = ReconciliationItem::from_u128(diff.b_minus_a[0]);

        // Same ID hash! Different state hashes -> Concurrent mutation accurately detected!
        assert_eq!(unp_a.id_hash, unp_b.id_hash);
        assert_ne!(unp_a.state_hash, unp_b.state_hash);
    }

    #[test]
    fn test_strata_estimator_fast_path() {
        let mut strata_a = StrataEstimator::with_default_params(DEFAULT_IBF_SEED);
        let mut strata_b = StrataEstimator::with_default_params(DEFAULT_IBF_SEED);

        // 1000 common items
        for i in 0..1000 {
            let item = ReconciliationItem::new(i, 42).to_u128();
            strata_a.insert(item);
            strata_b.insert(item);
        }

        // Small divergence (5 differences: A has 3, B has 2)
        let a1 = ReconciliationItem::new(5001, 1).to_u128();
        let a2 = ReconciliationItem::new(5002, 1).to_u128();
        let a3 = ReconciliationItem::new(5003, 1).to_u128();
        strata_a.insert(a1);
        strata_a.insert(a2);
        strata_a.insert(a3);

        let b1 = ReconciliationItem::new(6001, 2).to_u128();
        let b2 = ReconciliationItem::new(6002, 2).to_u128();
        strata_b.insert(b1);
        strata_b.insert(b2);

        let estimate = strata_a.estimate_difference(&strata_b).expect("estimate");

        // Fast-path triggered!
        assert!(estimate.fully_decoded, "fast-path should decode completely");
        assert_eq!(estimate.estimated_diff, 5);

        let diff = estimate.direct_diff.expect("direct diff present");
        assert_eq!(diff.a_minus_b.len(), 3);
        assert_eq!(diff.b_minus_a.len(), 2);
    }
}
