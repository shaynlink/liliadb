//! Zstandard dictionary compression engine with hybrid native & WASM support.
//!
//! On native targets (macOS, Linux, Windows, Android, iOS), libzstd is used for dictionary
//! training, streaming, and bulk compression/decompression.
//! On `wasm32-unknown-unknown`, `ruzstd` is used for 100% pure-Rust dictionary decompression.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::sync::{Arc, RwLock};

use crate::{ErrorCode, LiliaError, Result};

/// Magic header for dictionary-compressed binary documents with 64-bit symbol signature mask (v2).
pub const MAGIC_ZSTD: &[u8; 4] = b"LBZ\x02";
/// Legacy magic header without symbol signature mask (v1).
pub const MAGIC_ZSTD_V1: &[u8; 4] = b"LBZ\x01";

/// Compact 64-bit symbol signature for compressed-domain filtering.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SymbolSignature(pub u64);

impl SymbolSignature {
    pub const EMPTY: Self = Self(0);

    #[must_use]
    pub const fn new(mask: u64) -> Self {
        Self(mask)
    }

    #[must_use]
    pub const fn matches(self, query: Self) -> bool {
        (self.0 & query.0) == query.0
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Hash an arbitrary token to a 2-bit Bloom signature in a 64-bit mask.
#[must_use]
pub fn hash_token_to_mask(token: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in token.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let bit1 = (hash & 0x3f) as u8;
    let bit2 = ((hash >> 6) & 0x3f) as u8;
    (1u64 << bit1) | (1u64 << bit2)
}

fn collect_tokens_from_binary_value(val: crate::json::BinaryValueRef<'_>, out: &mut Vec<String>) {
    match val {
        crate::json::BinaryValueRef::String(s) => {
            out.push(s.to_string());
        }
        crate::json::BinaryValueRef::Int64(i) => {
            out.push(i.to_string());
        }
        crate::json::BinaryValueRef::Float64(f) => {
            out.push(f.to_string());
        }
        crate::json::BinaryValueRef::Bool(b) => {
            out.push(if b { "true".into() } else { "false".into() });
        }
        crate::json::BinaryValueRef::Array(arr) => {
            for item in arr.iter() {
                collect_tokens_from_binary_value(item, out);
            }
        }
        crate::json::BinaryValueRef::Object(obj) => {
            for (k, v) in obj.iter() {
                out.push(k.to_string());
                collect_tokens_from_binary_value(v, out);
            }
        }
        _ => {}
    }
}

fn collect_tokens_from_json_value(val: &serde_json::Value, out: &mut Vec<String>) {
    match val {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Number(n) => out.push(n.to_string()),
        serde_json::Value::Bool(b) => out.push(if *b { "true".into() } else { "false".into() }),
        serde_json::Value::Array(arr) => {
            for item in arr {
                collect_tokens_from_json_value(item, out);
            }
        }
        serde_json::Value::Object(obj) => {
            for (k, v) in obj {
                out.push(k.clone());
                collect_tokens_from_json_value(v, out);
            }
        }
        serde_json::Value::Null => {}
    }
}

/// Extract printable alphanumeric tokens from arbitrary byte slices (JSON or binary JSON).
#[must_use]
pub fn extract_tokens_from_bytes(bytes: &[u8]) -> Vec<String> {
    let mut tokens = Vec::new();

    // 1. Try zero-copy BinaryDoc if it has LBJ\x01 magic
    if bytes.starts_with(crate::json::MAGIC) {
        if let Ok(root) = crate::json::BinaryDoc::read_root_from_slice(bytes) {
            collect_tokens_from_binary_value(root, &mut tokens);
            return tokens;
        }
    }

    // 2. Try JSON parse if it looks like JSON
    if bytes.starts_with(b"{") || bytes.starts_with(b"[") {
        if let Ok(val) = serde_json::from_slice::<serde_json::Value>(bytes) {
            collect_tokens_from_json_value(&val, &mut tokens);
            return tokens;
        }
    }

    // 3. Fallback: scan ASCII tokens from raw buffer
    let mut start: Option<usize> = None;

    for (i, &b) in bytes.iter().enumerate() {
        let is_token_char = b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
        if is_token_char {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            let len = i - s;
            if (2..=64).contains(&len) {
                if let Ok(tok) = std::str::from_utf8(&bytes[s..i]) {
                    tokens.push(tok.to_string());
                }
            }
        }
    }

    if let Some(s) = start {
        let len = bytes.len() - s;
        if (2..=64).contains(&len) {
            if let Ok(tok) = std::str::from_utf8(&bytes[s..]) {
                tokens.push(tok.to_string());
            }
        }
    }

    tokens
}

/// Symbol table mapping collection vocabulary to 64-bit signatures.
#[derive(Clone, Debug, Default)]
pub struct DictionarySymbolTable {
    /// Mapping of top-64 vocabulary tokens to dedicated single bitmasks (1 << i)
    vocabulary: HashMap<String, u64>,
}

impl DictionarySymbolTable {
    #[must_use]
    pub fn new() -> Self {
        Self {
            vocabulary: HashMap::new(),
        }
    }

    /// Construct a symbol table from dictionary bytes and sample documents.
    #[must_use]
    pub fn from_dict_and_samples(dict_bytes: &[u8], samples: &[&[u8]]) -> Self {
        let mut freq: HashMap<String, usize> = HashMap::new();

        // 1. Extract tokens from sample documents
        for sample in samples {
            for token in extract_tokens_from_bytes(sample) {
                *freq.entry(token).or_insert(0) += 1;
            }
        }

        // 2. Also extract printable tokens from dictionary payload
        for token in extract_tokens_from_bytes(dict_bytes) {
            *freq.entry(token).or_insert(0) += 2;
        }

        // 3. Sort by frequency descending and assign top-64 tokens dedicated bits
        let mut sorted: Vec<(String, usize)> = freq.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let mut vocabulary = HashMap::new();
        for (i, (token, _)) in sorted.into_iter().take(64).enumerate() {
            let shift = u8::try_from(i).unwrap_or(0);
            vocabulary.insert(token, 1u64 << shift);
        }

        Self { vocabulary }
    }

    /// Get the 64-bit signature for a token (exact bit if in vocabulary, or 2-bit Bloom hash).
    #[must_use]
    pub fn token_signature(&self, token: &str) -> u64 {
        if let Some(&mask) = self.vocabulary.get(token) {
            mask
        } else {
            hash_token_to_mask(token)
        }
    }

    /// Compute signature for a slice of query tokens (logical AND requirement => bitwise OR).
    #[must_use]
    pub fn query_signature(&self, tokens: &[&str]) -> u64 {
        let mut sig = 0u64;
        for token in tokens {
            sig |= self.token_signature(token);
        }
        sig
    }

    /// Compute the 64-bit symbol mask for a document.
    #[must_use]
    pub fn compute_doc_signature(&self, doc_bytes: &[u8]) -> u64 {
        let mut mask = 0u64;
        for token in extract_tokens_from_bytes(doc_bytes) {
            mask |= self.token_signature(&token);
        }
        mask
    }

    /// Number of vocabulary tokens indexed with dedicated bits.
    #[must_use]
    pub fn vocabulary_len(&self) -> usize {
        self.vocabulary.len()
    }
}

/// Extract symbol signature mask from a stored compressed blob.
/// Returns `Some(mask)` if format is V2 (`LBZ\x02`), or `None` if legacy V1.
#[must_use]
pub fn extract_symbol_signature(blob: &[u8]) -> Option<u64> {
    if blob.starts_with(MAGIC_ZSTD) && blob.len() >= 16 {
        blob[8..16].try_into().ok().map(u64::from_le_bytes)
    } else {
        None
    }
}

/// In-memory representation of an active collection dictionary.
#[derive(Clone, Debug)]
pub struct CollectionDictionary {
    pub space: String,
    pub dict_id: u32,
    pub raw_bytes: Vec<u8>,
    pub symbols: Arc<DictionarySymbolTable>,
}

/// Thread-safe registry and cache for collection dictionaries.
#[derive(Clone, Default, Debug)]
pub struct DictionaryRegistry {
    entries: Arc<RwLock<HashMap<String, CollectionDictionary>>>,
}

impl DictionaryRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register or update a dictionary for a collection space.
    pub fn register(&self, space: &str, dict_id: u32, dict_bytes: Vec<u8>) {
        let symbols = Arc::new(DictionarySymbolTable::from_dict_and_samples(
            &dict_bytes,
            &[],
        ));
        self.register_with_symbols(space, dict_id, dict_bytes, symbols);
    }

    /// Register or update a dictionary with pre-built symbols for a collection space.
    pub fn register_with_symbols(
        &self,
        space: &str,
        dict_id: u32,
        dict_bytes: Vec<u8>,
        symbols: Arc<DictionarySymbolTable>,
    ) {
        if let Ok(mut lock) = self.entries.write() {
            lock.insert(
                space.to_string(),
                CollectionDictionary {
                    space: space.to_string(),
                    dict_id,
                    raw_bytes: dict_bytes,
                    symbols,
                },
            );
        }
    }

    /// Retrieve the active dictionary for a space.
    #[must_use]
    pub fn get(&self, space: &str) -> Option<CollectionDictionary> {
        self.entries.read().ok()?.get(space).cloned()
    }

    /// Retrieve a dictionary by its global numeric id.
    #[must_use]
    pub fn get_by_id(&self, dict_id: u32) -> Option<CollectionDictionary> {
        self.entries
            .read()
            .ok()?
            .values()
            .find(|d| d.dict_id == dict_id)
            .cloned()
    }

    /// Remove the active dictionary for a space.
    #[must_use]
    pub fn remove(&self, space: &str) -> Option<CollectionDictionary> {
        self.entries.write().ok()?.remove(space)
    }

    /// Check if a space has an active dictionary.
    #[must_use]
    pub fn contains(&self, space: &str) -> bool {
        self.entries.read().is_ok_and(|l| l.contains_key(space))
    }
}

/// Train a Zstandard dictionary from sample documents.
///
/// # Errors
/// Returns `LiliaError` if dictionary training fails.
#[cfg(not(target_arch = "wasm32"))]
pub fn train_dictionary(samples: &[&[u8]], max_size: usize) -> Result<Vec<u8>> {
    if samples.is_empty() {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "cannot train dictionary on empty sample set",
            false,
        ));
    }
    let total_bytes: usize = samples.iter().map(|s| s.len()).sum();
    let effective_size = max_size.min(total_bytes);
    zstd::dict::from_samples(samples, effective_size).map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("zstd dictionary training failed: {e}"),
            false,
        )
    })
}

