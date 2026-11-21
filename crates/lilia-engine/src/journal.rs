//! Append-only record log, used for the write-ahead journal and the manifest.
//!
//! The file is a sequence of 32 KiB blocks. A record is stored as one or more fragments, each with
//! a 7-byte header: masked CRC32C of type and payload (4), payload length (2), type (1). A
//! fragment never crosses a block boundary; fewer than 7 bytes left in a block are zero padding.
//! A zero header means "nothing written past here", so a preallocated or torn tail ends the log
//! cleanly, and any damaged fragment is detected by its checksum.

use std::io;
use std::sync::Arc;

use bytes::{Bytes, BytesMut};
use lilia_io::{File, SyncLevel};

use crate::coding::masked_crc;
use crate::error::{corruption, Result};

pub const BLOCK: usize = 32 * 1024;
const HEADER: usize = 7;

const FULL: u8 = 1;
const FIRST: u8 = 2;
const MIDDLE: u8 = 3;
const LAST: u8 = 4;

/// Appends records; one [`JournalWriter::append`] is one positional write.
#[derive(Debug)]
pub struct JournalWriter {
    file: Arc<dyn File>,
    offset: u64,
    buffer: Vec<u8>,
}

impl JournalWriter {
    /// A writer at the start of an empty file.
    pub fn new(file: Arc<dyn File>) -> Self {
        Self { file, offset: 0, buffer: Vec::new() }
    }

    /// Frames every record and writes them with a single positional write.
    pub fn append<'a>(&mut self, records: impl IntoIterator<Item = &'a [u8]>) -> io::Result<()> {
        self.buffer.clear();
        let mut block_offset = usize::try_from(self.offset % BLOCK as u64).unwrap_or_default();
        for record in records {
            let mut rest = record;
            let mut first = true;
            loop {
                let left = BLOCK - block_offset;
                if left < HEADER {
                    self.buffer.resize(self.buffer.len() + left, 0);
                    block_offset = 0;
                }
                let room = BLOCK - block_offset - HEADER;
                let (fragment, tail) = rest.split_at(rest.len().min(room));
                let last = tail.is_empty();
                let kind = match (first, last) {
                    (true, true) => FULL,
                    (true, false) => FIRST,
                    (false, false) => MIDDLE,
                    (false, true) => LAST,
                };
                let length = u16::try_from(fragment.len()).map_err(io::Error::other)?;
                self.buffer.extend_from_slice(&masked_crc(&[&[kind], fragment]).to_le_bytes());
                self.buffer.extend_from_slice(&length.to_le_bytes());
                self.buffer.push(kind);
                self.buffer.extend_from_slice(fragment);
                block_offset += HEADER + fragment.len();
                rest = tail;
                first = false;
                if last {
                    break;
                }
            }
        }
        self.file.write_all_at(&self.buffer, self.offset)?;
        self.offset += self.buffer.len() as u64;
        Ok(())
    }

    pub fn sync(&self, level: SyncLevel) -> io::Result<()> {
        self.file.sync(level)
    }

    /// Bytes written so far.
    #[cfg(test)]
    pub fn size(&self) -> u64 {
        self.offset
    }
}

/// Records read back from a journal file.
#[derive(Debug, Default)]
pub struct Recovered {
    pub records: Vec<Bytes>,
    /// Bytes after the last complete record were damaged or incomplete (a torn write).
    pub torn: bool,
}

