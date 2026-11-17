#![allow(unsafe_code)]

use cranelift_jit::JITModule;
use std::sync::Arc;

use super::helpers::DocRef;
use crate::json::BinaryDoc;

/// A thread-safe, immutable compiled query predicate executing native ARM64 / x86_64 instructions.
#[derive(Clone)]
pub struct CompiledPredicate {
    pub(crate) eval_fn: unsafe extern "C" fn(*const u8, usize) -> u8,
    pub(crate) batch_fn: unsafe extern "C" fn(*const DocRef, usize, *mut u32) -> usize,
    pub(crate) _strings: Arc<Vec<String>>,
    pub(crate) _module: Arc<parking_lot::Mutex<JITModule>>,
}

impl std::fmt::Debug for CompiledPredicate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledPredicate")
            .field("eval_fn", &(self.eval_fn as usize as *const ()))
            .field("batch_fn", &(self.batch_fn as usize as *const ()))
            .finish()
    }
}

impl CompiledPredicate {
    pub(crate) fn new(
        eval_fn: unsafe extern "C" fn(*const u8, usize) -> u8,
        batch_fn: unsafe extern "C" fn(*const DocRef, usize, *mut u32) -> usize,
        strings: Vec<String>,
        module: JITModule,
    ) -> Self {
        Self {
            eval_fn,
            batch_fn,
            _strings: Arc::new(strings),
            _module: Arc::new(parking_lot::Mutex::new(module)),
        }
    }

    /// Evaluates the compiled native predicate against a single [`BinaryDoc`].
    #[inline]
    #[must_use]
    pub fn eval(&self, doc: &BinaryDoc) -> bool {
        let bytes = doc.as_bytes();
        unsafe { (self.eval_fn)(bytes.as_ptr(), bytes.len()) != 0 }
    }

    /// Evaluates the compiled predicate against a raw document byte buffer.
    #[inline]
    #[must_use]
    pub fn eval_bytes(&self, bytes: &[u8]) -> bool {
        unsafe { (self.eval_fn)(bytes.as_ptr(), bytes.len()) != 0 }
    }

    /// Vectorized batch scan over a slice of [`BinaryDoc`] references.
    ///
    /// Evaluates the predicate in a tight native loop directly over CPU registers.
    /// Matching indices within `docs` are appended into `matches`.
    pub fn eval_batch(&self, docs: &[&BinaryDoc], matches: &mut Vec<usize>) {
        if docs.is_empty() {
            return;
        }

        // Process in L1-cache friendly chunks (up to 1024 docs per batch call)
        const CHUNK_SIZE: usize = 1024;
        let mut doc_refs = Vec::with_capacity(docs.len().min(CHUNK_SIZE));
        let mut match_buf = vec![0u32; docs.len().min(CHUNK_SIZE)];

        for (chunk_idx, chunk) in docs.chunks(CHUNK_SIZE).enumerate() {
            doc_refs.clear();
            for doc in chunk {
                let bytes = doc.as_bytes();
                doc_refs.push(DocRef {
                    ptr: bytes.as_ptr(),
                    len: bytes.len(),
                });
            }

            let base_idx = chunk_idx * CHUNK_SIZE;
            let matched_count = unsafe {
                (self.batch_fn)(doc_refs.as_ptr(), doc_refs.len(), match_buf.as_mut_ptr())
            };

            for &m_idx in &match_buf[..matched_count] {
                matches.push(base_idx + m_idx as usize);
            }
        }
    }

    /// Vectorized batch scan over raw document byte slices.
    pub fn eval_batch_bytes(&self, bytes_slices: &[&[u8]], matches: &mut Vec<usize>) {
        if bytes_slices.is_empty() {
            return;
        }

        const CHUNK_SIZE: usize = 1024;
        let mut doc_refs = Vec::with_capacity(bytes_slices.len().min(CHUNK_SIZE));
        let mut match_buf = vec![0u32; bytes_slices.len().min(CHUNK_SIZE)];

        for (chunk_idx, chunk) in bytes_slices.chunks(CHUNK_SIZE).enumerate() {
            doc_refs.clear();
            for bytes in chunk {
                doc_refs.push(DocRef {
                    ptr: bytes.as_ptr(),
                    len: bytes.len(),
                });
            }

            let base_idx = chunk_idx * CHUNK_SIZE;
            let matched_count = unsafe {
                (self.batch_fn)(doc_refs.as_ptr(), doc_refs.len(), match_buf.as_mut_ptr())
            };

            for &m_idx in &match_buf[..matched_count] {
                matches.push(base_idx + m_idx as usize);
            }
        }
    }
}
