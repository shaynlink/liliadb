//! The full-text handle of a session or transaction.

use std::collections::{BTreeMap, HashMap};

use lilia_core::{
    double_metaphone, soundex, FtsFuzzyHit, FtsHit, FtsPhoneticHit, FtsSuggestion,
    LevenshteinAutomaton,
};

use super::search::{self, Match, Node, Searcher};
use super::store::{doc_key, DocRecord};
use super::{text, FtsWrite};
use crate::batch::{Outcome, Write};
use crate::context::{guard, Context};
use crate::error::{corruption, Result};
use crate::keys::{self, validate_name};
use crate::read::Source;
use crate::record::decode_doc;

/// Full-text collection `name` of a session or transaction. Suggestions, fuzzy and phonetic
/// search expand words from the committed vocabulary; a transaction's own documents are still
/// searched.
#[derive(Debug)]
pub struct Fts<'c, C: Context> {
    context: &'c C,
    name: String,
}

/// A ranked match, before its snippet.
struct Candidate {
    doc: String,
    matched: Match,
    rank: f64,
}

impl<'c, C: Context> Fts<'c, C> {
    pub(crate) fn new(context: &'c C, name: &str) -> Self {
        Self {
            context,
            name: name.to_owned(),
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    fn submit(&self, write: FtsWrite) -> Result<Outcome> {
        let mut outcomes = self.context.submit(vec![Write::Fts(write)])?;
        outcomes
            .pop()
            .ok_or_else(|| corruption("a full-text write produced no outcome"))
    }

    /// Indexes `content` as document `doc_id`, replacing its previous content. Content without
    /// any word removes the document.
    ///
    /// # Errors
    ///
    /// Invalid names; as [`crate::Session::commit`].
    pub fn index(&self, doc_id: &str, content: &str) -> Result<Outcome> {
        self.submit(FtsWrite::Index {
            collection: self.name.clone(),
            doc_id: doc_id.to_owned(),
            content: content.to_owned(),
        })
    }

    /// Removes document `doc_id`; `true` if it was indexed.
    ///
    /// # Errors
    ///
    /// As [`Fts::index`].
    pub fn delete(&self, doc_id: &str) -> Result<bool> {
        Ok(self
            .submit(FtsWrite::Delete {
                collection: self.name.clone(),
                doc_id: doc_id.to_owned(),
            })?
            .deleted)
    }

    /// Adds dictionary words, offered by suggestions and fuzzy search even if no document
    /// holds them.
    ///
    /// # Errors
    ///
    /// As [`Fts::index`].
    pub fn load_dictionary(&self, words: &[&str]) -> Result<()> {
        let words = words.iter().map(|w| (*w).to_owned()).collect();
        self.submit(FtsWrite::LoadDictionary {
            collection: self.name.clone(),
            words,
        })
        .map(drop)
    }

    /// Documents matching `query` (see [`crate::fts`] for the syntax), best first. `rank` is the
    /// negated BM25 score: lower is better.
    ///
    /// # Errors
    ///
    /// Invalid name, read errors.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<FtsHit>> {
        validate_name("collection", &self.name)?;
        let Some(node) = search::parse(query) else {
            return Ok(Vec::new());
        };
        let candidates = self.candidates(&node, |m| -m.score)?;
        self.finish(candidates, limit, |doc, _, snippet, rank| FtsHit {
            collection: self.name.clone(),
            doc_id: doc.to_owned(),
            snippet,
            rank,
        })
    }

    /// Vocabulary terms within `max_distance` edits (at most 1 for prefixes of 2 characters or
    /// less, at most 2 otherwise) of a prefix of `prefix`, best first.
    ///
    /// # Errors
    ///
    /// Invalid name, read errors building the vocabulary.
    pub fn suggest(
        &self,
        prefix: &str,
        max_distance: u8,
        limit: usize,
    ) -> Result<Vec<FtsSuggestion>> {
        validate_name("collection", &self.name)?;
        let clean = prefix.trim().to_lowercase();
        if clean.is_empty() {
            return Ok(Vec::new());
        }
        let distance = if clean.chars().count() <= 2 {
            max_distance.min(1)
        } else {
            max_distance.min(2)
        };
        let automaton = LevenshteinAutomaton::for_prefix(&clean, distance);
        self.context.database().vocabularies().with(
            self.context.database().engine(),
            &self.name,
            |v| v.trie.search_automaton(&automaton, limit),
        )
    }

    /// Search tolerant to typos: each word missing from the vocabulary is replaced by its three
    /// closest terms within `max_distance` edits (at most 2), or by phonetic matches. Hits are
    /// ordered by the edit distance of the terms they matched, then by rank.
    ///
    /// # Errors
    ///
    /// As [`Fts::suggest`].
    pub fn fuzzy_search(
        &self,
        query: &str,
        max_distance: u8,
        limit: usize,
    ) -> Result<Vec<FtsFuzzyHit>> {
        validate_name("collection", &self.name)?;
        let words = words(query);
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let distance = max_distance.min(2);
        let mut distances: HashMap<String, u8> = HashMap::new();
        let clauses = self.context.database().vocabularies().with(
            self.context.database().engine(),
            &self.name,
            |v| {
                words
                    .iter()
                    .map(|word| {
                        let mut alternatives = vec![word.clone()];
                        if v.trie.contains(word) || distance == 0 {
                            distances.insert(word.clone(), 0);
                        } else {
                            let automaton = LevenshteinAutomaton::for_exact(word, distance);
                            let mut close: Vec<(String, u8)> = v
                                .trie
                                .search_automaton(&automaton, 3)
                                .into_iter()
                                .map(|s| (s.term, s.distance))
                                .collect();
                            if close.is_empty() {
                                close = v
                                    .phonetic
                                    .find_candidates(word, 3)
                                    .into_iter()
                                    .map(|c| (c.term, 1))
                                    .collect();
                            }
                            for (term, d) in close {
                                let entry = distances.entry(term.clone()).or_insert(d);
                                *entry = (*entry).min(d);
                                alternatives.push(term);
                            }
                        }
                        alternatives_node(alternatives)
                    })
                    .collect::<Vec<_>>()
            },
        )?;
        let node = Node::And(clauses);
        let distance_of = |m: &Match| {
            m.terms
                .iter()
                .filter_map(|t| distances.get(t))
                .copied()
                .min()
                .unwrap_or(0)
        };
        let mut candidates = self.candidates(&node, |m| {
            -m.score / (1.0 + f64::from(distance_of(m)) * 0.5)
        })?;
        candidates.sort_by(|a, b| {
            distance_of(&a.matched)
                .cmp(&distance_of(&b.matched))
                .then_with(|| a.rank.total_cmp(&b.rank))
        });
        self.finish(candidates, limit, |doc, matched, snippet, rank| {
            FtsFuzzyHit {
                collection: self.name.clone(),
                doc_id: doc.to_owned(),
                snippet,
                rank,
                edit_distance: distance_of(matched),
                matched_terms: matched
                    .terms
                    .iter()
                    .filter(|t| distances.contains_key(*t))
                    .cloned()
                    .collect(),
            }
        })
    }

    /// Search by sound: each word also matches the vocabulary terms sharing its Double
    /// Metaphone or Soundex code (`Smith` finds `Smyth`). Ranks favour closer sounds.
    ///
    /// # Errors
    ///
    /// As [`Fts::suggest`].
    pub fn phonetic_search(&self, query: &str, limit: usize) -> Result<Vec<FtsPhoneticHit>> {
        validate_name("collection", &self.name)?;
        let words = words(query);
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let mut codes: Vec<String> = Vec::new();
        for word in &words {
            let metaphone = double_metaphone(word);
            for code in std::iter::once(metaphone.primary)
                .chain(metaphone.alternate)
                .chain(std::iter::once(soundex(word)))
            {
                if !code.is_empty() && !codes.contains(&code) {
                    codes.push(code);
                }
            }
        }
        let mut similarity: BTreeMap<String, f64> = BTreeMap::new();
        let clauses = self.context.database().vocabularies().with(
            self.context.database().engine(),
            &self.name,
            |v| {
                words
                    .iter()
                    .map(|word| {
                        similarity.insert(word.clone(), 1.0);
                        let mut alternatives = vec![word.clone()];
                        for candidate in v.phonetic.find_candidates(word, 5) {
                            let entry = similarity
                                .entry(candidate.term.clone())
                                .or_insert(candidate.similarity);
                            *entry = entry.max(candidate.similarity);
                            alternatives.push(candidate.term);
                        }
                        alternatives_node(alternatives)
                    })
                    .collect::<Vec<_>>()
            },
        )?;
        let closeness = |m: &Match| {
            m.terms
                .iter()
                .filter_map(|t| similarity.get(t))
                .copied()
                .fold(0.5_f64, f64::max)
        };
        let mut candidates = self.candidates(&Node::And(clauses), |m| -m.score * closeness(m))?;
        candidates.sort_by(|a, b| a.rank.total_cmp(&b.rank));
        self.finish(candidates, limit, |doc, matched, snippet, rank| {
            FtsPhoneticHit {
                collection: self.name.clone(),
                doc_id: doc.to_owned(),
                snippet,
                rank,
                matched_terms: matched
                    .terms
                    .iter()
                    .filter(|t| similarity.contains_key(*t))
                    .cloned()
                    .collect(),
                phonetic_codes: codes.clone(),
            }
        })
    }

    /// Every document matching `node`, ranked by `rank` (lower first).
    fn candidates(&self, node: &Node, rank: impl Fn(&Match) -> f64) -> Result<Vec<Candidate>> {
        let reader = self.context.reader(true);
        let searcher = Searcher::new(&reader, &self.name)?;
        if searcher.is_empty() {
            return Ok(Vec::new());
        }
        let mut candidates: Vec<Candidate> = search::ranked(searcher.eval(node)?)
            .into_iter()
            .map(|(doc, matched)| {
                let r = rank(&matched);
                Candidate {
                    doc,
                    matched,
                    rank: r,
                }
            })
            .collect();
        candidates.sort_by(|a, b| a.rank.total_cmp(&b.rank).then_with(|| a.doc.cmp(&b.doc)));
        Ok(candidates)
    }

    /// The first `limit` candidates the caller may read, with their snippets.
    fn finish<T>(
        &self,
        candidates: Vec<Candidate>,
        limit: usize,
        hit: impl Fn(&str, &Match, String, f64) -> T,
    ) -> Result<Vec<T>> {
        let reader = self.context.reader(true);
        let guard = guard(self.context, &self.name);
        let mut out = Vec::new();
        for candidate in candidates {
            if out.len() >= limit {
                break;
            }
            let Some(item) = reader.get(&doc_key(&self.name, &candidate.doc))? else {
                continue;
            };
            let record = DocRecord::decode(&item.value)?;
            let snippet = search::snippet(&record.content, &candidate.matched.terms);
            let document = if guard.checks_reads() {
                reader
                    .get(&keys::doc(&self.name, &candidate.doc))?
                    .map(|item| decode_doc(&item.value))
                    .transpose()?
            } else {
                None
            };
            if guard.related_hit(document.as_ref(), || {
                serde_json::json!({ "collection": self.name, "doc_id": candidate.doc, "snippet": snippet })
            }) {
                out.push(hit(
                    &candidate.doc,
                    &candidate.matched,
                    snippet,
                    candidate.rank,
                ));
            }
        }
        Ok(out)
    }
}

/// Distinct words of a query, as terms.
fn words(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for token in text::tokens(query) {
        if !out.contains(&token.term) {
            out.push(token.term);
        }
    }
    out
}

fn alternatives_node(mut terms: Vec<String>) -> Node {
    terms.dedup();
    if terms.len() == 1 {
        Node::Terms(terms)
    } else {
        Node::Or(terms.into_iter().map(|t| Node::Terms(vec![t])).collect())
    }
}