/// Reads every complete record of `file`, stopping at the first damaged or missing fragment.
pub fn read_all(file: &dyn File) -> Result<Recovered> {
    let len = usize::try_from(file.len()?).map_err(|_| corruption("journal larger than memory"))?;
    let mut data = vec![0u8; len];
    file.read_exact_at(&mut data, 0)?;
    let data = Bytes::from(data);
    let mut recovered = Recovered::default();
    let mut pending: Option<BytesMut> = None;
    let mut offset = 0;
    while offset < data.len() {
        let block_left = BLOCK - offset % BLOCK;
        if block_left < HEADER {
            offset += block_left;
            continue;
        }
        if data.len() - offset < HEADER {
            recovered.torn = data[offset..].iter().any(|&b| b != 0) || pending.is_some();
            return Ok(recovered);
        }
        let header = &data[offset..offset + HEADER];
        let crc = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        let length = usize::from(u16::from_le_bytes([header[4], header[5]]));
        let kind = header[6];
        if kind == 0 && length == 0 && crc == 0 {
            // Unwritten (preallocated or zero-filled) space: the log ends here.
            recovered.torn = pending.is_some() || data[offset..].iter().any(|&b| b != 0);
            return Ok(recovered);
        }
        let start = offset + HEADER;
        let valid = length <= block_left - HEADER
            && start + length <= data.len()
            && masked_crc(&[&[kind], &data[start..start + length]]) == crc;
        if !valid {
            recovered.torn = true;
            return Ok(recovered);
        }
        let fragment = &data[start..start + length];
        offset = start + length;
        match (kind, pending.as_mut()) {
            (FULL, None) => recovered.records.push(data.slice(start..start + length)),
            (FIRST, None) => pending = Some(BytesMut::from(fragment)),
            (MIDDLE, Some(buffer)) => buffer.extend_from_slice(fragment),
            (LAST, Some(buffer)) => {
                buffer.extend_from_slice(fragment);
                recovered.records.push(pending.take().map_or_else(Bytes::new, BytesMut::freeze));
            }
            _ => {
                recovered.torn = true;
                return Ok(recovered);
            }
        }
    }
    recovered.torn = pending.is_some();
    Ok(recovered)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use lilia_io::{CrashOptions, MemVfs, OpenMode, Vfs};

    use super::*;

    fn records() -> Vec<Vec<u8>> {
        (0..200u32).map(|i| vec![u8::try_from(i % 251).unwrap(); usize::try_from(i * 997 % 70_000).unwrap()]).collect()
    }

    #[test]
    fn records_of_any_size_round_trip_across_blocks() {
        let vfs = MemVfs::new(1);
        vfs.create_dir_all(Path::new("/j")).unwrap();
        let file = vfs.open(Path::new("/j/log"), OpenMode::CreateNew).unwrap();
        let mut writer = JournalWriter::new(Arc::clone(&file));
        let all = records();
        for chunk in all.chunks(7) {
            writer.append(chunk.iter().map(Vec::as_slice)).unwrap();
        }
        file.preallocate(writer.size() + 100_000).unwrap();
        let got = read_all(file.as_ref()).unwrap();
        assert!(!got.torn);
        assert_eq!(got.records.iter().map(|r| r.to_vec()).collect::<Vec<_>>(), all);
    }

    #[test]
    fn a_torn_tail_yields_exactly_a_prefix_of_complete_records() {
        let all = records();
        for seed in 0..200 {
            let vfs = MemVfs::new(seed);
            vfs.create_dir_all(Path::new("/j")).unwrap();
            let file = vfs.open(Path::new("/j/log"), OpenMode::CreateNew).unwrap();
            vfs.sync_dir(Path::new("/j")).unwrap();
            let mut writer = JournalWriter::new(Arc::clone(&file));
            let synced = usize::try_from(seed % 50).unwrap();
            writer.append(all[..synced].iter().map(Vec::as_slice)).unwrap();
            writer.sync(SyncLevel::Full).unwrap();
            writer.append(all[synced..].iter().map(Vec::as_slice)).unwrap();
            vfs.crash();
            vfs.restart(CrashOptions::TORN);
            let file = vfs.open(Path::new("/j/log"), OpenMode::Read).unwrap();
            let got = read_all(file.as_ref()).unwrap();
            assert!(got.records.len() >= synced, "seed {seed}: lost synced records");
            for (i, record) in got.records.iter().enumerate() {
                assert_eq!(record.as_ref(), all[i].as_slice(), "seed {seed}: record {i} differs");
            }
        }
    }
}
