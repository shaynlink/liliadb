//! High-performance Zero-Copy Paged Binary JSON representation.
//!
//! Provides sub-microsecond random access and nested path extraction (e.g. `doc.user.address.city`)
//! without parsing or deserializing the entire document into heap memory.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{ErrorCode, LiliaError, Result};

pub const MAGIC: &[u8; 4] = b"LBJ\x01";
pub const MAGIC_ZSTD: &[u8; 4] = b"LBZ\x01";

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
#[derive(Clone, PartialEq, Eq)]
pub struct BinaryDoc {
    data: Vec<u8>,
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
        let root_offset = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        read_value(data, root_offset).ok_or_else(|| {
            LiliaError::new(ErrorCode::Corrupt, "invalid root offset in binary doc", false)
        })
    }

    /// Get a zero-copy reference to the root value of this document.
    #[must_use]
    pub fn root(&self) -> BinaryValueRef<'_> {
        let root_offset = u32::from_le_bytes(self.data[4..8].try_into().unwrap()) as usize;
        read_value(&self.data, root_offset).unwrap_or(BinaryValueRef::Null)
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
    #[must_use]
    pub fn from_json_value(val: &serde_json::Value) -> Self {
        let builder = BinaryDocBuilder::new();
        builder.build_from_value(val)
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

impl Serialize for BinaryDoc {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.to_json_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for BinaryDoc {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let val = serde_json::Value::deserialize(deserializer)?;
        Ok(Self::from_json_value(&val))
    }
}

/// A zero-copy borrowed reference to a value inside a `BinaryDoc`.
#[derive(Clone, Copy, PartialEq)]
pub enum BinaryValueRef<'a> {
    Null,
    Bool(bool),
    Int64(i64),
    Float64(f64),
    String(&'a str),
    Bytes(&'a [u8]),
    Array(BinaryArrayRef<'a>),
    Object(BinaryObjectRef<'a>),
}

impl fmt::Debug for BinaryValueRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => write!(f, "null"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Int64(n) => write!(f, "{n}"),
            Self::Float64(n) => write!(f, "{n}"),
            Self::String(s) => write!(f, "\"{s}\""),
            Self::Bytes(b) => write!(f, "<bytes len={}>", b.len()),
            Self::Array(a) => a.fmt(f),
            Self::Object(o) => o.fmt(f),
        }
    }
}

impl<'a> BinaryValueRef<'a> {
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&'a str> {
        match self {
            Self::String(s) => Some(*s),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int64(n) => Some(*n),
            Self::Float64(f) => {
                if f.fract() == 0.0 && *f >= i64::MIN as f64 && *f <= i64::MAX as f64 {
                    Some(*f as i64)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float64(f) => Some(*f),
            Self::Int64(n) => Some(*n as f64),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_array(&self) -> Option<BinaryArrayRef<'a>> {
        match self {
            Self::Array(a) => Some(*a),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_object(&self) -> Option<BinaryObjectRef<'a>> {
        match self {
            Self::Object(o) => Some(*o),
            _ => None,
        }
    }

    /// Extract a nested value by path (e.g. `user.address.city`).
    #[must_use]
    pub fn extract(&self, path: &str) -> Option<BinaryValueRef<'a>> {
        let segments = parse_path_segments(path);
        let mut curr = *self;
        for seg in segments {
            match curr {
                Self::Object(obj) => {
                    curr = obj.get(&seg)?;
                }
                Self::Array(arr) => {
                    let idx: usize = seg.parse().ok()?;
                    curr = arr.get(idx)?;
                }
                _ => return None,
            }
        }
        Some(curr)
    }

    /// Convert to `serde_json::Value` on demand.
    #[must_use]
    pub fn to_json_value(&self) -> serde_json::Value {
        match self {
            Self::Null => serde_json::Value::Null,
            Self::Bool(b) => serde_json::Value::Bool(*b),
            Self::Int64(n) => serde_json::Value::Number((*n).into()),
            Self::Float64(n) => serde_json::Number::from_f64(*n)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            Self::String(s) => serde_json::Value::String((*s).to_string()),
            Self::Bytes(b) => {
                use serde_json::json;
                let hex_str: String = b.iter().map(|byte| format!("{byte:02x}")).collect();
                json!({ "$bytes": hex_str })
            }
            Self::Array(arr) => {
                let items: Vec<serde_json::Value> =
                    arr.iter().map(|item| item.to_json_value()).collect();
                serde_json::Value::Array(items)
            }
            Self::Object(obj) => {
                let mut map = serde_json::Map::with_capacity(obj.len());
                for (k, v) in obj.iter() {
                    map.insert(k.to_string(), v.to_json_value());
                }
                serde_json::Value::Object(map)
            }
        }
    }
}

/// A zero-copy borrowed reference to an array inside a `BinaryDoc`.
#[derive(Clone, Copy, PartialEq)]
pub struct BinaryArrayRef<'a> {
    data: &'a [u8],
    offset: usize,
    count: usize,
}

impl fmt::Debug for BinaryArrayRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<'a> BinaryArrayRef<'a> {
    #[must_use]
    pub fn len(&self) -> usize {
        self.count
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Access element at `index` in $O(1)$ time.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<BinaryValueRef<'a>> {
        if index >= self.count {
            return None;
        }
        let offset_pos = self.offset + 5 + index * 4;
        let elem_offset =
            u32::from_le_bytes(self.data[offset_pos..offset_pos + 4].try_into().ok()?) as usize;
        read_value(self.data, elem_offset)
    }

    pub fn iter(self) -> impl Iterator<Item = BinaryValueRef<'a>> {
        (0..self.count).filter_map(move |i| self.get(i))
    }
}

/// A zero-copy borrowed reference to an object inside a `BinaryDoc`.
///
/// Keys are stored lexicographically sorted, enabling $O(\log K)$ binary search lookup.
#[derive(Clone, Copy, PartialEq)]
pub struct BinaryObjectRef<'a> {
    data: &'a [u8],
    offset: usize,
    count: usize,
}

impl fmt::Debug for BinaryObjectRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut map = f.debug_map();
        for (k, v) in self.iter() {
            map.entry(&k, &v);
        }
        map.finish()
    }
}

