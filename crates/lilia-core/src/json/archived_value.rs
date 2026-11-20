#![allow(unsafe_code)]

//! Relative-Pointer Zero-Copy Dynamic JSON representation powered by `rkyv`.
//!
//! Employs a flat, cache-contiguous node arena layout to eliminate recursion overhead,
//! enabling 0-nanosecond deserialization, $O(\log K)$ zero-allocation key lookup,
//! and in-situ nested path extraction (e.g. `doc.user.address.city`) over raw byte slices.

use rkyv::rancor::Error as RancorError;
use rkyv::util::AlignedVec;
use rkyv::{Archive, Deserialize, Serialize};

use crate::{ErrorCode, LiliaError, Result};

/// A flat, cache-contiguous node representation of a JSON value.
#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq))]
pub enum FlatNode {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    /// Child node indices into the document arena.
    Array(Vec<u32>),
    /// Lexicographically sorted `(key, child_index)` pairs for $O(\log K)$ binary search.
    Object(Vec<(String, u32)>),
}

/// A zero-copy JSON document represented as a flat arena of relative nodes.
#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq))]
pub struct JsonDocument {
    pub root: u32,
    pub nodes: Vec<FlatNode>,
}

impl JsonDocument {
    /// Convert standard `serde_json::Value` into a compact, sorted `JsonDocument`.
    #[must_use]
    pub fn from_serde(value: &serde_json::Value) -> Self {
        let mut nodes = Vec::new();
        let root = Self::insert_serde_value(value, &mut nodes);
        Self { root, nodes }
    }

    fn insert_serde_value(value: &serde_json::Value, nodes: &mut Vec<FlatNode>) -> u32 {
        let node = match value {
            serde_json::Value::Null => FlatNode::Null,
            serde_json::Value::Bool(b) => FlatNode::Bool(*b),
            serde_json::Value::Number(num) => {
                if let Some(i) = num.as_i64() {
                    FlatNode::Int(i)
                } else if let Some(f) = num.as_f64() {
                    FlatNode::Float(f)
                } else {
                    FlatNode::Null
                }
            }
            serde_json::Value::String(s) => FlatNode::String(s.clone()),
            serde_json::Value::Array(items) => {
                let idx = nodes.len() as u32;
                // Reserve slot for array
                nodes.push(FlatNode::Null);
                let mut child_indices = Vec::with_capacity(items.len());
                for item in items {
                    child_indices.push(Self::insert_serde_value(item, nodes));
                }
                nodes[idx as usize] = FlatNode::Array(child_indices);
                return idx;
            }
            serde_json::Value::Object(map) => {
                let idx = nodes.len() as u32;
                // Reserve slot for object
                nodes.push(FlatNode::Null);
                let mut entries: Vec<(&String, &serde_json::Value)> = map.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                let mut child_entries = Vec::with_capacity(entries.len());
                for (k, v) in entries {
                    let child_idx = Self::insert_serde_value(v, nodes);
                    child_entries.push((k.clone(), child_idx));
                }
                nodes[idx as usize] = FlatNode::Object(child_entries);
                return idx;
            }
        };

        let idx = nodes.len() as u32;
        nodes.push(node);
        idx
    }

    /// Archive directly into an aligned zero-copy byte buffer.
    ///
    /// # Errors
    /// Returns `LiliaError` if serialization fails.
    pub fn to_archived_bytes(&self) -> Result<AlignedVec> {
        rkyv::to_bytes::<RancorError>(self).map_err(|e| {
            LiliaError::new(
                ErrorCode::Storage,
                format!("rkyv serialization failed: {e}"),
                false,
            )
        })
    }

    /// Parse a JSON byte slice directly into an aligned archived byte buffer.
    ///
    /// # Errors
    /// Returns `LiliaError` on invalid JSON.
    pub fn from_json_slice(bytes: &[u8]) -> Result<AlignedVec> {
        let val: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|e| LiliaError::new(ErrorCode::InvalidInput, e.to_string(), false))?;
        Self::from_serde(&val).to_archived_bytes()
    }
}

/// Safely inspect an aligned archived buffer with bytecheck validation.
///
/// # Errors
/// Returns `LiliaError` if validation fails.
pub fn access_archived_doc(bytes: &[u8]) -> Result<&ArchivedJsonDocument> {
    rkyv::access::<ArchivedJsonDocument, RancorError>(bytes).map_err(|e| {
        LiliaError::new(
            ErrorCode::Corrupt,
            format!("rkyv validation failed: {e}"),
            false,
        )
    })
}

