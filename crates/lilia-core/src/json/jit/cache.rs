use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::compiler::JitCompiler;
use super::predicate::CompiledPredicate;
use crate::json::Expr;
use crate::Result;

/// Maximum number of compiled predicates cached in memory per cache instance.
pub const DEFAULT_JIT_CACHE_CAPACITY: usize = 512;

/// Telemetry and diagnostics for the query JIT compiler and plan cache.
#[derive(Debug, Default, Clone, Copy)]
pub struct JitStats {
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub compilations: u64,
}

/// Thread-safe concurrent query plan cache for JIT-compiled native predicates.
pub struct JitPredicateCache {
    cache: RwLock<HashMap<String, Arc<CompiledPredicate>>>,
    capacity: usize,
    hits: AtomicU64,
    misses: AtomicU64,
    compilations: AtomicU64,
}

fn expr_cache_key(expr: &Expr) -> String {
    format!("{expr:?}")
}

impl Default for JitPredicateCache {
    fn default() -> Self {
        Self::new(DEFAULT_JIT_CACHE_CAPACITY)
    }
}

impl std::fmt::Debug for JitPredicateCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JitPredicateCache")
            .field("entries", &self.cache.read().len())
            .field("capacity", &self.capacity)
            .field("stats", &self.stats())
            .finish()
    }
}

impl JitPredicateCache {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            cache: RwLock::new(HashMap::with_capacity(capacity.min(64))),
            capacity: capacity.max(1),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            compilations: AtomicU64::new(0),
        }
    }

    /// Retrieves an already-compiled predicate or compiles and caches it.
    pub fn get_or_compile(&self, expr: &Expr) -> Result<Arc<CompiledPredicate>> {
        let key = expr_cache_key(expr);

        // Fast read lock path
        {
            let read = self.cache.read();
            if let Some(pred) = read.get(&key) {
                self.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(Arc::clone(pred));
            }
        }

        self.misses.fetch_add(1, Ordering::Relaxed);

        // Slow compile path
        let mut write = self.cache.write();
        if let Some(pred) = write.get(&key) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(Arc::clone(pred));
        }

        // Evict if cache exceeds capacity
        if write.len() >= self.capacity {
            write.clear();
        }

        let compiler = JitCompiler::new()?;
        let compiled = Arc::new(compiler.compile(expr)?);
        self.compilations.fetch_add(1, Ordering::Relaxed);

        write.insert(key, Arc::clone(&compiled));
        Ok(compiled)
    }

    /// Number of compiled predicates currently in cache.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cache.read().len()
    }

    /// Whether the cache is currently empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cache.read().is_empty()
    }

    /// Clears all cached compiled predicates.
    pub fn clear(&self) {
        self.cache.write().clear();
    }

    /// Returns compilation and lookup statistics.
    #[must_use]
    pub fn stats(&self) -> JitStats {
        JitStats {
            cache_hits: self.hits.load(Ordering::Relaxed),
            cache_misses: self.misses.load(Ordering::Relaxed),
            compilations: self.compilations.load(Ordering::Relaxed),
        }
    }
}