impl<'a> BinaryObjectRef<'a> {
    #[must_use]
    pub fn len(&self) -> usize {
        self.count
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Look up a field by key in $O(\log K)$ time via binary search directly over memory slices.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<BinaryValueRef<'a>> {
        if self.count == 0 {
            return None;
        }

        let mut low = 0;
        let mut high = self.count;

        while low < high {
            let mid = low + (high - low) / 2;
            let entry_pos = self.offset + 5 + mid * 10;
            let k_off =
                u32::from_le_bytes(self.data[entry_pos..entry_pos + 4].try_into().ok()?) as usize;
            let k_len = u16::from_le_bytes(self.data[entry_pos + 4..entry_pos + 6].try_into().ok()?)
                as usize;
            let k_str = std::str::from_utf8(&self.data[k_off..k_off + k_len]).ok()?;

            match k_str.cmp(key) {
                std::cmp::Ordering::Equal => {
                    let val_off = u32::from_le_bytes(
                        self.data[entry_pos + 6..entry_pos + 10].try_into().ok()?,
                    ) as usize;
                    return read_value(self.data, val_off);
                }
                std::cmp::Ordering::Less => {
                    low = mid + 1;
                }
                std::cmp::Ordering::Greater => {
                    high = mid;
                }
            }
        }

        None
    }

