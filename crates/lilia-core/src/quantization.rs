//! Quantization helpers and SIMD-friendly vector distance metrics.

/// Quantizes a slice of 32-bit floating point numbers to signed 8-bit integers using symmetric scaling.
///
/// Returns the quantized `Vec<i8>` and the scaling factor (`scale`).
/// To approximate the original floats: `float ≈ i8_val as f32 * scale`.
#[must_use]
pub fn quantize_f32_to_i8(floats: &[f32]) -> (Vec<i8>, f32) {
    if floats.is_empty() {
        return (Vec::new(), 1.0);
    }
    let mut max_abs = 0.0f32;
    for &val in floats {
        let abs = val.abs();
        if abs > max_abs {
            max_abs = abs;
        }
    }
    if max_abs == 0.0 {
        return (vec![0i8; floats.len()], 1.0);
    }
    let scale = max_abs / 127.0;
    let inv_scale = 1.0 / scale;
    let mut quantized = Vec::with_capacity(floats.len());
    for &val in floats {
        let q = (val * inv_scale).round().clamp(-128.0, 127.0) as i8;
        quantized.push(q);
    }
    (quantized, scale)
}

/// Dequantizes a slice of signed 8-bit integers back to 32-bit floating point numbers using `scale`.
#[must_use]
pub fn dequantize_i8_to_f32(i8s: &[i8], scale: f32) -> Vec<f32> {
    i8s.iter().map(|&q| (q as f32) * scale).collect()
}

/// Quantizes a slice of 32-bit floating point numbers to 1-bit binary representations.
///
/// Each float > 0.0 produces a 1 bit, while floats <= 0.0 produce a 0 bit.
/// Bits are packed MSB-first into bytes (`(floats.len() + 7) / 8` bytes).
#[must_use]
pub fn quantize_f32_to_bits(floats: &[f32]) -> Vec<u8> {
    let num_bytes = floats.len().div_ceil(8);
    let mut packed = vec![0u8; num_bytes];
    for (i, &val) in floats.iter().enumerate() {
        if val > 0.0 {
            let byte_idx = i / 8;
            let bit_idx = 7 - (i % 8);
            packed[byte_idx] |= 1 << bit_idx;
        }
    }
    packed
}

/// Calculates the cosine distance between two 32-bit float vectors.
///
/// Returns `1.0 - cosine_similarity`, clamped in `[0.0, 2.0]`.
/// If either vector has zero magnitude, returns `1.0`.
#[must_use]
pub fn cosine_distance_f32(a: &[f32], b: &[f32]) -> f64 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 1.0;
    }
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;
    for i in 0..len {
        let va = a[i] as f64;
        let vb = b[i] as f64;
        dot += va * vb;
        norm_a += va * va;
        norm_b += vb * vb;
    }
    if norm_a <= 0.0 || norm_b <= 0.0 {
        return 1.0;
    }
    let sim = dot / (norm_a.sqrt() * norm_b.sqrt());
    (1.0 - sim).clamp(0.0, 2.0)
}

/// Calculates the cosine distance directly between two signed 8-bit integer vectors.
///
/// Because symmetric scale factors cancel out in cosine similarity ($\frac{s_a \cdot a \cdot s_b \cdot b}{\|s_a \cdot a\| \|s_b \cdot b\|} = \frac{a \cdot b}{\|a\| \|b\|}$),
/// no float dequantization is necessary.
#[must_use]
pub fn cosine_distance_i8(a: &[i8], b: &[i8]) -> f64 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 1.0;
    }
    let mut dot = 0i64;
    let mut norm_a = 0i64;
    let mut norm_b = 0i64;
    for i in 0..len {
        let va = a[i] as i64;
        let vb = b[i] as i64;
        dot += va * vb;
        norm_a += va * va;
        norm_b += vb * vb;
    }
    if norm_a <= 0 || norm_b <= 0 {
        return 1.0;
    }
    let sim = (dot as f64) / ((norm_a as f64).sqrt() * (norm_b as f64).sqrt());
    (1.0 - sim).clamp(0.0, 2.0)
}

