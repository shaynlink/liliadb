//! Galois Field GF(2^8) arithmetic and Reed-Solomon erasure coding.
//!
//! Provides 100% safe Rust Reed-Solomon encoding and erasure decoding over GF(2^8),
//! designed for physical NAND flash auto-repair, bitflip detection, and page reconstruction.

use serde::{Deserialize, Serialize};

use crate::{ErrorCode, LiliaError, Result};

/// Irreducible polynomial for GF(2^8): x^8 + x^4 + x^3 + x^2 + 1 (0x11d, decimal 285).
const GF_POLYNOMIAL: usize = 0x11d;

/// Precomputed tables for GF(2^8) arithmetic.
pub struct Gf256 {
    exp: [u8; 512],
    log: [u8; 256],
}

impl std::fmt::Debug for Gf256 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gf256").finish()
    }
}

impl Gf256 {
    /// Initializes exponential and logarithm tables for GF(2^8).
    #[must_use]
    pub const fn new() -> Self {
        let mut exp = [0u8; 512];
        let mut log = [0u8; 256];

        let mut x = 1usize;
        let mut i = 0usize;
        while i < 255 {
            exp[i] = x as u8;
            exp[i + 255] = x as u8;
            log[x] = i as u8;
            x <<= 1;
            if x >= 256 {
                x ^= GF_POLYNOMIAL;
            }
            i += 1;
        }
        exp[510] = exp[0];
        exp[511] = exp[1];

        Self { exp, log }
    }

    /// Addition in GF(2^8): bitwise XOR.
    #[inline(always)]
    #[must_use]
    pub const fn add(&self, a: u8, b: u8) -> u8 {
        a ^ b
    }

    /// Subtraction in GF(2^8): identical to addition (bitwise XOR).
    #[inline(always)]
    #[must_use]
    pub const fn sub(&self, a: u8, b: u8) -> u8 {
        a ^ b
    }

    /// Multiplication in GF(2^8).
    #[inline(always)]
    #[must_use]
    pub const fn mul(&self, a: u8, b: u8) -> u8 {
        if a == 0 || b == 0 {
            0
        } else {
            let log_a = self.log[a as usize] as usize;
            let log_b = self.log[b as usize] as usize;
            self.exp[log_a + log_b]
        }
    }

    /// Division in GF(2^8).
    ///
    /// # Errors
    /// Returns an error if attempting to divide by zero.
    #[inline(always)]
    pub fn div(&self, a: u8, b: u8) -> Result<u8> {
        if b == 0 {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "GF(2^8) division by zero",
                false,
            ));
        }
        if a == 0 {
            Ok(0)
        } else {
            let log_a = self.log[a as usize] as usize;
            let log_b = self.log[b as usize] as usize;
            Ok(self.exp[log_a + 255 - log_b])
        }
    }

    /// Multiplicative inverse in GF(2^8): a^(-1) such that a * a^(-1) = 1.
    ///
    /// # Errors
    /// Returns an error if `a == 0`.
    #[inline(always)]
    pub fn inv(&self, a: u8) -> Result<u8> {
        if a == 0 {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "GF(2^8) inverse of zero",
                false,
            ));
        }
        let log_a = self.log[a as usize] as usize;
        Ok(self.exp[255 - log_a])
    }
}

impl Default for Gf256 {
    fn default() -> Self {
        Self::new()
    }
}

/// Static shared GF(2^8) instance.
pub static GF: Gf256 = Gf256::new();

/// Systematic Reed-Solomon Erasure Codec using a Cauchy Generator Matrix.
///
/// For `k` data shards and `m` parity shards, any subset of `k` surviving shards
/// is mathematically sufficient to reconstruct the entire group of `k + m` shards.
#[derive(Debug, Clone)]
pub struct ReedSolomon {
    data_shards: usize,
    parity_shards: usize,
    /// Generator matrix of size (data_shards + parity_shards) x data_shards.
    /// Top (data_shards x data_shards) is Identity matrix.
    /// Bottom (parity_shards x data_shards) is Cauchy matrix.
    matrix: Vec<Vec<u8>>,
}

