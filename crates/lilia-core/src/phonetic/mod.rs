//! Native Phonetic Algorithms (Double Metaphone & Soundex) and Vocabulary Index.

pub mod double_metaphone;
pub mod soundex;
pub mod vocabulary;

pub use double_metaphone::{double_metaphone, DoubleMetaphoneResult};
pub use soundex::soundex;
pub use vocabulary::{PhoneticCandidate, PhoneticVocabulary};

/// Normalizes Latin accented characters to their basic ASCII counterparts.
#[must_use]
pub fn remove_accents(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => {
                out.push('a')
            }
            'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => out.push('e'),
            'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => out.push('i'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' => out.push('o'),
            'ù' | 'ú' | 'û' | 'ü' | 'Ù' | 'Ú' | 'Û' | 'Ü' => out.push('u'),
            'ý' | 'ÿ' | 'Ý' => out.push('y'),
            'ç' | 'Ç' => out.push('c'),
            'ñ' | 'Ñ' => out.push('n'),
            'æ' | 'Æ' => {
                out.push('a');
                out.push('e');
            }
            'œ' | 'Œ' => {
                out.push('o');
                out.push('e');
            }
            other => out.push(other),
        }
    }
    out
}

/// Checks whether two words are phonetically equivalent via Double Metaphone or Soundex.
#[must_use]
pub fn is_phonetic_match(a: &str, b: &str) -> bool {
    let clean_a = a.trim();
    let clean_b = b.trim();
    if clean_a.is_empty() || clean_b.is_empty() {
        return false;
    }
    if clean_a.eq_ignore_ascii_case(clean_b) {
        return true;
    }

    let dm_a = double_metaphone(clean_a);
    let dm_b = double_metaphone(clean_b);
    if dm_a.matches(&dm_b) {
        return true;
    }

    let sx_a = soundex(clean_a);
    let sx_b = soundex(clean_b);
    !sx_a.is_empty() && sx_a == sx_b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_phonetic_matches() {
        assert!(is_phonetic_match("Gérard", "Guerard"));
        assert!(is_phonetic_match("Smith", "Smyth"));
        assert!(is_phonetic_match("Pfizer", "Fizer"));
        assert!(!is_phonetic_match("Smith", "Johnson"));
    }
}
