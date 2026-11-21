//! Layout of the engine's key space.
//!
//! Every engine key starts with a tag byte that names its model, so each model owns one
//! contiguous, ordered range:
//!
//! | tag | key | value |
//! |-----|-----|-------|
//! | `0x00` | `schema\0` name, `format`, `next-index` | catalog records |
//! | `0x01` | namespace `\0` key | key-value record ([`crate::record`]) |
//! | `0x02` | space `\0` id | JSON document record |
//! | `0x03` | space `\0` index id (8 bytes) escaped value, id | empty: one secondary index entry |
//! | `0x04` | `n` position, `c` commit, `v` locator, `h` | ledger ([`crate::ledger`]) |
//! | `0x05` | graph `\0` node id | graph node ([`crate::graph`]) |
//! | `0x06` | graph `\0` source `\0` kind `\0` target | edge weight |
//! | `0x07` | graph `\0` target `\0` kind `\0` source | edge weight (inbound mirror) |
//! | `0x08` | collection `\0` document id | indexed document ([`crate::fts`]) |
//! | `0x09` | collection `\0` term `\0` document id | posting: positions, document length |
//! | `0x0A` | collection `\0` shard | document and token counts |
//! | `0x0B` | collection `\0` term | dictionary word count |
//! | `0x0C` | collection `\0` id | vector ([`crate::vectors`]) |
//!
//! Collection names never contain control characters ([`validate_name`]), so the `\0` after a
//! name separates collections and keeps them in name order.

use bytes::Bytes;

use crate::error::{invalid, Result};

pub const KV: u8 = 0x01;
pub const DOC: u8 = 0x02;
pub const INDEX: u8 = 0x03;
pub const LEDGER: u8 = 0x04;
pub const GRAPH_NODE: u8 = 0x05;
pub const EDGE_OUT: u8 = 0x06;
pub const EDGE_IN: u8 = 0x07;
pub const FTS_DOC: u8 = 0x08;
pub const FTS_POSTING: u8 = 0x09;
pub const FTS_STATS: u8 = 0x0A;
pub const FTS_DICTIONARY: u8 = 0x0B;
pub const VECTOR: u8 = 0x0C;

/// Tags of the keyspaces whose records the ledger attests. Derived keyspaces (secondary
/// index entries, inbound edge mirrors) are not records of their own.
pub const RECORD_TAGS: [u8; 7] = [
    KV,
    DOC,
    GRAPH_NODE,
    EDGE_OUT,
    FTS_DOC,
    FTS_DICTIONARY,
    VECTOR,
];

pub const FORMAT: &[u8] = b"\x00format";
pub const NEXT_INDEX: &[u8] = b"\x00next-index";
pub const SCHEMA_PREFIX: &[u8] = b"\x00schema\x00";
pub const LEDGER_HEAD: &[u8] = &[LEDGER, b'h'];
pub const LEDGER_COMMITS: &[u8] = &[LEDGER, b'c'];

/// Checks a collection, index or policy name: 1 to 255 bytes, no control characters.
///
/// # Errors
///
/// [`crate::Error::InvalidArgument`] naming `field`.
pub fn validate_name(field: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 255 || value.chars().any(char::is_control) {
        return Err(invalid(format!(
            "{field} must contain 1..=255 bytes and no control characters"
        )));
    }
    Ok(())
}

/// `tag | name | 0x00`: the prefix of every key of one collection.
pub fn collection(tag: u8, name: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(name.len() + 34);
    key.push(tag);
    key.extend_from_slice(name.as_bytes());
    key.push(0);
    key
}

pub fn kv(namespace: &str, key: &[u8]) -> Bytes {
    let mut out = collection(KV, namespace);
    out.extend_from_slice(key);
    out.into()
}

pub fn doc(space: &str, id: &str) -> Bytes {
    let mut out = collection(DOC, space);
    out.extend_from_slice(id.as_bytes());
    out.into()
}

/// Prefix of every entry of one index.
pub fn index(space: &str, index: u64) -> Vec<u8> {
    let mut out = collection(INDEX, space);
    out.extend_from_slice(&index.to_be_bytes());
    out
}

/// Prefix of the entries of one index value: the value is escaped (`00` becomes `00 FF`) and
/// terminated by `00 01`, so values keep their byte order and no value is a prefix of another.
pub fn index_value(space: &str, index_id: u64, value: &str) -> Vec<u8> {
    let mut out = index(space, index_id);
    for &byte in value.as_bytes() {
        out.push(byte);
        if byte == 0 {
            out.push(0xFF);
        }
    }
    out.extend_from_slice(&[0, 1]);
    out
}