/// Calculates the Euclidean (L2) distance between two 32-bit float vectors.
#[must_use]
pub fn l2_distance_f32(a: &[f32], b: &[f32]) -> f64 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 0.0;
    }
    let mut sum_sq = 0.0f64;
    for i in 0..len {
        let diff = (a[i] - b[i]) as f64;
        sum_sq += diff * diff;
    }
    sum_sq.sqrt()
}

/// Calculates the dot product distance between two 32-bit float vectors (`-dot_product`).
#[must_use]
pub fn dot_distance_f32(a: &[f32], b: &[f32]) -> f64 {
    let len = a.len().min(b.len());
    let mut dot = 0.0f64;
    for i in 0..len {
        dot += (a[i] * b[i]) as f64;
    }
    -dot
}

/// Calculates the Hamming distance between two packed bit vectors.
///
/// Uses the CPU `count_ones` instruction (POPCOUNT).
#[must_use]
pub fn hamming_distance_bits(a: &[u8], b: &[u8]) -> u32 {
    let len = a.len().min(b.len());
    let mut dist = 0u32;
    for i in 0..len {
        dist += (a[i] ^ b[i]).count_ones();
    }
    dist
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quantize_dequantize_f32_to_i8_roundtrip() {
        let original = vec![0.0, 0.5, -0.5, 1.0, -1.0, 0.123];
        let (quantized, scale) = quantize_f32_to_i8(&original);
        assert_eq!(quantized.len(), original.len());
        assert_eq!(quantized[3], 127);
        assert_eq!(quantized[4], -127);
        let restored = dequantize_i8_to_f32(&quantized, scale);
        for (orig, rest) in original.iter().zip(restored.iter()) {
            assert!((orig - rest).abs() < 0.02);
        }
    }

    #[test]
    fn test_quantize_f32_to_bits_and_hamming() {
        let v1 = vec![1.0, -1.0, 2.0, -0.5, 0.1, 0.2, -3.0, 4.0];
        let v2 = vec![1.0, 1.0, 2.0, -0.5, 0.1, -0.2, -3.0, 4.0];
        let b1 = quantize_f32_to_bits(&v1);
        let b2 = quantize_f32_to_bits(&v2);
        assert_eq!(b1.len(), 1);
        assert_eq!(b2.len(), 1);
        // Bit differences at index 1 (-1.0 vs 1.0) and index 5 (0.2 vs -0.2) -> 2 differences
        assert_eq!(hamming_distance_bits(&b1, &b2), 2);
    }

    #[test]
    fn test_cosine_distance_f32_and_i8() {
        let v1 = vec![1.0, 2.0, 3.0];
        let v2 = vec![1.0, 2.0, 3.0];
        let v3 = vec![-1.0, -2.0, -3.0];
        let v4 = vec![1.0, 0.0, 0.0];
        let v5 = vec![0.0, 1.0, 0.0];

        // Same direction -> cosine dist = 0.0
        assert!(cosine_distance_f32(&v1, &v2) < 1e-6);
        // Opposite direction -> cosine dist = 2.0
        assert!((cosine_distance_f32(&v1, &v3) - 2.0).abs() < 1e-6);
        // Orthogonal -> cosine dist = 1.0
        assert!((cosine_distance_f32(&v4, &v5) - 1.0).abs() < 1e-6);

        // Test i8 cosine distance
        let (q1, _) = quantize_f32_to_i8(&v1);
        let (q2, _) = quantize_f32_to_i8(&v2);
        let (q3, _) = quantize_f32_to_i8(&v3);
        assert!(cosine_distance_i8(&q1, &q2) < 1e-6);
        assert!((cosine_distance_i8(&q1, &q3) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn test_l2_and_dot_distances() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 6.0, 3.0];
        // diff: [3, 4, 0] -> sqrt(9 + 16) = 5
        assert!((l2_distance_f32(&a, &b) - 5.0).abs() < 1e-6);
        // dot: 1*4 + 2*6 + 3*3 = 4 + 12 + 9 = 25 -> dot_distance = -25
        assert!((dot_distance_f32(&a, &b) - (-25.0)).abs() < 1e-6);
    }
}
