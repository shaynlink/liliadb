//! Bloom filter over the user keys of a sorted run, so point lookups skip runs that cannot hold
//! the key. Layout: the bit array, then one byte holding the number of probes.

/// 64-bit hash with good avalanche (wyhash-style multiply-fold), stable across platforms.
pub fn hash(key: &[u8]) -> u64 {
    const P0: u64 = 0xA076_1D64_78BD_642F;
    const P1: u64 = 0xE703_7ED1_A0B4_28DB;
    #[allow(clippy::cast_possible_truncation)] // reason: folding keeps both halves of the product.
    let fold = |a: u64, b: u64| {
        let product = u128::from(a) * u128::from(b);
        (product as u64) ^ ((product >> 64) as u64)
    };
    let mut h = P0 ^ (key.len() as u64).wrapping_mul(P1);
    let (words, rest) = key.as_chunks::<8>();
    for word in words {
        h = fold(h ^ u64::from_le_bytes(*word), P1);
    }
    let mut tail = [0u8; 8];
    tail[..rest.len()].copy_from_slice(rest);
    fold(h ^ u64::from_le_bytes(tail), P0 ^ 0x9E37_79B9_7F4A_7C15)
}

/// Builds a filter for `hashes` with `bits_per_key` bits per key.
pub fn build(hashes: &[u64], bits_per_key: usize) -> Vec<u8> {
    // ln 2 × bits per key minimises the false-positive rate.
    let probes = u8::try_from((bits_per_key * 69 / 100).clamp(1, 30)).unwrap_or(30);
    let bits = (hashes.len() * bits_per_key).max(64);
    let bytes = bits.div_ceil(8);
    let bits = bytes * 8;
    let mut filter = vec![0u8; bytes + 1];
    for &h in hashes {
        let (mut index, step) = (h, h.rotate_right(17) | 1);
        for _ in 0..probes {
            let bit = usize::try_from(index % bits as u64).unwrap_or_default();
            filter[bit / 8] |= 1 << (bit % 8);
            index = index.wrapping_add(step);
        }
    }
    filter[bytes] = probes;
    filter
}

/// Whether `h` may be in the set; never false for a key that was added.
pub fn may_contain(filter: &[u8], h: u64) -> bool {
    let Some((&probes, bits)) = filter.split_last() else { return true };
    if bits.is_empty() || probes == 0 || probes > 30 {
        // Unknown layout: do not risk a false negative.
        return true;
    }
    let len = bits.len() as u64 * 8;
    let (mut index, step) = (h, h.rotate_right(17) | 1);
    for _ in 0..probes {
        let bit = index % len;
        if bits[(bit / 8) as usize] & (1 << (bit % 8)) == 0 {
            return false;
        }
        index = index.wrapping_add(step);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_false_negatives_and_about_one_percent_false_positives() {
        let keys: Vec<u64> = (0..10_000u32).map(|i| hash(&i.to_le_bytes())).collect();
        let filter = build(&keys, 10);
        assert!(keys.iter().all(|&h| may_contain(&filter, h)));
        let false_positives = (10_000..110_000u32).filter(|i| may_contain(&filter, hash(&i.to_le_bytes()))).count();
        assert!(false_positives < 2_000, "{false_positives} false positives in 100,000");
    }
}
