//! Sorted runs: immutable files of internal-key-ordered entries.
//!
//! Layout: data blocks (about `block_bytes` each), the filter, the index block (last key of each
//! data block → its offset and size), then a 48-byte footer: filter offset and size, index offset
//! and size, entry count, magic. Every block is followed by its masked CRC32C.

pub mod block;
pub mod cache;
pub mod filter;

use std::cmp::Ordering;
use std::sync::Arc;

use bytes::Bytes;
use lilia_io::{File, SyncLevel};

use self::block::{Block, BlockBuilder, BlockIter, Cursor, INDEX_RESTART_INTERVAL};
use self::cache::BlockCache;
use crate::coding::{masked_crc, put_varint, Reader};
use crate::error::{corruption, Result};
use crate::iter::InternalIterator;
use crate::key;
use crate::memtable::Found;

const MAGIC: u64 = 0x4C49_4C49_4153_5354; // "LILIASST"
const FOOTER: usize = 48;
const CRC: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Handle {
    offset: u64,
    size: u64,
}

/// What a finished table contains, for the manifest.
#[derive(Debug, Clone)]
pub struct TableInfo {
    pub size: u64,
    pub smallest: Vec<u8>,
    pub largest: Vec<u8>,
}

/// Writes one table; entries must be added in internal-key order.
#[derive(Debug)]
pub struct TableBuilder {
    file: Arc<dyn File>,
    offset: u64,
    block_bytes: usize,
    bits_per_key: usize,
    data: BlockBuilder,
    index: BlockBuilder,
    /// Last key of the block just written, waiting for its index entry.
    pending: Option<(Vec<u8>, Handle)>,
    hashes: Vec<u64>,
    last_user: Vec<u8>,
    last_key: Vec<u8>,
    smallest: Option<Vec<u8>>,
    entries: u64,
}

impl TableBuilder {
    pub fn new(file: Arc<dyn File>, block_bytes: usize, bits_per_key: usize) -> Self {
        Self {
            file,
            offset: 0,
            block_bytes,
            bits_per_key,
            data: BlockBuilder::default(),
            index: BlockBuilder::new(INDEX_RESTART_INTERVAL),
            pending: None,
            hashes: Vec::new(),
            last_user: Vec::new(),
            last_key: Vec::new(),
            smallest: None,
            entries: 0,
        }
    }

    pub fn add(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        debug_assert!(self.entries == 0 || key::compare(&self.last_key, key) == Ordering::Less);
        if let Some((last, handle)) = self.pending.take() {
            self.index_entry(&last, handle);
        }
        let user = key::user(key);
        if self.entries == 0 || user != self.last_user.as_slice() {
            self.hashes.push(filter::hash(user));
            self.last_user.clear();
            self.last_user.extend_from_slice(user);
        }
        if self.smallest.is_none() {
            self.smallest = Some(key.to_vec());
        }
        self.data.add(key, value);
        self.last_key.clear();
        self.last_key.extend_from_slice(key);
        self.entries += 1;
        if self.data.estimated_size() >= self.block_bytes {
            self.flush_block()?;
        }
        Ok(())
    }

    /// Bytes written so far plus the block being built.
    pub fn estimated_size(&self) -> u64 {
        self.offset + self.data.estimated_size() as u64
    }

    pub fn entries(&self) -> u64 {
        self.entries
    }

    fn index_entry(&mut self, last: &[u8], handle: Handle) {
        let mut value = Vec::with_capacity(16);
        put_varint(&mut value, handle.offset);
        put_varint(&mut value, handle.size);
        self.index.add(last, &value);
    }

    fn write_block(&mut self, contents: &[u8]) -> Result<Handle> {
        let handle = Handle { offset: self.offset, size: contents.len() as u64 };
        let mut framed = Vec::with_capacity(contents.len() + CRC);
        framed.extend_from_slice(contents);
        framed.extend_from_slice(&masked_crc(&[contents]).to_le_bytes());
        self.file.write_all_at(&framed, self.offset)?;
        self.offset += framed.len() as u64;
        Ok(handle)
    }

    fn flush_block(&mut self) -> Result<()> {
        if self.data.is_empty() {
            return Ok(());
        }
        let block = self.data.finish();
        let handle = self.write_block(&block)?;
        self.pending = Some((self.last_key.clone(), handle));
        Ok(())
    }

