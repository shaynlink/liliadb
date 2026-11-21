//! Keys and records of the full-text keyspaces, and the changes each write makes to them.
//!
//! The changes are computed once, from a function reading the current record of a key, so the
//! write planner and a transaction's view apply exactly the same effect.

use std::collections::{BTreeMap, BTreeSet};

use bytes::Bytes;

use super::text;
use crate::error::{corruption, invalid, Result};
use crate::keys;

const FORMAT: u8 = 1;
/// Shards of the per-collection document and token counts, so concurrent indexers rarely
/// update the same counter.
pub const STATS_SHARDS: u8 = 64;

/// Reads the record currently at a key.
pub type Read<'r> = dyn FnMut(&Bytes) -> Result<Option<Bytes>> + 'r;

fn with_name(tag: u8, collection: &str, rest: &[&[u8]]) -> Vec<u8> {
    let mut key = keys::collection(tag, collection);
    for (i, part) in rest.iter().enumerate() {
        if i > 0 {
            key.push(0);
        }
        key.extend_from_slice(part);
    }
    key
}

pub fn doc_key(collection: &str, doc_id: &str) -> Bytes {
    with_name(keys::FTS_DOC, collection, &[doc_id.as_bytes()]).into()
}

/// `tag collection\0`: the prefix of every posting of a collection.
pub fn postings_base(collection: &str) -> Vec<u8> {
    keys::collection(keys::FTS_POSTING, collection)
}

/// Prefix of the postings of `term`.
pub fn term_prefix(collection: &str, term: &str) -> Vec<u8> {
    let mut key = postings_base(collection);
    key.extend_from_slice(term.as_bytes());
    key.push(0);
    key
}

fn posting_key(collection: &str, term: &str, doc_id: &str) -> Bytes {
    let mut key = term_prefix(collection, term);
    key.extend_from_slice(doc_id.as_bytes());
    key.into()
}

pub fn stats_base(collection: &str) -> Vec<u8> {
    keys::collection(keys::FTS_STATS, collection)
}

fn stats_key(collection: &str, doc_id: &str) -> Bytes {
    // FNV-1a: stable across versions, as the shard of a document is stored implicitly.
    let hash = doc_id.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    let mut key = stats_base(collection);
    key.push(u8::try_from(hash % u64::from(STATS_SHARDS)).unwrap_or_default());
    key.into()
}

pub fn dictionary_base(collection: &str) -> Vec<u8> {
    keys::collection(keys::FTS_DICTIONARY, collection)
}

pub fn dictionary_key(collection: &str, term: &str) -> Bytes {
    with_name(keys::FTS_DICTIONARY, collection, &[term.as_bytes()]).into()
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push(u8::try_from(value & 0x7F).unwrap_or_default() | 0x80);
        value >>= 7;
    }
    out.push(u8::try_from(value).unwrap_or_default());
}

struct Reader<'a> {
    data: &'a [u8],
}

impl Reader<'_> {
    fn varint(&mut self) -> Result<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let (&byte, rest) = self
                .data
                .split_first()
                .ok_or_else(|| corruption("full-text record: truncated"))?;
            self.data = rest;
            value |= u64::from(byte & 0x7F) << shift;
            if byte < 0x80 {
                return Ok(value);
            }
        }
        Err(corruption("full-text record: varint too long"))
    }

    fn usize(&mut self) -> Result<usize> {
        usize::try_from(self.varint()?)
            .map_err(|_| corruption("full-text record: length overflows"))
    }

    fn text(&mut self) -> Result<String> {
        let len = self.usize()?;
        if self.data.len() < len {
            return Err(corruption("full-text record: truncated text"));
        }
        let (text, rest) = self.data.split_at(len);
        self.data = rest;
        String::from_utf8(text.to_vec())
            .map_err(|_| corruption("full-text record: text is not UTF-8"))
    }

    fn format(&mut self) -> Result<()> {
        match self.data.split_first() {
            Some((&FORMAT, rest)) => {
                self.data = rest;
                Ok(())
            }
            _ => Err(corruption("full-text record with an unknown format")),
        }
    }
}

