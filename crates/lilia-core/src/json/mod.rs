pub mod binary;
pub mod expr;
pub mod simd;

pub use binary::{
    BinaryArrayRef, BinaryDoc, BinaryDocBuilder, BinaryObjectRef, BinaryValueRef, MAGIC, MAGIC_ZSTD,
};
pub use expr::{
    eval_check, eval_expr, eval_filter_predicate, parse_expr, referenced_paths, validate_check_expr,
    BinaryOp, CheckOutcome, EvaluatedValue, Expr, LiteralValue, UnaryOp,
};
pub use simd::{parse_json_bytes_to_binary, parse_json_slice_to_binary};
