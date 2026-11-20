//! Lawrence Philips' Double Metaphone phonetic algorithm.

use serde::{Deserialize, Serialize};

use super::remove_accents;

/// Result containing the primary and optional alternate Double Metaphone phonetic codes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DoubleMetaphoneResult {
    pub primary: String,
    pub alternate: Option<String>,
}

impl DoubleMetaphoneResult {
    /// Checks if this phonetic result matches another (if primary or alternate codes intersect).
    #[must_use]
    pub fn matches(&self, other: &DoubleMetaphoneResult) -> bool {
        if self.primary == other.primary {
            return true;
        }
        if let Some(ref alt) = self.alternate {
            if *alt == other.primary {
                return true;
            }
            if let Some(ref other_alt) = other.alternate {
                if alt == other_alt {
                    return true;
                }
            }
        }
        if let Some(ref other_alt) = other.alternate {
            if self.primary == *other_alt {
                return true;
            }
        }
        false
    }

    /// Checks if a given code matches either the primary or alternate code.
    #[must_use]
    pub fn is_match(&self, code: &str) -> bool {
        if self.primary == code {
            return true;
        }
        if let Some(ref alt) = self.alternate {
            if alt == code {
                return true;
            }
        }
        false
    }
}

/// Encodes a word into its primary and alternate Double Metaphone phonetic keys (up to 4 characters).
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn double_metaphone(word: &str) -> DoubleMetaphoneResult {
    let unaccented = remove_accents(word);
    let raw = unaccented.to_ascii_uppercase();
    let s: Vec<char> = raw.chars().filter(char::is_ascii_alphabetic).collect();

    if s.is_empty() {
        return DoubleMetaphoneResult {
            primary: String::new(),
            alternate: None,
        };
    }

    let mut primary = String::with_capacity(4);
    let mut alternate = String::with_capacity(4);

    let len = s.len();
    let mut current = 0;

    // Check for initial silent letters or special combinations
    if starts_with(&s, "GN")
        || starts_with(&s, "KN")
        || starts_with(&s, "PN")
        || starts_with(&s, "WR")
        || starts_with(&s, "PS")
    {
        current += 1;
    } else if starts_with(&s, "X") {
        primary.push('S');
        alternate.push('S');
        current += 1;
    }

    while (primary.len() < 4 || alternate.len() < 4) && current < len {
        let c = s[current];

        match c {
            'A' | 'E' | 'I' | 'O' | 'U' | 'Y' => {
                if current == 0 {
                    primary.push('A');
                    alternate.push('A');
                }
                current += 1;
            }
            'B' => {
                push_both(&mut primary, &mut alternate, 'P');
                if char_at(&s, current + 1) == Some('B') {
                    current += 2;
                } else {
                    current += 1;
                }
            }
            'C' => {
                if current > 1
                    && !is_vowel(char_at(&s, current.wrapping_sub(2)))
                    && char_at(&s, current.wrapping_sub(1)) == Some('A')
                    && char_at(&s, current + 1) == Some('H')
                    && char_at(&s, current + 2) != Some('I')
                    && (char_at(&s, current + 2) != Some('E')
                        || (char_at(&s, current.wrapping_sub(3)) == Some('B')
                            || char_at(&s, current.wrapping_sub(3)) == Some('M')
                            || char_at(&s, current.wrapping_sub(3)) == Some('R')))
                {
                    push_both(&mut primary, &mut alternate, 'K');
                    current += 2;
                } else if current == 0 && starts_with(&s, "CAESAR") {
                    push_both(&mut primary, &mut alternate, 'S');
                    current += 2;
                } else if starts_at(&s, current, "CHIA") {
                    push_both(&mut primary, &mut alternate, 'K');
                    current += 2;
                } else if starts_at(&s, current, "CH") {
                    if (current > 0 && starts_at(&s, current, "CHAE"))
                        || (current == 0
                            && (starts_at(&s, current + 1, "HARAC")
                                || starts_at(&s, current + 1, "HARIS")
                                || starts_at(&s, current + 1, "HOR")
                                || starts_at(&s, current + 1, "HYM")
                                || starts_at(&s, current + 1, "HIA")
                                || starts_at(&s, current + 1, "HEM")))
                    {
                        push_both(&mut primary, &mut alternate, 'K');
                        current += 2;
                    } else {
                        if current == 0 {
                            push_both(&mut primary, &mut alternate, 'X');
                        } else {
                            push(&mut primary, 'X');
                            push(&mut alternate, 'K');
                        }
                        current += 2;
                    }
                } else if starts_at(&s, current, "CZ") && !starts_at(&s, current.wrapping_sub(2), "WICZ") {
                    push(&mut primary, 'S');
                    push(&mut alternate, 'X');
                    current += 2;
                } else if starts_at(&s, current + 1, "IA") {
                    push_both(&mut primary, &mut alternate, 'X');
                    current += 3;
                } else if starts_at(&s, current, "CC")
                    && !(current == 1 && char_at(&s, 0) == Some('M'))
                {
                    if char_at(&s, current + 2) == Some('I')
                        || char_at(&s, current + 2) == Some('E')
                        || char_at(&s, current + 2) == Some('H')
                    {
                        push_both(&mut primary, &mut alternate, 'X');
                        current += 3;
                    } else {
                        push_both(&mut primary, &mut alternate, 'K');
                        current += 2;
                    }
                } else if starts_at(&s, current, "CK")
                    || starts_at(&s, current, "CG")
                    || starts_at(&s, current, "CQ")
                {
                    push_both(&mut primary, &mut alternate, 'K');
                    current += 2;
                } else if starts_at(&s, current, "CI")
                    || starts_at(&s, current, "CE")
                    || starts_at(&s, current, "CY")
                {
                    if starts_at(&s, current, "CIO")
                        || starts_at(&s, current, "CIE")
                        || starts_at(&s, current, "CIA")
                    {
                        push(&mut primary, 'S');
                        push(&mut alternate, 'X');
                    } else {
                        push_both(&mut primary, &mut alternate, 'S');
                    }
                    current += 2;
                } else {
                    push_both(&mut primary, &mut alternate, 'K');
                    if starts_at(&s, current + 1, " C")
                        || starts_at(&s, current + 1, " Q")
                        || starts_at(&s, current + 1, " G")
                    {
                        current += 3;
                    } else if char_at(&s, current + 1) == Some('C')
                        || char_at(&s, current + 1) == Some('K')
                        || char_at(&s, current + 1) == Some('Q')
                    {
                        current += 2;
                    } else {
                        current += 1;
                    }
                }
            }
            'D' => {
                if starts_at(&s, current, "DG") {
                    if char_at(&s, current + 2) == Some('I')
                        || char_at(&s, current + 2) == Some('E')
                        || char_at(&s, current + 2) == Some('Y')
                    {
                        push_both(&mut primary, &mut alternate, 'J');
                        current += 3;
                    } else {
                        push_both(&mut primary, &mut alternate, 'T');
                        push_both(&mut primary, &mut alternate, 'K');
                        current += 2;
                    }
                } else if starts_at(&s, current, "DT") || starts_at(&s, current, "DD") {
                    push_both(&mut primary, &mut alternate, 'T');
                    current += 2;
                } else {
                    push_both(&mut primary, &mut alternate, 'T');
                    current += 1;
                }
            }
            'F' => {
                push_both(&mut primary, &mut alternate, 'F');
                if char_at(&s, current + 1) == Some('F') {
                    current += 2;
                } else {
                    current += 1;
                }
            }
            'G' => {
                if char_at(&s, current + 1) == Some('H') {
                    if current > 0 && !is_vowel(char_at(&s, current.wrapping_sub(1))) {
                        push_both(&mut primary, &mut alternate, 'K');
                        current += 2;
                    } else if current == 0 {
                        if char_at(&s, current + 2) == Some('I') {
                            push_both(&mut primary, &mut alternate, 'J');
                        } else {
                            push_both(&mut primary, &mut alternate, 'K');
                        }
                        current += 2;
                    } else if current > 1
                        && (char_at(&s, current.wrapping_sub(2)) == Some('B')
                            || char_at(&s, current.wrapping_sub(2)) == Some('H')
                            || char_at(&s, current.wrapping_sub(2)) == Some('D'))
                    {
                        current += 2;
                    } else {
                        push_both(&mut primary, &mut alternate, 'F');
                        current += 2;
                    }
                } else if char_at(&s, current + 1) == Some('N') {
                    if current == 1 && is_vowel(char_at(&s, 0)) {
                        push(&mut primary, 'K');
                        push(&mut alternate, 'N');
                    } else {
                        push_both(&mut primary, &mut alternate, 'N');
                    }
                    current += 2;
                } else if starts_at(&s, current, "GLI") {
                    push(&mut primary, 'K');
                    push(&mut alternate, 'L');
                    current += 3;
                } else if current == 0
                    && (char_at(&s, current + 1) == Some('Y')
                        || starts_at(&s, current + 1, "ES")
                        || starts_at(&s, current + 1, "EP")
                        || starts_at(&s, current + 1, "EB")
                        || starts_at(&s, current + 1, "EL")
                        || starts_at(&s, current + 1, "EY")
                        || starts_at(&s, current + 1, "IB")
                        || starts_at(&s, current + 1, "IL")
                        || starts_at(&s, current + 1, "IN")
                        || starts_at(&s, current + 1, "IE"))
                {
                    push(&mut primary, 'K');
                    push(&mut alternate, 'J');
                    current += 2;
                } else if (char_at(&s, current + 1) == Some('E')
                    || char_at(&s, current + 1) == Some('I')
                    || char_at(&s, current + 1) == Some('Y'))
                    || (starts_at(&s, current, "GER") || starts_at(&s, current, "GIE"))
                {
                    // Soft G vs Hard G (e.g. Gérard / Guerard)
                    push(&mut primary, 'J');
                    push(&mut alternate, 'K');
                    current += 2;
                } else if starts_at(&s, current, "GU") {
                    // French GU followed by vowel is pronounced hard G (K), but alternates with soft G (J)
                    push(&mut primary, 'K');
                    push(&mut alternate, 'J');
                    if char_at(&s, current + 2) == Some('E')
                        || char_at(&s, current + 2) == Some('I')
                        || char_at(&s, current + 2) == Some('A')
                    {
                        current += 2;
                    } else {
                        current += 1;
                    }
                } else {
                    push_both(&mut primary, &mut alternate, 'K');
                    if char_at(&s, current + 1) == Some('G') {
                        current += 2;
                    } else {
                        current += 1;
                    }
                }
            }
            'H' => {
                if (current == 0 || is_vowel(char_at(&s, current.wrapping_sub(1))))
                    && is_vowel(char_at(&s, current + 1))
                {
                    push_both(&mut primary, &mut alternate, 'H');
                    current += 2;
                } else {
                    current += 1;
                }
            }
            'J' => {
                if starts_at(&s, current, "JOSE") || current == 0 && starts_at(&s, current, "SAN ")
                {
                    push_both(&mut primary, &mut alternate, 'H');
                } else if current == 0 {
                    push(&mut primary, 'J');
                    push(&mut alternate, 'A');
                } else if is_vowel(char_at(&s, current.wrapping_sub(1))) && !is_vowel(char_at(&s, current + 1))
                {
                    push(&mut primary, 'J');
                    push(&mut alternate, 'H');
                } else {
                    push(&mut primary, 'J');
                    push(&mut alternate, 'K');
                }
                if char_at(&s, current + 1) == Some('J') {
                    current += 2;
                } else {
                    current += 1;
                }
            }
            'K' => {
                push_both(&mut primary, &mut alternate, 'K');
                if char_at(&s, current + 1) == Some('K') {
                    current += 2;
                } else {
                    current += 1;
                }
            }
            'L' => {
                if char_at(&s, current + 1) == Some('L') {
                    if (current == len - 3
                        && (starts_at(&s, current.wrapping_sub(1), "ILLO")
                            || starts_at(&s, current.wrapping_sub(1), "ILLA")
                            || starts_at(&s, current.wrapping_sub(1), "ALLE")))
                        || (starts_at(&s, len - 2, "AS") || starts_at(&s, len - 2, "OS"))
                    {
                        push(&mut primary, 'L');
                        current += 2;
                    } else {
                        push_both(&mut primary, &mut alternate, 'L');
                        current += 2;
                    }
                } else {
                    push_both(&mut primary, &mut alternate, 'L');
                    current += 1;
                }
            }
            'M' => {
                push_both(&mut primary, &mut alternate, 'M');
                if char_at(&s, current + 1) == Some('M') {
                    current += 2;
                } else {
                    current += 1;
                }
            }
            'N' => {
                push_both(&mut primary, &mut alternate, 'N');
                if char_at(&s, current + 1) == Some('N') {
                    current += 2;
                } else {
                    current += 1;
                }
            }
            'P' => {
                if char_at(&s, current + 1) == Some('H') {
                    push_both(&mut primary, &mut alternate, 'F');
                    current += 2;
                } else if char_at(&s, current + 1) == Some('F') {
                    // Initial or embedded PF -> F (Pfizer -> FSR)
                    push_both(&mut primary, &mut alternate, 'F');
                    current += 2;
                } else {
                    push_both(&mut primary, &mut alternate, 'P');
                    if char_at(&s, current + 1) == Some('P') {
                        current += 2;
                    } else {
                        current += 1;
                    }
                }
            }
            'Q' => {
                push_both(&mut primary, &mut alternate, 'K');
                if char_at(&s, current + 1) == Some('Q') {
                    current += 2;
                } else {
                    current += 1;
                }
            }
            'R' => {
                push_both(&mut primary, &mut alternate, 'R');
                if char_at(&s, current + 1) == Some('R') {
                    current += 2;
                } else {
                    current += 1;
                }
            }
            'S' => {
                if starts_at(&s, current, "SH") {
                    push_both(&mut primary, &mut alternate, 'X');
                    current += 2;
                } else if starts_at(&s, current, "SIO") || starts_at(&s, current, "SIA") {
                    push(&mut primary, 'S');
                    push(&mut alternate, 'X');
                    current += 3;
                } else if starts_at(&s, current, "SCH") {
                    push_both(&mut primary, &mut alternate, 'S');
                    current += 3;
                } else if starts_at(&s, current, "SC") {
                    if char_at(&s, current + 2) == Some('E')
                        || char_at(&s, current + 2) == Some('I')
                        || char_at(&s, current + 2) == Some('Y')
                    {
                        push_both(&mut primary, &mut alternate, 'S');
                        current += 3;
                    } else {
                        push_both(&mut primary, &mut alternate, 'S');
                        push_both(&mut primary, &mut alternate, 'K');
                        current += 3;
                    }
                } else {
                    push_both(&mut primary, &mut alternate, 'S');
                    if char_at(&s, current + 1) == Some('S') {
                        current += 2;
                    } else {
                        current += 1;
                    }
                }
            }
            'T' => {
                if starts_at(&s, current, "TION")
                    || starts_at(&s, current, "TIA")
                    || starts_at(&s, current, "TCH")
                {
                    push_both(&mut primary, &mut alternate, 'X');
                    current += 3;
                } else if starts_at(&s, current, "TH") {
                    // Smith / Smyth -> SM0 / SMT
                    push(&mut primary, '0');
                    push(&mut alternate, 'T');
                    current += 2;
                } else if starts_at(&s, current, "TT") {
                    push_both(&mut primary, &mut alternate, 'T');
                    current += 2;
                } else {
                    push_both(&mut primary, &mut alternate, 'T');
                    current += 1;
                }
            }
            'V' => {
                push_both(&mut primary, &mut alternate, 'F');
                if char_at(&s, current + 1) == Some('V') {
                    current += 2;
                } else {
                    current += 1;
                }
            }
            'W' => {
                if starts_at(&s, current, "WR") {
                    push_both(&mut primary, &mut alternate, 'R');
                    current += 2;
                } else if current == 0
                    && (is_vowel(char_at(&s, current + 1)) || starts_at(&s, current, "WH"))
                {
                    push(&mut primary, 'A');
                    push(&mut alternate, 'F');
                    current += 1;
                } else {
                    current += 1;
                }
            }
            'X' => {
                if !(current == len - 1
                    && (starts_at(&s, current.wrapping_sub(3), "IAU")
                        || starts_at(&s, current.wrapping_sub(3), "EAU")
                        || starts_at(&s, current.wrapping_sub(2), "AU")
                        || starts_at(&s, current.wrapping_sub(2), "OU")))
                {
                    push(&mut primary, 'K');
                    push(&mut primary, 'S');
                    push(&mut alternate, 'K');
                    push(&mut alternate, 'S');
                }
                if char_at(&s, current + 1) == Some('C') || char_at(&s, current + 1) == Some('X') {
                    current += 2;
                } else {
                    current += 1;
                }
            }
            'Z' => {
                if char_at(&s, current + 1) == Some('H') {
                    push_both(&mut primary, &mut alternate, 'J');
                    current += 2;
                } else {
                    push_both(&mut primary, &mut alternate, 'S');
                    if char_at(&s, current + 1) == Some('Z') {
                        current += 2;
                    } else {
                        current += 1;
                    }
                }
            }
            _ => {
                current += 1;
            }
        }
    }

    truncate_to(&mut primary, 4);
    truncate_to(&mut alternate, 4);

    let alt = if alternate.is_empty() || alternate == primary {
        None
    } else {
        Some(alternate)
    };

    DoubleMetaphoneResult {
        primary,
        alternate: alt,
    }
}

