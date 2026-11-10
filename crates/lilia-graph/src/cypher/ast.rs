use std::collections::HashMap;

use lilia_core::Direction;

#[derive(Debug, Clone, PartialEq)]
pub struct CypherQuery {
    pub pattern: PatternChain,
    pub where_clause: Option<WhereExpr>,
    pub return_clause: ReturnClause,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PatternChain {
    pub nodes: Vec<NodePattern>,
    pub edges: Vec<EdgePattern>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodePattern {
    pub variable: Option<String>,
    pub label: Option<String>,
    pub properties: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EdgePattern {
    pub variable: Option<String>,
    pub kind: Option<String>,
    pub direction: Direction,
    pub min_hops: usize,
    pub max_hops: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WhereExpr {
    And(Box<WhereExpr>, Box<WhereExpr>),
    Or(Box<WhereExpr>, Box<WhereExpr>),
    Comparison {
        target: FieldRef,
        op: CmpOp,
        value: serde_json::Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldRef {
    pub variable: String,
    pub field: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    NotEq,
    Gt,
    Gte,
    Lt,
    Lte,
    Contains,
    StartsWith,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReturnClause {
    pub items: Vec<ReturnItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReturnItem {
    pub target: FieldRef,
    pub alias: Option<String>,
}
