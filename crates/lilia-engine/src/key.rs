//! Internal keys: a user key followed by an 8-byte trailer `(sequence << 8) | kind`, little
//! endian. They order by user key ascending, then by sequence descending, so the newest version
//! of a key is met first.

use std::cmp::Ordering;

use bytes::Bytes;

use crate::error::{corruption, Result};

/// Largest sequence number: the trailer keeps 8 bits for the kind.
pub const MAX_SEQUENCE: u64 = (1 << 56) - 1;
pub const TRAILER: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Delete = 0,
    Put = 1,
}

impl Kind {
    pub fn from_u8(byte: u8) -> Result<Self> {
        match byte {
            0 => Ok(Self::Delete),
            1 => Ok(Self::Put),
            other => Err(corruption(format!("unknown entry kind {other}"))),
        }
    }
}

pub fn encode(user: &[u8], sequence: u64, kind: Kind) -> Vec<u8> {
    let mut key = Vec::with_capacity(user.len() + TRAILER);
    key.extend_from_slice(user);
    key.extend_from_slice(&((sequence << 8) | kind as u64).to_le_bytes());
    key
}

/// The key that seeks to the newest version of `user` visible at `sequence`.
pub fn lookup(user: &[u8], sequence: u64) -> Vec<u8> {
    encode(user, sequence, Kind::Put)
}

fn trailer(key: &[u8]) -> u64 {
    let mut bytes = [0u8; TRAILER];
    bytes.copy_from_slice(&key[key.len() - TRAILER..]);
    u64::from_le_bytes(bytes)
}

/// User key of a well-formed internal key.
pub fn user(key: &[u8]) -> &[u8] {
    &key[..key.len() - TRAILER]
}

pub fn sequence(key: &[u8]) -> u64 {
    trailer(key) >> 8
}

pub fn kind(key: &[u8]) -> Result<Kind> {
    Kind::from_u8(trailer(key).to_le_bytes()[0])
}

/// Rejects byte strings too short to hold a trailer; called on every key read from disk.
pub fn check(key: &[u8]) -> Result<()> {
    if key.len() < TRAILER {
        return Err(corruption(format!("internal key of {} bytes", key.len())));
    }
    Kind::from_u8(key[key.len() - TRAILER]).map(drop)
}

pub fn compare(a: &[u8], b: &[u8]) -> Ordering {
    user(a)
        .cmp(user(b))
        .then_with(|| trailer(b).cmp(&trailer(a)))
}

/// An encoded internal key ordered by [`compare`], for ordered containers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InternalKey(pub Bytes);

impl Ord for InternalKey {
    fn cmp(&self, other: &Self) -> Ordering {
        compare(&self.0, &other.0)
    }
}

impl PartialOrd for InternalKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_version_sorts_first_and_prefix_keys_sort_by_user_key() {
        let a5 = encode(b"a", 5, Kind::Put);
        let a3 = encode(b"a", 3, Kind::Delete);
        let ab1 = encode(b"ab", 1, Kind::Put);
        assert_eq!(compare(&a5, &a3), Ordering::Less);
        assert_eq!(compare(&a3, &ab1), Ordering::Less);
        assert_eq!(compare(&lookup(b"a", 4), &a3), Ordering::Less);
        assert_eq!(compare(&lookup(b"a", 4), &a5), Ordering::Greater);
        assert_eq!(
            (user(&a3), sequence(&a3), kind(&a3).unwrap()),
            (&b"a"[..], 3, Kind::Delete)
        );
        assert!(check(b"short").is_err());
    }
}
