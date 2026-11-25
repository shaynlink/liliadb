//! Manifest records: the changes from one version of the set of sorted runs to the next.

use crate::coding::{put_bytes, put_varint, Reader};
use crate::error::{corruption, Result};
use crate::key;
use crate::options::LEVELS;

const LOG_NUMBER: u64 = 1;
const NEXT_FILE: u64 = 2;
const LAST_SEQUENCE: u64 = 3;
const ADDED: u64 = 4;
const DELETED: u64 = 5;

/// A sorted run as recorded in the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFile {
    pub level: usize,
    pub number: u64,
    pub size: u64,
    pub smallest: Vec<u8>,
    pub largest: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VersionEdit {
    /// Journals numbered below this hold only flushed writes.
    pub log_number: Option<u64>,
    pub next_file: Option<u64>,
    pub last_sequence: Option<u64>,
    pub added: Vec<NewFile>,
    /// `(level, number)`.
    pub deleted: Vec<(usize, u64)>,
}

impl VersionEdit {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (tag, value) in [
            (LOG_NUMBER, self.log_number),
            (NEXT_FILE, self.next_file),
            (LAST_SEQUENCE, self.last_sequence),
        ] {
            if let Some(value) = value {
                put_varint(&mut out, tag);
                put_varint(&mut out, value);
            }
        }
        for &(level, number) in &self.deleted {
            put_varint(&mut out, DELETED);
            put_varint(&mut out, level as u64);
            put_varint(&mut out, number);
        }
        for file in &self.added {
            put_varint(&mut out, ADDED);
            put_varint(&mut out, file.level as u64);
            put_varint(&mut out, file.number);
            put_varint(&mut out, file.size);
            put_bytes(&mut out, &file.smallest);
            put_bytes(&mut out, &file.largest);
        }
        out
    }

    pub fn decode(record: &[u8]) -> Result<Self> {
        let mut reader = Reader::new(record, "manifest record");
        let mut edit = Self::default();
        let level = |reader: &mut Reader<'_>| -> Result<usize> {
            let level = reader.varint_usize()?;
            if level >= LEVELS {
                return Err(corruption(format!(
                    "manifest record: level {level} out of range"
                )));
            }
            Ok(level)
        };
        while !reader.is_empty() {
            match reader.varint()? {
                LOG_NUMBER => edit.log_number = Some(reader.varint()?),
                NEXT_FILE => edit.next_file = Some(reader.varint()?),
                LAST_SEQUENCE => edit.last_sequence = Some(reader.varint()?),
                DELETED => {
                    let level = level(&mut reader)?;
                    edit.deleted.push((level, reader.varint()?));
                }
                ADDED => {
                    let level = level(&mut reader)?;
                    let (number, size) = (reader.varint()?, reader.varint()?);
                    let smallest = reader.bytes()?.to_vec();
                    let largest = reader.bytes()?.to_vec();
                    key::check(&smallest)?;
                    key::check(&largest)?;
                    edit.added.push(NewFile {
                        level,
                        number,
                        size,
                        smallest,
                        largest,
                    });
                }
                tag => return Err(corruption(format!("manifest record: unknown tag {tag}"))),
            }
        }
        Ok(edit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::{encode, Kind};

    #[test]
    fn edits_round_trip() {
        let edit = VersionEdit {
            log_number: Some(4),
            next_file: Some(9),
            last_sequence: Some(1_000),
            added: vec![NewFile {
                level: 1,
                number: 7,
                size: 4_096,
                smallest: encode(b"a", 3, Kind::Put),
                largest: encode(b"z", 1, Kind::Delete),
            }],
            deleted: vec![(0, 5), (0, 6)],
        };
        assert_eq!(VersionEdit::decode(&edit.encode()).unwrap(), edit);
        assert!(VersionEdit::decode(&[u8::try_from(DELETED).unwrap(), 9, 1]).is_err());
        assert!(VersionEdit::decode(&[42]).is_err());
    }
}
