use crate::{ErrorCode, LiliaError, Result};

/// Compress arbitrary bytes using Deflate (miniz_oxide level 6).
#[must_use]
pub fn compress_bytes(input: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec(input, 6)
}

/// Decompress Deflate-compressed bytes.
pub fn decompress_bytes(compressed: &[u8]) -> Result<Vec<u8>> {
    miniz_oxide::inflate::decompress_to_vec(compressed).map_err(|error| {
        LiliaError::new(
            ErrorCode::Corrupt,
            format!("failed to decompress data: {error:?}"),
            false,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_decompress_roundtrip() {
        let original =
            b"The quick brown fox jumps over the lazy dog. Repetition repetition repetition!";
        let compressed = compress_bytes(original);
        assert!(!compressed.is_empty());
        let decompressed = decompress_bytes(&compressed).expect("decompression succeeds");
        assert_eq!(decompressed, original);
    }

    #[test]
    fn test_empty_roundtrip() {
        let original = b"";
        let compressed = compress_bytes(original);
        let decompressed = decompress_bytes(&compressed).expect("empty decompression succeeds");
        assert_eq!(decompressed, original);
    }

    #[test]
    fn test_corrupt_data() {
        let corrupt = b"not valid deflate compressed data";
        assert!(decompress_bytes(corrupt).is_err());
    }
}