impl ReedSolomon {
    /// Creates a new Reed-Solomon erasure codec.
    ///
    /// # Errors
    /// Fails if `data_shards == 0`, `parity_shards == 0`, or `data_shards + parity_shards > 256`.
    pub fn new(data_shards: usize, parity_shards: usize) -> Result<Self> {
        if data_shards == 0 || parity_shards == 0 {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "data_shards and parity_shards must be non-zero",
                false,
            ));
        }
        if data_shards + parity_shards > 256 {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "total shards (k + m) cannot exceed 256 in GF(2^8)",
                false,
            ));
        }

        let total_shards = data_shards + parity_shards;
        let mut matrix = vec![vec![0u8; data_shards]; total_shards];

        // Systematic: Top k rows form Identity matrix
        for (i, row) in matrix.iter_mut().take(data_shards).enumerate() {
            row[i] = 1;
        }

        // Bottom m rows: Cauchy matrix C[i][j] = 1 / (x_i ^ y_j)
        // With x_i = i, y_j = m + j.
        for i in 0..parity_shards {
            let x = i as u8;
            for (j, cell) in matrix[data_shards + i].iter_mut().enumerate() {
                let y = (parity_shards + j) as u8;
                let diff = GF.add(x, y);
                *cell = GF.inv(diff)?;
            }
        }

        Ok(Self {
            data_shards,
            parity_shards,
            matrix,
        })
    }

    #[must_use]
    pub const fn data_shards(&self) -> usize {
        self.data_shards
    }

    #[must_use]
    pub const fn parity_shards(&self) -> usize {
        self.parity_shards
    }

    #[must_use]
    pub const fn total_shards(&self) -> usize {
        self.data_shards + self.parity_shards
    }

    /// Encodes `data` shards to produce `parity` shards.
    ///
    /// Each slice in `data` and `parity` must have the same length.
    ///
    /// # Errors
    /// Returns an error if slice lengths mismatch or slice counts do not match configured parameters.
    pub fn encode(&self, data: &[&[u8]], parity: &mut [&mut [u8]]) -> Result<()> {
        if data.len() != self.data_shards {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!(
                    "expected {} data shards, got {}",
                    self.data_shards,
                    data.len()
                ),
                false,
            ));
        }
        if parity.len() != self.parity_shards {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!(
                    "expected {} parity shards, got {}",
                    self.parity_shards,
                    parity.len()
                ),
                false,
            ));
        }

        let shard_len = data[0].len();
        for (i, d) in data.iter().enumerate() {
            if d.len() != shard_len {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("data shard {i} length {} != expected {shard_len}", d.len()),
                    false,
                ));
            }
        }
        for (i, p) in parity.iter_mut().enumerate() {
            if p.len() != shard_len {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!(
                        "parity shard {i} length {} != expected {shard_len}",
                        p.len()
                    ),
                    false,
                ));
            }
            p.fill(0);
        }

        for (p_idx, p_buf) in parity.iter_mut().enumerate() {
            let row = &self.matrix[self.data_shards + p_idx];
            for (d_idx, d_buf) in data.iter().enumerate() {
                let coeff = row[d_idx];
                if coeff == 1 {
                    for (dst, &src) in p_buf.iter_mut().zip(d_buf.iter()) {
                        *dst ^= src;
                    }
                } else if coeff != 0 {
                    for (dst, &src) in p_buf.iter_mut().zip(d_buf.iter()) {
                        *dst ^= GF.mul(coeff, src);
                    }
                }
            }
        }

        Ok(())
    }

    /// Reconstructs missing shards in-place.
    ///
    /// `shards` is a slice of `total_shards` elements.
    /// Present shards contain `Some(bytes)`. Erased or corrupted shards contain `None`.
    /// `erased_indices` lists the indices in `0..total_shards` that must be reconstructed.
    ///
    /// Returns the reconstructed shards as byte vectors indexed by shard index.
    ///
    /// # Errors
    /// Returns an error if fewer than `data_shards` are present, or matrix inversion fails.
    pub fn reconstruct_erased(
        &self,
        shards: &[Option<&[u8]>],
        erased_indices: &[usize],
    ) -> Result<Vec<(usize, Vec<u8>)>> {
        if shards.len() != self.total_shards() {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!(
                    "expected {} shards, got {}",
                    self.total_shards(),
                    shards.len()
                ),
                false,
            ));
        }

        if erased_indices.is_empty() {
            return Ok(Vec::new());
        }

        // Determine shard byte length from first surviving shard
        let shard_len = shards
            .iter()
            .flatten()
            .map(|s| s.len())
            .next()
            .ok_or_else(|| {
                LiliaError::new(
                    ErrorCode::InvalidInput,
                    "no surviving shards available for reconstruction",
                    false,
                )
            })?;

        // Collect k surviving shards
        let mut submatrix = Vec::with_capacity(self.data_shards);
        let mut surviving_indices = Vec::with_capacity(self.data_shards);

        for (idx, shard_opt) in shards.iter().enumerate() {
            if shard_opt.is_some() && !erased_indices.contains(&idx) {
                submatrix.push(self.matrix[idx].clone());
                surviving_indices.push(idx);
                if submatrix.len() == self.data_shards {
                    break;
                }
            }
        }

        if submatrix.len() < self.data_shards {
            return Err(LiliaError::new(
                ErrorCode::Storage,
                format!(
                    "insufficient surviving shards for Reed-Solomon recovery: need {}, available {}",
                    self.data_shards,
                    submatrix.len()
                ),
                false,
            ));
        }

        // Invert the k x k submatrix using Gaussian elimination
        let inverted = invert_matrix(&submatrix)?;

        // First reconstruct any missing data shards (indices 0..k)
        let mut reconstructed_data = vec![vec![0u8; shard_len]; self.data_shards];

        for d_idx in 0..self.data_shards {
            // Check if data shard is already surviving
            if let Some(pos) = surviving_indices.iter().position(|&s| s == d_idx) {
                let surviving = shards[surviving_indices[pos]].expect("present shard");
                reconstructed_data[d_idx].copy_from_slice(surviving);
            } else {
                // Recover data shard d_idx via inverted matrix row d_idx
                let inv_row = &inverted[d_idx];
                for (s_idx, &surv_pos) in surviving_indices.iter().enumerate() {
                    let coeff = inv_row[s_idx];
                    let surv_buf = shards[surv_pos].expect("present shard");
                    if coeff == 1 {
                        for (dst, &src) in reconstructed_data[d_idx].iter_mut().zip(surv_buf.iter())
                        {
                            *dst ^= src;
                        }
                    } else if coeff != 0 {
                        for (dst, &src) in reconstructed_data[d_idx].iter_mut().zip(surv_buf.iter())
                        {
                            *dst ^= GF.mul(coeff, src);
                        }
                    }
                }
            }
        }

        // Build results for requested erased indices
        let mut results = Vec::with_capacity(erased_indices.len());
        for &erased in erased_indices {
            if erased < self.data_shards {
                results.push((erased, reconstructed_data[erased].clone()));
            } else {
                // Reconstruct parity shard
                let p_idx = erased - self.data_shards;
                let mut parity_buf = vec![0u8; shard_len];
                let row = &self.matrix[self.data_shards + p_idx];
                for (d_idx, d_buf) in reconstructed_data.iter().enumerate() {
                    let coeff = row[d_idx];
                    if coeff == 1 {
                        for (dst, &src) in parity_buf.iter_mut().zip(d_buf.iter()) {
                            *dst ^= src;
                        }
                    } else if coeff != 0 {
                        for (dst, &src) in parity_buf.iter_mut().zip(d_buf.iter()) {
                            *dst ^= GF.mul(coeff, src);
                        }
                    }
                }
                results.push((erased, parity_buf));
            }
        }

        Ok(results)
    }
}

