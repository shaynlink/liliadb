//! Invertible Bloom Filter Cell implementation.
//!
//! Stores count, XOR sum of 128-bit item keys, and XOR sum of 32-bit verification checksums.

#![allow(clippy::doc_markdown)]

use serde::{Deserialize, Serialize};

/// Fixed 32-byte domain separation key for IBF checksum derivation.
const CHECKSUM_KEY: [u8; 32] = *b"LiliaIBFChecksumSeed-V1.0-BLAKE3";

/// Computes a 32-bit verification checksum for an element using keyed BLAKE3.
#[inline]
#[must_use]
pub fn compute_checksum(key: u128) -> u32 {
    let mut hasher = blake3::Hasher::new_keyed(&CHECKSUM_KEY);
    hasher.update(&key.to_le_bytes());
    let hash = hasher.finalize();
    let bytes = hash.as_bytes();
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// A single cell in an Invertible Bloom Filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct IbfCell {
    /// Net count of elements mapped to this bucket (+1 per insert, -1 per delete).
    pub count: i32,
    /// XOR sum of 128-bit element keys.
    pub key_sum: u128,
    /// XOR sum of 32-bit element verification checksums.
    pub hash_sum: u32,
}

impl IbfCell {
    /// An empty cell with zero count, `key_sum`, and `hash_sum`.
    pub const EMPTY: Self = Self {
        count: 0,
        key_sum: 0,
        hash_sum: 0,
    };

    /// Adds an element into this cell.
    #[inline]
    pub fn add(&mut self, key: u128, checksum: u32) {
        self.count = self.count.saturating_add(1);
        self.key_sum ^= key;
        self.hash_sum ^= checksum;
    }

    /// Removes an element from this cell.
    #[inline]
    pub fn remove(&mut self, key: u128, checksum: u32) {
        self.count = self.count.saturating_sub(1);
        self.key_sum ^= key;
        self.hash_sum ^= checksum;
    }

    /// Subtracts another cell from this cell (vector subtraction for set difference).
    #[inline]
    pub fn subtract(&mut self, other: &Self) {
        self.count = self.count.saturating_sub(other.count);
        self.key_sum ^= other.key_sum;
        self.hash_sum ^= other.hash_sum;
    }

    /// Checks if this cell is "pure" (contains exactly one element with high confidence).
    /// Returns `Some((count, key))` where count is +1 (element in A \ B) or -1 (element in B \ A).
    #[inline]
    #[must_use]
    pub fn is_pure(&self) -> Option<(i32, u128)> {
        if self.count == 1 || self.count == -1 {
            let chk = compute_checksum(self.key_sum);
            if chk == self.hash_sum {
                return Some((self.count, self.key_sum));
            }
        }
        None
    }

    /// Returns `true` if this cell has completely cancelled out to zero.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count == 0 && self.key_sum == 0 && self.hash_sum == 0
    }
}
