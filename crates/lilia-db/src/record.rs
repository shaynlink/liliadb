//! Encodings of stored records: key-value records with an optional expiry, and JSON documents.
//!
//! Each record starts with a format byte, so later formats can be read next to these ones.

use bytes::Bytes;
use lilia_core::BinaryDoc;

use crate::error::{corruption, Result};

const KV_PLAIN: u8 = 0;
const KV_EXPIRING: u8 = 1;
const DOC_BINARY: u8 = 0;

/// A decoded key-value record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvRecord {
    pub expires_at_ms: Option<i64>,
    pub value: Bytes,
}

impl KvRecord {
    pub fn expired(&self, now_ms: i64) -> bool {
        self.expires_at_ms.is_some_and(|at| at <= now_ms)
    }
}

pub fn encode_kv(value: &[u8], expires_at_ms: Option<i64>) -> Bytes {
    let mut out = Vec::with_capacity(value.len() + 9);
    match expires_at_ms {
        None => out.push(KV_PLAIN),
        Some(at) => {
            out.push(KV_EXPIRING);
            out.extend_from_slice(&at.to_le_bytes());
        }
    }
    out.extend_from_slice(value);
    out.into()
}

/// Decodes a key-value record; the value shares `raw`'s buffer.
///
/// # Errors
///
/// [`crate::Error::Corruption`] for an unknown format or a truncated expiry.
pub fn decode_kv(raw: &Bytes) -> Result<KvRecord> {
    match raw.first() {
        Some(&KV_PLAIN) => Ok(KvRecord {
            expires_at_ms: None,
            value: raw.slice(1..),
        }),
        Some(&KV_EXPIRING) if raw.len() >= 9 => {
            let mut at = [0u8; 8];
            at.copy_from_slice(&raw[1..9]);
            Ok(KvRecord {
                expires_at_ms: Some(i64::from_le_bytes(at)),
                value: raw.slice(9..),
            })
        }
        _ => Err(corruption("key-value record with an unknown format")),
    }
}

pub fn encode_doc(doc: &BinaryDoc) -> Bytes {
    let bytes = doc.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.push(DOC_BINARY);
    out.extend_from_slice(bytes);
    out.into()
}

/// # Errors
///
/// [`crate::Error::Corruption`] for an unknown format or a document that fails validation.
pub fn decode_doc(raw: &[u8]) -> Result<BinaryDoc> {
    match raw.split_first() {
        Some((&DOC_BINARY, body)) => BinaryDoc::from_bytes(body.to_vec())
            .map_err(|e| corruption(format!("document record: {e}"))),
        _ => Err(corruption("document record with an unknown format")),
    }
}

/// Milliseconds since the Unix epoch, the clock of expiries and ledger commits.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip() {
        let plain = decode_kv(&encode_kv(b"v", None)).unwrap();
        assert_eq!(
            plain,
            KvRecord {
                expires_at_ms: None,
                value: Bytes::from_static(b"v")
            }
        );
        let expiring = decode_kv(&encode_kv(b"", Some(-5))).unwrap();
        assert_eq!(expiring.expires_at_ms, Some(-5));
        assert!(expiring.expired(0) && !plain.expired(i64::MAX));
        assert!(decode_kv(&Bytes::from_static(&[1, 2])).is_err());
        assert!(decode_kv(&Bytes::new()).is_err());

        let doc = BinaryDoc::from_json_value(&serde_json::json!({"a": [1, "x"]})).unwrap();
        assert_eq!(decode_doc(&encode_doc(&doc)).unwrap(), doc);
        assert!(decode_doc(&[9]).is_err());
    }
}
