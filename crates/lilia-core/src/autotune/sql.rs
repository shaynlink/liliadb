const MAX_NORMALIZED_SQL_CHARS: usize = 512;

fn push_token(out: &mut String, token: &str, pending_space: &mut bool) {
    if *pending_space && !out.is_empty() {
        out.push(' ');
    }
    *pending_space = false;
    out.push_str(token);
}

fn copy_quoted(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    out: &mut String,
    close: char,
) {
    for c in chars.by_ref() {
        out.push(c);
        if c == close {
            break;
        }
    }
}

/// Reduces a SQL statement to its shape: literals become `?`, comments are removed and
/// whitespace is collapsed, so statements differing only by constants share one statistics entry.
/// The result is truncated to a bounded length.
#[must_use]
pub fn normalize_sql(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len().min(MAX_NORMALIZED_SQL_CHARS));
    let mut chars = sql.chars().peekable();
    let mut pending_space = false;

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                while let Some(inner) = chars.next() {
                    if inner == '\'' {
                        if chars.peek() == Some(&'\'') {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                }
                push_token(&mut out, "?", &mut pending_space);
            }
            '"' | '`' | '[' => {
                push_token(&mut out, "", &mut pending_space);
                out.push(c);
                copy_quoted(&mut chars, &mut out, if c == '[' { ']' } else { c });
            }
            '-' if chars.peek() == Some(&'-') => {
                for skipped in chars.by_ref() {
                    if skipped == '\n' {
                        break;
                    }
                }
                pending_space = true;
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = '\0';
                for skipped in chars.by_ref() {
                    if previous == '*' && skipped == '/' {
                        break;
                    }
                    previous = skipped;
                }
                pending_space = true;
            }
            '?' => {
                while chars.peek().is_some_and(char::is_ascii_digit) {
                    chars.next();
                }
                push_token(&mut out, "?", &mut pending_space);
            }
            c if c.is_whitespace() => pending_space = true,
            c if c.is_ascii_digit()
                && (pending_space
                    || !out
                        .chars()
                        .next_back()
                        .is_some_and(|prev| prev.is_alphanumeric() || prev == '_' || prev == '$')) =>
            {
                while chars
                    .peek()
                    .is_some_and(|next| next.is_ascii_alphanumeric() || *next == '.')
                {
                    chars.next();
                }
                push_token(&mut out, "?", &mut pending_space);
            }
            c => {
                let mut buffer = [0_u8; 4];
                push_token(&mut out, c.encode_utf8(&mut buffer), &mut pending_space);
            }
        }
    }

    if let Some((boundary, _)) = out.char_indices().nth(MAX_NORMALIZED_SQL_CHARS) {
        out.truncate(boundary);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_become_placeholders() {
        assert_eq!(
            normalize_sql("SELECT * FROM t WHERE a = 42 AND b = 'it''s' AND c = 3.14"),
            "SELECT * FROM t WHERE a = ? AND b = ? AND c = ?"
        );
    }

    #[test]
    fn whitespace_and_comments_are_collapsed() {
        assert_eq!(
            normalize_sql("SELECT  a\n  FROM t -- trailing\n WHERE /* skip */ x = ?1"),
            "SELECT a FROM t WHERE x = ?"
        );
    }

    #[test]
    fn identifiers_containing_digits_are_preserved() {
        assert_eq!(
            normalize_sql("SELECT col1, \"my col2\" FROM t2 WHERE col1 > 7"),
            "SELECT col1, \"my col2\" FROM t2 WHERE col1 > ?"
        );
    }

    #[test]
    fn output_is_bounded() {
        let long = format!("SELECT {}", "a, ".repeat(1_000));
        assert!(normalize_sql(&long).chars().count() <= MAX_NORMALIZED_SQL_CHARS);
    }
}
