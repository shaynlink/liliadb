//! Cryptographic Immutable Ledger (Integrated Merkle Ledger) primitives.
//!
//! Provides transactional Merkle DAG structures, binary Merkle trees,
//! inclusion proofs, domain-separated BLAKE3 hashing, and ledger verification.

use serde::{Deserialize, Serialize};

/// Domain separation prefix for leaf nodes (RFC 6962 / BLAKE3 tree mode).
pub const DOMAIN_LEAF: u8 = 0x00;
/// Domain separation prefix for internal binary Merkle nodes.
pub const DOMAIN_NODE: u8 = 0x01;
/// Domain separation prefix for commit DAG blocks.
pub const DOMAIN_COMMIT: u8 = 0x02;

/// A single hop in a Merkle audit path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditHop {
    /// Sibling hash at this level of the tree.
    #[serde(with = "hex_bytes")]
    pub sibling_hash: [u8; 32],
    /// `true` if the sibling is to the left of the current node; `false` if to the right.
    pub is_left: bool,
}

/// Verification mode executed on database open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerVerifyMode {
    /// Full verification: verify the entire commit Merkle DAG chain, recalculate
    /// all commit hashes and transaction roots, and verify that all live rows
    /// in the database match their cryptographic commitments.
    #[default]
    Strict,
    /// Fast verification: verify the entire commit Merkle DAG chain and the
    /// latest Merkle root without scanning every individual row in the live tables.
    Fast,
    /// Disabled: skip verification on database open.
    None,
}

/// Action performed on a record in a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerAction {
    Put,
    Delete,
}

/// Canonical summary of a mutation inside a commit delta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerDeltaEntry {
    /// Model name: e.g. "json", "kv", "graph", "vector".
    pub model: String,
    /// Collection, space, or namespace name.
    pub space: String,
    /// Document ID or key string/hex.
    pub id: String,
    /// Record version assigned to this mutation.
    pub version: u64,
    /// Action applied.
    pub action: LedgerAction,
    /// BLAKE3 hash of the payload (or zero hash if deleted).
    #[serde(with = "hex_bytes")]
    pub payload_hash: [u8; 32],
}

impl LedgerDeltaEntry {
    /// Computes the domain-separated leaf hash for this delta entry.
    #[must_use]
    pub fn compute_leaf_hash(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&[DOMAIN_LEAF]);
        hasher.update(self.model.as_bytes());
        hasher.update(b":");
        hasher.update(self.space.as_bytes());
        hasher.update(b":");
        hasher.update(self.id.as_bytes());
        hasher.update(b":");
        hasher.update(&self.version.to_be_bytes());
        hasher.update(b":");
        let action_byte = match self.action {
            LedgerAction::Put => 0x01,
            LedgerAction::Delete => 0x02,
        };
        hasher.update(&[action_byte]);
        hasher.update(b":");
        hasher.update(&self.payload_hash);
        *hasher.finalize().as_bytes()
    }
}

/// Computes the cryptographic hash of two internal Merkle nodes.
#[must_use]
pub fn hash_internal_node(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[DOMAIN_NODE]);
    hasher.update(left);
    hasher.update(right);
    *hasher.finalize().as_bytes()
}

/// Computes the commit block hash linking to the previous commit.
#[must_use]
pub fn compute_commit_hash(
    commit_seq: u64,
    timestamp_ms: i64,
    parent_hash: &[u8; 32],
    tx_root: &[u8; 32],
    operation_count: u32,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[DOMAIN_COMMIT]);
    hasher.update(&commit_seq.to_be_bytes());
    hasher.update(&timestamp_ms.to_be_bytes());
    hasher.update(parent_hash);
    hasher.update(tx_root);
    hasher.update(&operation_count.to_be_bytes());
    *hasher.finalize().as_bytes()
}

/// Evaluates a Merkle audit path from a starting leaf/hash to the root.
#[must_use]
pub fn verify_audit_path(start_hash: &[u8; 32], path: &[AuditHop]) -> [u8; 32] {
    let mut curr = *start_hash;
    for hop in path {
        if hop.is_left {
            curr = hash_internal_node(&hop.sibling_hash, &curr);
        } else {
            curr = hash_internal_node(&curr, &hop.sibling_hash);
        }
    }
    curr
}

/// Balanced binary Merkle tree built over a fixed set of leaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerkleTree {
    /// Tree levels: level 0 contains the leaves, level N contains the single root.
    levels: Vec<Vec<[u8; 32]>>,
}

