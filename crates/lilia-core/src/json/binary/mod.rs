//! High-performance Zero-Copy Paged Binary JSON representation.
//!
//! Provides sub-microsecond random access and nested path extraction (e.g. `doc.user.address.city`)
//! without parsing or deserializing the entire document into heap memory.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{ErrorCode, LiliaError, Result};

mod builder;
mod view;

#[cfg(test)]
mod tests;

pub use builder::BinaryDocBuilder;
pub use view::{BinaryArrayRef, BinaryObjectRef, BinaryValueRef};

pub const MAGIC: &[u8; 4] = b"LBJ\x01";

pub const TAG_NULL: u8 = 0x00;
pub const TAG_BOOL_FALSE: u8 = 0x01;
pub const TAG_BOOL_TRUE: u8 = 0x02;
pub const TAG_INT64: u8 = 0x03;
pub const TAG_FLOAT64: u8 = 0x04;
pub const TAG_STRING: u8 = 0x05;
pub const TAG_BYTES: u8 = 0x06;
pub const TAG_ARRAY: u8 = 0x07;
pub const TAG_OBJECT: u8 = 0x08;

/// A validated, owned zero-copy binary JSON document.
#[derive(Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq))]
pub struct BinaryDoc {
    pub(crate) data: Vec<u8>,
}

impl fmt::Debug for BinaryDoc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BinaryDoc")
            .field("len", &self.data.len())
            .field("root", &self.root())
            .finish()
    }
}

impl BinaryDoc {
    /// Create a `BinaryDoc` from validated binary bytes.
    ///
    /// # Errors
    /// Returns `LiliaError` if the magic header or root offset is corrupt.
    pub fn from_bytes(data: Vec<u8>) -> Result<Self> {
        validate_binary_header(&data)?;
        Ok(Self { data })
    }

    /// Access the underlying raw byte buffer.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Consume the document and return the underlying byte buffer.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.data
    }

    /// Read the root value from a borrowed slice without allocating an owned `BinaryDoc`.
    ///
    /// # Errors
    /// Returns `LiliaError` if the magic header or root offset is corrupt.
    pub fn read_root_from_slice(data: &[u8]) -> Result<BinaryValueRef<'_>> {
        validate_binary_header(data)?;
        root_offset(data)
            .and_then(|offset| read_value(data, offset))
            .ok_or_else(|| {
                LiliaError::new(
                    ErrorCode::Corrupt,
                    "invalid root offset in binary doc",
                    false,
                )
            })
    }

    /// Get a zero-copy reference to the root value of this document.
    #[must_use]
    pub fn root(&self) -> BinaryValueRef<'_> {
        root_offset(&self.data)
            .and_then(|offset| read_value(&self.data, offset))
            .unwrap_or(BinaryValueRef::Null)
    }

    /// Extract a field value along a path (e.g. `user.address.city`, `/user/address/city`, `users.0.name`)
    /// without deserializing the entire document.
    #[must_use]
    pub fn extract(&self, path: &str) -> Option<BinaryValueRef<'_>> {
        self.root().extract(path)
    }

    /// Extract a string slice along a path.
    #[must_use]
    pub fn extract_str(&self, path: &str) -> Option<&str> {
        self.extract(path).and_then(|v| v.as_str())
    }

    /// Extract an `i64` integer along a path.
    #[must_use]
    pub fn extract_i64(&self, path: &str) -> Option<i64> {
        self.extract(path).and_then(|v| v.as_i64())
    }

    /// Extract an `f64` float along a path.
    #[must_use]
    pub fn extract_f64(&self, path: &str) -> Option<f64> {
        self.extract(path).and_then(|v| v.as_f64())
    }

    /// Extract a `bool` along a path.
    #[must_use]
    pub fn extract_bool(&self, path: &str) -> Option<bool> {
        self.extract(path).and_then(|v| v.as_bool())
    }

    /// Encode a `serde_json::Value` into an owned `BinaryDoc`.
    ///
    /// # Errors
    /// `InvalidInput` when `val` exceeds the limits of the format (see
    /// [`BinaryDocBuilder::build_from_value`]).
    pub fn from_json_value(val: &serde_json::Value) -> Result<Self> {
        BinaryDocBuilder::new().build_from_value(val)
    }

    /// Convert this binary document to a standard `serde_json::Value` on demand.
    #[must_use]
    pub fn to_json_value(&self) -> serde_json::Value {
        self.root().to_json_value()
    }

    /// Deserialize this document directly into a strongly typed struct.
    ///
    /// # Errors
    /// Returns error if deserialization fails.
    pub fn to_serde<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_value(self.to_json_value())
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))
    }
}

/// The document's JSON form, written straight from the binary view.
impl Serialize for BinaryDoc {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.root().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for BinaryDoc {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let val = serde_json::Value::deserialize(deserializer)?;
        Self::from_json_value(&val).map_err(serde::de::Error::custom)
    }
}

fn validate_binary_header(data: &[u8]) -> Result<()> {
    if data.len() < 8 {
        return Err(LiliaError::new(
            ErrorCode::Corrupt,
            "binary document too short for header",
            false,
        ));
    }
    if &data[0..4] != MAGIC {
        return Err(LiliaError::new(
            ErrorCode::Corrupt,
            "invalid binary document magic header",
            false,
        ));
    }
    if root_offset(data).is_none_or(|offset| offset >= data.len()) {
        return Err(LiliaError::new(
            ErrorCode::Corrupt,
            "invalid root offset in binary document",
            false,
        ));
    }
    Ok(())
}

