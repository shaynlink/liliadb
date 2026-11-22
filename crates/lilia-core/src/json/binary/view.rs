//! Zero-copy borrowed view references into BinaryDoc data.

use std::fmt;

use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};

use super::{exact_int, int_as_real, parse_path_segments, read_value};

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
            Self::Float64(f) => exact_int(*f),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float64(f) => Some(*f),
            Self::Int64(n) => Some(int_as_real(*n)),
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
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
            Self::String(s) => serde_json::Value::String((*s).to_string()),
            Self::Bytes(b) => {
                use serde_json::json;
                json!({ "$bytes": hex::encode(b) })
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
    pub(crate) data: &'a [u8],
    pub(crate) offset: usize,
    pub(crate) count: usize,
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
    pub(crate) data: &'a [u8],
    pub(crate) offset: usize,
    pub(crate) count: usize,
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

/// Serializes the value as its JSON form ([`BinaryValueRef::to_json_value`]) without building
/// it: the view is walked into the serializer. Non-finite floats are `null`, bytes are
/// `{"$bytes": "<hex>"}`, and object keys come in their stored (sorted) order.
impl Serialize for BinaryValueRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match *self {
            Self::Float64(n) if n.is_finite() => serializer.serialize_f64(n),
            Self::Null | Self::Float64(_) => serializer.serialize_unit(),
            Self::Bool(b) => serializer.serialize_bool(b),
            Self::Int64(n) => serializer.serialize_i64(n),
            Self::String(s) => serializer.serialize_str(s),
            Self::Bytes(b) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("$bytes", &hex::encode(b))?;
                map.end()
            }
            Self::Array(array) => array.serialize(serializer),
            Self::Object(object) => object.serialize(serializer),
        }
    }
}

impl Serialize for BinaryArrayRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.count))?;
        for item in self.iter() {
            seq.serialize_element(&item)?;
        }
        seq.end()
    }
}

impl Serialize for BinaryObjectRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (key, value) in self.iter() {
            map.serialize_entry(key, &value)?;
        }
        map.end()
    }
}

#[cfg(test)]
mod serialize_tests {
    use serde_json::json;

    use crate::json::binary::BinaryDoc;

    #[test]
    fn streamed_serialization_equals_the_value_form() {
        let value = json!({
            "z": [1, -2, 3.5, null, true, "é\"\n"],
            "a": {"nested": {"deep": [[], {}], "n": 9_007_199_254_740_993_i64}},
            "m": "text",
            "f": 1.0e-7,
            "empty": {}
        });
        let doc = BinaryDoc::from_json_value(&value).unwrap();
        let streamed = serde_json::to_string(&doc).unwrap();
        let through_value = serde_json::to_string(&doc.to_json_value()).unwrap();
        assert_eq!(streamed, through_value);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&streamed).unwrap(),
            value
        );
    }
}
