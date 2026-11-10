use std::collections::HashMap;

use lilia_core::{Direction, ErrorCode, LiliaError, Result};

use super::ast::{
    CmpOp, CypherQuery, EdgePattern, FieldRef, NodePattern, PatternChain, ReturnClause, ReturnItem,
    WhereExpr,
};
use super::lexer::Token;

pub fn parse(tokens: &[Token]) -> Result<CypherQuery> {
    let mut parser = Parser::new(tokens);
    parser.parse_query()
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [Token]) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<&Token> {
        let tok = self.tokens.get(self.pos);
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    fn expect(&mut self, expected: &Token) -> Result<()> {
        match self.next() {
            Some(tok) if tok == expected => Ok(()),
            Some(tok) => Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!("Expected {expected:?}, got {tok:?}"),
                false,
            )),
            None => Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!("Expected {expected:?}, but reached end of input"),
                false,
            )),
        }
    }

    fn parse_query(&mut self) -> Result<CypherQuery> {
        self.expect(&Token::Match)?;
        let pattern = self.parse_pattern_chain()?;

        let where_clause = if self.peek() == Some(&Token::Where) {
            self.next();
            Some(self.parse_where_expr()?)
        } else {
            None
        };

        self.expect(&Token::Return)?;
        let return_clause = self.parse_return_clause()?;

        let limit = if self.peek() == Some(&Token::Limit) {
            self.next();
            match self.next() {
                Some(Token::NumberLiteral(n)) => Some(*n as usize),
                Some(other) => {
                    return Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!("Expected number after LIMIT, got {other:?}"),
                        false,
                    ))
                }
                None => {
                    return Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        "Expected number after LIMIT",
                        false,
                    ))
                }
            }
        } else {
            None
        };

        Ok(CypherQuery {
            pattern,
            where_clause,
            return_clause,
            limit,
        })
    }

    fn parse_pattern_chain(&mut self) -> Result<PatternChain> {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        nodes.push(self.parse_node_pattern()?);

        while self.is_edge_start() {
            let edge = self.parse_edge_pattern()?;
            edges.push(edge);
            nodes.push(self.parse_node_pattern()?);
        }

        Ok(PatternChain { nodes, edges })
    }

    fn is_edge_start(&self) -> bool {
        matches!(self.peek(), Some(Token::Dash) | Some(Token::ArrowLeft))
    }

    fn parse_node_pattern(&mut self) -> Result<NodePattern> {
        self.expect(&Token::LParen)?;

        let mut variable = None;
        let mut label = None;
        let mut properties = HashMap::new();

        if let Some(Token::Ident(name)) = self.peek() {
            variable = Some(name.clone());
            self.next();
        }

        if self.peek() == Some(&Token::Colon) {
            self.next();
            if let Some(Token::Ident(lbl)) = self.next() {
                label = Some(lbl.clone());
            } else {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "Expected node label after ':'",
                    false,
                ));
            }
        }

        if self.peek() == Some(&Token::LBrace) {
            self.next();
            while self.peek() != Some(&Token::RBrace) {
                let key = if let Some(Token::Ident(k)) = self.next() {
                    k.clone()
                } else {
                    return Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        "Expected property key in node pattern",
                        false,
                    ));
                };
                self.expect(&Token::Colon)?;
                let val = self.parse_literal_value()?;
                properties.insert(key, val);

                if self.peek() == Some(&Token::Comma) {
                    self.next();
                } else {
                    break;
                }
            }
            self.expect(&Token::RBrace)?;
        }

        self.expect(&Token::RParen)?;

        Ok(NodePattern {
            variable,
            label,
            properties,
        })
    }

    fn parse_edge_pattern(&mut self) -> Result<EdgePattern> {
        let incoming = if self.peek() == Some(&Token::ArrowLeft) {
            self.next();
            true
        } else {
            self.expect(&Token::Dash)?;
            false
        };

        let mut variable = None;
        let mut kind = None;
        let mut min_hops = 1;
        let mut max_hops = 1;

        if self.peek() == Some(&Token::LBracket) {
            self.next();

            if let Some(Token::Ident(var)) = self.peek() {
                variable = Some(var.clone());
                self.next();
            }

            if self.peek() == Some(&Token::Colon) {
                self.next();
                if let Some(Token::Ident(k)) = self.next() {
                    kind = Some(k.clone());
                } else {
                    return Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        "Expected edge kind after ':'",
                        false,
                    ));
                }
            }

            if self.peek() == Some(&Token::Star) {
                self.next();
                if let Some(Token::NumberLiteral(min_h)) = self.peek() {
                    min_hops = *min_h as usize;
                    self.next();
                    if self.peek() == Some(&Token::DotDot) {
                        self.next();
                        if let Some(Token::NumberLiteral(max_h)) = self.next() {
                            max_hops = *max_h as usize;
                        } else {
                            max_hops = 10;
                        }
                    } else {
                        max_hops = min_hops;
                    }
                } else {
                    min_hops = 1;
                    max_hops = 5;
                }
            }

            self.expect(&Token::RBracket)?;
        }

        let direction = if incoming {
            self.expect(&Token::Dash)?;
            Direction::Incoming
        } else if self.peek() == Some(&Token::ArrowRight) {
            self.next();
            Direction::Outgoing
        } else {
            self.expect(&Token::Dash)?;
            Direction::Both
        };

        Ok(EdgePattern {
            variable,
            kind,
            direction,
            min_hops,
            max_hops,
        })
    }

    fn parse_where_expr(&mut self) -> Result<WhereExpr> {
        let left = self.parse_comparison_expr()?;

        if self.peek() == Some(&Token::And) {
            self.next();
            let right = self.parse_where_expr()?;
            Ok(WhereExpr::And(Box::new(left), Box::new(right)))
        } else if self.peek() == Some(&Token::Or) {
            self.next();
            let right = self.parse_where_expr()?;
            Ok(WhereExpr::Or(Box::new(left), Box::new(right)))
        } else {
            Ok(left)
        }
    }

    fn parse_comparison_expr(&mut self) -> Result<WhereExpr> {
        let target = self.parse_field_ref()?;

        let op = match self.next() {
            Some(Token::Eq) => CmpOp::Eq,
            Some(Token::NotEq) => CmpOp::NotEq,
            Some(Token::Gt) => CmpOp::Gt,
            Some(Token::Gte) => CmpOp::Gte,
            Some(Token::Lt) => CmpOp::Lt,
            Some(Token::Lte) => CmpOp::Lte,
            Some(other) => {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("Expected comparison operator in WHERE, got {other:?}"),
                    false,
                ))
            }
            None => {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "Expected comparison operator in WHERE clause",
                    false,
                ))
            }
        };

        let value = self.parse_literal_value()?;

        Ok(WhereExpr::Comparison { target, op, value })
    }

    fn parse_field_ref(&mut self) -> Result<FieldRef> {
        let var = if let Some(Token::Ident(name)) = self.next() {
            name.clone()
        } else {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "Expected identifier in WHERE expression",
                false,
            ));
        };

        let field = if self.peek() == Some(&Token::Dot) {
            self.next();
            if let Some(Token::Ident(f)) = self.next() {
                Some(f.clone())
            } else {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "Expected field name after '.'",
                    false,
                ));
            }
        } else {
            None
        };

        Ok(FieldRef {
            variable: var,
            field,
        })
    }

    fn parse_return_clause(&mut self) -> Result<ReturnClause> {
        let mut items = Vec::new();

        loop {
            let target = self.parse_field_ref()?;
            let mut alias = None;

            if let Some(Token::Ident(keyword)) = self.peek() {
                if keyword.eq_ignore_ascii_case("AS") {
                    self.next();
                    if let Some(Token::Ident(al)) = self.next() {
                        alias = Some(al.clone());
                    }
                }
            }

            items.push(ReturnItem { target, alias });

            if self.peek() == Some(&Token::Comma) {
                self.next();
            } else {
                break;
            }
        }

        Ok(ReturnClause { items })
    }

    fn parse_literal_value(&mut self) -> Result<serde_json::Value> {
        match self.next() {
            Some(Token::StringLiteral(s)) => Ok(serde_json::Value::String(s.clone())),
            Some(Token::NumberLiteral(n)) => {
                if n.fract() == 0.0 {
                    Ok(serde_json::json!(*n as i64))
                } else {
                    Ok(serde_json::json!(*n))
                }
            }
            Some(Token::True) => Ok(serde_json::Value::Bool(true)),
            Some(Token::False) => Ok(serde_json::Value::Bool(false)),
            Some(Token::Null) => Ok(serde_json::Value::Null),
            Some(other) => Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!("Expected literal value, got {other:?}"),
                false,
            )),
            None => Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "Expected literal value",
                false,
            )),
        }
    }
}
