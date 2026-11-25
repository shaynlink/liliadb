//! Queries: parsing, evaluation over the postings, BM25 ranking, snippets.
//!
//! Query syntax (as in v0.1's FTS5 queries): every word must match; `"quoted words"` is a phrase;
//! `word*` matches every term starting with `word`; `OR`, `AND` and `NOT` (upper case) combine
//! clauses, and parentheses group them. A word that tokenizes into several tokens (`e-mail`) is
//! a phrase.

use std::collections::{BTreeSet, HashMap};
use std::ops::Bound;

use super::store::{postings_base, stats_base, term_prefix, Posting, Stats};
use super::text;
use crate::error::{corruption, Result};
use crate::keys;
use crate::read::Source;

/// A parsed query.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    /// One term, or a phrase of consecutive terms.
    Terms(Vec<String>),
    Prefix(String),
    And(Vec<Node>),
    Or(Vec<Node>),
    /// Matches of the first not matched by the second.
    Not(Box<Node>, Box<Node>),
}

#[derive(Debug, Clone, PartialEq)]
enum Lexeme {
    Open,
    Close,
    Phrase(String),
    Word(String),
    And,
    Or,
    Not,
}

fn lex(query: &str) -> Vec<Lexeme> {
    let mut out = Vec::new();
    let mut chars = query.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            '(' => {
                chars.next();
                out.push(Lexeme::Open);
            }
            ')' => {
                chars.next();
                out.push(Lexeme::Close);
            }
            '"' => {
                chars.next();
                let phrase: String = chars.by_ref().take_while(|&c| c != '"').collect();
                out.push(Lexeme::Phrase(phrase));
            }
            c if c.is_whitespace() => {
                chars.next();
            }
            _ => {
                let mut word = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || matches!(c, '(' | ')' | '"') {
                        break;
                    }
                    word.push(c);
                    chars.next();
                }
                out.push(match word.as_str() {
                    "AND" => Lexeme::And,
                    "OR" => Lexeme::Or,
                    "NOT" => Lexeme::Not,
                    _ => Lexeme::Word(word),
                });
            }
        }
    }
    out
}

/// The node of one query word: a term, a phrase, a prefix, or (for CJK) an AND of words.
fn word_node(word: &str) -> Option<Node> {
    if let Some(stem) = word.strip_suffix('*') {
        let mut sequences = text::query_word(stem);
        if let [only] = sequences.as_mut_slice() {
            if only.len() == 1 {
                return only.pop().map(Node::Prefix);
            }
        }
    }
    let mut parts: Vec<Node> = text::query_word(word)
        .into_iter()
        .map(Node::Terms)
        .collect();
    match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(Node::And(parts)),
    }
}

struct Parser {
    lexemes: Vec<Lexeme>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Lexeme> {
        self.lexemes.get(self.at)
    }

    fn or(&mut self) -> Option<Node> {
        let mut parts: Vec<Node> = self.and().into_iter().collect();
        while self.peek() == Some(&Lexeme::Or) {
            self.at += 1;
            parts.extend(self.and());
        }
        collapse(parts, Node::Or)
    }

    fn and(&mut self) -> Option<Node> {
        let mut parts: Vec<Node> = Vec::new();
        loop {
            match self.peek() {
                Some(Lexeme::And) => self.at += 1,
                Some(Lexeme::Or | Lexeme::Close) | None => break,
                Some(_) => parts.extend(self.not()),
            }
        }
        collapse(parts, Node::And)
    }

    fn not(&mut self) -> Option<Node> {
        let mut node = self.primary();
        while self.peek() == Some(&Lexeme::Not) {
            self.at += 1;
            let excluded = self.primary();
            node = match (node, excluded) {
                (Some(kept), Some(excluded)) => Some(Node::Not(Box::new(kept), Box::new(excluded))),
                (kept, None) => kept,
                (None, Some(_)) => None,
            };
        }
        node
    }

