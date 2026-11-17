//! Native Query Compilation via Cranelift (LiliaDB JIT).
//!
//! Compiles query predicates directly into host machine code (x86_64 / ARM64)
//! executing in hardware CPU registers, achieving 5x to 20x faster filtering
//! compared to recursive AST interpretation.

#[cfg(all(not(target_arch = "wasm32"), feature = "jit"))]
pub mod cache;
#[cfg(all(not(target_arch = "wasm32"), feature = "jit"))]
pub mod compiler;
#[cfg(all(not(target_arch = "wasm32"), feature = "jit"))]
pub mod helpers;
#[cfg(all(not(target_arch = "wasm32"), feature = "jit"))]
pub mod predicate;

#[cfg(all(not(target_arch = "wasm32"), feature = "jit"))]
pub use cache::{JitPredicateCache, JitStats, DEFAULT_JIT_CACHE_CAPACITY};
#[cfg(all(not(target_arch = "wasm32"), feature = "jit"))]
pub use compiler::{can_jit_expr, JitCompiler};
#[cfg(all(not(target_arch = "wasm32"), feature = "jit"))]
pub use predicate::CompiledPredicate;

#[cfg(all(not(target_arch = "wasm32"), feature = "jit"))]
use crate::json::{BinaryDoc, Expr};
#[cfg(all(not(target_arch = "wasm32"), feature = "jit"))]
use crate::Result;

/// Compiles an AST filter expression into a native machine code predicate.
#[cfg(all(not(target_arch = "wasm32"), feature = "jit"))]
pub fn compile_predicate(expr: &Expr) -> Result<CompiledPredicate> {
    let compiler = JitCompiler::new()?;
    compiler.compile(expr)
}

/// Evaluates a filter predicate using native JIT compilation if supported,
/// otherwise falling back to interpreted evaluation.
#[cfg(all(not(target_arch = "wasm32"), feature = "jit"))]
#[must_use]
pub fn eval_filter_predicate_jit(predicate: &Expr, doc: &BinaryDoc) -> bool {
    if can_jit_expr(predicate) {
        if let Ok(compiled) = compile_predicate(predicate) {
            return compiled.eval(doc);
        }
    }
    crate::json::eval_filter_predicate(predicate, doc)
}

// ---------------------------------------------------------------------------
// Fallback implementation for targets without JIT (e.g. wasm32 or disabled)
// ---------------------------------------------------------------------------

#[cfg(not(all(not(target_arch = "wasm32"), feature = "jit")))]
use crate::json::{BinaryDoc, Expr};
#[cfg(not(all(not(target_arch = "wasm32"), feature = "jit")))]
use crate::Result;

#[cfg(not(all(not(target_arch = "wasm32"), feature = "jit")))]
pub const DEFAULT_JIT_CACHE_CAPACITY: usize = 512;

#[cfg(not(all(not(target_arch = "wasm32"), feature = "jit")))]
#[derive(Debug, Default, Clone, Copy)]
pub struct JitStats {
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub compilations: u64,
}

#[cfg(not(all(not(target_arch = "wasm32"), feature = "jit")))]
#[derive(Debug, Default)]
pub struct JitCompiler;

#[cfg(not(all(not(target_arch = "wasm32"), feature = "jit")))]
impl JitCompiler {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }

    pub fn compile(&self, expr: &Expr) -> Result<CompiledPredicate> {
        compile_predicate(expr)
    }
}

#[cfg(not(all(not(target_arch = "wasm32"), feature = "jit")))]
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledPredicate {
    expr: Expr,
}

#[cfg(not(all(not(target_arch = "wasm32"), feature = "jit")))]
impl CompiledPredicate {
    #[must_use]
    pub fn eval(&self, doc: &BinaryDoc) -> bool {
        crate::json::eval_filter_predicate(&self.expr, doc)
    }

    #[must_use]
    pub fn eval_bytes(&self, bytes: &[u8]) -> bool {
        if let Ok(doc) = BinaryDoc::from_bytes(bytes.to_vec()) {
            self.eval(&doc)
        } else {
            false
        }
    }

    pub fn eval_batch(&self, docs: &[&BinaryDoc], matches: &mut Vec<usize>) {
        for (i, doc) in docs.iter().enumerate() {
            if self.eval(doc) {
                matches.push(i);
            }
        }
    }

    pub fn eval_batch_bytes(&self, bytes_slices: &[&[u8]], matches: &mut Vec<usize>) {
        for (i, bytes) in bytes_slices.iter().enumerate() {
            if self.eval_bytes(bytes) {
                matches.push(i);
            }
        }
    }
}

#[cfg(not(all(not(target_arch = "wasm32"), feature = "jit")))]
#[must_use]
pub fn can_jit_expr(_expr: &Expr) -> bool {
    false
}

#[cfg(not(all(not(target_arch = "wasm32"), feature = "jit")))]
pub fn compile_predicate(expr: &Expr) -> Result<CompiledPredicate> {
    Ok(CompiledPredicate { expr: expr.clone() })
}

#[cfg(not(all(not(target_arch = "wasm32"), feature = "jit")))]
#[must_use]
pub fn eval_filter_predicate_jit(predicate: &Expr, doc: &BinaryDoc) -> bool {
    crate::json::eval_filter_predicate(predicate, doc)
}

#[cfg(not(all(not(target_arch = "wasm32"), feature = "jit")))]
#[derive(Debug, Default, Clone)]
pub struct JitPredicateCache;

#[cfg(not(all(not(target_arch = "wasm32"), feature = "jit")))]
impl JitPredicateCache {
    #[must_use]
    pub fn new(_capacity: usize) -> Self {
        Self
    }

    pub fn get_or_compile(&self, expr: &Expr) -> Result<std::sync::Arc<CompiledPredicate>> {
        Ok(std::sync::Arc::new(CompiledPredicate {
            expr: expr.clone(),
        }))
    }
}
