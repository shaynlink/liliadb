pub mod ast;
pub mod executor;
pub mod lexer;
pub mod parser;
pub mod result;

pub use result::CypherResult;

use lilia_core::Result;

use crate::GraphStore;

pub fn execute_cypher(store: &GraphStore<'_>, query: &str) -> Result<CypherResult> {
    let tokens = lexer::tokenize(query)?;
    let parsed = parser::parse(&tokens)?;
    executor::execute(store, &parsed)
}
