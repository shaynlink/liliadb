//! Tokenization: the same rule for indexing, queries and snippets.
//!
//! A token is a maximal run of alphanumeric characters, lowercased. Text containing CJK is first
//! segmented into words and bigrams separated by spaces ([`lilia_core::prepare_cjk_for_indexing`]),
//! and words carrying a field-level encryption envelope (`ENC:` prefix) are not indexed.

use lilia_core::cjk::cjk_query_words;
use lilia_core::{contains_cjk, prepare_cjk_for_indexing, ENC_PREFIX};

/// One token of a text: its lowercased form and its byte range in the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub term: String,
    pub start: usize,
    pub end: usize,
}

/// The tokens of `text`, in order.
pub fn tokens(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut start = 0;
    for (at, c) in text.char_indices() {
        if c.is_alphanumeric() {
            if current.is_empty() {
                start = at;
            }
            current.extend(c.to_lowercase());
        } else if !current.is_empty() {
            out.push(Token {
                term: std::mem::take(&mut current),
                start,
                end: at,
            });
        }
    }
    if !current.is_empty() {
        out.push(Token {
            term: current,
            start,
            end: text.len(),
        });
    }
    out
}

/// The text a document is indexed from: encrypted words removed, CJK segmented.
pub fn prepare(content: &str) -> String {
    let clean = if content.contains(ENC_PREFIX) {
        content
            .split_whitespace()
            .filter(|w| !w.starts_with(ENC_PREFIX))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        content.to_owned()
    };
    prepare_cjk_for_indexing(&clean)
}

/// The token sequences one query word stands for: one sequence for an ordinary word (several
/// tokens make a phrase, as `e-mail`), one per segmented word for CJK.
pub fn query_word(word: &str) -> Vec<Vec<String>> {
    let words = if contains_cjk(word) {
        cjk_query_words(word)
    } else {
        vec![word.to_owned()]
    };
    words
        .iter()
        .map(|w| tokens(w).into_iter().map(|t| t.term).collect::<Vec<_>>())
        .filter(|t| !t.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_lowercased_alphanumeric_runs_with_offsets() {
        let text = "Hello, Wörld! e-mail 42x";
        let got: Vec<(String, &str)> = tokens(text)
            .into_iter()
            .map(|t| (t.term, &text[t.start..t.end]))
            .collect();
        assert_eq!(
            got,
            [
                ("hello".into(), "Hello"),
                ("wörld".into(), "Wörld"),
                ("e".into(), "e"),
                ("mail".into(), "mail"),
                ("42x".into(), "42x")
            ]
        );
        assert_eq!(tokens("  ,; "), Vec::<Token>::new());
        assert_eq!(prepare("secret ENC:abcd visible"), "secret visible");
        assert_eq!(
            query_word("E-Mail"),
            [vec!["e".to_owned(), "mail".to_owned()]]
        );
        assert_eq!(query_word("--"), Vec::<Vec<String>>::new());
    }
}
