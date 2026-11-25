//! Vectorized SIMD JSON parser with automatic scalar fallback.
//!
//! Uses AVX-512 / AVX2 (`x86_64`), ARM Neon (aarch64), or Wasm SIMD128 (wasm32) via `simd-json`
//! when available, with a transparent fallback to scalar `serde_json` for unsupported targets.

use crate::json::binary::BinaryDoc;
use crate::{ErrorCode, LiliaError, Result};

/// Parse a raw JSON byte slice and encode it directly into a zero-copy `BinaryDoc`.
///
/// If `input` is mutable and the SIMD feature is available, vector instructions
/// are used to parse in-place at several gigabytes per second.
///
/// # Errors
/// Returns `LiliaError` if the JSON is malformed.
pub fn parse_json_bytes_to_binary(input: &mut [u8]) -> Result<BinaryDoc> {
    #[cfg(feature = "simd")]
    {
        parse_with_simd(input)
    }
    #[cfg(not(feature = "simd"))]
    {
        parse_with_scalar(input)
    }
}

/// Fallback parser for immutable slices or scalar targets.
///
/// # Errors
/// Returns `LiliaError` if JSON parsing fails.
pub fn parse_json_slice_to_binary(input: &[u8]) -> Result<BinaryDoc> {
    let mut copy = input.to_vec();
    parse_json_bytes_to_binary(&mut copy)
}

#[cfg(feature = "simd")]
fn parse_with_simd(input: &mut [u8]) -> Result<BinaryDoc> {
    let val = simd_json::to_borrowed_value(input).map_err(|e| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("SIMD JSON parsing error: {e}"),
            false,
        )
    })?;

    let json_val: serde_json::Value = borrowed_to_serde(val);
    BinaryDoc::from_json_value(&json_val)
}

#[cfg(feature = "simd")]
fn borrowed_to_serde(val: simd_json::BorrowedValue<'_>) -> serde_json::Value {
    use simd_json::borrowed::Value as BV;
    use simd_json::StaticNode;

    match val {
        BV::Static(StaticNode::Null) => serde_json::Value::Null,
        BV::Static(StaticNode::Bool(b)) => serde_json::Value::Bool(b),
        BV::Static(StaticNode::I64(n)) => serde_json::Value::Number(n.into()),
        BV::Static(StaticNode::U64(n)) => serde_json::Value::Number(n.into()),
        BV::Static(StaticNode::F64(n)) => serde_json::Number::from_f64(n)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        BV::String(s) => serde_json::Value::String(s.into_owned()),
        BV::Array(arr) => {
            let items = arr.into_iter().map(borrowed_to_serde).collect();
            serde_json::Value::Array(items)
        }
        BV::Object(obj) => {
            let mut map = serde_json::Map::with_capacity(obj.len());
            for (k, v) in obj.into_iter() {
                map.insert(k.into_owned(), borrowed_to_serde(v));
            }
            serde_json::Value::Object(map)
        }
    }
}

#[cfg(not(feature = "simd"))]
fn parse_with_scalar(input: &[u8]) -> Result<BinaryDoc> {
    let val: serde_json::Value = serde_json::from_slice(input)
        .map_err(|e| LiliaError::new(ErrorCode::InvalidInput, e.to_string(), false))?;
    BinaryDoc::from_json_value(&val)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_json_bytes_to_binary() {
        let mut raw = br#"{"title":"Vectorized","count":42,"tags":["fast","neon"]}"#.to_vec();
        let doc = parse_json_bytes_to_binary(&mut raw).expect("simd parse succeeds");

        assert_eq!(doc.extract_str("title"), Some("Vectorized"));
        assert_eq!(doc.extract_i64("count"), Some(42));
        assert_eq!(doc.extract_str("tags.0"), Some("fast"));
    }
}
