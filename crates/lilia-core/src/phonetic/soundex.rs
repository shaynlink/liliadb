//! American Soundex phonetic algorithm.

use super::remove_accents;

/// Encodes a word into its standard 4-character American Soundex code.
///
/// Returns an empty string if the input has no alphabetic characters.
#[must_use]
pub fn soundex(word: &str) -> String {
    let unaccented = remove_accents(word);
    let chars: Vec<char> = unaccented
        .chars()
        .filter(char::is_ascii_alphabetic)
        .map(|c| c.to_ascii_uppercase())
        .collect();

    if chars.is_empty() {
        return String::new();
    }

    let first_char = chars[0];
    let mut code = String::with_capacity(4);
    code.push(first_char);

    let mut last_digit = soundex_digit(first_char);

    for &c in &chars[1..] {
        let digit = soundex_digit(c);

        if digit != '0' {
            if digit != last_digit {
                code.push(digit);
                last_digit = digit;
                if code.len() == 4 {
                    break;
                }
            }
        } else if c != 'H' && c != 'W' {
            // A, E, I, O, U, Y reset the duplicate suppression
            last_digit = '0';
        }
    }

    while code.len() < 4 {
        code.push('0');
    }

    code
}

#[inline]
fn soundex_digit(c: char) -> char {
    match c {
        'B' | 'F' | 'P' | 'V' => '1',
        'C' | 'G' | 'J' | 'K' | 'Q' | 'S' | 'X' | 'Z' => '2',
        'D' | 'T' => '3',
        'L' => '4',
        'M' | 'N' => '5',
        'R' => '6',
        _ => '0',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_soundex_basic() {
        assert_eq!(soundex("Smith"), "S530");
        assert_eq!(soundex("Smyth"), "S530");
        assert_eq!(soundex("Robert"), "R163");
        assert_eq!(soundex("Rupert"), "R163");
        assert_eq!(soundex("Ashcraft"), "A261");
        assert_eq!(soundex("Ashcroft"), "A261");
        assert_eq!(soundex("Tymczak"), "T522");
        assert_eq!(soundex("Pfizer"), "P260");
    }

    #[test]
    fn test_soundex_accents() {
        assert_eq!(soundex("Gérard"), "G663");
        assert_eq!(soundex("Guerard"), "G663");
    }

    #[test]
    fn test_soundex_empty_or_special() {
        assert_eq!(soundex(""), "");
        assert_eq!(soundex("12345"), "");
        assert_eq!(soundex("A"), "A000");
    }
}