#[cfg(target_arch = "wasm32")]
pub fn train_dictionary(_samples: &[&[u8]], _max_size: usize) -> Result<Vec<u8>> {
    Err(LiliaError::new(
        ErrorCode::Unsupported,
        "zstd dictionary training is not supported on wasm32; use native server or import pre-trained dictionary",
        false,
    ))
}

/// Compress arbitrary bytes using a Zstandard collection dictionary.
///
/// # Errors
/// Returns `LiliaError` if compression fails.
#[cfg(not(target_arch = "wasm32"))]
pub fn compress_with_dict(data: &[u8], dict: &[u8], level: i32) -> Result<Vec<u8>> {
    let mut encoder = zstd::stream::read::Encoder::with_dictionary(Cursor::new(data), level, dict)
        .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
    let mut out = Vec::with_capacity(data.len() / 2 + 32);
    encoder
        .read_to_end(&mut out)
        .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
    Ok(out)
}

#[cfg(target_arch = "wasm32")]
pub fn compress_with_dict(data: &[u8], _dict: &[u8], _level: i32) -> Result<Vec<u8>> {
    // On wasm32, return raw uncompressed or use pure rust fallback
    Ok(data.to_vec())
}

/// Decompress Zstandard-compressed bytes using a collection dictionary.
///
/// Supports native via `libzstd` and WebAssembly (`wasm32-unknown-unknown`) via `ruzstd`.
///
/// # Errors
/// Returns `LiliaError` if decompression fails.
pub fn decompress_with_dict(compressed: &[u8], dict: &[u8]) -> Result<Vec<u8>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut decoder =
            zstd::stream::read::Decoder::with_dictionary(Cursor::new(compressed), dict)
                .map_err(|e| LiliaError::new(ErrorCode::Corrupt, e.to_string(), false))?;
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .map_err(|e| LiliaError::new(ErrorCode::Corrupt, e.to_string(), false))?;
        Ok(out)
    }

    #[cfg(target_arch = "wasm32")]
    {
        use ruzstd::decoding::{Dictionary, FrameDecoder, StreamingDecoder};

        let dictionary = Dictionary::decode_dict(dict).map_err(|e| {
            LiliaError::new(
                ErrorCode::Corrupt,
                format!("invalid ruzstd dictionary: {e}"),
                false,
            )
        })?;

        let mut frame_decoder = FrameDecoder::new();
        frame_decoder.add_dict(dictionary).map_err(|e| {
            LiliaError::new(
                ErrorCode::Corrupt,
                format!("ruzstd add_dict failed: {e}"),
                false,
            )
        })?;

        let mut decoder =
            StreamingDecoder::new_with_decoder(Cursor::new(compressed), frame_decoder).map_err(
                |e| {
                    LiliaError::new(
                        ErrorCode::Corrupt,
                        format!("ruzstd init failed: {e}"),
                        false,
                    )
                },
            )?;

        let mut out = Vec::new();
        decoder.read_to_end(&mut out).map_err(|e| {
            LiliaError::new(
                ErrorCode::Corrupt,
                format!("ruzstd decode error: {e}"),
                false,
            )
        })?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn test_dictionary_training_and_compression_roundtrip() {
        let sample_docs: Vec<Vec<u8>> = (0..50)
            .map(|i| {
                format!(
                    r#"{{"user_id":{i},"first_name":"User{i}","last_name":"Doe","role":"member","created_at":"2026-11-10T12:00:00Z","settings":{{"theme":"dark","notifications":true}}}}"#
                )
                .into_bytes()
            })
            .collect();

        let samples: Vec<&[u8]> = sample_docs.iter().map(Vec::as_slice).collect();
        let dict_bytes = train_dictionary(&samples, 1024).expect("training succeeds");
        assert_ne!(dict_bytes, b"");

        let target_doc = br#"{"user_id":999,"first_name":"User999","last_name":"Doe","role":"admin","created_at":"2026-11-10T12:30:00Z","settings":{"theme":"dark","notifications":false}}"#;
        let compressed =
            compress_with_dict(target_doc, &dict_bytes, 3).expect("compression succeeds");

        // Decompress with dictionary
        let decompressed =
            decompress_with_dict(&compressed, &dict_bytes).expect("decompression succeeds");
        assert_eq!(&decompressed, target_doc);
    }

    #[test]
    fn test_symbol_signature_matching() {
        let symbols = DictionarySymbolTable::new();
        let sig_archived = symbols.token_signature("archived");
        let sig_active = symbols.token_signature("active");
        let sig_status = symbols.token_signature("status");

        assert_ne!(sig_archived, 0);
        assert_ne!(sig_active, 0);
        assert_ne!(sig_status, 0);

        let doc1 = br#"{"id":"doc_1","status":"archived","owner":"alice"}"#;
        let doc2 = br#"{"id":"doc_2","status":"active","owner":"bob"}"#;

        let mask1 = symbols.compute_doc_signature(doc1);
        let mask2 = symbols.compute_doc_signature(doc2);

        // doc1 contains "archived" and "status"
        assert!(SymbolSignature(mask1).matches(SymbolSignature(sig_archived)));
        assert!(SymbolSignature(mask1).matches(SymbolSignature(sig_status)));
        // doc1 does NOT contain "active"
        assert!(!SymbolSignature(mask1).matches(SymbolSignature(sig_active)));

        // doc2 contains "active" and "status"
        assert!(SymbolSignature(mask2).matches(SymbolSignature(sig_active)));
        assert!(SymbolSignature(mask2).matches(SymbolSignature(sig_status)));
        // doc2 does NOT contain "archived"
        assert!(!SymbolSignature(mask2).matches(SymbolSignature(sig_archived)));
    }

    #[test]
    fn test_extract_symbol_signature_header() {
        let mut blob = Vec::new();
        blob.extend_from_slice(MAGIC_ZSTD); // LBZ\x02
        blob.extend_from_slice(&42u32.to_le_bytes()); // dict_id
        blob.extend_from_slice(&0xdead_beef_cafe_babe_u64.to_le_bytes()); // symbol_mask
        blob.extend_from_slice(b"fake_compressed_payload");

        let extracted = extract_symbol_signature(&blob);
        assert_eq!(extracted, Some(0xdead_beef_cafe_babe));

        // Legacy V1 without mask returns None
        let legacy_blob = b"LBZ\x01\x01\x00\x00\x00data";
        assert_eq!(extract_symbol_signature(legacy_blob), None);
    }
}
