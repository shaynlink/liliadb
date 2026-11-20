//! Blocks of sorted entries with prefix-compressed keys.
//!
//! Entry: `shared` (varint), `unshared` (varint), `value length` (varint), the unshared key bytes,
//! the value. Every `RESTART_INTERVAL` entries the key is stored in full (`shared` = 0) and its
//! offset is recorded; the block ends with the restart offsets (u32 LE each) and their count.

use std::cmp::Ordering;
use std::sync::Arc;

use bytes::Bytes;

use crate::coding::{put_varint, Reader};
use crate::error::{corruption, Result};
use crate::iter::InternalIterator;
use crate::key;

const RESTART_INTERVAL: usize = 16;

#[derive(Debug, Default)]
pub struct BlockBuilder {
    buffer: Vec<u8>,
    restarts: Vec<u32>,
    since_restart: usize,
    last_key: Vec<u8>,
}

impl BlockBuilder {
    /// Adds an entry; keys must arrive in increasing internal-key order.
    pub fn add(&mut self, key: &[u8], value: &[u8]) {
        let shared = if self.since_restart == RESTART_INTERVAL || self.buffer.is_empty() {
            self.restarts.push(u32::try_from(self.buffer.len()).unwrap_or(u32::MAX));
            self.since_restart = 0;
            0
        } else {
            self.last_key.iter().zip(key).take_while(|(a, b)| a == b).count()
        };
        put_varint(&mut self.buffer, shared as u64);
        put_varint(&mut self.buffer, (key.len() - shared) as u64);
        put_varint(&mut self.buffer, value.len() as u64);
        self.buffer.extend_from_slice(&key[shared..]);
        self.buffer.extend_from_slice(value);
        self.last_key.clear();
        self.last_key.extend_from_slice(key);
        self.since_restart += 1;
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub fn estimated_size(&self) -> usize {
        self.buffer.len() + self.restarts.len() * 4 + 4
    }

    /// The encoded block; the builder is reset for the next one.
    pub fn finish(&mut self) -> Vec<u8> {
        let mut block = std::mem::take(&mut self.buffer);
        for &restart in &self.restarts {
            block.extend_from_slice(&restart.to_le_bytes());
        }
        block.extend_from_slice(&u32::try_from(self.restarts.len()).unwrap_or(u32::MAX).to_le_bytes());
        self.restarts.clear();
        self.since_restart = 0;
        self.last_key.clear();
        block
    }
}

/// A decoded, immutable block.
#[derive(Debug)]
pub struct Block {
    data: Bytes,
    /// Offset where the restart array starts (end of the entries).
    restarts: usize,
    count: usize,
}

impl Block {
    pub fn new(data: Bytes) -> Result<Self> {
        if data.len() < 4 {
            return Err(corruption("block shorter than its restart count"));
        }
        let count = u32::from_le_bytes(data[data.len() - 4..].try_into().map_err(|_| corruption("block"))?) as usize;
        let restarts = data
            .len()
            .checked_sub(4 + count * 4)
            .filter(|_| count > 0 || data.len() == 4)
            .ok_or_else(|| corruption("block restart array out of bounds"))?;
        Ok(Self { data, restarts, count })
    }

    pub fn size(&self) -> usize {
        self.data.len()
    }

    fn restart(&self, i: usize) -> usize {
        let at = self.restarts + i * 4;
        u32::from_le_bytes([self.data[at], self.data[at + 1], self.data[at + 2], self.data[at + 3]]) as usize
    }