/// An indexed document: its length in tokens, the text it was indexed from (for snippets), and
/// its distinct terms (so a deletion removes exactly the postings it wrote).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocRecord {
    pub length: u64,
    pub content: String,
    pub terms: Vec<String>,
}

impl DocRecord {
    pub fn encode(&self) -> Bytes {
        let mut out = vec![FORMAT];
        put_varint(&mut out, self.length);
        put_varint(&mut out, self.content.len() as u64);
        out.extend_from_slice(self.content.as_bytes());
        put_varint(&mut out, self.terms.len() as u64);
        for term in &self.terms {
            put_varint(&mut out, term.len() as u64);
            out.extend_from_slice(term.as_bytes());
        }
        out.into()
    }

    /// # Errors
    ///
    /// [`crate::Error::Corruption`] for a record that does not decode.
    pub fn decode(raw: &[u8]) -> Result<Self> {
        let mut reader = Reader { data: raw };
        reader.format()?;
        let length = reader.varint()?;
        let content = reader.text()?;
        let count = reader.usize()?;
        let terms = (0..count).map(|_| reader.text()).collect::<Result<_>>()?;
        Ok(Self {
            length,
            content,
            terms,
        })
    }
}

/// Positions of a term in a document, and the document's length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Posting {
    pub length: u64,
    pub positions: Vec<u32>,
}

impl Posting {
    fn encode(&self) -> Bytes {
        let mut out = vec![FORMAT];
        put_varint(&mut out, self.length);
        put_varint(&mut out, self.positions.len() as u64);
        let mut previous = 0;
        for &position in &self.positions {
            put_varint(&mut out, u64::from(position - previous));
            previous = position;
        }
        out.into()
    }

    /// # Errors
    ///
    /// [`crate::Error::Corruption`] for a posting that does not decode.
    pub fn decode(raw: &[u8]) -> Result<Self> {
        let mut reader = Reader { data: raw };
        reader.format()?;
        let length = reader.varint()?;
        let count = reader.usize()?;
        let mut positions = Vec::with_capacity(count.min(raw.len()));
        let mut previous = 0u32;
        for _ in 0..count {
            let delta = u32::try_from(reader.varint()?)
                .map_err(|_| corruption("posting position overflows"))?;
            previous += delta;
            positions.push(previous);
        }
        Ok(Self { length, positions })
    }
}

/// Document and token counts of one shard of a collection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub docs: u64,
    pub tokens: u64,
}

impl Stats {
    fn encode(self) -> Bytes {
        let mut out = vec![FORMAT];
        out.extend_from_slice(&self.docs.to_le_bytes());
        out.extend_from_slice(&self.tokens.to_le_bytes());
        out.into()
    }

    /// # Errors
    ///
    /// [`crate::Error::Corruption`] for counts that do not decode.
    pub fn decode(raw: &[u8]) -> Result<Self> {
        match raw {
            [FORMAT, rest @ ..] if rest.len() == 16 => {
                let (docs, tokens) = rest.split_at(8);
                let word = |b: &[u8]| u64::from_le_bytes(b.try_into().unwrap_or_default());
                Ok(Self {
                    docs: word(docs),
                    tokens: word(tokens),
                })
            }
            _ => Err(corruption("full-text counts with an unknown format")),
        }
    }
}

/// One key a write changes; `record` marks the key the ledger attests.
#[derive(Debug, Clone)]
pub struct Change {
    pub key: Bytes,
    pub value: Option<Bytes>,
    pub record: bool,
}

/// Everything a full-text write does.
#[derive(Debug, Default)]
pub struct Effect {
    pub changes: Vec<Change>,
    pub deleted: bool,
    /// Changes of the number of documents (or dictionary loads) per term.
    pub vocabulary: Vec<(String, i64)>,
}