    fn primary(&mut self) -> Option<Node> {
        let lexeme = self.peek()?.clone();
        self.at += 1;
        match lexeme {
            Lexeme::Open => {
                let inner = self.or();
                if self.peek() == Some(&Lexeme::Close) {
                    self.at += 1;
                }
                inner
            }
            Lexeme::Phrase(phrase) => {
                let terms: Vec<String> = text::tokens(&text::prepare(&phrase))
                    .into_iter()
                    .map(|t| t.term)
                    .collect();
                (!terms.is_empty()).then_some(Node::Terms(terms))
            }
            Lexeme::Word(word) => word_node(&word),
            Lexeme::Close | Lexeme::And | Lexeme::Or | Lexeme::Not => None,
        }
    }
}

fn collapse(mut parts: Vec<Node>, combine: fn(Vec<Node>) -> Node) -> Option<Node> {
    match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(combine(parts)),
    }
}

/// Parses `query`; `None` when it holds no term.
pub fn parse(query: &str) -> Option<Node> {
    let mut parser = Parser {
        lexemes: lex(query),
        at: 0,
    };
    let mut parts = Vec::new();
    while parser.at < parser.lexemes.len() {
        match parser.or() {
            Some(node) => parts.push(node),
            // A stray `)` or operator: skip it.
            None => parser.at += 1,
        }
    }
    collapse(parts, Node::And)
}

/// A matching document: its BM25 score and the terms it matched.
#[derive(Debug, Clone, Default)]
pub struct Match {
    pub score: f64,
    pub terms: BTreeSet<String>,
}

const K1: f64 = 1.2;
const B: f64 = 0.75;

/// Evaluates queries over the postings of one collection.
pub struct Searcher<'s, S: Source> {
    source: &'s S,
    collection: String,
    docs: f64,
    average_length: f64,
}