    pub fn iter(self: &Arc<Self>) -> BlockIter {
        BlockIter { block: Arc::clone(self), next: self.restarts, key: Vec::new(), value: 0..0, valid: false }
    }
}

/// Iterator over one block.
pub struct BlockIter {
    block: Arc<Block>,
    /// Offset of the entry after the current one.
    next: usize,
    key: Vec<u8>,
    value: std::ops::Range<usize>,
    valid: bool,
}

impl BlockIter {
    /// Decodes the entry at `offset`, which shares a prefix with the current key.
    fn decode_at(&mut self, offset: usize) -> Result<()> {
        if offset >= self.block.restarts {
            self.valid = false;
            return Ok(());
        }
        let data = &self.block.data[offset..self.block.restarts];
        let mut reader = Reader::new(data, "block entry");
        let shared = reader.varint_usize()?;
        let unshared = reader.varint_usize()?;
        let value_len = reader.varint_usize()?;
        if shared > self.key.len() {
            return Err(corruption("block entry shares more than the previous key"));
        }
        let suffix = reader.take(unshared)?;
        let value_start = offset + (data.len() - reader.remaining().len());
        reader.take(value_len)?;
        self.key.truncate(shared);
        self.key.extend_from_slice(suffix);
        key::check(&self.key)?;
        self.value = value_start..value_start + value_len;
        self.next = value_start + value_len;
        self.valid = true;
        Ok(())
    }
}

impl InternalIterator for BlockIter {
    fn valid(&self) -> bool {
        self.valid
    }

    fn seek_to_first(&mut self) -> Result<()> {
        self.key.clear();
        self.decode_at(if self.block.count == 0 { self.block.restarts } else { self.block.restart(0) })
    }

    fn seek(&mut self, target: &[u8]) -> Result<()> {
        // Last restart point whose key is before `target`, then a linear scan.
        let (mut low, mut high) = (0, self.block.count);
        while high - low > 1 {
            let mid = low.midpoint(high);
            self.key.clear();
            self.decode_at(self.block.restart(mid))?;
            if key::compare(&self.key, target) == Ordering::Less {
                low = mid;
            } else {
                high = mid;
            }
        }
        self.seek_to_first_at(low)?;
        while self.valid && key::compare(&self.key, target) == Ordering::Less {
            self.next()?;
        }
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        if self.valid {
            self.decode_at(self.next)?;
        }
        Ok(())
    }

    fn key(&self) -> &[u8] {
        &self.key
    }

    fn value(&self) -> &[u8] {
        &self.block.data[self.value.clone()]
    }
}

impl BlockIter {
    fn seek_to_first_at(&mut self, restart: usize) -> Result<()> {
        self.key.clear();
        if self.block.count == 0 {
            self.valid = false;
            return Ok(());
        }
        self.decode_at(self.block.restart(restart))
    }

    /// The value as a shared slice of the block, without copying.
    pub fn value_bytes(&self) -> Bytes {
        self.block.data.slice(self.value.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::{encode, Kind};

    #[test]
    fn seek_finds_every_key_and_the_gaps_between_them() {
        let keys: Vec<Vec<u8>> = (0..500u32).map(|i| encode(format!("key{:05}", i * 2).as_bytes(), 7, Kind::Put)).collect();
        let mut builder = BlockBuilder::default();
        for (i, k) in keys.iter().enumerate() {
            builder.add(k, format!("v{i}").as_bytes());
        }
        let block = Arc::new(Block::new(Bytes::from(builder.finish())).unwrap());
        let mut iter = block.iter();
        iter.seek_to_first().unwrap();
        let mut n = 0;
        while iter.valid() {
            assert_eq!(iter.key(), keys[n].as_slice());
            assert_eq!(iter.value(), format!("v{n}").as_bytes());
            iter.next().unwrap();
            n += 1;
        }
        assert_eq!(n, keys.len());
        for i in 0..500u32 {
            iter.seek(&key::lookup(format!("key{:05}", i * 2 + 1).as_bytes(), 9)).unwrap();
            if i == 499 {
                assert!(!iter.valid());
            } else {
                assert_eq!(iter.key(), keys[i as usize + 1].as_slice());
            }
        }
        assert!(Block::new(Bytes::from_static(&[1, 0, 0, 0])).is_err());
    }
}