    pub fn keys(self) -> impl Iterator<Item = &'a str> {
        (0..self.count).filter_map(move |i| {
            let entry_pos = self.offset + 5 + i * 10;
            let k_off =
                u32::from_le_bytes(self.data[entry_pos..entry_pos + 4].try_into().ok()?) as usize;
            let k_len = u16::from_le_bytes(self.data[entry_pos + 4..entry_pos + 6].try_into().ok()?)
                as usize;
            std::str::from_utf8(&self.data[k_off..k_off + k_len]).ok()
        })
    }

    pub fn iter(self) -> impl Iterator<Item = (&'a str, BinaryValueRef<'a>)> {
        (0..self.count).filter_map(move |i| {
            let entry_pos = self.offset + 5 + i * 10;
            let k_off =
                u32::from_le_bytes(self.data[entry_pos..entry_pos + 4].try_into().ok()?) as usize;
            let k_len = u16::from_le_bytes(self.data[entry_pos + 4..entry_pos + 6].try_into().ok()?)
                as usize;
            let k_str = std::str::from_utf8(&self.data[k_off..k_off + k_len]).ok()?;
            let val_off =
                u32::from_le_bytes(self.data[entry_pos + 6..entry_pos + 10].try_into().ok()?)
                    as usize;
            let val = read_value(self.data, val_off)?;
            Some((k_str, val))
        })
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
    let root_offset = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
    if root_offset >= data.len() {
        return Err(LiliaError::new(
            ErrorCode::Corrupt,
            "invalid root offset in binary document",
            false,
        ));
    }
    Ok(())
}

fn read_value(data: &[u8], offset: usize) -> Option<BinaryValueRef<'_>> {
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

/// Builder for constructing compact, sorted `BinaryDoc` buffers.
#[derive(Debug, Default)]
pub struct BinaryDocBuilder {
    buf: Vec<u8>,
}

impl BinaryDocBuilder {
    #[must_use]
    pub fn new() -> Self {
        let mut buf = Vec::with_capacity(256);
        // Reserve 8 bytes for Header (magic 4 bytes + root offset 4 bytes)
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&[0; 4]);
        Self { buf }
    }

    #[must_use]
    pub fn build_from_value(mut self, val: &serde_json::Value) -> BinaryDoc {
        let root_offset = self.write_value(val);
        // Patch root offset at byte 4..8
        let root_bytes = (root_offset as u32).to_le_bytes();
        self.buf[4..8].copy_from_slice(&root_bytes);
        BinaryDoc { data: self.buf }
    }

    fn write_value(&mut self, val: &serde_json::Value) -> usize {
        match val {
            serde_json::Value::Null => self.write_null(),
            serde_json::Value::Bool(b) => self.write_bool(*b),
            serde_json::Value::Number(num) => {
                if let Some(i) = num.as_i64() {
                    self.write_int64(i)
                } else if let Some(u) = num.as_u64() {
                    if u <= i64::MAX as u64 {
                        self.write_int64(u as i64)
                    } else {
                        self.write_float64(u as f64)
                    }
                } else if let Some(f) = num.as_f64() {
                    self.write_float64(f)
                } else {
                    self.write_null()
                }
            }
            serde_json::Value::String(s) => self.write_string(s),
            serde_json::Value::Array(items) => {
                let mut offsets = Vec::with_capacity(items.len());
                for item in items {
                    offsets.push(self.write_value(item));
                }
                let start_offset = self.buf.len();
                self.buf.push(TAG_ARRAY);
                self.buf
                    .extend_from_slice(&(items.len() as u32).to_le_bytes());
                for off in offsets {
                    self.buf.extend_from_slice(&(off as u32).to_le_bytes());
                }
                start_offset
            }
            serde_json::Value::Object(map) => {
                // Collect and sort keys for binary search
                let mut sorted_entries: Vec<(&String, &serde_json::Value)> = map.iter().collect();
                sorted_entries.sort_by(|a, b| a.0.cmp(b.0));

                let mut encoded_entries = Vec::with_capacity(sorted_entries.len());
                for (k, v) in sorted_entries {
                    let k_off = self.write_raw_str(k);
                    let v_off = self.write_value(v);
                    let k_len = k.len() as u16;
                    encoded_entries.push((k_off, k_len, v_off));
                }

                let start_offset = self.buf.len();
                self.buf.push(TAG_OBJECT);
                self.buf
                    .extend_from_slice(&(encoded_entries.len() as u32).to_le_bytes());
                for (k_off, k_len, v_off) in encoded_entries {
                    self.buf.extend_from_slice(&(k_off as u32).to_le_bytes());
                    self.buf.extend_from_slice(&k_len.to_le_bytes());
                    self.buf.extend_from_slice(&(v_off as u32).to_le_bytes());
                }
                start_offset
            }
        }
    }

    fn write_null(&mut self) -> usize {
        let offset = self.buf.len();
        self.buf.push(TAG_NULL);
        offset
    }

    fn write_bool(&mut self, b: bool) -> usize {
        let offset = self.buf.len();
        self.buf
            .push(if b { TAG_BOOL_TRUE } else { TAG_BOOL_FALSE });
        offset
    }

    fn write_int64(&mut self, n: i64) -> usize {
        let offset = self.buf.len();
        self.buf.push(TAG_INT64);
        self.buf.extend_from_slice(&n.to_le_bytes());
        offset
    }

    fn write_float64(&mut self, n: f64) -> usize {
        let offset = self.buf.len();
        self.buf.push(TAG_FLOAT64);
        self.buf.extend_from_slice(&n.to_le_bytes());
        offset
    }

    fn write_string(&mut self, s: &str) -> usize {
        let offset = self.buf.len();
        self.buf.push(TAG_STRING);
        self.buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
        self.buf.extend_from_slice(s.as_bytes());
        offset
    }

    fn write_raw_str(&mut self, s: &str) -> usize {
        let offset = self.buf.len();
        self.buf.extend_from_slice(s.as_bytes());
        offset
    }
}

