//! Atomic groups of writes, with optional version expectations.

use bytes::Bytes;

use crate::coding::{put_bytes, put_varint, Reader};
use crate::error::{corruption, Result};
use crate::key::Kind;

/// One write of a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Put { key: Bytes, value: Bytes },
    Delete { key: Bytes },
}

impl Op {
    pub fn key(&self) -> &Bytes {
        match self {
            Self::Put { key, .. } | Self::Delete { key } => key,
        }
    }

    fn kind(&self) -> Kind {
        match self {
            Self::Put { .. } => Kind::Put,
            Self::Delete { .. } => Kind::Delete,
        }
    }
}

/// A condition checked when the batch commits: the newest version of `key` must be `version`
/// (`None`: the key must not exist). The version of a key is the sequence number of the write
/// that produced it, as returned by [`crate::Db::write`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expectation {
    pub key: Bytes,
    pub version: Option<u64>,
}

/// Writes applied atomically: all of them, under consecutive sequence numbers, or none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriteBatch {
    pub(crate) ops: Vec<Op>,
    pub(crate) expectations: Vec<Expectation>,
    bytes: usize,
}

impl WriteBatch {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&mut self, key: impl Into<Bytes>, value: impl Into<Bytes>) -> &mut Self {
        let (key, value) = (key.into(), value.into());
        self.bytes += key.len() + value.len() + 16;
        self.ops.push(Op::Put { key, value });
        self
    }

    pub fn delete(&mut self, key: impl Into<Bytes>) -> &mut Self {
        let key = key.into();
        self.bytes += key.len() + 16;
        self.ops.push(Op::Delete { key });
        self
    }

    /// Commits only if the newest version of `key` is `version` (`None`: absent).
    pub fn expect(&mut self, key: impl Into<Bytes>, version: Option<u64>) -> &mut Self {
        self.expectations.push(Expectation { key: key.into(), version });
        self
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Approximate encoded size, used to bound group commits.
    #[must_use]
    pub fn approximate_bytes(&self) -> usize {
        self.bytes
    }

    #[must_use]
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }
}

/// Journal payload of a committed batch: its first sequence number, then its writes.
pub fn encode(out: &mut Vec<u8>, first_sequence: u64, ops: &[Op]) {
    out.extend_from_slice(&first_sequence.to_le_bytes());
    put_varint(out, ops.len() as u64);
    for op in ops {
        out.push(op.kind() as u8);
        put_bytes(out, op.key());
        if let Op::Put { value, .. } = op {
            put_bytes(out, value);
        }
    }
}

/// Inverse of [`encode`]; `record` is shared so keys and values are not copied.
pub fn decode(record: &Bytes) -> Result<(u64, Vec<Op>)> {
    let mut reader = Reader::new(record, "journal record");
    let first = reader.fixed_u64()?;
    let count = reader.varint_usize()?;
    let mut ops = Vec::with_capacity(count.min(record.len()));
    let slice = |part: &[u8]| record.slice_ref(part);
    for _ in 0..count {
        let kind = Kind::from_u8(reader.u8()?)?;
        let key = slice(reader.bytes()?);
        ops.push(match kind {
            Kind::Put => Op::Put { key, value: slice(reader.bytes()?) },
            Kind::Delete => Op::Delete { key },
        });
    }
    if !reader.is_empty() {
        return Err(corruption("journal record: trailing bytes after the last write"));
    }
    Ok((first, ops))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_round_trip_through_the_journal_encoding() {
        let mut batch = WriteBatch::new();
        batch.put("a", "1").delete("b").put("", vec![0u8; 300]).expect("a", None);
        let mut out = Vec::new();
        encode(&mut out, 42, batch.ops());
        let (first, ops) = decode(&Bytes::from(out.clone())).unwrap();
        assert_eq!(first, 42);
        assert_eq!(ops, batch.ops);
        out.push(0);
        assert!(decode(&Bytes::from(out)).is_err());
    }
}
