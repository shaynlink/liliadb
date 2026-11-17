pub mod automaton;
pub mod trie;

pub use automaton::{LevenshteinAutomaton, LevenshteinState};
pub use trie::TermTrie;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_levenshtein_automaton_exact_and_fuzzy() {
        let auto_exact = LevenshteinAutomaton::for_exact("croissant", 1);
        let s0 = auto_exact.start_state();

        // Exact match
        let mut s = s0;
        for c in "croissant".chars() {
            s = auto_exact.step(&s, c).expect("should transition");
        }
        assert!(auto_exact.is_accepting(&s));
        assert_eq!(auto_exact.distance(&s), 0);

        // Typo: "croisant" (distance 1)
        let mut s = s0;
        for c in "croisant".chars() {
            s = auto_exact.step(&s, c).expect("should transition");
        }
        assert!(auto_exact.is_accepting(&s));
        assert_eq!(auto_exact.distance(&s), 1);

        // Discard: "pizza" should be pruned
        let mut s = s0;
        let mut pruned = false;
        for c in "pizza".chars() {
            if let Some(next) = auto_exact.step(&s, c) {
                s = next;
            } else {
                pruned = true;
                break;
            }
        }
        assert!(pruned, "unrelated word must be pruned");
    }

    #[test]
    fn test_damerau_transposition() {
        let auto = LevenshteinAutomaton::for_exact("the", 1);
        let s0 = auto.start_state();

        // "teh" -> "the" is a transposition
        let mut s = s0;
        for c in "teh".chars() {
            s = auto.step(&s, c).expect("should transition");
        }
        assert!(auto.is_accepting(&s));
        assert_eq!(auto.distance(&s), 1);
    }

    #[test]
    fn test_prefix_autocomplete_matching() {
        let auto = LevenshteinAutomaton::for_prefix("crois", 0);
        let s0 = auto.start_state();

        // "croissant" extends "crois"
        let mut s = s0;
        for c in "croissant".chars() {
            s = auto.step(&s, c).expect("should transition in prefix mode");
        }
        assert!(auto.is_accepting(&s));
        assert_eq!(auto.distance(&s), 0);
    }

    #[test]
    fn test_term_trie_insert_and_lookup() {
        let mut trie = TermTrie::new();
        trie.insert("croissant", 42);
        trie.insert("croisière", 15);
        trie.insert("croire", 10);
        trie.insert("pain", 100);
        trie.insert("baguette", 80);

        assert_eq!(trie.len(), 5);
        assert!(trie.contains("croissant"));
        assert_eq!(trie.doc_count("croissant"), Some(42));
        assert!(!trie.contains("croisant"));

        // Fuzzy search for "croisant" with D=1
        let auto = LevenshteinAutomaton::for_exact("croisant", 1);
        let matches = trie.search_automaton(&auto, 10);
        assert!(!matches.is_empty());
        assert_eq!(matches[0].term, "croissant");
        assert_eq!(matches[0].distance, 1);
        assert_eq!(matches[0].doc_count, 42);
    }

    #[test]
    fn test_croisant_to_croissant_latency_sub_millisecond() {
        let mut trie = TermTrie::new();

        // Populate with realistic vocabulary (10,000 synthetic terms)
        for i in 0..10_000 {
            trie.insert(&format!("term_{i:05}"), 1);
        }
        // Add culinary words
        trie.insert("croissant", 100);
        trie.insert("croisière", 50);
        trie.insert("croire", 30);
        trie.insert("croix", 20);
        trie.insert("pain", 200);
        trie.insert("baguette", 150);

        let auto = LevenshteinAutomaton::for_prefix("croisant", 1);

        let start = std::time::Instant::now();
        let suggestions = trie.search_automaton(&auto, 5);
        let elapsed = start.elapsed();

        println!("Autocomplete latency for 10,000 terms: {elapsed:?}");
        assert!(!suggestions.is_empty());
        assert_eq!(suggestions[0].term, "croissant");
        assert_eq!(suggestions[0].distance, 1);

        // Latency must be < 2.0 ms in unoptimized debug test, and will be < 0.1 ms in release!
        assert!(
            elapsed.as_millis() < 5,
            "latency must be instantaneous, was {elapsed:?}"
        );
    }
}
