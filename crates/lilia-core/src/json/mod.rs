pub mod archived_value;
pub mod binary;
pub mod expr;
pub mod jit;
pub mod simd;

pub use archived_value::{
    access_archived_doc, access_archived_doc_unchecked, ArchivedFlatNode, ArchivedJsonDocument,
    ArchivedJsonView, FlatNode, JsonDocument,
};
pub use binary::{
    BinaryArrayRef, BinaryDoc, BinaryDocBuilder, BinaryObjectRef, BinaryValueRef, MAGIC,
    MAGIC_ZSTD, MAGIC_ZSTD_V1,
};
pub use expr::{
    eval_check, eval_expr, eval_filter_predicate, parse_expr, referenced_paths,
    validate_check_expr, BinaryOp, CheckOutcome, EvaluatedValue, Expr, LiteralValue, UnaryOp,
};
pub use jit::{
    can_jit_expr, compile_predicate, eval_filter_predicate_jit, CompiledPredicate, JitCompiler,
    JitPredicateCache, JitStats, DEFAULT_JIT_CACHE_CAPACITY,
};
pub use simd::{parse_json_bytes_to_binary, parse_json_slice_to_binary};
