//! Per-collection vocabularies (terms and how many documents hold each, plus dictionary
//! words) for suggestions, fuzzy and phonetic search.
//!
//! A vocabulary is built from the postings the first time a collection needs one, then kept
//! exact by the term counts every committed write reports ([`Vocabularies::apply`]). Builds and
//! updates take the same lock, so a commit that lands during a build is applied after it.

use std::collections::{BTreeMap, HashMap};
use std::ops::Bound;

use lilia_core::{PhoneticVocabulary, TermTrie};
use lilia_engine::Db;
use parking_lot::Mutex;

use super::store::{decode_count, dictionary_base, postings_base};
use crate::error::{corruption, Result};
use crate::keys;

/// The terms of one collection.
#[derive(Debug, Default)]
pub struct Vocabulary {
    pub trie: TermTrie,
    pub phonetic: PhoneticVocabulary,
    /// Sequence of the snapshot the vocabulary was built from: it already counts every commit
    /// up to there.
    built_at: u64,
}

impl Vocabulary {
    fn add(&mut self, term: &str, delta: i64) {
        let current = i64::from(self.trie.doc_count(term).unwrap_or(0));
        let next = u32::try_from((current + delta).max(0)).unwrap_or(u32::MAX);
        if current > 0 {
            self.trie.remove(term);
        }
        if next > 0 {
            self.trie.insert(term, next);
            self.phonetic.insert(term, next);
        }
    }
}

/// Vocabularies of the collections used so far.
#[derive(Debug, Default)]
pub struct Vocabularies {
    loaded: Mutex<HashMap<String, Vocabulary>>,
}

/// Term count changes of one commit: `(collection, term, change)`.
pub type Deltas = Vec<(String, String, i64)>;

fn build(engine: &Db, collection: &str) -> Result<Vocabulary> {
    let snapshot = engine.snapshot();
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    let base = postings_base(collection);
    let end = keys::prefix_end(&base).unwrap_or_default();
    for entry in engine.range_at(&snapshot, Bound::Included(&base), Bound::Excluded(&end))? {
        let entry = entry?;
        let rest = keys::suffix(&entry.key, &base);
        let split = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| corruption("posting key without a document"))?;
        let term = std::str::from_utf8(&rest[..split])
            .map_err(|_| corruption("posting key is not UTF-8"))?;
        *counts.entry(term.to_owned()).or_default() += 1;
    }
    let base = dictionary_base(collection);
    let end = keys::prefix_end(&base).unwrap_or_default();
    for entry in engine.range_at(&snapshot, Bound::Included(&base), Bound::Excluded(&end))? {
        let entry = entry?;
        let term = std::str::from_utf8(keys::suffix(&entry.key, &base))
            .map_err(|_| corruption("dictionary key is not UTF-8"))?;
        *counts.entry(term.to_owned()).or_default() += i64::from(decode_count(&entry.value)?);
    }
    let mut vocabulary = Vocabulary {
        built_at: snapshot.sequence(),
        ..Vocabulary::default()
    };
    for (term, count) in counts {
        vocabulary.add(&term, count);
    }
    Ok(vocabulary)
}

impl Vocabularies {
    /// Runs `f` on the vocabulary of `collection`, building it first if needed.
    ///
    /// # Errors
    ///
    /// Read errors of the build.
    pub fn with<R>(
        &self,
        engine: &Db,
        collection: &str,
        f: impl FnOnce(&Vocabulary) -> R,
    ) -> Result<R> {
        let mut loaded = self.loaded.lock();
        if !loaded.contains_key(collection) {
            let vocabulary = build(engine, collection)?;
            loaded.insert(collection.to_owned(), vocabulary);
        }
        let vocabulary = loaded
            .get(collection)
            .ok_or_else(|| corruption("a vocabulary was dropped while in use"))?;
        Ok(f(vocabulary))
    }

    /// Forgets the vocabulary of `collection`, after a write that reported no term counts (an
    /// undo): it is built again when next needed.
    pub fn evict(&self, collection: &str) {
        self.loaded.lock().remove(collection);
    }

    /// Applies the term counts of a write committed at sequence `first` to the vocabularies
    /// built before it.
    pub fn apply(&self, deltas: &Deltas, first: u64) {
        if deltas.is_empty() {
            return;
        }
        let mut loaded = self.loaded.lock();
        for (collection, term, delta) in deltas {
            if let Some(vocabulary) = loaded.get_mut(collection).filter(|v| v.built_at < first) {
                vocabulary.add(term, *delta);
            }
        }
    }
}
