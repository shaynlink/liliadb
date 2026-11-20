//! Invertible Bloom Filter (IBF / IBLT) core engine.
//!
//! Implements Kirsch-Mitzenmacher double-hashing, vector subtraction,
//! and iterative peeling to recover the symmetric difference between two datasets.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::missing_panics_doc
)]

use std::collections::{BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use super::cell::{compute_checksum, IbfCell};

/// Default number of hash functions for an IBF (k=3 is mathematically optimal for peeling threshold c ~ 1.222).
pub const DEFAULT_NUM_HASHES: u32 = 3;

/// Default master seed for hashing.
pub const DEFAULT_IBF_SEED: u64 = 0x4c69_6c69_6144_4231; // "LiliaDB1"

/// Error during IBF operations or peeling.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IbfError {
    #[error("mismatched IBF parameters: len ({self_len} vs {other_len}), hashes ({self_hashes} vs {other_hashes}), seed ({self_seed} vs {other_seed})")]
    MismatchedParameters {
        self_len: usize,
        other_len: usize,
        self_hashes: u32,
        other_hashes: u32,
        self_seed: u64,
        other_seed: u64,
    },
    #[error("decoding stalled: {unpeeled_cells} unpeeled cells out of {total_cells} (decoded {decoded_a} on A, {decoded_b} on B)")]
    DecodingStalled {
        unpeeled_cells: usize,
        total_cells: usize,
        decoded_a: usize,
        decoded_b: usize,
    },
    #[error("zero capacity IBF")]
    ZeroCapacity,
}

/// Result of peeling two subtracted IBFs (`IBF_A` - `IBF_B`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct IbfDiff {
    /// Elements belonging to A but not to B (net count = +1).
    pub a_minus_b: Vec<u128>,
    /// Elements belonging to B but not to A (net count = -1).
    pub b_minus_a: Vec<u128>,
}

impl IbfDiff {
    /// Returns true if the two sets are completely identical (zero differences).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.a_minus_b.is_empty() && self.b_minus_a.is_empty()
    }

    /// Total number of differing items.
    #[must_use]
    pub fn total_diff(&self) -> usize {
        self.a_minus_b.len() + self.b_minus_a.len()
    }
}

/// Kirsch-Mitzenmacher double-hashing function returning `k` distinct bucket indices in `[0, m-1]`.
#[must_use]
pub fn get_hash_buckets(key: u128, m: usize, k: u32, seed: u64) -> Vec<usize> {
    if m == 0 {
        return Vec::new();
    }
    if m == 1 {
        return vec![0];
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(&seed.to_le_bytes());
    hasher.update(&key.to_le_bytes());
    let hash = hasher.finalize();
    let bytes = hash.as_bytes();

    let h1 = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
    let h2 = u64::from_le_bytes(bytes[8..16].try_into().unwrap()) | 1; // ensure h2 is odd

    let target_count = (k as usize).min(m);
    let mut buckets = Vec::with_capacity(target_count);
    let mut i = 0u64;

    while buckets.len() < target_count {
        let bucket = (h1.wrapping_add(i.wrapping_mul(h2)) % (m as u64)) as usize;
        if !buckets.contains(&bucket) {
            buckets.push(bucket);
        }
        i += 1;
    }

    buckets
}

/// Invertible Bloom Filter data structure.
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
    rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(crate = rkyv)]
pub struct InvertibleBloomFilter {
    /// Array of cells.
    pub cells: Vec<IbfCell>,
    /// Number of independent hash functions.
    pub num_hashes: u32,
    /// Randomization seed.
    pub seed: u64,
}

impl InvertibleBloomFilter {
    /// Creates a new IBF with the given capacity, number of hash functions, and seed.
    ///
    /// # Errors
    /// Returns `IbfError::ZeroCapacity` if `capacity == 0`.
    pub fn new(capacity: usize, num_hashes: u32, seed: u64) -> Result<Self, IbfError> {
        if capacity == 0 {
            return Err(IbfError::ZeroCapacity);
        }
        Ok(Self {
            cells: vec![IbfCell::EMPTY; capacity],
            num_hashes: num_hashes.max(1),
            seed,
        })
    }

