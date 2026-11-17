//! Content Addressable Storage (CAS) types, descriptors, hashing, and reference detection.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Supported hashing algorithms for Content Addressable Storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CasHashAlgorithm {
    /// SHA-256 (standard, 32 bytes output).
    #[default]
    Sha256,
    /// BLAKE3 (high-throughput, 32 bytes output).
    Blake3,
}

/// Metadata descriptor for a stored or referenced BLOB.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobDescriptor {
    /// Hex-encoded cryptographic hash of the content.
    pub hash: String,
    /// Size of the content in bytes.
    pub size_bytes: u64,
    /// Optional MIME type (e.g. "image/jpeg", "application/pdf").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

impl BlobDescriptor {
    /// Create a new `BlobDescriptor` from a hash and content size.
    #[must_use]
    pub fn new(hash: impl Into<String>, size_bytes: u64) -> Self {
        Self {
            hash: hash.into(),
            size_bytes,
            mime_type: None,
        }
    }

    /// Set an optional MIME type.
    #[must_use]
    pub fn with_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }

    /// Convert descriptor into a `serde_json::Value` (e.g. `{ "$blob": "...", "size_bytes": ... }`).
    #[must_use]
    pub fn to_json_value(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert("$blob".to_string(), serde_json::Value::String(self.hash.clone()));
        map.insert("size_bytes".to_string(), serde_json::Value::Number(self.size_bytes.into()));
        if let Some(ref mime) = self.mime_type {
            map.insert("mime_type".to_string(), serde_json::Value::String(mime.clone()));
        }
        serde_json::Value::Object(map)
    }
}

/// Configuration options for Content Addressable Storage (CAS).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CasOptions {
    /// Whether CAS offloading is enabled.
    pub enabled: bool,
    /// Size threshold in bytes above which values are stored in CAS (default: 64 KiB = 65,536).
    pub threshold_bytes: usize,
    /// Cryptographic hashing algorithm used to compute content addresses.
    pub hash_algorithm: CasHashAlgorithm,
    /// Explicit directory path for sealed blob storage (defaults to `<db_path>.blobs`).
    pub blobs_dir: Option<PathBuf>,
    /// Whether orphaned blobs (ref_count = 0) are automatically purged during WAL checkpoints.
    pub auto_purge_on_checkpoint: bool,
}

impl Default for CasOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_bytes: 64 * 1024,
            hash_algorithm: CasHashAlgorithm::Sha256,
            blobs_dir: None,
            auto_purge_on_checkpoint: true,
        }
    }
}

/// Metrics and statistics returned after an atomic Garbage Collector pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobGcStats {
    /// Number of orphaned BLOB files purged from sealed storage.
    pub orphans_purged: usize,
    /// Total bytes of disk space reclaimed.
    pub bytes_reclaimed: u64,
}

/// Streaming hasher for Content Addressable Storage.
#[allow(clippy::large_enum_variant)]
pub enum CasHasher {
    /// SHA-256 state.
    Sha256(sha2::Sha256),
    /// BLAKE3 state.
    Blake3(blake3::Hasher),
}

impl CasHasher {
    /// Create a new hasher for the given algorithm.
    #[must_use]
    pub fn new(algo: CasHashAlgorithm) -> Self {
        match algo {
            CasHashAlgorithm::Sha256 => {
                use sha2::Digest;
                Self::Sha256(sha2::Sha256::new())
            }
            CasHashAlgorithm::Blake3 => Self::Blake3(blake3::Hasher::new()),
        }
    }

    /// Update the hash with a chunk of data.
    pub fn update(&mut self, data: &[u8]) {
        match self {
            Self::Sha256(h) => {
                use sha2::Digest;
                h.update(data);
            }
            Self::Blake3(h) => {
                h.update(data);
            }
        }
    }

    /// Finalize the hash and return `(hex_string, raw_32_bytes)`.
    #[must_use]
    pub fn finalize(self) -> (String, [u8; 32]) {
        match self {
            Self::Sha256(h) => {
                use sha2::Digest;
                let res = h.finalize();
                let mut raw = [0u8; 32];
                raw.copy_from_slice(&res);
                (hex::encode(raw), raw)
            }
            Self::Blake3(h) => {
                let res = h.finalize();
                let raw = *res.as_bytes();
                (hex::encode(raw), raw)
            }
        }
    }
}

impl std::fmt::Debug for CasHasher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sha256(_) => write!(f, "CasHasher::Sha256"),
            Self::Blake3(_) => write!(f, "CasHasher::Blake3"),
        }
    }
}

/// Compute the hash of a byte slice according to the specified algorithm.
///
/// Returns a tuple of `(hex_string, raw_32_bytes)`.
#[must_use]
pub fn compute_hash(algo: CasHashAlgorithm, data: &[u8]) -> (String, [u8; 32]) {
    match algo {
        CasHashAlgorithm::Sha256 => {
            use sha2::Digest;
            let mut hasher = sha2::Sha256::new();
            hasher.update(data);
            let result = hasher.finalize();
            let mut raw = [0u8; 32];
            raw.copy_from_slice(&result);
            (hex::encode(raw), raw)
        }
        CasHashAlgorithm::Blake3 => {
            let mut hasher = blake3::Hasher::new();
            hasher.update(data);
            let result = hasher.finalize();
            let raw = *result.as_bytes();
            (hex::encode(raw), raw)
        }
    }
}