/// 0-nanosecond unchecked pointer reinterpretation into trusted memory.
///
/// # Safety
/// The caller must ensure that `bytes` is correctly aligned and represents
/// a valid archived `JsonDocument`.
#[inline]
#[must_use]
pub unsafe fn access_archived_doc_unchecked(bytes: &[u8]) -> &ArchivedJsonDocument {
    rkyv::access_unchecked::<ArchivedJsonDocument>(bytes)
}

/// A borrowed zero-copy view over a specific node inside an `ArchivedJsonDocument`.
#[derive(Debug, Clone, Copy)]
pub struct ArchivedJsonView<'a> {
    pub(crate) doc: &'a ArchivedJsonDocument,
    pub(crate) node_idx: u32,
}

impl<'a> ArchivedJsonView<'a> {
    #[inline]
    #[must_use]
    pub fn node(&self) -> Option<&'a ArchivedFlatNode> {
        self.doc.nodes.get(self.node_idx as usize)
    }

    #[inline]
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self.node(), Some(ArchivedFlatNode::Null) | None)
    }

    #[inline]
    #[must_use]
    pub fn as_str(&self) -> Option<&'a str> {
        match self.node()? {
            ArchivedFlatNode::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    #[inline]
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self.node()? {
            ArchivedFlatNode::Int(i) => Some(i.to_native()),
            ArchivedFlatNode::Float(f) => {
                let val = f.to_native();
                #[allow(clippy::cast_possible_truncation)]
                if val.fract() == 0.0 {
                    Some(val as i64)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    #[inline]
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self.node()? {
            ArchivedFlatNode::Float(f) => Some(f.to_native()),
            #[allow(clippy::cast_precision_loss)]
            ArchivedFlatNode::Int(i) => Some(i.to_native() as f64),
            _ => None,
        }
    }

    #[inline]
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self.node()? {
            ArchivedFlatNode::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Access field of an object by key in $O(\log K)$ via binary search over relative slices.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<ArchivedJsonView<'a>> {
        match self.node()? {
            ArchivedFlatNode::Object(entries) => {
                let idx = entries
                    .binary_search_by(|entry| entry.0.as_str().cmp(key))
                    .ok()?;
                Some(ArchivedJsonView {
                    doc: self.doc,
                    node_idx: entries[idx].1.to_native(),
                })
            }
            _ => None,
        }
    }

    /// In-situ path extraction across relative pointers without any heap allocation.
    #[must_use]
    pub fn extract(&self, path: &str) -> Option<ArchivedJsonView<'a>> {
        if path.is_empty() {
            return Some(*self);
        }
        let segments = super::binary::parse_path_segments(path);
        let mut curr = *self;
        for seg in segments {
            match curr.node()? {
                ArchivedFlatNode::Object(entries) => {
                    let idx = entries
                        .binary_search_by(|entry| entry.0.as_str().cmp(&seg))
                        .ok()?;
                    curr = ArchivedJsonView {
                        doc: self.doc,
                        node_idx: entries[idx].1.to_native(),
                    };
                }
                ArchivedFlatNode::Array(items) => {
                    let idx: usize = seg.parse().ok()?;
                    let child_node = items.get(idx)?.to_native();
                    curr = ArchivedJsonView {
                        doc: self.doc,
                        node_idx: child_node,
                    };
                }
                _ => return None,
            }
        }
        Some(curr)
    }

    #[must_use]
    pub fn extract_str(&self, path: &str) -> Option<&'a str> {
        self.extract(path).and_then(|v| v.as_str())
    }

    #[must_use]
    pub fn extract_i64(&self, path: &str) -> Option<i64> {
        self.extract(path).and_then(|v| v.as_i64())
    }

    #[must_use]
    pub fn extract_f64(&self, path: &str) -> Option<f64> {
        self.extract(path).and_then(|v| v.as_f64())
    }

    #[must_use]
    pub fn extract_bool(&self, path: &str) -> Option<bool> {
        self.extract(path).and_then(|v| v.as_bool())
    }

    /// Convert this node and its subtree to `serde_json::Value` on demand.
    #[must_use]
    pub fn to_serde(&self) -> serde_json::Value {
        match self.node() {
            Some(ArchivedFlatNode::Null) | None => serde_json::Value::Null,
            Some(ArchivedFlatNode::Bool(b)) => serde_json::Value::Bool(*b),
            Some(ArchivedFlatNode::Int(i)) => serde_json::Value::Number(i.to_native().into()),
            Some(ArchivedFlatNode::Float(f)) => serde_json::Number::from_f64(f.to_native())
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
            Some(ArchivedFlatNode::String(s)) => serde_json::Value::String(s.as_str().to_owned()),
            Some(ArchivedFlatNode::Array(items)) => {
                let arr = items
                    .iter()
                    .map(|child_idx| {
                        ArchivedJsonView {
                            doc: self.doc,
                            node_idx: child_idx.to_native(),
                        }
                        .to_serde()
                    })
                    .collect();
                serde_json::Value::Array(arr)
            }
            Some(ArchivedFlatNode::Object(entries)) => {
                let map = entries
                    .iter()
                    .map(|entry| {
                        let child = ArchivedJsonView {
                            doc: self.doc,
                            node_idx: entry.1.to_native(),
                        }
                        .to_serde();
                        (entry.0.as_str().to_owned(), child)
                    })
                    .collect();
                serde_json::Value::Object(map)
            }
        }
    }
}

