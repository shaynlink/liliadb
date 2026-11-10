use std::ffi::CString;
use std::slice;
use std::str;

use crate::ffi;

/// Safe wrapper around `sqlite3_context` passed to custom scalar functions.
#[derive(Debug)]
pub struct NativeContext(*mut ffi::sqlite3_context);

impl NativeContext {
    /// Creates a `NativeContext` from a raw SQLite pointer.
    ///
    /// # Safety
    /// `ctx` must be a valid, live pointer to `sqlite3_context`.
    #[inline]
    #[must_use]
    pub unsafe fn from_raw(ctx: *mut ffi::sqlite3_context) -> Self {
        Self(ctx)
    }

    #[inline]
    pub fn set_result_null(&self) {
        unsafe { ffi::sqlite3_result_null(self.0) };
    }

    #[inline]
    pub fn set_result_blob(&self, data: &[u8]) {
        unsafe {
            ffi::sqlite3_result_blob64(
                self.0,
                data.as_ptr().cast(),
                data.len() as u64,
                ffi::sqlite_transient(),
            );
        }
    }

    #[inline]
    pub fn set_result_text(&self, text: &str) {
        unsafe {
            ffi::sqlite3_result_text64(
                self.0,
                text.as_ptr().cast(),
                text.len() as u64,
                ffi::sqlite_transient(),
                ffi::SQLITE_UTF8,
            );
        }
    }

    #[inline]
    pub fn set_result_int64(&self, val: i64) {
        unsafe { ffi::sqlite3_result_int64(self.0, val) };
    }

    #[inline]
    pub fn set_result_double(&self, val: f64) {
        unsafe { ffi::sqlite3_result_double(self.0, val) };
    }

    #[inline]
    pub fn set_result_error(&self, msg: &str) {
        let c_str = CString::new(msg).unwrap_or_default();
        unsafe { ffi::sqlite3_result_error(self.0, c_str.as_ptr(), -1) };
    }
}

/// Safe wrapper around `sqlite3_value` argument passed to scalar functions.
#[derive(Debug)]
pub struct NativeValue(*mut ffi::sqlite3_value);

impl NativeValue {
    /// Creates a `NativeValue` from a raw SQLite pointer.
    ///
    /// # Safety
    /// `val` must be a valid pointer to `sqlite3_value`.
    #[inline]
    #[must_use]
    pub unsafe fn from_raw(val: *mut ffi::sqlite3_value) -> Self {
        Self(val)
    }

    #[inline]
    #[must_use]
    pub fn is_null(&self) -> bool {
        unsafe { ffi::sqlite3_value_type(self.0) == ffi::SQLITE_NULL }
    }

    #[inline]
    #[must_use]
    pub fn as_blob(&self) -> &[u8] {
        let ptr = unsafe { ffi::sqlite3_value_blob(self.0) };
        let len = unsafe { ffi::sqlite3_value_bytes(self.0) };
        if ptr.is_null() || len <= 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(ptr.cast::<u8>(), len as usize) }
        }
    }

    #[inline]
    #[must_use]
    pub fn as_text(&self) -> &str {
        let ptr = unsafe { ffi::sqlite3_value_text(self.0) };
        let len = unsafe { ffi::sqlite3_value_bytes(self.0) };
        if ptr.is_null() || len <= 0 {
            ""
        } else {
            let bytes = unsafe { slice::from_raw_parts(ptr, len as usize) };
            str::from_utf8(bytes).unwrap_or("")
        }
    }

    #[inline]
    #[must_use]
    pub fn as_int64(&self) -> i64 {
        unsafe { ffi::sqlite3_value_int64(self.0) }
    }

    #[inline]
    #[must_use]
    pub fn as_double(&self) -> f64 {
        unsafe { ffi::sqlite3_value_double(self.0) }
    }
}