    /// Creates a new IBF with default parameters for a target difference capacity.
    /// Uses peeling scaling factor c ~ 2.5 to ensure deterministic peeling even under
    /// statistical variance of difference estimators.
    ///
    /// # Errors
    /// Returns `IbfError::ZeroCapacity` if target capacity resolves to 0.
    pub fn with_expected_diff(expected_diff: usize) -> Result<Self, IbfError> {
        let capacity = ((expected_diff as f64 * 2.5).ceil() as usize).max(64);
        Self::new(capacity, DEFAULT_NUM_HASHES, DEFAULT_IBF_SEED)
    }

    /// Number of cells in this filter.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Returns `true` if the filter has zero cells.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Inserts an element into the IBF.
    pub fn insert(&mut self, key: u128) {
        let checksum = compute_checksum(key);
        let buckets = get_hash_buckets(key, self.cells.len(), self.num_hashes, self.seed);
        for b in buckets {
            self.cells[b].add(key, checksum);
        }
    }

    /// Removes an element from the IBF.
    pub fn remove(&mut self, key: u128) {
        let checksum = compute_checksum(key);
        let buckets = get_hash_buckets(key, self.cells.len(), self.num_hashes, self.seed);
        for b in buckets {
            self.cells[b].remove(key, checksum);
        }
    }

    /// Subtracts another IBF from this one in place (`self = self - other`).
    ///
    /// # Errors
    /// Returns `IbfError::MismatchedParameters` if sizes, hash count, or seeds do not match.
    pub fn subtract(&mut self, other: &Self) -> Result<(), IbfError> {
        if self.cells.len() != other.cells.len()
            || self.num_hashes != other.num_hashes
            || self.seed != other.seed
        {
            return Err(IbfError::MismatchedParameters {
                self_len: self.cells.len(),
                other_len: other.cells.len(),
                self_hashes: self.num_hashes,
                other_hashes: other.num_hashes,
                self_seed: self.seed,
                other_seed: other.seed,
            });
        }

        for (c_self, c_other) in self.cells.iter_mut().zip(&other.cells) {
            c_self.subtract(c_other);
        }

        Ok(())
    }

    /// Decodes (peels) the IBF to recover the differing elements.
    ///
    /// # Errors
    /// Returns `IbfError::DecodingStalled` if decoding gets stuck before resolving all cells.
    pub fn decode(&self) -> Result<IbfDiff, IbfError> {
        let mut working_cells = self.cells.clone();
        let mut pure_queue = VecDeque::new();

        // 1. Initial scan for pure cells
        for (idx, cell) in working_cells.iter().enumerate() {
            if let Some((sign, key)) = cell.is_pure() {
                pure_queue.push_back((idx, sign, key));
            }
        }

        let mut a_minus_b = BTreeSet::new();
        let mut b_minus_a = BTreeSet::new();

        // 2. Iterative cascading peeling
        while let Some((idx, sign, key)) = pure_queue.pop_front() {
            // Validate that the cell is still pure with the exact same key
            if working_cells[idx].is_pure() != Some((sign, key)) {
                continue;
            }

            let checksum = compute_checksum(key);
            let buckets = get_hash_buckets(key, working_cells.len(), self.num_hashes, self.seed);

            if sign == 1 {
                a_minus_b.insert(key);
                for b in buckets {
                    working_cells[b].remove(key, checksum);
                    if let Some((new_sign, new_key)) = working_cells[b].is_pure() {
                        pure_queue.push_back((b, new_sign, new_key));
                    }
                }
            } else if sign == -1 {
                b_minus_a.insert(key);
                for b in buckets {
                    working_cells[b].add(key, checksum);
                    if let Some((new_sign, new_key)) = working_cells[b].is_pure() {
                        pure_queue.push_back((b, new_sign, new_key));
                    }
                }
            }
        }

        // 3. Verification: all cells must be empty (cancelled to 0)
        let unpeeled = working_cells.iter().filter(|c| !c.is_empty()).count();
        if unpeeled == 0 {
            Ok(IbfDiff {
                a_minus_b: a_minus_b.into_iter().collect(),
                b_minus_a: b_minus_a.into_iter().collect(),
            })
        } else {
            Err(IbfError::DecodingStalled {
                unpeeled_cells: unpeeled,
                total_cells: working_cells.len(),
                decoded_a: a_minus_b.len(),
                decoded_b: b_minus_a.len(),
            })
        }
    }
}