pub fn index_entry(space: &str, index_id: u64, value: &str, id: &str) -> Bytes {
    let mut out = index_value(space, index_id, value);
    out.extend_from_slice(id.as_bytes());
    out.into()
}

/// The smallest key greater than every key that starts with `prefix`; `None` when there is
/// none (the prefix is all `FF`).
pub fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    while let Some(last) = end.pop() {
        if last < 0xFF {
            end.push(last + 1);
            return Some(end);
        }
    }
    None
}

pub fn schema(name: &str) -> Bytes {
    let mut out = SCHEMA_PREFIX.to_vec();
    out.extend_from_slice(name.as_bytes());
    out.into()
}

pub fn ledger_node(position: u64) -> Bytes {
    let mut out = vec![LEDGER, b'n'];
    out.extend_from_slice(&position.to_be_bytes());
    out.into()
}

pub fn ledger_commit(index: u64) -> Bytes {
    let mut out = LEDGER_COMMITS.to_vec();
    out.extend_from_slice(&index.to_be_bytes());
    out.into()
}

/// Locator of the ledger commit whose first write has `sequence`. The sequence is stored
/// inverted, so a forward seek from the locator of a version finds the newest commit that
/// started at or before it.
pub fn ledger_locator(sequence: u64) -> Bytes {
    let mut out = vec![LEDGER, b'v'];
    out.extend_from_slice(&(!sequence).to_be_bytes());
    out.into()
}

pub fn ledger_locator_prefix() -> &'static [u8] {
    &[LEDGER, b'v']
}

/// The part of `key` after `prefix`.
pub fn suffix<'k>(key: &'k [u8], prefix: &[u8]) -> &'k [u8] {
    key.get(prefix.len()..).unwrap_or_default()
}

/// The record a key belongs to, for error messages: `kv:namespace/key` or `json:space/id`.
pub fn describe(key: &[u8]) -> String {
    let Some((&tag, rest)) = key.split_first() else {
        return "an empty key".to_owned();
    };
    let Some(split) = rest.iter().position(|&b| b == 0) else {
        return hex::encode(key);
    };
    let (name, record) = (String::from_utf8_lossy(&rest[..split]), &rest[split + 1..]);
    match tag {
        KV => format!("kv:{name}/{}", String::from_utf8_lossy(record)),
        DOC => format!("json:{name}/{}", String::from_utf8_lossy(record)),
        _ => hex::encode(key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_values_keep_their_order_and_never_prefix_each_other() {
        let values = ["", "\0", "\0\0", "a", "a\0", "a\0b", "ab", "b"];
        let mut keys: Vec<Vec<u8>> = values.iter().map(|v| index_value("s", 1, v)).collect();
        let sorted = keys.clone();
        keys.sort();
        assert_eq!(keys, sorted, "escaped values sort like the values");
        for (i, a) in sorted.iter().enumerate() {
            for b in &sorted[i + 1..] {
                assert!(!b.starts_with(a), "{a:?} is a prefix of {b:?}");
            }
        }
        let entry = index_entry("s", 1, "a\0b", "id-7");
        assert_eq!(suffix(&entry, &index_value("s", 1, "a\0b")), b"id-7");
    }

    #[test]
    fn prefix_ends_bound_every_key_of_the_prefix() {
        assert_eq!(prefix_end(b"ab"), Some(b"ac".to_vec()));
        assert_eq!(prefix_end(&[1, 0xFF, 0xFF]), Some(vec![2]));
        assert_eq!(prefix_end(&[0xFF]), None);
        let prefix = collection(KV, "ns");
        let end = prefix_end(&prefix).unwrap();
        assert!(kv("ns", &[0xFF; 8]).as_ref() < end.as_slice());
        assert!(
            kv("ns2", b"").as_ref() >= end.as_slice(),
            "a longer name is another collection"
        );
    }

    #[test]
    fn locators_order_newest_first() {
        assert!(ledger_locator(10) < ledger_locator(9));
        assert_eq!(describe(&kv("ns", b"k")), "kv:ns/k");
        assert_eq!(describe(&doc("users", "u1")), "json:users/u1");
    }

    #[test]
    fn names_are_validated() {
        assert!(validate_name("space", "users").is_ok());
        assert!(validate_name("space", "").is_err());
        assert!(validate_name("space", "a\0b").is_err());
        assert!(validate_name("space", &"x".repeat(256)).is_err());
    }
}