/// Inverts a square matrix in GF(2^8) using Gauss-Jordan elimination.
fn invert_matrix(matrix: &[Vec<u8>]) -> Result<Vec<Vec<u8>>> {
    let n = matrix.len();
    let mut a: Vec<Vec<u8>> = matrix.to_vec();
    let mut inv = vec![vec![0u8; n]; n];
    for (i, row) in inv.iter_mut().enumerate() {
        row[i] = 1;
    }

    for col in 0..n {
        // Find pivot
        let mut pivot_row = None;
        for (row, a_row) in a.iter().enumerate().skip(col) {
            if a_row[col] != 0 {
                pivot_row = Some(row);
                break;
            }
        }

        let pivot = pivot_row.ok_or_else(|| {
            LiliaError::new(
                ErrorCode::Storage,
                "singular submatrix encountered during RS reconstruction",
                false,
            )
        })?;

        if pivot != col {
            a.swap(col, pivot);
            inv.swap(col, pivot);
        }

        // Normalize pivot row
        let pivot_val = a[col][col];
        if pivot_val != 1 {
            let inv_pivot = GF.inv(pivot_val)?;
            for j in 0..n {
                a[col][j] = GF.mul(a[col][j], inv_pivot);
                inv[col][j] = GF.mul(inv[col][j], inv_pivot);
            }
        }

        // Eliminate other rows
        for row in 0..n {
            if row != col {
                let factor = a[row][col];
                if factor != 0 {
                    for j in 0..n {
                        let sub_a = GF.mul(factor, a[col][j]);
                        a[row][j] = GF.sub(a[row][j], sub_a);
                        let sub_inv = GF.mul(factor, inv[col][j]);
                        inv[row][j] = GF.sub(inv[row][j], sub_inv);
                    }
                }
            }
        }
    }

    Ok(inv)
}

/// 16-byte BLAKE3 checksum for physical page validation.
pub type PageChecksum = [u8; 16];

/// Computes the truncated 16-byte BLAKE3 checksum of a page.
#[must_use]
pub fn compute_page_checksum(data: &[u8]) -> PageChecksum {
    let mut hash = [0u8; 16];
    let b3 = blake3::hash(data);
    hash.copy_from_slice(&b3.as_bytes()[..16]);
    hash
}