#[inline]
fn is_vowel(c: Option<char>) -> bool {
    matches!(c, Some('A' | 'E' | 'I' | 'O' | 'U' | 'Y'))
}

#[inline]
fn char_at(s: &[char], idx: usize) -> Option<char> {
    s.get(idx).copied()
}

#[inline]
fn starts_with(s: &[char], prefix: &str) -> bool {
    starts_at(s, 0, prefix)
}

#[inline]
/// Whether `prefix` is at `offset`. An offset before the start of the word wraps around to a
/// huge value, and matches nothing, like any position outside the word.
fn starts_at(s: &[char], offset: usize, prefix: &str) -> bool {
    let p_chars: Vec<char> = prefix.chars().collect();
    if offset.checked_add(p_chars.len()).is_none_or(|end| end > s.len()) {
        return false;
    }
    for (i, &pc) in p_chars.iter().enumerate() {
        if s[offset + i] != pc {
            return false;
        }
    }
    true
}

#[inline]
fn push(target: &mut String, c: char) {
    if target.len() < 4 {
        target.push(c);
    }
}

#[inline]
fn push_both(primary: &mut String, alternate: &mut String, c: char) {
    push(primary, c);
    push(alternate, c);
}

#[inline]
fn truncate_to(s: &mut String, max_len: usize) {
    if s.len() > max_len {
        s.truncate(max_len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups_before_the_start_of_short_words_match_nothing() {
        for word in ["fox", "ox", "x", "aux", "czar", "cz", "llo", "al", "wicz"] {
            let result = double_metaphone(word);
            assert!(!result.primary.is_empty() || word == "x", "{word}");
        }
        assert_eq!(double_metaphone("fox").primary, "FKS");
    }

    #[test]
    fn test_smith_smyth() {
        let r_smith = double_metaphone("Smith");
        let r_smyth = double_metaphone("Smyth");

        assert_eq!(r_smith.primary, "SM0");
        assert_eq!(r_smith.alternate.as_deref(), Some("SMT"));

        assert_eq!(r_smyth.primary, "SM0");
        assert_eq!(r_smyth.alternate.as_deref(), Some("SMT"));

        assert!(
            r_smith.matches(&r_smyth),
            "Smith and Smyth must match in Double Metaphone"
        );
    }

    #[test]
    fn test_pfizer_fizer() {
        let r_pfizer = double_metaphone("Pfizer");
        let r_fizer = double_metaphone("Fizer");

        assert_eq!(r_pfizer.primary, "FSR");
        assert_eq!(r_fizer.primary, "FSR");

        assert!(
            r_pfizer.matches(&r_fizer),
            "Pfizer and Fizer must match in Double Metaphone"
        );
    }

    #[test]
    fn test_gerard_guerard() {
        let r_gerard = double_metaphone("Gérard");
        let r_guerard = double_metaphone("Guerard");

        assert!(
            r_gerard.matches(&r_guerard),
            "Gérard and Guerard must match in Double Metaphone: gerard={r_gerard:?}, guerard={r_guerard:?}"
        );
    }

    #[test]
    fn test_accents_and_empty() {
        assert_eq!(double_metaphone("").primary, "");
        assert_eq!(double_metaphone("12345").primary, "");
        assert_eq!(double_metaphone("Catherine").primary, "K0RN");
        assert_eq!(double_metaphone("Katherine").primary, "K0RN");
        assert!(double_metaphone("Catherine").matches(&double_metaphone("Katherine")));
    }
}
