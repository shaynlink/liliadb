#![allow(
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::similar_names
)]

use super::automaton::{LevenshteinAutomaton, LevenshteinState};
use crate::FtsSuggestion;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RadixNode {
    edge: Box<str>,
    is_terminal: bool,
    doc_count: u32,
    children: Vec<RadixNode>,
}

impl RadixNode {
    fn new(edge: &str, is_terminal: bool, doc_count: u32) -> Self {
        Self {
            edge: edge.into(),
            is_terminal,
            doc_count,
            children: Vec::new(),
        }
    }

    fn first_char(&self) -> Option<char> {
        self.edge.chars().next()
    }
}

/// Compact Radix Tree (Term Trie) storing vocabulary terms and document counts.
///
/// Supports high-throughput fuzzy search and autocompletion when intersected
/// with a [`LevenshteinAutomaton`].
#[derive(Debug, Clone, Default)]
pub struct TermTrie {
    root: RadixNode,
    total_terms: usize,
}

impl TermTrie {
    /// Creates an empty term trie.
    #[must_use]
    pub fn new() -> Self {
        Self {
            root: RadixNode::new("", false, 0),
            total_terms: 0,
        }
    }

    /// Returns the number of distinct terms in the trie.
    #[must_use]
    pub fn len(&self) -> usize {
        self.total_terms
    }

    /// Returns `true` if the trie contains no terms.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total_terms == 0
    }

    /// Clears all terms from the trie.
    pub fn clear(&mut self) {
        self.root.children.clear();
        self.root.is_terminal = false;
        self.root.doc_count = 0;
        self.total_terms = 0;
    }

    /// Inserts a term with a document count increment.
    ///
    /// If the term already exists, its `doc_count` is incremented by `doc_count_delta`.
    pub fn insert(&mut self, term: &str, doc_count_delta: u32) {
        let term = term.trim();
        if term.is_empty() {
            return;
        }
        let delta = if doc_count_delta == 0 {
            1
        } else {
            doc_count_delta
        };
        if insert_rec(&mut self.root, term, delta) {
            self.total_terms += 1;
        }
    }

    /// Removes a term or decrements its document count.
    ///
    /// Returns `true` if the term was present.
    pub fn remove(&mut self, term: &str) -> bool {
        let term = term.trim();
        if term.is_empty() {
            return false;
        }
        if remove_rec(&mut self.root, term) {
            self.total_terms = self.total_terms.saturating_sub(1);
            true
        } else {
            false
        }
    }

    /// Checks if a term is present in the trie.
    #[must_use]
    pub fn contains(&self, term: &str) -> bool {
        let term = term.trim();
        if term.is_empty() {
            return false;
        }
        contains_rec(&self.root, term)
    }

    /// Returns the document count for a term if present.
    #[must_use]
    pub fn doc_count(&self, term: &str) -> Option<u32> {
        let term = term.trim();
        if term.is_empty() {
            return None;
        }
        doc_count_rec(&self.root, term)
    }

    /// Traverses the trie with a [`LevenshteinAutomaton`], returning up to `limit` suggestions.
    ///
    /// The search prunes impossible branches immediately in $O(|Q| \cdot \text{branches})$ time,
    /// returning matching terms ranked by:
    /// 1. Edit distance (ascending: 0, 1, 2)
    /// 2. Exact prefix match bonus (true before false)
    /// 3. Document count (descending: popular terms first)
    /// 4. Term length (ascending)
    /// 5. Lexicographical order
    #[must_use]
    pub fn search_automaton(
        &self,
        automaton: &LevenshteinAutomaton,
        limit: usize,
    ) -> Vec<FtsSuggestion> {
        if limit == 0 {
            return Vec::new();
        }

        let mut matches: Vec<FtsSuggestion> = Vec::new();
        let start_state = automaton.start_state();
        let mut path = String::with_capacity(32);

        // If root is terminal (e.g. empty string) and accepting
        if self.root.is_terminal && automaton.is_accepting(&start_state) {
            matches.push(FtsSuggestion {
                term: String::new(),
                distance: automaton.distance(&start_state),
                doc_count: self.root.doc_count,
                exact_prefix: false,
            });
        }

        for child in &self.root.children {
            search_rec(child, automaton, &start_state, &mut path, &mut matches);
        }

        // Rank results
        matches.sort_by(|a, b| {
            a.distance
                .cmp(&b.distance)
                .then_with(|| b.exact_prefix.cmp(&a.exact_prefix))
                .then_with(|| b.doc_count.cmp(&a.doc_count))
                .then_with(|| a.term.len().cmp(&b.term.len()))
                .then_with(|| a.term.cmp(&b.term))
        });

        if matches.len() > limit {
            matches.truncate(limit);
        }

        matches
    }

    /// Returns all terms and their document counts stored in the trie.
    #[must_use]
    pub fn terms(&self) -> Vec<(String, u32)> {
        let mut results = Vec::with_capacity(self.total_terms);
        let mut path = String::new();
        collect_terms_rec(&self.root, &mut path, &mut results);
        results
    }
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    let mut len = 0;
    for (ca, cb) in a.chars().zip(b.chars()) {
        if ca == cb {
            len += ca.len_utf8();
        } else {
            break;
        }
    }
    len
}