impl<'s, S: Source> Searcher<'s, S> {
    /// # Errors
    ///
    /// Read errors, counts that do not decode.
    pub fn new(source: &'s S, collection: &str) -> Result<Self> {
        let base = stats_base(collection);
        let end = keys::prefix_end(&base).unwrap_or_default();
        let mut total = Stats::default();
        for item in source.range(Bound::Included(&base), Bound::Excluded(&end))? {
            let item = item?;
            let stats = Stats::decode(&item.value)?;
            total.docs += stats.docs;
            total.tokens += stats.tokens;
        }
        #[allow(
            clippy::cast_precision_loss,
            reason = "counts far below 2^52; BM25 is a float formula"
        )]
        let (docs, tokens) = (total.docs as f64, total.tokens as f64);
        Ok(Self {
            source,
            collection: collection.to_owned(),
            docs,
            average_length: if docs > 0.0 { tokens / docs } else { 0.0 },
        })
    }

    pub fn is_empty(&self) -> bool {
        self.docs <= 0.0
    }

    fn bm25(&self, document_frequency: usize, frequency: usize, length: u64) -> f64 {
        #[allow(clippy::cast_precision_loss, reason = "as in `new`")]
        let (df, tf, len) = (document_frequency as f64, frequency as f64, length as f64);
        let idf = ((self.docs - df + 0.5) / (df + 0.5) + 1.0).ln();
        let norm = if self.average_length > 0.0 {
            len / self.average_length
        } else {
            1.0
        };
        idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * norm))
    }

    /// Postings of every term in `[start, end)`, grouped by term.
    fn postings(
        &self,
        start: &[u8],
        end: &[u8],
    ) -> Result<HashMap<String, Vec<(String, Posting)>>> {
        let base = postings_base(&self.collection);
        let mut by_term: HashMap<String, Vec<(String, Posting)>> = HashMap::new();
        for item in self
            .source
            .range(Bound::Included(start), Bound::Excluded(end))?
        {
            let item = item?;
            self.source.observe(&item);
            let rest = keys::suffix(&item.key, &base);
            let split = rest
                .iter()
                .position(|&b| b == 0)
                .ok_or_else(|| corruption("posting key without a document"))?;
            let text = |b: &[u8]| {
                String::from_utf8(b.to_vec()).map_err(|_| corruption("posting key is not UTF-8"))
            };
            by_term
                .entry(text(&rest[..split])?)
                .or_default()
                .push((text(&rest[split + 1..])?, Posting::decode(&item.value)?));
        }
        Ok(by_term)
    }

    fn term(&self, term: &str) -> Result<Vec<(String, Posting)>> {
        let prefix = term_prefix(&self.collection, term);
        let end = keys::prefix_end(&prefix).unwrap_or_default();
        Ok(self
            .postings(&prefix, &end)?
            .remove(term)
            .unwrap_or_default())
    }

    fn score(
        &self,
        matches: Vec<(String, usize, u64)>,
        terms: &[String],
    ) -> HashMap<String, Match> {
        let df = matches.len();
        matches
            .into_iter()
            .map(|(doc, tf, length)| {
                (
                    doc,
                    Match {
                        score: self.bm25(df, tf, length),
                        terms: terms.iter().cloned().collect(),
                    },
                )
            })
            .collect()
    }

    /// Documents matching `node`, with their scores.
    ///
    /// # Errors
    ///
    /// Read errors, postings that do not decode.
    pub fn eval(&self, node: &Node) -> Result<HashMap<String, Match>> {
        match node {
            Node::Terms(terms) => self.phrase(terms),
            Node::Prefix(stem) => {
                let mut start = postings_base(&self.collection);
                start.extend_from_slice(stem.as_bytes());
                let end = keys::prefix_end(&start).unwrap_or_default();
                let mut out: HashMap<String, Match> = HashMap::new();
                for (term, postings) in self.postings(&start, &end)? {
                    let matches = postings
                        .into_iter()
                        .map(|(doc, p)| (doc, p.positions.len(), p.length))
                        .collect();
                    merge_or(&mut out, self.score(matches, std::slice::from_ref(&term)));
                }
                Ok(out)
            }
            Node::And(parts) => {
                let mut parts = parts.iter();
                let Some(first) = parts.next() else {
                    return Ok(HashMap::new());
                };
                let mut out = self.eval(first)?;
                for part in parts {
                    if out.is_empty() {
                        break;
                    }
                    let next = self.eval(part)?;
                    out.retain(|doc, _| next.contains_key(doc));
                    for (doc, m) in next {
                        if let Some(hit) = out.get_mut(&doc) {
                            hit.score += m.score;
                            hit.terms.extend(m.terms);
                        }
                    }
                }
                Ok(out)
            }
            Node::Or(parts) => {
                let mut out = HashMap::new();
                for part in parts {
                    merge_or(&mut out, self.eval(part)?);
                }
                Ok(out)
            }
            Node::Not(kept, excluded) => {
                let mut out = self.eval(kept)?;
                let excluded = self.eval(excluded)?;
                out.retain(|doc, _| !excluded.contains_key(doc));
                Ok(out)
            }
        }
    }

    /// Documents holding `terms` at consecutive positions (one term: holding it).
    fn phrase(&self, terms: &[String]) -> Result<HashMap<String, Match>> {
        let Some((first, rest)) = terms.split_first() else {
            return Ok(HashMap::new());
        };
        let mut candidates: HashMap<String, (Posting, Vec<Vec<u32>>)> = self
            .term(first)?
            .into_iter()
            .map(|(doc, p)| (doc, (p, Vec::new())))
            .collect();
        for term in rest {
            if candidates.is_empty() {
                break;
            }
            let postings: HashMap<String, Posting> = self.term(term)?.into_iter().collect();
            candidates.retain(|doc, _| postings.contains_key(doc));
            for (doc, (_, later)) in &mut candidates {
                if let Some(p) = postings.get(doc) {
                    later.push(p.positions.clone());
                }
            }
        }
        let matches = candidates
            .into_iter()
            .filter_map(|(doc, (first, later))| {
                let occurrences = first
                    .positions
                    .iter()
                    .filter(|&&start| {
                        later.iter().enumerate().all(|(i, at)| {
                            at.binary_search(&(start + 1 + u32::try_from(i).unwrap_or(u32::MAX)))
                                .is_ok()
                        })
                    })
                    .count();
                (occurrences > 0).then_some((doc, occurrences, first.length))
            })
            .collect();
        Ok(self.score(matches, terms))
    }
}

