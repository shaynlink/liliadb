use rkyv::{Archive, Deserialize, Serialize};

/// High-performance relocatable KV entry with zero-copy relative pointers.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq))]
pub struct RelKvEntry {
    pub namespace: String,
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub version: u64,
    pub expires_at_ms: Option<i64>,
}

/// Zero-copy relocatable JSON Document entry.
#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq))]
pub struct RelJsonEntry {
    pub space: String,
    pub id: String,
    pub version: u64,
    pub payload: Vec<u8>,
}