impl Effect {
    fn set(&mut self, key: Bytes, value: Option<Bytes>, record: bool) {
        self.changes.push(Change { key, value, record });
    }
}

fn adjust_stats(
    read: &mut Read<'_>,
    effect: &mut Effect,
    collection: &str,
    doc_id: &str,
    docs: i64,
    tokens: i64,
) -> Result<()> {
    let key = stats_key(collection, doc_id);
    let mut stats = read(&key)?
        .map(|raw| Stats::decode(&raw))
        .transpose()?
        .unwrap_or_default();
    stats.docs = stats.docs.saturating_add_signed(docs);
    stats.tokens = stats.tokens.saturating_add_signed(tokens);
    effect.set(key, Some(stats.encode()), false);
    Ok(())
}

fn signed(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Indexes `content` as document `doc_id`, replacing what it was indexed from before. Content
/// without any token removes the document.
///
/// # Errors
///
/// Records that do not decode, read errors.
pub fn index(read: &mut Read<'_>, collection: &str, doc_id: &str, content: &str) -> Result<Effect> {
    let prepared = text::prepare(content);
    let tokens = text::tokens(&prepared);
    if tokens.is_empty() {
        return delete(read, collection, doc_id);
    }
    let mut positions: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for (i, token) in tokens.iter().enumerate() {
        let position =
            u32::try_from(i).map_err(|_| invalid("a document holds at most 2^32 tokens"))?;
        positions
            .entry(token.term.clone())
            .or_default()
            .push(position);
    }
    let length = tokens.len() as u64;
    let key = doc_key(collection, doc_id);
    let old = read(&key)?.map(|raw| DocRecord::decode(&raw)).transpose()?;
    let old_terms: BTreeSet<String> = old
        .as_ref()
        .map_or_default(|o| o.terms.iter().cloned().collect());
    let mut effect = Effect::default();
    for term in old_terms.iter().filter(|t| !positions.contains_key(*t)) {
        effect.set(posting_key(collection, term, doc_id), None, false);
        effect.vocabulary.push((term.clone(), -1));
    }
    for (term, at) in &positions {
        let posting = Posting {
            length,
            positions: at.clone(),
        };
        effect.set(
            posting_key(collection, term, doc_id),
            Some(posting.encode()),
            false,
        );
        if !old_terms.contains(term) {
            effect.vocabulary.push((term.clone(), 1));
        }
    }
    let record = DocRecord {
        length,
        content: prepared,
        terms: positions.into_keys().collect(),
    };
    effect.set(key, Some(record.encode()), true);
    let (docs, old_length) = old.map_or((1, 0), |o| (0, o.length));
    adjust_stats(
        read,
        &mut effect,
        collection,
        doc_id,
        docs,
        signed(length) - signed(old_length),
    )?;
    Ok(effect)
}

/// Removes document `doc_id` from the index.
///
/// # Errors
///
/// Records that do not decode, read errors.
pub fn delete(read: &mut Read<'_>, collection: &str, doc_id: &str) -> Result<Effect> {
    let key = doc_key(collection, doc_id);
    let Some(old) = read(&key)?.map(|raw| DocRecord::decode(&raw)).transpose()? else {
        return Ok(Effect::default());
    };
    let mut effect = Effect {
        deleted: true,
        ..Effect::default()
    };
    for term in &old.terms {
        effect.set(posting_key(collection, term, doc_id), None, false);
        effect.vocabulary.push((term.clone(), -1));
    }
    effect.set(key, None, true);
    adjust_stats(
        read,
        &mut effect,
        collection,
        doc_id,
        -1,
        -signed(old.length),
    )?;
    Ok(effect)
}

/// Adds `words` to the collection's dictionary: terms offered by suggestions and fuzzy search
/// even when no document holds them. Each load of a word counts once more.
///
/// # Errors
///
/// Records that do not decode, read errors.
pub fn dictionary(read: &mut Read<'_>, collection: &str, words: &[String]) -> Result<Effect> {
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for word in words {
        let term = word.trim().to_lowercase();
        if !term.is_empty() && !term.contains(char::is_control) {
            *counts.entry(term).or_default() += 1;
        }
    }
    let mut effect = Effect::default();
    for (term, added) in counts {
        let key = dictionary_key(collection, &term);
        let current = read(&key)?
            .map(|raw| decode_count(&raw))
            .transpose()?
            .unwrap_or_default();
        effect.set(key, Some(encode_count(current.saturating_add(added))), true);
        effect.vocabulary.push((term, i64::from(added)));
    }
    Ok(effect)
}

pub fn encode_count(count: u32) -> Bytes {
    let mut out = vec![FORMAT];
    out.extend_from_slice(&count.to_le_bytes());
    out.into()
}

/// # Errors
///
/// [`crate::Error::Corruption`] for a count that does not decode.
pub fn decode_count(raw: &[u8]) -> Result<u32> {
    match raw {
        [FORMAT, a, b, c, d] => Ok(u32::from_le_bytes([*a, *b, *c, *d])),
        _ => Err(corruption("dictionary entry with an unknown format")),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn records_round_trip() {
        let record = DocRecord {
            length: 3,
            content: "a b a".into(),
            terms: vec!["a".into(), "b".into()],
        };
        assert_eq!(DocRecord::decode(&record.encode()).unwrap(), record);
        let posting = Posting {
            length: 9,
            positions: vec![0, 4, 300],
        };
        assert_eq!(Posting::decode(&posting.encode()).unwrap(), posting);
        let stats = Stats {
            docs: 2,
            tokens: 70,
        };
        assert_eq!(Stats::decode(&stats.encode()).unwrap(), stats);
        assert_eq!(decode_count(&encode_count(7)).unwrap(), 7);
        assert!(DocRecord::decode(&[2]).is_err());
    }

    #[test]
    fn reindexing_replaces_postings_and_keeps_counts() {
        let mut store: HashMap<Bytes, Bytes> = HashMap::new();
        let apply = |effect: Effect, store: &mut HashMap<Bytes, Bytes>| {
            for change in effect.changes {
                match change.value {
                    Some(v) => store.insert(change.key, v),
                    None => store.remove(&change.key),
                };
            }
            effect.vocabulary
        };
        let first = index(
            &mut |k| Ok(store.get(k).cloned()),
            "c",
            "d1",
            "red fox, red",
        )
        .unwrap();
        assert_eq!(first.vocabulary, [("fox".into(), 1), ("red".into(), 1)]);
        apply(first, &mut store);
        let second = index(&mut |k| Ok(store.get(k).cloned()), "c", "d1", "blue fox").unwrap();
        assert_eq!(second.vocabulary, [("red".into(), -1), ("blue".into(), 1)]);
        apply(second, &mut store);
        assert!(!store.contains_key(&posting_key("c", "red", "d1")));
        let fox = Posting::decode(&store[&posting_key("c", "fox", "d1")]).unwrap();
        assert_eq!(
            fox,
            Posting {
                length: 2,
                positions: vec![1]
            }
        );
        let stats: Vec<Stats> = store
            .iter()
            .filter(|(k, _)| k.starts_with(&stats_base("c")))
            .map(|(_, v)| Stats::decode(v).unwrap())
            .collect();
        assert_eq!(stats, [Stats { docs: 1, tokens: 2 }]);
        let gone = delete(&mut |k| Ok(store.get(k).cloned()), "c", "d1").unwrap();
        assert!(gone.deleted);
        apply(gone, &mut store);
        assert_eq!(store.len(), 1, "only the (now zero) counts remain");
        assert!(
            !delete(&mut |k| Ok(store.get(k).cloned()), "c", "d1")
                .unwrap()
                .deleted
        );
    }
}