impl ArchivedJsonDocument {
    /// Get the root node view of this archived document.
    #[inline]
    #[must_use]
    pub fn root_view(&self) -> ArchivedJsonView<'_> {
        ArchivedJsonView {
            doc: self,
            node_idx: self.root.to_native(),
        }
    }

    /// Extract a field along a path in 0-nanoseconds.
    #[inline]
    #[must_use]
    pub fn extract(&self, path: &str) -> Option<ArchivedJsonView<'_>> {
        self.root_view().extract(path)
    }

    /// Extract a string slice along a path.
    #[inline]
    #[must_use]
    pub fn extract_str(&self, path: &str) -> Option<&str> {
        self.root_view().extract_str(path)
    }

    /// Extract an integer along a path.
    #[inline]
    #[must_use]
    pub fn extract_i64(&self, path: &str) -> Option<i64> {
        self.root_view().extract_i64(path)
    }

    /// Extract a float along a path.
    #[inline]
    #[must_use]
    pub fn extract_f64(&self, path: &str) -> Option<f64> {
        self.root_view().extract_f64(path)
    }

    /// Extract a boolean along a path.
    #[inline]
    #[must_use]
    pub fn extract_bool(&self, path: &str) -> Option<bool> {
        self.root_view().extract_bool(path)
    }

    /// Convert the whole document back to `serde_json::Value`.
    #[inline]
    #[must_use]
    pub fn to_serde(&self) -> serde_json::Value {
        self.root_view().to_serde()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zero_copy_flat_document_roundtrip() {
        let json_val = serde_json::json!({
            "id": 1001,
            "profile": {
                "name": "Ada Lovelace",
                "email": "ada@lovelace.org",
                "active": true,
                "score": 98.75
            },
            "tags": ["math", "computing", "pioneer"]
        });

        let doc = JsonDocument::from_serde(&json_val);
        let bytes = doc.to_archived_bytes().expect("archive to bytes");

        // Safe access with validation
        let archived = access_archived_doc(&bytes).expect("safe access");

        // 0-nanosecond in-situ extraction
        assert_eq!(archived.extract_i64("id"), Some(1001));
        assert_eq!(archived.extract_str("profile.name"), Some("Ada Lovelace"));
        assert_eq!(archived.extract_str("profile.email"), Some("ada@lovelace.org"));
        assert_eq!(archived.extract_bool("profile.active"), Some(true));
        assert_eq!(archived.extract_f64("profile.score"), Some(98.75));
        assert_eq!(archived.extract_str("tags.1"), Some("computing"));

        // Unchecked 0 ns cast in trusted memory
        let direct = unsafe { access_archived_doc_unchecked(&bytes) };
        assert_eq!(direct.extract_str("profile.email"), Some("ada@lovelace.org"));

        // Relocation verification: copy to an offset memory buffer
        let mut relocated = vec![0u8; 16]; // offset by 16 bytes
        relocated.extend_from_slice(&bytes);
        let relocated_slice = &relocated[16..];
        let relocated_archived = access_archived_doc(relocated_slice).expect("relocated access");
        assert_eq!(
            relocated_archived.extract_str("profile.name"),
            Some("Ada Lovelace")
        );

        // Roundtrip to serde value matches original
        let reconstructed = direct.to_serde();
        assert_eq!(reconstructed, json_val);
    }
}