impl MerkleTree {
    /// Builds a Merkle tree from a slice of leaf hashes.
    #[must_use]
    pub fn build(leaves: &[[u8; 32]]) -> Self {
        if leaves.is_empty() {
            return Self {
                levels: vec![vec![[0u8; 32]]],
            };
        }

        let mut levels = vec![leaves.to_vec()];
        while let Some(prev) = levels.last().filter(|lvl| lvl.len() > 1) {
            // An odd last node is paired with itself.
            let next = prev
                .chunks(2)
                .map(|pair| hash_internal_node(&pair[0], pair.get(1).unwrap_or(&pair[0])))
                .collect();
            levels.push(next);
        }

        Self { levels }
    }

    /// Returns the root hash of the tree.
    #[must_use]
    pub fn root(&self) -> [u8; 32] {
        self.levels
            .last()
            .and_then(|lvl| lvl.first())
            .copied()
            .unwrap_or([0u8; 32])
    }

    /// Generates an audit path for the leaf at `leaf_index`.
    #[must_use]
    pub fn audit_path(&self, mut leaf_index: usize) -> Vec<AuditHop> {
        let mut path = Vec::new();
        if self.levels.is_empty() || self.levels[0].is_empty() {
            return path;
        }

        for current_level in &self.levels[..self.levels.len() - 1] {
            let is_even = leaf_index.is_multiple_of(2);
            let sibling_index = if is_even {
                if leaf_index + 1 < current_level.len() {
                    leaf_index + 1
                } else {
                    leaf_index
                }
            } else {
                leaf_index - 1
            };

            let sibling_hash = current_level[sibling_index];
            path.push(AuditHop {
                sibling_hash,
                is_left: !is_even,
            });

            leaf_index /= 2;
        }

        path
    }
}

/// A stored ledger commit header.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LedgerCommit {
    /// Monotonic sequence number (1-based).
    pub commit_seq: u64,
    /// Unix timestamp in milliseconds.
    pub timestamp_ms: i64,
    /// Hash of the previous commit (or zero hash for genesis commit 1).
    #[serde(with = "hex_bytes")]
    pub parent_hash: [u8; 32],
    /// Merkle root of all delta operations in this commit.
    #[serde(with = "hex_bytes")]
    pub tx_root: [u8; 32],
    /// Cryptographic commit hash: `BLAKE3(0x02 || seq || time || parent || tx_root || count)`.
    #[serde(with = "hex_bytes")]
    pub commit_hash: [u8; 32],
    /// Cumulative database Merkle root after this commit.
    #[serde(with = "hex_bytes")]
    pub merkle_root: [u8; 32],
    /// Number of operations included in this commit.
    pub operation_count: u32,
    /// Optional metadata string (JSON).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,
}

/// Report summarizing ledger integrity verification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LedgerVerificationReport {
    /// Whether the database passed all integrity and cryptographic checks.
    pub is_valid: bool,
    /// Total number of commits verified in the DAG.
    pub commit_count: u64,
    /// Number of live records whose hashes were verified against the ledger.
    pub leaves_verified: u64,
    /// The current certified Merkle root.
    #[serde(with = "hex_bytes")]
    pub merkle_root: [u8; 32],
    /// Error message if verification failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Helper module for hex-encoding 32-byte arrays in serde.
pub mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    /// # Errors
    /// The serializer's own errors.
    pub fn serialize<S>(bytes: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&hex::encode(bytes))
    }

    /// # Errors
    /// When the input is not 64 hexadecimal digits.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 32], D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let mut out = [0u8; 32];
        hex::decode_to_slice(&s, &mut out).map_err(serde::de::Error::custom)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merkle_tree_empty() {
        let tree = MerkleTree::build(&[]);
        assert_eq!(tree.root(), [0u8; 32]);
    }

    #[test]
    fn test_merkle_tree_single() {
        let leaf = [42u8; 32];
        let tree = MerkleTree::build(&[leaf]);
        assert_eq!(tree.root(), leaf);
        let path = tree.audit_path(0);
        assert_eq!(path, []);
        assert_eq!(verify_audit_path(&leaf, &path), leaf);
    }

    #[test]
    fn test_merkle_tree_even_and_odd() {
        for n in 2u8..=15u8 {
            let leaves: Vec<[u8; 32]> = (0..n).map(|i| [i; 32]).collect();
            let tree = MerkleTree::build(&leaves);
            let root = tree.root();
            assert_ne!(root, [0u8; 32]);

            for (idx, leaf) in leaves.iter().enumerate() {
                let path = tree.audit_path(idx);
                let computed = verify_audit_path(leaf, &path);
                assert_eq!(computed, root, "failed for n={n}, idx={idx}");

                // Tampered leaf must fail
                let mut tampered = *leaf;
                tampered[0] ^= 0xFF;
                assert_ne!(verify_audit_path(&tampered, &path), root);
            }
        }
    }
}
