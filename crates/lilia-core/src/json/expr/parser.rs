use super::ast::{BinaryOp, Expr, LiteralValue, UnaryOp};
use crate::{ErrorCode, LiliaError, Result};

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    Path(String),
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
    // Symbols
    OpenParen,
    CloseParen,
    Comma,
    // Binary & comparison operators
    Eq,      // == or =
    NotEq,   // != or <>
    Lt,      // <
    Lte,     // <=
    Gt,      // >
    Gte,     // >=
    And,     // AND or &&
    Or,      // OR or ||
    Not,     // NOT or !
    Plus,    // +
    Minus,   // -
    Star,    // *
    Slash,   // /
    Is,      // IS
}

fn tokenize(input: &str) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let ch = chars[i];

        if ch.is_whitespace() {
            i += 1;
            continue;
        }

        // Parentheses and punctuation
        match ch {
            '(' => {
                tokens.push(Token::OpenParen);
                i += 1;
                continue;
            }
            ')' => {
                tokens.push(Token::CloseParen);
                i += 1;
                continue;
            }
            ',' => {
                tokens.push(Token::Comma);
                i += 1;
                continue;
            }
            '+' => {
                tokens.push(Token::Plus);
                i += 1;
                continue;
            }
            '-' => {
                tokens.push(Token::Minus);
                i += 1;
                continue;
            }
            '*' => {
                tokens.push(Token::Star);
                i += 1;
                continue;
            }
            '/' => {
                // If it's a JSON pointer like `/user/name`, tokenize as path
                if i + 1 < len && (chars[i + 1].is_alphanumeric() || chars[i + 1] == '_') {
                    let mut path = String::new();
                    while i < len
                        && (chars[i].is_alphanumeric()
                            || chars[i] == '_'
                            || chars[i] == '/'
                            || chars[i] == '.'
                            || chars[i] == '['
                            || chars[i] == ']')
                    {
                        path.push(chars[i]);
                        i += 1;
                    }
                    tokens.push(Token::Path(path));
                    continue;
                }
                tokens.push(Token::Slash);
                i += 1;
                continue;
            }
            '!' => {
                if i + 1 < len && chars[i + 1] == '=' {
                    tokens.push(Token::NotEq);
                    i += 2;
                } else {
                    tokens.push(Token::Not);
                    i += 1;
                }
                continue;
            }
            '=' => {
                if i + 1 < len && chars[i + 1] == '=' {
                    tokens.push(Token::Eq);
                    i += 2;
                } else {
                    tokens.push(Token::Eq);
                    i += 1;
                }
                continue;
            }
            '<' => {
                if i + 1 < len && chars[i + 1] == '=' {
                    tokens.push(Token::Lte);
                    i += 2;
                } else if i + 1 < len && chars[i + 1] == '>' {
                    tokens.push(Token::NotEq);
                    i += 2;
                } else {
                    tokens.push(Token::Lt);
                    i += 1;
                }
                continue;
            }
            '>' => {
                if i + 1 < len && chars[i + 1] == '=' {
                    tokens.push(Token::Gte);
                    i += 2;
                } else {
                    tokens.push(Token::Gt);
                    i += 1;
                }
                continue;
            }
            '&' => {
                if i + 1 < len && chars[i + 1] == '&' {
                    tokens.push(Token::And);
                    i += 2;
                    continue;
                }
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("unexpected character '&' at pos {i}"),
                    false,
                ));
            }
            '|' => {
                if i + 1 < len && chars[i + 1] == '|' {
                    tokens.push(Token::Or);
                    i += 2;
                    continue;
                }
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("unexpected character '|' at pos {i}"),
                    false,
                ));
            }
            _ => {}
        }

        // String literals: 'single' or "double"
        if ch == '\'' || ch == '"' {
            let quote = ch;
            i += 1;
            let mut s = String::new();
            let mut escaped = false;
            let mut closed = false;
            while i < len {
                let c = chars[i];
                if escaped {
                    match c {
                        'n' => s.push('\n'),
                        'r' => s.push('\r'),
                        't' => s.push('\t'),
                        '\\' => s.push('\\'),
                        _ => s.push(c),
                    }
                    escaped = false;
                    i += 1;
                } else if c == '\\' {
                    escaped = true;
                    i += 1;
                } else if c == quote {
                    closed = true;
                    i += 1;
                    break;
                } else {
                    s.push(c);
                    i += 1;
                }
            }
            if !closed {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "unterminated string literal in index expression",
                    false,
                ));
            }
            tokens.push(Token::Str(s));
            continue;
        }

        // JSON path starting with '$'
        if ch == '$' {
            let mut path = String::new();
            while i < len
                && (chars[i].is_alphanumeric()
                    || chars[i] == '$'
                    || chars[i] == '.'
                    || chars[i] == '_'
                    || chars[i] == '['
                    || chars[i] == ']'
                    || chars[i] == '-')
            {
                path.push(chars[i]);
                i += 1;
            }
            tokens.push(Token::Path(path));
            continue;
        }

        // Numeric literal
        if ch.is_ascii_digit() {
            let mut num_str = String::new();
            let mut is_float = false;
            while i < len && (chars[i].is_ascii_digit() || chars[i] == '.') {
                if chars[i] == '.' {
                    if is_float {
                        break;
                    }
                    // Avoid consuming dot if it's followed by a letter (like path segment)
                    if i + 1 < len && !chars[i + 1].is_ascii_digit() {
                        break;
                    }
                    is_float = true;
                }
                num_str.push(chars[i]);
                i += 1;
            }
            if is_float {
                let f: f64 = num_str.parse().map_err(|e| {
                    LiliaError::new(ErrorCode::InvalidInput, format!("invalid float: {e}"), false)
                })?;
                tokens.push(Token::Float(f));
            } else {
                let int_val: i64 = num_str.parse().map_err(|e| {
                    LiliaError::new(ErrorCode::InvalidInput, format!("invalid integer: {e}"), false)
                })?;
                tokens.push(Token::Int(int_val));
            }
            continue;
        }

        // Word (keyword, identifier, function name, or path like `user.email`)
        if ch.is_alphabetic() || ch == '_' {
            let mut word = String::new();
            while i < len
                && (chars[i].is_alphanumeric()
                    || chars[i] == '_'
                    || chars[i] == '.'
                    || chars[i] == '['
                    || chars[i] == ']')
            {
                word.push(chars[i]);
                i += 1;
            }

            // Check keywords if it does not contain '.' or '['
            if !word.contains('.') && !word.contains('[') {
                match word.to_ascii_uppercase().as_str() {
                    "TRUE" => {
                        tokens.push(Token::Bool(true));
                        continue;
                    }
                    "FALSE" => {
                        tokens.push(Token::Bool(false));
                        continue;
                    }
                    "NULL" => {
                        tokens.push(Token::Null);
                        continue;
                    }
                    "AND" => {
                        tokens.push(Token::And);
                        continue;
                    }
                    "OR" => {
                        tokens.push(Token::Or);
                        continue;
                    }
                    "NOT" => {
                        tokens.push(Token::Not);
                        continue;
                    }
                    "IS" => {
                        tokens.push(Token::Is);
                        continue;
                    }
                    _ => {}
                }
            }

            // If it's followed immediately by '(', it is a function identifier!
            // Even if word was `json_array_length` or `lower`
            if i < len && chars[i] == '(' {
                tokens.push(Token::Ident(word));
            } else if word.contains('.') || word.contains('[') {
                tokens.push(Token::Path(word));
            } else {
                // Standalone identifier treated as field path (e.g. `priority`)
                tokens.push(Token::Path(word));
            }
            continue;
        }

        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            format!("unexpected character '{ch}' at position {i}"),
            false,
        ));
    }

    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<Token> {
        if self.pos < self.tokens.len() {
            let tok = self.tokens[self.pos].clone();
            self.pos += 1;
            Some(tok)
        } else {
            None
        }
    }

    fn parse_expression(&mut self) -> Result<Expr> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr> {
        let mut left = self.parse_and()?;
        while let Some(Token::Or) = self.peek() {
            self.advance();
            let right = self.parse_and()?;
            left = Expr::Binary {
                op: BinaryOp::Or,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr> {
        let mut left = self.parse_comparison()?;
        while let Some(Token::And) = self.peek() {
            self.advance();
            let right = self.parse_comparison()?;
            left = Expr::Binary {
                op: BinaryOp::And,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<Expr> {
        let left = self.parse_term()?;

        // Check for IS / IS NOT NULL
        if let Some(Token::Is) = self.peek() {
            self.advance();
            if let Some(Token::Not) = self.peek() {
                self.advance();
                if let Some(Token::Null) = self.peek() {
                    self.advance();
                    return Ok(Expr::IsNotNull(Box::new(left)));
                }
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "expected NULL after IS NOT",
                    false,
                ));
            } else if let Some(Token::Null) = self.peek() {
                self.advance();
                return Ok(Expr::IsNull(Box::new(left)));
            }
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "expected NULL or NOT NULL after IS",
                false,
            ));
        }

        let op = match self.peek() {
            Some(Token::Eq) => Some(BinaryOp::Eq),
            Some(Token::NotEq) => Some(BinaryOp::NotEq),
            Some(Token::Lt) => Some(BinaryOp::Lt),
            Some(Token::Lte) => Some(BinaryOp::Lte),
            Some(Token::Gt) => Some(BinaryOp::Gt),
            Some(Token::Gte) => Some(BinaryOp::Gte),
            _ => None,
        };

        if let Some(bin_op) = op {
            self.advance();
            let right = self.parse_term()?;
            Ok(Expr::Binary {
                op: bin_op,
                left: Box::new(left),
                right: Box::new(right),
            })
        } else {
            Ok(left)
        }
    }

    fn parse_term(&mut self) -> Result<Expr> {
        let mut left = self.parse_factor()?;
        while let Some(op) = match self.peek() {
            Some(Token::Plus) => Some(BinaryOp::Add),
            Some(Token::Minus) => Some(BinaryOp::Sub),
            _ => None,
        } {
            self.advance();
            let right = self.parse_factor()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_factor(&mut self) -> Result<Expr> {
        let mut left = self.parse_unary()?;
        while let Some(op) = match self.peek() {
            Some(Token::Star) => Some(BinaryOp::Mul),
            Some(Token::Slash) => Some(BinaryOp::Div),
            _ => None,
        } {
            self.advance();
            let right = self.parse_unary()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        if let Some(Token::Not) = self.peek() {
            self.advance();
            let expr = self.parse_unary()?;
            return Ok(Expr::Unary {
                op: UnaryOp::Not,
                expr: Box::new(expr),
            });
        }
        if let Some(Token::Minus) = self.peek() {
            self.advance();
            let expr = self.parse_unary()?;
            return Ok(Expr::Unary {
                op: UnaryOp::Neg,
                expr: Box::new(expr),
            });
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        let Some(tok) = self.advance() else {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "unexpected end of expression",
                false,
            ));
        };

        match tok {
            Token::OpenParen => {
                let inner = self.parse_expression()?;
                if let Some(Token::CloseParen) = self.advance() {
                    Ok(inner)
                } else {
                    Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        "unmatched parenthesis in expression",
                        false,
                    ))
                }
            }
            Token::Path(p) => Ok(Expr::Path(p)),
            Token::Str(s) => Ok(Expr::Literal(LiteralValue::String(s))),
            Token::Int(i) => Ok(Expr::Literal(LiteralValue::Int(i))),
            Token::Float(f) => Ok(Expr::Literal(LiteralValue::Float(f))),
            Token::Bool(b) => Ok(Expr::Literal(LiteralValue::Bool(b))),
            Token::Null => Ok(Expr::Literal(LiteralValue::Null)),
            Token::Ident(name) => {
                // Must be function call: expect '('
                if let Some(Token::OpenParen) = self.advance() {
                    let mut args = Vec::new();
                    if let Some(Token::CloseParen) = self.peek() {
                        self.advance();
                        return Ok(Expr::FunctionCall { name, args });
                    }
                    loop {
                        let arg = self.parse_expression()?;
                        args.push(arg);
                        match self.peek() {
                            Some(Token::Comma) => {
                                self.advance();
                            }
                            Some(Token::CloseParen) => {
                                self.advance();
                                break;
                            }
                            _ => {
                                return Err(LiliaError::new(
                                    ErrorCode::InvalidInput,
                                    format!("expected ',' or ')' in function call '{name}'"),
                                    false,
                                ));
                            }
                        }
                    }
                    Ok(Expr::FunctionCall { name, args })
                } else {
                    // Treat standalone ident as path
                    Ok(Expr::Path(name))
                }
            }
            other => Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!("unexpected token '{other:?}' in expression"),
                false,
            )),
        }
    }
}

/// Parse an index expression or a filter predicate string into an AST.
pub fn parse_expr(input: &str) -> Result<Expr> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "expression cannot be empty",
            false,
        ));
    }
    let tokens = tokenize(trimmed)?;
    let mut parser = Parser::new(tokens);
    let ast = parser.parse_expression()?;
    if parser.pos < parser.tokens.len() {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            format!(
                "unparsed tokens remaining at end of expression: {:?}",
                &parser.tokens[parser.pos..]
            ),
            false,
        ));
    }
    Ok(ast)
}