/// Magic identifier for a KV CAS pointer in SQLite (4 bytes).
pub const CAS_KV_MAGIC: &[u8; 4] = b"LBC\x01";

/// Exact byte length of a compact KV CAS pointer:
/// - 4 bytes magic `LBC\x01`
/// - 1 byte algorithm ID (1 = SHA-256, 2 = BLAKE3)
/// - 32 bytes raw content hash
/// - 8 bytes u64 little-endian payload size
pub const CAS_KV_POINTER_LEN: usize = 4 + 1 + 32 + 8; // 45 bytes

/// Encode a compact 45-byte KV pointer referencing a CAS BLOB.
#[must_use]
pub fn encode_kv_pointer(algo: CasHashAlgorithm, hash_bytes: &[u8; 32], size: u64) -> Vec<u8> {
    let mut buf = Vec::with_capacity(CAS_KV_POINTER_LEN);
    buf.extend_from_slice(CAS_KV_MAGIC);
    let algo_byte = match algo {
        CasHashAlgorithm::Sha256 => 1u8,
        CasHashAlgorithm::Blake3 => 2u8,
    };
    buf.push(algo_byte);
    buf.extend_from_slice(hash_bytes);
    buf.extend_from_slice(&size.to_le_bytes());
    buf
}

/// Decode a compact KV CAS pointer.
///
/// Returns `Some((algo, hex_hash, size_bytes))` if `data` is a valid `LBC\x01` pointer.
#[must_use]
pub fn decode_kv_pointer(data: &[u8]) -> Option<(CasHashAlgorithm, String, u64)> {
    if data.len() != CAS_KV_POINTER_LEN || !data.starts_with(CAS_KV_MAGIC) {
        return None;
    }
    let algo = match data[4] {
        1 => CasHashAlgorithm::Sha256,
        2 => CasHashAlgorithm::Blake3,
        _ => return None,
    };
    let hash_bytes = &data[5..37];
    let hash_hex = hex::encode(hash_bytes);
    let size = u64::from_le_bytes(data[37..45].try_into().ok()?);
    Some((algo, hash_hex, size))
}

/// Check if a byte slice is a KV CAS pointer.
#[inline]
#[must_use]
pub fn is_kv_pointer(data: &[u8]) -> bool {
    data.len() == CAS_KV_POINTER_LEN && data.starts_with(CAS_KV_MAGIC)
}

/// Extract all distinct `$blob` hash strings referenced anywhere inside a JSON value.
#[must_use]
pub fn extract_blob_hashes_from_value(value: &serde_json::Value) -> Vec<String> {
    let mut hashes = Vec::new();
    collect_blob_hashes(value, &mut hashes);
    hashes
}

fn collect_blob_hashes(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(hash_str)) = map.get("$blob") {
                if !hash_str.is_empty() && !out.contains(hash_str) {
                    out.push(hash_str.clone());
                }
            }
            for v in map.values() {
                collect_blob_hashes(v, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_blob_hashes(v, out);
            }
        }
        _ => {}
    }
}

/// Extract all distinct `$blob` hash strings referenced in a binary document.
#[must_use]
pub fn extract_blob_hashes_from_binary_doc(doc: &crate::json::BinaryDoc) -> Vec<String> {
    extract_blob_hashes_from_value(&doc.to_json_value())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_hash_sha256() {
        let (hex_str, raw) = compute_hash(CasHashAlgorithm::Sha256, b"hello world");
        assert_eq!(
            hex_str,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        assert_eq!(raw.len(), 32);
    }

    #[test]
    fn test_compute_hash_blake3() {
        let (hex_str, raw) = compute_hash(CasHashAlgorithm::Blake3, b"hello world");
        assert_eq!(
            hex_str,
            "d74981efa70a0c880b8d8c1985d075dbcbf679b99a5f9914e5aaf96b831a9e24"
        );
        assert_eq!(raw.len(), 32);
    }

    #[test]
    fn test_encode_decode_kv_pointer() {
        let (_, raw) = compute_hash(CasHashAlgorithm::Sha256, b"test content");
        let ptr = encode_kv_pointer(CasHashAlgorithm::Sha256, &raw, 123456);
        assert_eq!(ptr.len(), CAS_KV_POINTER_LEN);
        assert!(is_kv_pointer(&ptr));

        let (algo, hash_hex, size) = decode_kv_pointer(&ptr).unwrap();
        assert_eq!(algo, CasHashAlgorithm::Sha256);
        assert_eq!(hash_hex, hex::encode(raw));
        assert_eq!(size, 123456);
    }

    #[test]
    fn test_extract_blob_hashes_from_json() {
        let val = serde_json::json!({
            "name": "Report",
            "attachment": {
                "$blob": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "size_bytes": 42
            },
            "gallery": [
                {
                    "$blob": "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
                },
                {
                    "$blob": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                }
            ]
        });

        let hashes = extract_blob_hashes_from_value(&val);
        assert_eq!(hashes.len(), 2);
        assert!(hashes.contains(&"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string()));
        assert!(hashes.contains(&"b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9".to_string()));
    }
}