fn insert_rec(node: &mut RadixNode, rem: &str, doc_count_delta: u32) -> bool {
    let first = rem.chars().next().expect("rem must be non-empty");

    // Binary search child by first character
    let idx = node
        .children
        .binary_search_by(|c| c.first_char().cmp(&Some(first)));

    match idx {
        Ok(child_idx) => {
            let child = &mut node.children[child_idx];
            let c_len = common_prefix_len(&child.edge, rem);

            if c_len == child.edge.len() {
                // Whole child edge matches
                if c_len == rem.len() {
                    // Exact match on child node
                    let is_new = !child.is_terminal;
                    child.is_terminal = true;
                    child.doc_count = child.doc_count.saturating_add(doc_count_delta);
                    is_new
                } else {
                    // Recurse with remainder
                    let suffix = &rem[c_len..];
                    insert_rec(child, suffix, doc_count_delta)
                }
            } else {
                // Partial match on edge: split the child
                let common_edge = child.edge[..c_len].to_string();
                let child_rem_edge = child.edge[c_len..].to_string();
                let new_rem = &rem[c_len..];

                let mut split_node =
                    RadixNode::new(&child_rem_edge, child.is_terminal, child.doc_count);
                std::mem::swap(&mut split_node.children, &mut child.children);

                child.edge = common_edge.into();

                if new_rem.is_empty() {
                    child.is_terminal = true;
                    child.doc_count = doc_count_delta;
                    child.children = vec![split_node];
                    true
                } else {
                    child.is_terminal = false;
                    child.doc_count = 0;
                    let new_child = RadixNode::new(new_rem, true, doc_count_delta);
                    if split_node.first_char() < new_child.first_char() {
                        child.children = vec![split_node, new_child];
                    } else {
                        child.children = vec![new_child, split_node];
                    }
                    true
                }
            }
        }
        Err(insert_pos) => {
            // New child edge
            let new_child = RadixNode::new(rem, true, doc_count_delta);
            node.children.insert(insert_pos, new_child);
            true
        }
    }
}

fn contains_rec(node: &RadixNode, rem: &str) -> bool {
    let first = rem.chars().next().expect("rem must be non-empty");
    let idx = node
        .children
        .binary_search_by(|c| c.first_char().cmp(&Some(first)));

    if let Ok(child_idx) = idx {
        let child = &node.children[child_idx];
        if rem.starts_with(&*child.edge) {
            if rem.len() == child.edge.len() {
                child.is_terminal
            } else {
                contains_rec(child, &rem[child.edge.len()..])
            }
        } else {
            false
        }
    } else {
        false
    }
}

fn doc_count_rec(node: &RadixNode, rem: &str) -> Option<u32> {
    let first = rem.chars().next().expect("rem must be non-empty");
    let idx = node
        .children
        .binary_search_by(|c| c.first_char().cmp(&Some(first)));

    if let Ok(child_idx) = idx {
        let child = &node.children[child_idx];
        if rem.starts_with(&*child.edge) {
            if rem.len() == child.edge.len() {
                if child.is_terminal {
                    Some(child.doc_count)
                } else {
                    None
                }
            } else {
                doc_count_rec(child, &rem[child.edge.len()..])
            }
        } else {
            None
        }
    } else {
        None
    }
}

fn remove_rec(node: &mut RadixNode, rem: &str) -> bool {
    let first = rem.chars().next().expect("rem must be non-empty");
    let idx = node
        .children
        .binary_search_by(|c| c.first_char().cmp(&Some(first)));

    if let Ok(child_idx) = idx {
        let child = &mut node.children[child_idx];
        if rem.starts_with(&*child.edge) {
            if rem.len() == child.edge.len() {
                if child.is_terminal {
                    child.is_terminal = false;
                    child.doc_count = 0;
                    // Clean up leaf if no children
                    if child.children.is_empty() {
                        node.children.remove(child_idx);
                    }
                    true
                } else {
                    false
                }
            } else {
                let removed = remove_rec(child, &rem[child.edge.len()..]);
                if removed && !child.is_terminal && child.children.is_empty() {
                    node.children.remove(child_idx);
                }
                removed
            }
        } else {
            false
        }
    } else {
        false
    }
}

fn search_rec(
    node: &RadixNode,
    automaton: &LevenshteinAutomaton,
    state: &LevenshteinState,
    path: &mut String,
    results: &mut Vec<FtsSuggestion>,
) {
    let mut current_state = *state;

    // Step automaton across each character of the edge
    for c in node.edge.chars() {
        if let Some(next_state) = automaton.step(&current_state, c) {
            current_state = next_state;
        } else {
            // Prune this branch immediately!
            return;
        }
    }

    let prev_len = path.len();
    path.push_str(&node.edge);

    if node.is_terminal && automaton.is_accepting(&current_state) {
        let distance = automaton.distance(&current_state);
        results.push(FtsSuggestion {
            term: path.clone(),
            distance,
            doc_count: node.doc_count,
            exact_prefix: distance == 0,
        });
    }

    // Recurse into children
    for child in &node.children {
        search_rec(child, automaton, &current_state, path, results);
    }

    path.truncate(prev_len);
}

fn collect_terms_rec(node: &RadixNode, path: &mut String, results: &mut Vec<(String, u32)>) {
    let prev_len = path.len();
    path.push_str(&node.edge);

    if node.is_terminal {
        results.push((path.clone(), node.doc_count));
    }

    for child in &node.children {
        collect_terms_rec(child, path, results);
    }

    path.truncate(prev_len);
}