    /// Writes the remaining blocks and the footer, then syncs the file to `sync`.
    pub fn finish(mut self, sync: SyncLevel) -> Result<TableInfo> {
        self.flush_block()?;
        if let Some((last, handle)) = self.pending.take() {
            self.index_entry(&last, handle);
        }
        let filter = filter::build(&self.hashes, self.bits_per_key);
        let filter_handle = self.write_block(&filter)?;
        let index = self.index.finish();
        let index_handle = self.write_block(&index)?;
        let mut footer = Vec::with_capacity(FOOTER);
        for value in [filter_handle.offset, filter_handle.size, index_handle.offset, index_handle.size, self.entries, MAGIC] {
            footer.extend_from_slice(&value.to_le_bytes());
        }
        self.file.write_all_at(&footer, self.offset)?;
        self.offset += FOOTER as u64;
        self.file.sync(sync)?;
        Ok(TableInfo {
            size: self.offset,
            smallest: self.smallest.unwrap_or_default(),
            largest: self.last_key,
        })
    }
}

/// An open table.
#[derive(Debug)]
pub struct Table {
    number: u64,
    file: Arc<dyn File>,
    index: Arc<Block>,
    filter: Bytes,
    cache: Arc<BlockCache>,
}

fn read_block(file: &dyn File, handle: Handle, what: &str) -> Result<Bytes> {
    let size = usize::try_from(handle.size).map_err(|_| corruption(format!("{what}: block size overflows")))?;
    let mut data = vec![0u8; size + CRC];
    file.read_exact_at(&mut data, handle.offset)?;
    let stored = u32::from_le_bytes(data[size..].try_into().map_err(|_| corruption(what.to_owned()))?);
    data.truncate(size);
    if masked_crc(&[&data]) != stored {
        return Err(corruption(format!("{what}: checksum mismatch in block at {}", handle.offset)));
    }
    Ok(Bytes::from(data))
}

impl Table {
    pub fn open(number: u64, file: Arc<dyn File>, size: u64, cache: Arc<BlockCache>) -> Result<Self> {
        let what = format!("table {number}");
        if size < FOOTER as u64 {
            return Err(corruption(format!("{what}: {size} bytes is shorter than a footer")));
        }
        let mut footer = [0u8; FOOTER];
        file.read_exact_at(&mut footer, size - FOOTER as u64)?;
        let mut reader = Reader::new(&footer, "table footer");
        let mut next = || reader.fixed_u64();
        let filter = Handle { offset: next()?, size: next()? };
        let index = Handle { offset: next()?, size: next()? };
        let _entries = next()?;
        if next()? != MAGIC {
            return Err(corruption(format!("{what}: bad magic number")));
        }
        let filter = read_block(file.as_ref(), filter, &what)?;
        let index = Arc::new(Block::new(read_block(file.as_ref(), index, &what)?)?);
        Ok(Self { number, file, index, filter, cache })
    }

    fn data_block(&self, handle_value: &[u8]) -> Result<Arc<Block>> {
        let mut reader = Reader::new(handle_value, "index entry");
        let handle = Handle { offset: reader.varint()?, size: reader.varint()? };
        if let Some(block) = self.cache.get(self.number, handle.offset) {
            return Ok(block);
        }
        let block = Arc::new(Block::new(read_block(self.file.as_ref(), handle, &format!("table {}", self.number))?)?);
        self.cache.insert(self.number, handle.offset, Arc::clone(&block));
        Ok(block)
    }

    /// The first entry at or after `target` if it belongs to `user`: the newest version visible
    /// at the target's sequence number.
    pub fn get(&self, target: &[u8], user: &[u8]) -> Result<Option<Found>> {
        if !filter::may_contain(&self.filter, filter::hash(user)) {
            return Ok(None);
        }
        // One cursor for the index, then for the data block: one key buffer per lookup.
        let mut cursor = Cursor::default();
        cursor.seek(&self.index, target)?;
        if !cursor.valid() {
            return Ok(None);
        }
        let block = self.data_block(cursor.value(&self.index))?;
        cursor.seek(&block, target)?;
        if !cursor.valid() || key::user(cursor.key()) != user {
            return Ok(None);
        }
        Ok(Some(Found { kind: key::kind(cursor.key())?, sequence: key::sequence(cursor.key()), value: block.slice(cursor.value_range()) }))
    }

    pub fn iter(self: &Arc<Self>) -> TableIter {
        TableIter { table: Arc::clone(self), index: self.index.iter(), data: None }
    }
}

/// Two-level iterator: the index, then the data block it points to.
pub struct TableIter {
    table: Arc<Table>,
    index: BlockIter,
    data: Option<BlockIter>,
}

