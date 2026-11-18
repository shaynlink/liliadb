//! Strata Estimator implementation for estimating set difference size.
//!
//! Based on Eppstein, Goodrich, Uyeda, and Varghese (2011):
//! "What's the Difference? Efficient Set Reconciliation without Prior Knowledge".
//!
//! Partitions items geometrically into L strata using trailing zeros of a hash function.
//! Stratum 0 contains ~50% of items, stratum 1 ~25%, stratum j ~ 1/2^(j+1).

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::missing_panics_doc
)]

use serde::{Deserialize, Serialize};

use super::filter::{IbfDiff, IbfError, InvertibleBloomFilter, DEFAULT_NUM_HASHES};

/// Default number of strata levels (32 levels covers datasets up to billions of elements).
pub const DEFAULT_NUM_STRATA: usize = 32;

/// Default cell count per stratum (64 or 80 cells provides reliable peeling up to ~40 differences in stratum 0).
pub const DEFAULT_CELLS_PER_STRATUM: usize = 64;

/// Result of evaluating two Strata Estimators.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrataEstimateResult {
    /// Estimated number of differing elements |A ⊕ B|.
    pub estimated_diff: usize,
    /// `true` if all strata (including stratum 0) decoded completely to 0 unpeeled cells.
    /// In this case, `direct_diff` contains the 100% exact difference! (Fast-Path 1-RTT).
    pub fully_decoded: bool,
    /// If `fully_decoded` is true, contains the exact symmetric difference elements.
    pub direct_diff: Option<IbfDiff>,
}

/// Computes the stratum level in `[0, num_strata - 1]` for an element using trailing zeros.
#[inline]
#[must_use]
pub fn stratum_for_key(key: u128, num_strata: usize, seed: u64) -> usize {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&seed.wrapping_add(0x9e37_79b9_7f4a_7c15).to_le_bytes()); // golden ratio offset
    hasher.update(&key.to_le_bytes());
    let hash = hasher.finalize();
    let h = u32::from_le_bytes(hash.as_bytes()[0..4].try_into().unwrap());
    let tz = h.trailing_zeros() as usize;
    tz.min(num_strata.saturating_sub(1))
}

/// A collection of geometric-sampling IBFs for difference estimation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrataEstimator {
    /// Array of small IBFs, one per stratum level 0..L-1.
    pub strata: Vec<InvertibleBloomFilter>,
    /// Master seed.
    pub seed: u64,
}

impl StrataEstimator {
    /// Creates a new Strata Estimator.
    ///
    /// # Errors
    /// Returns `IbfError::ZeroCapacity` if `num_strata == 0` or `cells_per_stratum == 0`.
    pub fn new(num_strata: usize, cells_per_stratum: usize, seed: u64) -> Result<Self, IbfError> {
        if num_strata == 0 || cells_per_stratum == 0 {
            return Err(IbfError::ZeroCapacity);
        }

        let mut strata = Vec::with_capacity(num_strata);
        for _ in 0..num_strata {
            strata.push(InvertibleBloomFilter::new(
                cells_per_stratum,
                DEFAULT_NUM_HASHES,
                seed,
            )?);
        }

        Ok(Self { strata, seed })
    }

    /// Creates a default Strata Estimator (32 strata, 64 cells per stratum).
    ///
    /// # Panics
    /// Will not panic with positive default constants.
    #[must_use]
    pub fn with_default_params(seed: u64) -> Self {
        Self::new(DEFAULT_NUM_STRATA, DEFAULT_CELLS_PER_STRATUM, seed)
            .expect("valid default strata params")
    }

    /// Number of strata levels.
    #[must_use]
    pub fn len(&self) -> usize {
        self.strata.len()
    }

    /// Returns `true` if this estimator has zero strata.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strata.is_empty()
    }

    /// Inserts an element into its assigned stratum.
    pub fn insert(&mut self, key: u128) {
        let level = stratum_for_key(key, self.strata.len(), self.seed);
        self.strata[level].insert(key);
    }

    /// Removes an element from its assigned stratum.
    pub fn remove(&mut self, key: u128) {
        let level = stratum_for_key(key, self.strata.len(), self.seed);
        self.strata[level].remove(key);
    }

    /// Estimates the symmetric difference |A ⊕ B| against another peer's Strata Estimator.
    ///
    /// # Errors
    /// Propagates `IbfError` if estimators have incompatible parameters.
    pub fn estimate_difference(&self, other: &Self) -> Result<StrataEstimateResult, IbfError> {
        if self.strata.len() != other.strata.len() || self.seed != other.seed {
            return Err(IbfError::MismatchedParameters {
                self_len: self.strata.len(),
                other_len: other.strata.len(),
                self_hashes: DEFAULT_NUM_HASHES,
                other_hashes: DEFAULT_NUM_HASHES,
                self_seed: self.seed,
                other_seed: other.seed,
            });
        }

        let num_strata = self.strata.len();
        let mut diff_strata = self.clone();

        for (s_self, s_other) in diff_strata.strata.iter_mut().zip(&other.strata) {
            s_self.subtract(s_other)?;
        }

        let mut decoded = vec![None; num_strata];
        let mut all_decoded = true;

        for i in (0..num_strata).rev() {
            match diff_strata.strata[i].decode() {
                Ok(diff) => {
                    decoded[i] = Some(diff);
                }
                Err(_) => {
                    all_decoded = false;
                }
            }
        }

        // Fast-Path: if stratum 0 decoded, every stratum decoded!
        // We know the exact elements without needing an extra round trip.
        if all_decoded && decoded[0].is_some() {
            let mut union_a = Vec::new();
            let mut union_b = Vec::new();

            for diff in decoded.into_iter().flatten() {
                union_a.extend(diff.a_minus_b);
                union_b.extend(diff.b_minus_a);
            }

            union_a.sort_unstable();
            union_a.dedup();
            union_b.sort_unstable();
            union_b.dedup();

            let total = union_a.len() + union_b.len();
            return Ok(StrataEstimateResult {
                estimated_diff: total,
                fully_decoded: true,
                direct_diff: Some(IbfDiff {
                    a_minus_b: union_a,
                    b_minus_a: union_b,
                }),
            });
        }

        // Normal-Path: Find the lowest stratum j* that successfully decoded.
        // Stratum j* has sampling rate 1 / 2^(j*+1).
        let lowest_successful_stratum = decoded.iter().position(Option::is_some);

        if let Some(j_star) = lowest_successful_stratum {
            // Sum of differences decoded in all strata >= j_star.
            // The probability that an element falls in stratum >= j_star is 2^(-j_star).
            let mut sum_higher = 0usize;
            for diff in decoded.iter().skip(j_star).flatten() {
                sum_higher = sum_higher.saturating_add(diff.total_diff());
            }

            let multiplier = 1usize << j_star;
            let estimate = multiplier.saturating_mul(sum_higher);

            Ok(StrataEstimateResult {
                estimated_diff: estimate,
                fully_decoded: false,
                direct_diff: None,
            })
        } else {
            // If even the top stratum failed, difference is astronomically large
            let max_fallback = 1usize << (num_strata.min(30));
            Ok(StrataEstimateResult {
                estimated_diff: max_fallback,
                fully_decoded: false,
                direct_diff: None,
            })
        }
    }
}