fn merge_or(out: &mut HashMap<String, Match>, more: HashMap<String, Match>) {
    for (doc, m) in more {
        let hit = out.entry(doc).or_default();
        hit.score += m.score;
        hit.terms.extend(m.terms);
    }
}

/// The documents of `matches`, best first: by score, then by id.
pub fn ranked(matches: HashMap<String, Match>) -> Vec<(String, Match)> {
    let mut ranked: Vec<(String, Match)> = matches.into_iter().collect();
    ranked.sort_by(|a, b| b.1.score.total_cmp(&a.1.score).then_with(|| a.0.cmp(&b.0)));
    ranked
}

/// Tokens a snippet shows at most.
const SNIPPET_TOKENS: usize = 32;

/// The passage of `content` with the most matched terms, at most 32 tokens, matched tokens
/// wrapped in `<b>` and `</b>`, and `...` where the passage cuts the text.
pub fn snippet(content: &str, terms: &BTreeSet<String>) -> String {
    let tokens = text::tokens(content);
    if tokens.is_empty() {
        return content.to_owned();
    }
    let hits: Vec<bool> = tokens.iter().map(|t| terms.contains(&t.term)).collect();
    let width = SNIPPET_TOKENS.min(tokens.len());
    let mut best = (0, 0usize);
    let mut count: usize = hits[..width].iter().filter(|&&h| h).count();
    best.1 = count;
    for start in 1..=tokens.len() - width {
        count = count + usize::from(hits[start + width - 1]) - usize::from(hits[start - 1]);
        if count > best.1 {
            best = (start, count);
        }
    }
    let (first, last) = (best.0, best.0 + width - 1);
    let mut out = String::new();
    let mut cursor = if first == 0 {
        0
    } else {
        out.push_str("...");
        tokens[first].start
    };
    for (token, &hit) in tokens[first..=last].iter().zip(&hits[first..=last]) {
        out.push_str(&content[cursor..token.start]);
        if hit {
            out.push_str("<b>");
            out.push_str(&content[token.start..token.end]);
            out.push_str("</b>");
        } else {
            out.push_str(&content[token.start..token.end]);
        }
        cursor = token.end;
    }
    if last + 1 == tokens.len() {
        out.push_str(&content[cursor..]);
    } else {
        out.push_str("...");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(words: &[&str]) -> Node {
        Node::Terms(words.iter().map(|w| (*w).to_owned()).collect())
    }

    #[test]
    fn queries_parse_like_fts5() {
        assert_eq!(
            parse("red fox"),
            Some(Node::And(vec![terms(&["red"]), terms(&["fox"])]))
        );
        assert_eq!(parse("\"Red Fox\""), Some(terms(&["red", "fox"])));
        assert_eq!(parse("e-mail"), Some(terms(&["e", "mail"])));
        assert_eq!(parse("fo*"), Some(Node::Prefix("fo".into())));
        assert_eq!(
            parse("red OR blue"),
            Some(Node::Or(vec![terms(&["red"]), terms(&["blue"])]))
        );
        assert_eq!(
            parse("(red OR blue) AND fox NOT dog"),
            Some(Node::And(vec![
                Node::Or(vec![terms(&["red"]), terms(&["blue"])]),
                Node::Not(Box::new(terms(&["fox"])), Box::new(terms(&["dog"]))),
            ]))
        );
        assert_eq!(parse("  ,, "), None);
        assert_eq!(parse(") OR"), None);
    }

    #[test]
    fn snippets_window_the_densest_passage() {
        let set = |w: &[&str]| w.iter().map(|s| (*s).to_owned()).collect::<BTreeSet<_>>();
        assert_eq!(
            snippet("The quick fox.", &set(&["fox"])),
            "The quick <b>fox</b>."
        );
        let long: String = (0..100)
            .map(|i| {
                if i == 60 {
                    "target ".to_owned()
                } else {
                    format!("w{i} ")
                }
            })
            .collect();
        let s = snippet(long.trim(), &set(&["target"]));
        assert!(
            s.starts_with("...w29") && s.ends_with("...") && s.contains("<b>target</b>"),
            "{s}"
        );
    }
}