pub(crate) fn read_value(data: &[u8], offset: usize) -> Option<BinaryValueRef<'_>> {
    if offset >= data.len() {
        return None;
    }
    let tag = data[offset];
    match tag {
        TAG_NULL => Some(BinaryValueRef::Null),
        TAG_BOOL_FALSE => Some(BinaryValueRef::Bool(false)),
        TAG_BOOL_TRUE => Some(BinaryValueRef::Bool(true)),
        TAG_INT64 => {
            if offset + 9 > data.len() {
                return None;
            }
            let n = i64::from_le_bytes(data[offset + 1..offset + 9].try_into().ok()?);
            Some(BinaryValueRef::Int64(n))
        }
        TAG_FLOAT64 => {
            if offset + 9 > data.len() {
                return None;
            }
            let n = f64::from_le_bytes(data[offset + 1..offset + 9].try_into().ok()?);
            Some(BinaryValueRef::Float64(n))
        }
        TAG_STRING => {
            if offset + 5 > data.len() {
                return None;
            }
            let len = u32::from_le_bytes(data[offset + 1..offset + 5].try_into().ok()?) as usize;
            let start = offset + 5;
            let end = start + len;
            if end > data.len() {
                return None;
            }
            let s = std::str::from_utf8(&data[start..end]).ok()?;
            Some(BinaryValueRef::String(s))
        }
        TAG_BYTES => {
            if offset + 5 > data.len() {
                return None;
            }
            let len = u32::from_le_bytes(data[offset + 1..offset + 5].try_into().ok()?) as usize;
            let start = offset + 5;
            let end = start + len;
            if end > data.len() {
                return None;
            }
            Some(BinaryValueRef::Bytes(&data[start..end]))
        }
        TAG_ARRAY => {
            if offset + 5 > data.len() {
                return None;
            }
            let count = u32::from_le_bytes(data[offset + 1..offset + 5].try_into().ok()?) as usize;
            if offset + 5 + count * 4 > data.len() {
                return None;
            }
            Some(BinaryValueRef::Array(BinaryArrayRef {
                data,
                offset,
                count,
            }))
        }
        TAG_OBJECT => {
            if offset + 5 > data.len() {
                return None;
            }
            let count = u32::from_le_bytes(data[offset + 1..offset + 5].try_into().ok()?) as usize;
            if offset + 5 + count * 10 > data.len() {
                return None;
            }
            Some(BinaryValueRef::Object(BinaryObjectRef {
                data,
                offset,
                count,
            }))
        }
        _ => None,
    }
}

pub(crate) fn le_u32(value: usize, what: &str) -> Result<[u8; 4]> {
    u32::try_from(value).map(u32::to_le_bytes).map_err(|_| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("{what} exceeds the binary JSON format limit"),
            false,
        )
    })
}

fn root_offset(data: &[u8]) -> Option<usize> {
    let bytes: [u8; 4] = data.get(4..8)?.try_into().ok()?;
    usize::try_from(u32::from_le_bytes(bytes)).ok()
}

/// `value` as the nearest double, as JSON numbers are read by every JavaScript runtime.
#[allow(
    clippy::cast_precision_loss,
    reason = "integers beyond 2^53 round to the nearest double, as in JavaScript"
)]
pub(crate) fn int_as_real(value: i64) -> f64 {
    value as f64
}

/// `value` as an `i64` when it is whole and within the range of `i64`.
#[must_use]
pub fn exact_int(value: f64) -> Option<i64> {
    const BOUND: f64 = 9_223_372_036_854_775_808.0;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the value is checked whole and within the range of i64 before the cast"
    )]
    let exact = (value.fract() == 0.0 && (-BOUND..BOUND).contains(&value)).then_some(value as i64);
    exact
}

#[must_use]
pub fn parse_path_segments(path: &str) -> Vec<String> {
    if path.is_empty() {
        return Vec::new();
    }

    let trimmed = path.strip_prefix("$.").unwrap_or(path);
    let trimmed = trimmed.strip_prefix('$').unwrap_or(trimmed);
    let trimmed = trimmed.strip_prefix('/').unwrap_or(trimmed);

    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut segments = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = trimmed.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];
        if ch == '.' || ch == '/' {
            if !current.is_empty() {
                segments.push(current.clone());
                current.clear();
            }
        } else if ch == '[' {
            if !current.is_empty() {
                segments.push(current.clone());
                current.clear();
            }
            i += 1;
            while i < chars.len() && chars[i] != ']' {
                current.push(chars[i]);
                i += 1;
            }
            if !current.is_empty() {
                segments.push(current.clone());
                current.clear();
            }
        } else if ch == '~' && i + 1 < chars.len() {
            if chars[i + 1] == '1' {
                current.push('/');
                i += 1;
            } else if chars[i + 1] == '0' {
                current.push('~');
                i += 1;
            } else {
                current.push(ch);
            }
        } else {
            current.push(ch);
        }
        i += 1;
    }

    if !current.is_empty() {
        segments.push(current);
    }

    segments
}
