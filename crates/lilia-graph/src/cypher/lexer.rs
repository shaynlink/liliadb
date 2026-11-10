use lilia_core::{ErrorCode, LiliaError, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Match,
    Where,
    Return,
    Limit,
    And,
    Or,
    Not,
    True,
    False,
    Null,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Colon,
    Comma,
    Dot,
    ArrowLeft,  // <-
    ArrowRight, // ->
    Dash,       // -
    Star,       // *
    DotDot,     // ..
    Eq,         // =
    NotEq,      // != or <>
    Gt,         // >
    Gte,        // >=
    Lt,         // <
    Lte,        // <=
    Ident(String),
    StringLiteral(String),
    NumberLiteral(f64),
}

pub fn tokenize(input: &str) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let n = chars.len();

    while i < n {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        match c {
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            '[' => {
                tokens.push(Token::LBracket);
                i += 1;
            }
            ']' => {
                tokens.push(Token::RBracket);
                i += 1;
            }
            '{' => {
                tokens.push(Token::LBrace);
                i += 1;
            }
            '}' => {
                tokens.push(Token::RBrace);
                i += 1;
            }
            ':' => {
                tokens.push(Token::Colon);
                i += 1;
            }
            ',' => {
                tokens.push(Token::Comma);
                i += 1;
            }
            '*' => {
                tokens.push(Token::Star);
                i += 1;
            }
            '=' => {
                tokens.push(Token::Eq);
                i += 1;
            }
            '!' if i + 1 < n && chars[i + 1] == '=' => {
                tokens.push(Token::NotEq);
                i += 2;
            }
            '<' => {
                if i + 1 < n && chars[i + 1] == '-' {
                    tokens.push(Token::ArrowLeft);
                    i += 2;
                } else if i + 1 < n && chars[i + 1] == '>' {
                    tokens.push(Token::NotEq);
                    i += 2;
                } else if i + 1 < n && chars[i + 1] == '=' {
                    tokens.push(Token::Lte);
                    i += 2;
                } else {
                    tokens.push(Token::Lt);
                    i += 1;
                }
            }
            '>' => {
                if i + 1 < n && chars[i + 1] == '=' {
                    tokens.push(Token::Gte);
                    i += 2;
                } else {
                    tokens.push(Token::Gt);
                    i += 1;
                }
            }
            '-' => {
                if i + 1 < n && chars[i + 1] == '>' {
                    tokens.push(Token::ArrowRight);
                    i += 2;
                } else {
                    tokens.push(Token::Dash);
                    i += 1;
                }
            }
            '.' => {
                if i + 1 < n && chars[i + 1] == '.' {
                    tokens.push(Token::DotDot);
                    i += 2;
                } else {
                    tokens.push(Token::Dot);
                    i += 1;
                }
            }
            '"' | '\'' => {
                let quote = c;
                i += 1;
                let start = i;
                let mut escaped = false;
                let mut string_val = String::new();
                while i < n {
                    let sc = chars[i];
                    if escaped {
                        string_val.push(sc);
                        escaped = false;
                    } else if sc == '\\' {
                        escaped = true;
                    } else if sc == quote {
                        break;
                    } else {
                        string_val.push(sc);
                    }
                    i += 1;
                }
                if i >= n {
                    return Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!("Unterminated string literal at position {start}"),
                        false,
                    ));
                }
                i += 1; // skip closing quote
                tokens.push(Token::StringLiteral(string_val));
            }
            _ if c.is_ascii_digit() => {
                let start = i;
                while i < n && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let num_str: String = chars[start..i].iter().collect();
                let num: f64 = num_str.parse().map_err(|e| {
                    LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!("Invalid number '{num_str}': {e}"),
                        false,
                    )
                })?;
                tokens.push(Token::NumberLiteral(num));
            }
            _ if c.is_alphanumeric() || c == '_' || c == '$' => {
                let start = i;
                while i < n && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$') {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                match word.to_uppercase().as_str() {
                    "MATCH" => tokens.push(Token::Match),
                    "WHERE" => tokens.push(Token::Where),
                    "RETURN" => tokens.push(Token::Return),
                    "LIMIT" => tokens.push(Token::Limit),
                    "AND" => tokens.push(Token::And),
                    "OR" => tokens.push(Token::Or),
                    "NOT" => tokens.push(Token::Not),
                    "TRUE" => tokens.push(Token::True),
                    "FALSE" => tokens.push(Token::False),
                    "NULL" => tokens.push(Token::Null),
                    _ => tokens.push(Token::Ident(word)),
                }
            }
            _ => {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("Unexpected character '{c}' in Cypher query"),
                    false,
                ));
            }
        }
    }

    Ok(tokens)
}
