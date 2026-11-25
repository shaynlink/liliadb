//! Reading the letters of a word and writing the two keys.

#[inline]
pub(super) fn is_vowel(c: Option<char>) -> bool {
    matches!(c, Some('A' | 'E' | 'I' | 'O' | 'U' | 'Y'))
}

#[inline]
pub(super) fn char_at(s: &[char], idx: usize) -> Option<char> {
    s.get(idx).copied()
}

#[inline]
pub(super) fn starts_with(s: &[char], prefix: &str) -> bool {
    starts_at(s, 0, prefix)
}

#[inline]
/// Whether `prefix` is at `offset`. An offset before the start of the word wraps around to a
/// huge value, and matches nothing, like any position outside the word.
pub(super) fn starts_at(s: &[char], offset: usize, prefix: &str) -> bool {
    let p_chars: Vec<char> = prefix.chars().collect();
    if offset
        .checked_add(p_chars.len())
        .is_none_or(|end| end > s.len())
    {
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
pub(super) fn push(target: &mut String, c: char) {
    if target.len() < 4 {
        target.push(c);
    }
}

#[inline]
pub(super) fn push_both(primary: &mut String, alternate: &mut String, c: char) {
    push(primary, c);
    push(alternate, c);
}

#[inline]
pub(super) fn truncate_to(s: &mut String, max_len: usize) {
    if s.len() > max_len {
        s.truncate(max_len);
    }
}