/// Parity group configuration for mobile flash tolerance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParityConfig {
    /// Number of data pages per parity group (default 8).
    pub data_shards: usize,
    /// Number of Reed-Solomon parity blocks per group (default 1).
    pub parity_shards: usize,
    /// Whether to perform automatic pre-flight verification on database open.
    pub auto_repair_on_open: bool,
}

impl Default for ParityConfig {
    fn default() -> Self {
        Self {
            data_shards: 8,
            parity_shards: 1,
            auto_repair_on_open: true,
        }
    }
}

impl ParityConfig {
    /// Validates parameters.
    ///
    /// # Errors
    /// Returns error if data_shards is 0 or total shards > 256.
    pub fn validate(&self) -> Result<()> {
        if self.data_shards == 0 || self.parity_shards == 0 {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "data_shards and parity_shards must be non-zero",
                false,
            ));
        }
        if self.data_shards + self.parity_shards > 256 {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "total shards (k + m) cannot exceed 256",
                false,
            ));
        }
        Ok(())
    }
}

/// Summary report of an auto-repair operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRepairOutcome {
    pub page_id: u32,
    pub group_index: u64,
    pub checksum_matches: bool,
    pub bytes_reconstructed: usize,
}

/// Overall database repair summary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairSummary {
    pub total_pages_inspected: usize,
    pub total_groups_inspected: usize,
    pub bitflips_detected: usize,
    pub pages_repaired: usize,
    pub unrecoverable_pages: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gf256_arithmetic() {
        assert_eq!(GF.add(42, 42), 0);
        assert_eq!(GF.sub(42, 42), 0);
        assert_eq!(GF.mul(0, 100), 0);
        assert_eq!(GF.mul(100, 0), 0);
        assert_eq!(GF.mul(1, 100), 100);

        for a in 1..=255u8 {
            let inv = GF.inv(a).unwrap();
            assert_eq!(GF.mul(a, inv), 1, "inv check failed for {a}");
        }

        for a in 1..=255u8 {
            for b in 1..=10u8 {
                let div = GF.div(a, b).unwrap();
                assert_eq!(GF.mul(div, b), a, "div check failed for {a}/{b}");
            }
        }
    }

    #[test]
    fn test_reed_solomon_single_parity_reconstruct() {
        let rs = ReedSolomon::new(4, 1).unwrap();
        let p1 = [0x11u8; 64];
        let p2 = vec![0x22u8; 64];
        let p3 = [0x33u8; 64];
        let p4 = [0x44u8; 64];

        let data = [&p1[..], &p2[..], &p3[..], &p4[..]];
        let mut parity_buf = [0u8; 64];
        let mut parity = [&mut parity_buf[..]];

        rs.encode(&data, &mut parity).unwrap();

        // Simulate erasure of p2 (index 1)
        let shards: Vec<Option<&[u8]>> = vec![
            Some(&p1[..]),
            None, // erased!
            Some(&p3[..]),
            Some(&p4[..]),
            Some(&parity_buf[..]),
        ];

        let recovered = rs.reconstruct_erased(&shards, &[1]).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].0, 1);
        assert_eq!(
            recovered[0].1, p2,
            "reconstructed p2 must match byte-for-byte"
        );
    }

    #[test]
    fn test_reed_solomon_double_parity_two_erasures() {
        let rs = ReedSolomon::new(6, 2).unwrap();
        let pages: Vec<Vec<u8>> = (0..6u8).map(|i| vec![i + 10; 128]).collect();
        let data: Vec<&[u8]> = pages.iter().map(Vec::as_slice).collect();

        let mut par0 = vec![0u8; 128];
        let mut par1 = vec![0u8; 128];
        let mut parity = [&mut par0[..], &mut par1[..]];

        rs.encode(&data, &mut parity).unwrap();

        // Erase pages 0 and 3
        let shards: Vec<Option<&[u8]>> = vec![
            None, // 0 erased
            Some(&pages[1]),
            Some(&pages[2]),
            None, // 3 erased
            Some(&pages[4]),
            Some(&pages[5]),
            Some(&par0),
            Some(&par1),
        ];

        let recovered = rs.reconstruct_erased(&shards, &[0, 3]).unwrap();
        assert_eq!(recovered.len(), 2);
        assert_eq!(recovered[0].1, pages[0]);
        assert_eq!(recovered[1].1, pages[3]);
    }

    #[test]
    fn test_page_checksum_bitflip_detection() {
        let mut page = vec![0xAAu8; 4096];
        let orig_hash = compute_page_checksum(&page);

        // Single bitflip at offset 1337
        page[1337] ^= 0x01;
        let corrupted_hash = compute_page_checksum(&page);

        assert_ne!(
            orig_hash, corrupted_hash,
            "bitflip must be detected by BLAKE3"
        );
    }
}
