//! Zstandard dictionary compression engine with hybrid native & WASM support.
//!
//! On native targets (macOS, Linux, Windows, Android, iOS), libzstd is used for dictionary
//! training, streaming, and bulk compression/decompression.
//! On `wasm32-unknown-unknown`, `ruzstd` is used for 100% pure-Rust dictionary decompression.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::sync::{Arc, RwLock};

use crate::{ErrorCode, LiliaError, Result};

/// In-memory representation of an active collection dictionary.
#[derive(Clone, Debug)]
pub struct CollectionDictionary {
    pub space: String,
    pub dict_id: u32,
    pub raw_bytes: Vec<u8>,
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
        if let Ok(mut lock) = self.entries.write() {
            lock.insert(
                space.to_string(),
                CollectionDictionary {
                    space: space.to_string(),
                    dict_id,
                    raw_bytes: dict_bytes,
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
        self.entries.read().ok()?.values().find(|d| d.dict_id == dict_id).cloned()
    }

    /// Remove the active dictionary for a space.
    pub fn remove(&self, space: &str) -> Option<CollectionDictionary> {
        self.entries.write().ok()?.remove(space)
    }

    /// Check if a space has an active dictionary.
    #[must_use]
    pub fn contains(&self, space: &str) -> bool {
        self.entries
            .read()
            .map(|l| l.contains_key(space))
            .unwrap_or(false)
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

        let samples: Vec<&[u8]> = sample_docs.iter().map(|d| d.as_slice()).collect();
        let dict_bytes = train_dictionary(&samples, 1024).expect("training succeeds");
        assert!(!dict_bytes.is_empty());

        let target_doc = br#"{"user_id":999,"first_name":"User999","last_name":"Doe","role":"admin","created_at":"2026-11-10T12:30:00Z","settings":{"theme":"dark","notifications":false}}"#;
        let compressed =
            compress_with_dict(target_doc, &dict_bytes, 3).expect("compression succeeds");

        // Decompress with dictionary
        let decompressed =
            decompress_with_dict(&compressed, &dict_bytes).expect("decompression succeeds");
        assert_eq!(&decompressed, target_doc);
    }
}
