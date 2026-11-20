//! Phonetic vocabulary dictionary mapping Soundex and Double Metaphone codes to terms.

use std::collections::{HashMap, HashSet};

use super::double_metaphone::{double_metaphone, DoubleMetaphoneResult};
use super::soundex::soundex;

/// Candidate term matched phonetically against a query word.
#[derive(Debug, Clone, PartialEq)]
pub struct PhoneticCandidate {
    pub term: String,
    pub doc_count: u32,
    pub dm_match: bool,
    pub soundex_match: bool,
    pub similarity: f64,
}

/// In-memory vocabulary holding term frequencies and phonetic index maps for fast lookups.
#[derive(Debug, Clone, Default)]
pub struct PhoneticVocabulary {
    terms: HashMap<String, u32>,
    soundex_map: HashMap<String, HashSet<String>>,
    dm_map: HashMap<String, HashSet<String>>,
}

impl PhoneticVocabulary {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or updates a term in the vocabulary with its document occurrence count.
    pub fn insert(&mut self, term: &str, doc_count: u32) {
        let clean = term.trim().to_lowercase();
        if clean.is_empty() {
            return;
        }

        self.terms
            .entry(clean.clone())
            .and_modify(|c| *c = (*c).max(doc_count))
            .or_insert(doc_count);

        // Soundex
        let sx = soundex(&clean);
        if !sx.is_empty() {
            self.soundex_map
                .entry(sx)
                .or_default()
                .insert(clean.clone());
        }

        // Double Metaphone
        let dm: DoubleMetaphoneResult = double_metaphone(&clean);
        if !dm.primary.is_empty() {
            self.dm_map
                .entry(dm.primary.clone())
                .or_default()
                .insert(clean.clone());
        }
        if let Some(ref alt) = dm.alternate {
            if !alt.is_empty() {
                self.dm_map.entry(alt.clone()).or_default().insert(clean);
            }
        }
    }

    /// Finds candidate words from the vocabulary matching the phonetic codes of `word`.
    #[must_use]
    pub fn find_candidates(&self, word: &str, limit: usize) -> Vec<PhoneticCandidate> {
        let clean = word.trim().to_lowercase();
        if clean.is_empty() {
            return Vec::new();
        }

        let sx = soundex(&clean);
        let dm = double_metaphone(&clean);

        let mut matched: HashMap<String, (bool, bool)> = HashMap::new();

        // 1. Double Metaphone lookup
        if !dm.primary.is_empty() {
            if let Some(set) = self.dm_map.get(&dm.primary) {
                for t in set {
                    matched.entry(t.clone()).or_insert((false, false)).0 = true;
                }
            }
        }
        if let Some(ref alt) = dm.alternate {
            if !alt.is_empty() {
                if let Some(set) = self.dm_map.get(alt) {
                    for t in set {
                        matched.entry(t.clone()).or_insert((false, false)).0 = true;
                    }
                }
            }
        }

        // 2. Soundex lookup
        if !sx.is_empty() {
            if let Some(set) = self.soundex_map.get(&sx) {
                for t in set {
                    matched.entry(t.clone()).or_insert((false, false)).1 = true;
                }
            }
        }

        let mut candidates = Vec::with_capacity(matched.len());
        for (term, (dm_match, sx_match)) in matched {
            let doc_count = self.terms.get(&term).copied().unwrap_or(1);
            let similarity = match (dm_match, sx_match) {
                (true, true) => 1.0,
                (true, false) => 0.85,
                (false, true) => 0.75,
                (false, false) => 0.5,
            };

            candidates.push(PhoneticCandidate {
                term,
                doc_count,
                dm_match,
                soundex_match: sx_match,
                similarity,
            });
        }

        // Sort by similarity desc, then doc_count desc
        candidates.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.doc_count.cmp(&a.doc_count))
        });

        if limit > 0 && candidates.len() > limit {
            candidates.truncate(limit);
        }

        candidates
    }
}