impl TableIter {
    /// Moves to the first entry of the data block under the index cursor, skipping empty blocks.
    fn settle(&mut self, target: Option<&[u8]>) -> Result<()> {
        loop {
            if !self.index.valid() {
                self.data = None;
                return Ok(());
            }
            let mut data = self.table.data_block(self.index.value())?.iter();
            match target {
                Some(t) => data.seek(t)?,
                None => data.seek_to_first()?,
            }
            if data.valid() {
                self.data = Some(data);
                return Ok(());
            }
            self.index.next()?;
        }
    }
}

impl InternalIterator for TableIter {
    fn valid(&self) -> bool {
        self.data.as_ref().is_some_and(BlockIter::valid)
    }

    fn seek_to_first(&mut self) -> Result<()> {
        self.index.seek_to_first()?;
        self.settle(None)
    }

    fn seek(&mut self, target: &[u8]) -> Result<()> {
        self.index.seek(target)?;
        self.settle(Some(target))
    }

    fn next(&mut self) -> Result<()> {
        let Some(data) = self.data.as_mut() else { return Ok(()) };
        data.next()?;
        if !data.valid() {
            self.index.next()?;
            self.settle(None)?;
        }
        Ok(())
    }

    fn key(&self) -> &[u8] {
        self.data.as_ref().map_or(&[], InternalIterator::key)
    }

    fn value(&self) -> &[u8] {
        self.data.as_ref().map_or(&[], InternalIterator::value)
    }

    fn value_bytes(&self) -> Bytes {
        self.data.as_ref().map_or_else(Bytes::new, InternalIterator::value_bytes)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use lilia_io::{MemVfs, OpenMode, Vfs};

    use super::*;
    use crate::key::{encode, lookup, Kind};

    #[test]
    fn tables_round_trip_point_lookups_and_scans() {
        let vfs = MemVfs::new(1);
        vfs.create_dir_all(Path::new("/t")).unwrap();
        let file = vfs.open(Path::new("/t/1.sst"), OpenMode::CreateNew).unwrap();
        let mut builder = TableBuilder::new(Arc::clone(&file), 512, 10);
        let n = 3_000u32;
        for i in 0..n {
            let user = format!("user{i:06}");
            builder.add(&encode(user.as_bytes(), 10, Kind::Put), format!("new{i}").as_bytes()).unwrap();
            builder.add(&encode(user.as_bytes(), 5, Kind::Put), format!("old{i}").as_bytes()).unwrap();
        }
        assert_eq!(builder.entries(), u64::from(n) * 2);
        let info = builder.finish(SyncLevel::Full).unwrap();
        let table = Arc::new(Table::open(1, file, info.size, Arc::new(BlockCache::new(1 << 20))).unwrap());

        for i in (0..n).step_by(7) {
            let user = format!("user{i:06}");
            let newest = table.get(&lookup(user.as_bytes(), 20), user.as_bytes()).unwrap().unwrap();
            assert_eq!((newest.value, newest.sequence), (Bytes::from(format!("new{i}")), 10));
            let older = table.get(&lookup(user.as_bytes(), 7), user.as_bytes()).unwrap().unwrap();
            assert_eq!(older.value, format!("old{i}"));
            assert!(table.get(&lookup(user.as_bytes(), 4), user.as_bytes()).unwrap().is_none());
        }
        assert!(table.get(&lookup(b"user999999", 20), b"user999999").unwrap().is_none());

        let mut iter = table.iter();
        iter.seek_to_first().unwrap();
        let mut count = 0;
        while iter.valid() {
            count += 1;
            iter.next().unwrap();
        }
        assert_eq!(count, n * 2);
        iter.seek(&lookup(b"user001500", 7)).unwrap();
        assert_eq!(key::user(iter.key()), b"user001500");
        assert_eq!(iter.value(), b"old1500");
    }

    #[test]
    fn a_damaged_block_is_reported_as_corruption() {
        let vfs = MemVfs::new(2);
        vfs.create_dir_all(Path::new("/t")).unwrap();
        let file = vfs.open(Path::new("/t/2.sst"), OpenMode::CreateNew).unwrap();
        let mut builder = TableBuilder::new(Arc::clone(&file), 4096, 10);
        builder.add(&encode(b"k", 1, Kind::Put), b"value").unwrap();
        let info = builder.finish(SyncLevel::Full).unwrap();
        file.write_all_at(b"X", 3).unwrap();
        let table = Table::open(2, file, info.size, Arc::new(BlockCache::new(0))).unwrap();
        assert!(matches!(table.get(&lookup(b"k", 5), b"k"), Err(crate::Error::Corruption(_))));
    }
}