/// Parse unified path segments across:
/// - Dot notation: `user.address.city`
/// - RFC 6901 pointer: `/user/address/city`
/// - SQLite JSON path: `$.user.address.city`
/// - Array indexing: `users.0.name` or `users[0].name`
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
            // read until ']'
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
            // RFC 6901 escape
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_binary_doc_roundtrip_primitives() {
        let val = json!({
            "name": "Alice",
            "age": 30,
            "active": true,
            "score": 98.5,
            "notes": null,
            "tags": ["admin", "developer"],
            "nested": {
                "deep": {
                    "city": "Paris"
                }
            }
        });

        let doc = BinaryDoc::from_json_value(&val);
        assert_eq!(doc.extract_str("name"), Some("Alice"));
        assert_eq!(doc.extract_i64("age"), Some(30));
        assert_eq!(doc.extract_bool("active"), Some(true));
        assert_eq!(doc.extract_f64("score"), Some(98.5));
        assert!(doc.extract("notes").unwrap().is_null());

        // Nested zero-copy extraction
        assert_eq!(doc.extract_str("nested.deep.city"), Some("Paris"));
        assert_eq!(doc.extract_str("/nested/deep/city"), Some("Paris"));
        assert_eq!(doc.extract_str("$.nested.deep.city"), Some("Paris"));

        // Array indexing
        assert_eq!(doc.extract_str("tags.0"), Some("admin"));
        assert_eq!(doc.extract_str("tags[1]"), Some("developer"));

        // Full JSON roundtrip
        let restored = doc.to_json_value();
        assert_eq!(restored, val);
    }

    #[test]
    fn test_binary_search_large_object() {
        let mut map = serde_json::Map::new();
        for i in 0..200 {
            map.insert(format!("field_{i:03}"), json!(i * 10));
        }
        let doc = BinaryDoc::from_json_value(&serde_json::Value::Object(map));

        assert_eq!(doc.extract_i64("field_042"), Some(420));
        assert_eq!(doc.extract_i64("field_199"), Some(1990));
        assert_eq!(doc.extract("field_999"), None);
    }
}
