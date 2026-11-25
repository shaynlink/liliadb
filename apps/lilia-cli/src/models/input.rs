//! Values given on the command line: bytes in an [`Encoding`], vectors, JSON.

use base64::Engine;
use lilia_core::{ErrorCode, LiliaError, VectorElementType};
use serde_json::Value;

use crate::args::Encoding;

pub(crate) fn invalid(message: impl Into<String>) -> LiliaError {
    LiliaError::new(ErrorCode::InvalidInput, message, false)
}

/// The bytes `raw` encodes as `encoding`; `field` names it in errors.
pub(crate) fn bytes(raw: &str, encoding: Encoding, field: &str) -> Result<Vec<u8>, LiliaError> {
    match encoding {
        Encoding::Text => Ok(raw.as_bytes().to_vec()),
        Encoding::Hex => {
            hex::decode(raw).map_err(|e| invalid(format!("invalid {field} hex encoding: {e}")))
        }
        Encoding::Base64 => base64::engine::general_purpose::STANDARD
            .decode(raw)
            .map_err(|e| invalid(format!("invalid {field} base64 encoding: {e}"))),
    }
}

/// The JSON of `raw`; `field` names it in errors.
pub(crate) fn json(raw: &str, field: &str) -> Result<Value, LiliaError> {
    serde_json::from_str(raw).map_err(|e| invalid(format!("invalid {field} JSON: {e}")))
}

/// A vector and its dimensions, from a JSON array, comma-separated numbers, or base64 of its
/// bytes.
pub(crate) fn vector(
    raw: &str,
    element_type: VectorElementType,
) -> Result<(Vec<u8>, u32), LiliaError> {
    let trimmed = raw.trim();
    let numbers = if trimmed.starts_with('[') {
        let array: Vec<Value> = serde_json::from_str(trimmed)
            .map_err(|e| invalid(format!("invalid vector JSON: {e}")))?;
        Some(
            array
                .iter()
                .map(|item| {
                    item.as_f64()
                        .ok_or_else(|| invalid(format!("vector element {item} is not a number")))
                })
                .collect::<Result<Vec<f64>, _>>()?,
        )
    } else {
        trimmed
            .split(',')
            .map(|part| part.trim().parse::<f64>())
            .collect::<Result<Vec<f64>, _>>()
            .ok()
    };
    let bytes = match numbers {
        Some(numbers) => encode(&numbers, element_type)?,
        None => base64::engine::general_purpose::STANDARD
            .decode(trimmed)
            .map_err(|_| {
                invalid("a vector is a JSON array, comma-separated numbers, or base64 of its bytes")
            })?,
    };
    let dimensions = match element_type {
        VectorElementType::Float32 => Some(bytes.len() / 4),
        VectorElementType::Int8 => Some(bytes.len()),
        VectorElementType::Bit => bytes.len().checked_mul(8),
    };
    let dim = dimensions
        .and_then(|d| u32::try_from(d).ok())
        .ok_or_else(|| {
            invalid(format!(
                "a vector of {} bytes has too many dimensions",
                bytes.len()
            ))
        })?;
    Ok((bytes, dim))
}

/// `numbers` as `element_type` bytes: little-endian `f32`s, one `i8` per element, or bytes of
/// eight bits. Integer element types refuse non-integral or out-of-range values.
fn encode(numbers: &[f64], element_type: VectorElementType) -> Result<Vec<u8>, LiliaError> {
    let integer = |n: f64, range: &str| {
        lilia_core::json::binary::exact_int(n).ok_or_else(|| {
            invalid(format!(
                "{element_type:?} element {n} is not an integer in {range}"
            ))
        })
    };
    match element_type {
        VectorElementType::Float32 => Ok(numbers
            .iter()
            .flat_map(|&n| nearest_f32(n).to_le_bytes())
            .collect()),
        VectorElementType::Int8 => numbers
            .iter()
            .map(|&n| {
                let value = integer(n, "-128..=127")?;
                i8::try_from(value)
                    .map(i8::cast_unsigned)
                    .map_err(|_| invalid(format!("int8 element {n} is not in -128..=127")))
            })
            .collect(),
        VectorElementType::Bit => numbers
            .iter()
            .map(|&n| {
                let value = integer(n, "0..=255")?;
                u8::try_from(value).map_err(|_| invalid(format!("bit byte {n} is not in 0..=255")))
            })
            .collect(),
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "an f32 vector stores the nearest representable value of each element"
)]
fn nearest_f32(value: f64) -> f32 {
    value as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors_parse_from_every_form() {
        let (bytes, dim) = vector("[1.0, -2.5]", VectorElementType::Float32).unwrap();
        assert_eq!((bytes.len(), dim), (8, 2));
        assert_eq!(
            vector("1, 2, 3", VectorElementType::Int8).unwrap(),
            (vec![1, 2, 3], 3)
        );
        assert_eq!(
            vector("AP8=", VectorElementType::Bit).unwrap(),
            (vec![0, 255], 16)
        );
        assert!(vector("[1.5]", VectorElementType::Int8).is_err());
        assert!(vector("[300]", VectorElementType::Int8).is_err());
        assert!(vector("not a vector!", VectorElementType::Float32).is_err());
    }

    #[test]
    fn bytes_decode_each_encoding() {
        assert_eq!(bytes("ab", Encoding::Text, "key").unwrap(), b"ab");
        assert_eq!(bytes("6162", Encoding::Hex, "key").unwrap(), b"ab");
        assert_eq!(bytes("YWI=", Encoding::Base64, "key").unwrap(), b"ab");
        assert_eq!(
            bytes("zz", Encoding::Hex, "key").unwrap_err().code,
            ErrorCode::InvalidInput
        );
    }
}
