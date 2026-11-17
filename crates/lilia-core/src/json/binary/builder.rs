//! Builder for constructing compact, sorted BinaryDoc buffers.

use super::{
    le_u32, BinaryDoc, MAGIC, TAG_ARRAY, TAG_BOOL_FALSE, TAG_BOOL_TRUE, TAG_FLOAT64, TAG_INT64,
    TAG_NULL, TAG_OBJECT, TAG_STRING,
};
use crate::{ErrorCode, LiliaError, Result};

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

    /// Encodes `val` into a document.
    ///
    /// # Errors
    /// `InvalidInput` when `val` exceeds the limits of the format: 4 GiB per document,
    /// `u32::MAX` elements per array or object and 65,535 bytes per object key.
    pub fn build_from_value(mut self, val: &serde_json::Value) -> Result<BinaryDoc> {
        let root_offset = self.write_value(val)?;
        let root_bytes = le_u32(root_offset, "document size")?;
        self.buf[4..8].copy_from_slice(&root_bytes);
        Ok(BinaryDoc { data: self.buf })
    }

    fn write_value(&mut self, val: &serde_json::Value) -> Result<usize> {
        Ok(match val {
            serde_json::Value::Null => self.write_tag(TAG_NULL),
            serde_json::Value::Bool(b) => {
                self.write_tag(if *b { TAG_BOOL_TRUE } else { TAG_BOOL_FALSE })
            }
            serde_json::Value::Number(num) => {
                // Integers beyond the `i64` range are stored as their nearest `f64`.
                if let Some(i) = num.as_i64() {
                    self.write_scalar(TAG_INT64, &i.to_le_bytes())
                } else if let Some(f) = num.as_f64() {
                    self.write_scalar(TAG_FLOAT64, &f.to_le_bytes())
                } else {
                    self.write_tag(TAG_NULL)
                }
            }
            serde_json::Value::String(s) => {
                let len = le_u32(s.len(), "string length")?;
                let offset = self.write_scalar(TAG_STRING, &len);
                self.buf.extend_from_slice(s.as_bytes());
                offset
            }
            serde_json::Value::Array(items) => {
                let offsets = items
                    .iter()
                    .map(|item| self.write_value(item))
                    .collect::<Result<Vec<_>>>()?;
                let start_offset =
                    self.write_scalar(TAG_ARRAY, &le_u32(items.len(), "array length")?);
                for off in offsets {
                    self.buf.extend_from_slice(&le_u32(off, "document size")?);
                }
                start_offset
            }
            serde_json::Value::Object(map) => {
                // Collect and sort keys for binary search
                let mut sorted_entries: Vec<(&String, &serde_json::Value)> = map.iter().collect();
                sorted_entries.sort_by(|a, b| a.0.cmp(b.0));

                let mut encoded_entries = Vec::with_capacity(sorted_entries.len());
                for (k, v) in sorted_entries {
                    let k_len = u16::try_from(k.len()).map_err(|_| {
                        LiliaError::new(
                            ErrorCode::InvalidInput,
                            format!(
                                "object key of {} bytes exceeds the 65535-byte limit",
                                k.len()
                            ),
                            false,
                        )
                    })?;
                    let k_off = self.buf.len();
                    self.buf.extend_from_slice(k.as_bytes());
                    let v_off = self.write_value(v)?;
                    encoded_entries.push((k_off, k_len, v_off));
                }

                let start_offset =
                    self.write_scalar(TAG_OBJECT, &le_u32(encoded_entries.len(), "object size")?);
                for (k_off, k_len, v_off) in encoded_entries {
                    self.buf.extend_from_slice(&le_u32(k_off, "document size")?);
                    self.buf.extend_from_slice(&k_len.to_le_bytes());
                    self.buf.extend_from_slice(&le_u32(v_off, "document size")?);
                }
                start_offset
            }
        })
    }

    /// Appends a value made of `tag` alone and returns its offset.
    fn write_tag(&mut self, tag: u8) -> usize {
        let offset = self.buf.len();
        self.buf.push(tag);
        offset
    }

    /// Appends `tag` followed by `payload` and returns the offset of the tag.
    fn write_scalar(&mut self, tag: u8, payload: &[u8]) -> usize {
        let offset = self.write_tag(tag);
        self.buf.extend_from_slice(payload);
        offset
    }
}
