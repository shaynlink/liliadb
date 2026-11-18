use serde::{Deserialize, Serialize};

/// An Abstract Syntax Tree (AST) node for index expressions and filter predicates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Expr {
    /// A JSON path reference (e.g. `$.email`, `$.user.address.city`, `items[0]`, `status`).
    Path(String),
    /// A literal constant value (string, integer, float, bool, or null).
    Literal(LiteralValue),
    /// A function invocation with an identifier name and argument expressions.
    FunctionCall { name: String, args: Vec<Expr> },
    /// A binary operation (e.g. comparisons like `==`, `!=`, `<`, `>` or logical operators `AND`, `OR`).
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// A unary operation (e.g. `NOT`, unary negation `-`).
    Unary { op: UnaryOp, expr: Box<Expr> },
    /// Special predicate checking if an expression evaluates to NULL or is missing.
    IsNull(Box<Expr>),
    /// Special predicate checking if an expression evaluates to a non-NULL value.
    IsNotNull(Box<Expr>),
}

/// Binary operators supported in expressions and filter predicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryOp {
    Eq,
    NotEq,
    Lt,
    Lte,
    Gt,
    Gte,
    And,
    Or,
    Add,
    Sub,
    Mul,
    Div,
}

/// Unary operators supported in expressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnaryOp {
    Not,
    Neg,
}

/// Literal values that can appear in expressions or filter predicates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LiteralValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
}

impl From<&str> for LiteralValue {
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}

impl From<String> for LiteralValue {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<bool> for LiteralValue {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<i64> for LiteralValue {
    fn from(i: i64) -> Self {
        Self::Int(i)
    }
}

impl From<f64> for LiteralValue {
    fn from(f: f64) -> Self {
        Self::Float(f)
    }
}

/// The evaluated outcome of executing an expression on a document.
#[derive(Debug, Clone, PartialEq)]
pub enum EvaluatedValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    /// For multi-key indexing (e.g. if the expression targeted an array of scalar tags).
    List(Vec<String>),
}

impl EvaluatedValue {
    #[must_use]
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Bool(b) => *b,
            Self::Null => false,
            Self::Int(i) => *i != 0,
            Self::Float(f) => *f != 0.0 && !f.is_nan(),
            Self::String(s) => !s.is_empty(),
            Self::List(l) => !l.is_empty(),
        }
    }

    #[must_use]
    pub fn to_index_strings(&self) -> Vec<String> {
        match self {
            Self::Null => vec!["null".to_string()],
            Self::Bool(b) => vec![b.to_string()],
            Self::Int(i) => vec![i.to_string()],
            Self::Float(f) => vec![f.to_string()],
            Self::String(s) => vec![s.clone()],
            Self::List(items) => items.clone(),
        }
    }
}
