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
    Eq,    // == or =
    NotEq, // != or <>
    Lt,    // <
    Lte,   // <=
    Gt,    // >
    Gte,   // >=
    And,   // AND or &&
    Or,    // OR or ||
    Not,   // NOT or !
    Plus,  // +
    Minus, // -
    Star,  // *
    Slash, // /
    Is,    // IS
}

fn tokenize(input: &str) -> Result<Vec<Token>> {
    let mut lexer = Lexer {
        chars: input.chars().collect(),
        pos: 0,
    };
    let mut tokens = Vec::new();
    while let Some(ch) = lexer.peek() {
        if ch.is_whitespace() {
            lexer.pos += 1;
            continue;
        }
        let token = if let Some(symbol) = lexer.symbol(ch)? {
            symbol
        } else if ch == '\'' || ch == '"' {
            lexer.string_literal(ch)?
        } else if ch == '$' {
            // JSON path starting with '$'
            Token::Path(lexer.take_while(|c| {
                c.is_alphanumeric() || matches!(c, '$' | '.' | '_' | '[' | ']' | '-')
            }))
        } else if ch.is_ascii_digit() {
            lexer.number()?
        } else if ch.is_alphabetic() || ch == '_' {
            lexer.word()
        } else {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!("unexpected character '{ch}' at position {}", lexer.pos),
                false,
            ));
        };
        tokens.push(token);
    }
    Ok(tokens)
}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
}

impl Lexer {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next_is(&self, expected: char) -> bool {
        self.chars.get(self.pos + 1) == Some(&expected)
    }

    fn take_while(&mut self, accept: impl Fn(char) -> bool) -> String {
        let start = self.pos;
        while self.peek().is_some_and(&accept) {
            self.pos += 1;
        }
        self.chars[start..self.pos].iter().collect()
    }

    /// Consumes `width` characters and returns `token`.
    fn emit(&mut self, width: usize, token: Token) -> Token {
        self.pos += width;
        token
    }

    /// Punctuation and operators starting with `ch`, `None` for any other character.
    fn symbol(&mut self, ch: char) -> Result<Option<Token>> {
        let token = match ch {
            '(' => self.emit(1, Token::OpenParen),
            ')' => self.emit(1, Token::CloseParen),
            ',' => self.emit(1, Token::Comma),
            '+' => self.emit(1, Token::Plus),
            '-' => self.emit(1, Token::Minus),
            '*' => self.emit(1, Token::Star),
            // A JSON pointer like `/user/name` is a path, a lone `/` a division.
            '/' if self
                .chars
                .get(self.pos + 1)
                .is_some_and(|c| c.is_alphanumeric() || *c == '_') =>
            {
                Token::Path(self.take_while(|c| {
                    c.is_alphanumeric() || matches!(c, '_' | '/' | '.' | '[' | ']')
                }))
            }
            '/' => self.emit(1, Token::Slash),
            '!' if self.next_is('=') => self.emit(2, Token::NotEq),
            '!' => self.emit(1, Token::Not),
            '=' if self.next_is('=') => self.emit(2, Token::Eq),
            '=' => self.emit(1, Token::Eq),
            '<' if self.next_is('=') => self.emit(2, Token::Lte),
            '<' if self.next_is('>') => self.emit(2, Token::NotEq),
            '<' => self.emit(1, Token::Lt),
            '>' if self.next_is('=') => self.emit(2, Token::Gte),
            '>' => self.emit(1, Token::Gt),
            '&' if self.next_is('&') => self.emit(2, Token::And),
            '|' if self.next_is('|') => self.emit(2, Token::Or),
            '&' | '|' => {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("unexpected character '{ch}' at pos {}", self.pos),
                    false,
                ));
            }
            _ => return Ok(None),
        };
        Ok(Some(token))
    }

    /// A 'single' or "double" quoted string, with `\n`, `\r`, `\t` and `\\` escapes.
    fn string_literal(&mut self, quote: char) -> Result<Token> {
        self.pos += 1;
        let mut s = String::new();
        let mut escaped = false;
        while let Some(c) = self.peek() {
            self.pos += 1;
            if escaped {
                s.push(match c {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    other => other,
                });
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == quote {
                return Ok(Token::Str(s));
            } else {
                s.push(c);
            }
        }
        Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "unterminated string literal in index expression",
            false,
        ))
    }

    fn number(&mut self) -> Result<Token> {
        let start = self.pos;
        let mut is_float = false;
        while let Some(c) = self.peek() {
            if c == '.' {
                // A second dot, or a dot not followed by a digit (a path segment), ends the number.
                if is_float
                    || !self
                        .chars
                        .get(self.pos + 1)
                        .is_some_and(char::is_ascii_digit)
                {
                    break;
                }
                is_float = true;
            } else if !c.is_ascii_digit() {
                break;
            }
            self.pos += 1;
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        if is_float {
            text.parse().map(Token::Float).map_err(|e| {
                LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("invalid float: {e}"),
                    false,
                )
            })
        } else {
            text.parse().map(Token::Int).map_err(|e| {
                LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("invalid integer: {e}"),
                    false,
                )
            })
        }
    }

    /// A keyword, a function name (followed by `(`) or a field path like `user.email`.
    fn word(&mut self) -> Token {
        let word = self.take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '[' | ']'));
        let is_path = word.contains('.') || word.contains('[');
        if !is_path {
            match word.to_ascii_uppercase().as_str() {
                "TRUE" => return Token::Bool(true),
                "FALSE" => return Token::Bool(false),
                "NULL" => return Token::Null,
                "AND" => return Token::And,
                "OR" => return Token::Or,
                "NOT" => return Token::Not,
                "IS" => return Token::Is,
                _ => {}
            }
        }
        if self.peek() == Some('(') {
            Token::Ident(word)
        } else {
            // Standalone identifiers are field paths too (e.g. `priority`).
            Token::Path(word)
        }
    }
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
///
/// # Errors
/// `InvalidInput` when `input` is empty or not a valid expression.
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
