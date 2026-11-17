#![allow(clippy::missing_errors_doc, clippy::cast_possible_truncation)]

const MAX_QUERY_LEN: usize = 64;

/// State of a Levenshtein automaton during trie/string traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevenshteinState {
    pub(crate) distances: [u8; MAX_QUERY_LEN],
    pub(crate) prev_distances: [u8; MAX_QUERY_LEN],
    pub(crate) len: usize,
    pub(crate) prev_char: Option<char>,
    pub(crate) matched_prefix_dist: Option<u8>,
}

/// Precompiled Levenshtein / Damerau-Levenshtein automaton.
///
/// Efficiently matches strings and prefix autocompletions within edit distance $D \le 2$ (or 3).
/// Transition execution is non-allocating and executes in tens of nanoseconds.
#[derive(Debug, Clone)]
pub struct LevenshteinAutomaton {
    chars: Vec<char>,
    max_distance: u8,
    damerau: bool,
    prefix_mode: bool,
}

impl LevenshteinAutomaton {
    /// Creates a new compiled Levenshtein automaton for target `query`.
    ///
    /// If `max_distance` exceeds 3, it is clamped to 3 to prevent state space explosion.
    #[must_use]
    pub fn new(query: &str, max_distance: u8, damerau: bool, prefix_mode: bool) -> Self {
        let chars: Vec<char> = query.chars().take(MAX_QUERY_LEN - 1).collect();
        Self {
            chars,
            max_distance: max_distance.min(3),
            damerau,
            prefix_mode,
        }
    }

    /// Helper constructor for autocompletion (prefix mode, Damerau enabled, max distance $\le 2$).
    #[must_use]
    pub fn for_prefix(query: &str, max_distance: u8) -> Self {
        Self::new(query, max_distance, true, true)
    }

    /// Helper constructor for exact fuzzy word matching (prefix mode disabled, Damerau enabled).
    #[must_use]
    pub fn for_exact(query: &str, max_distance: u8) -> Self {
        Self::new(query, max_distance, true, false)
    }

    /// Returns the initial start state of the automaton.
    #[must_use]
    pub fn start_state(&self) -> LevenshteinState {
        let n = self.chars.len();
        let limit = self.max_distance.saturating_add(1);
        let mut distances = [limit; MAX_QUERY_LEN];
        for (i, d) in distances.iter_mut().enumerate().take(n + 1) {
            *d = (i as u8).min(limit);
        }

        let matched_prefix_dist = if self.prefix_mode && n == 0 {
            Some(0)
        } else {
            None
        };

        LevenshteinState {
            distances,
            prev_distances: distances,
            len: n + 1,
            prev_char: None,
            matched_prefix_dist,
        }
    }

    /// Steps the automaton with a single character `c`.
    ///
    /// Returns `None` if the next state is not viable (i.e. minimum distance exceeds `max_distance`),
    /// allowing the caller to prune the entire trie branch immediately.
    #[must_use]
    pub fn step(&self, state: &LevenshteinState, c: char) -> Option<LevenshteinState> {
        let n = self.chars.len();
        let limit = self.max_distance.saturating_add(1);

        // If in prefix mode and an exact prefix match (dist 0) was already achieved,
        // all descendant characters maintain this optimal prefix match.
        if self.prefix_mode && state.matched_prefix_dist == Some(0) {
            return Some(LevenshteinState {
                distances: state.distances,
                prev_distances: state.prev_distances,
                len: state.len,
                prev_char: Some(c),
                matched_prefix_dist: Some(0),
            });
        }

        let mut next_d = [limit; MAX_QUERY_LEN];
        next_d[0] = (state.distances[0].saturating_add(1)).min(limit);

        for j in 1..=n {
            let cost: u8 = if self.chars[j - 1] == c { 0 } else { 1 };
            let sub = state.distances[j - 1].saturating_add(cost);
            let ins = next_d[j - 1].saturating_add(1);
            let del = state.distances[j].saturating_add(1);
            let mut d = sub.min(ins).min(del);

            if self.damerau && j >= 2 {
                if let Some(prev_c) = state.prev_char {
                    if prev_c == self.chars[j - 1] && c == self.chars[j - 2] {
                        let trans = state.prev_distances[j - 2].saturating_add(1);
                        d = d.min(trans);
                    }
                }
            }

            next_d[j] = d.min(limit);
        }

        // Update matched prefix distance if current node matches or improves it
        let mut matched_prefix_dist = state.matched_prefix_dist;
        if self.prefix_mode && next_d[n] <= self.max_distance {
            matched_prefix_dist = Some(match matched_prefix_dist {
                Some(prev) => prev.min(next_d[n]),
                None => next_d[n],
            });
        }

        // Pruning condition: if all prefix distances exceed max_distance, no extension can match!
        let mut min_d = limit;
        for &val in &next_d[0..=n] {
            if val < min_d {
                min_d = val;
            }
        }

        if min_d > self.max_distance && matched_prefix_dist.is_none() {
            return None;
        }

        Some(LevenshteinState {
            distances: next_d,
            prev_distances: state.distances,
            len: n + 1,
            prev_char: Some(c),
            matched_prefix_dist,
        })
    }

    /// Returns `true` if the current state is accepting.
    #[must_use]
    pub fn is_accepting(&self, state: &LevenshteinState) -> bool {
        if self.prefix_mode {
            if state.matched_prefix_dist.is_some() {
                return true;
            }
            state.distances[self.chars.len()] <= self.max_distance
        } else {
            state.distances[self.chars.len()] <= self.max_distance
        }
    }

    /// Returns the minimum edit distance in the current state.
    #[must_use]
    pub fn distance(&self, state: &LevenshteinState) -> u8 {
        if self.prefix_mode {
            if let Some(d) = state.matched_prefix_dist {
                return d;
            }
        }
        state.distances[self.chars.len()]
    }

    /// Returns the original query target chars.
    #[must_use]
    pub fn target_len(&self) -> usize {
        self.chars.len()
    }
}
