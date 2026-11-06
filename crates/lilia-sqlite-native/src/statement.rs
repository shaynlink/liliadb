#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};
use std::ptr;
use std::slice;
use std::str;

use lilia_core::{ErrorCode, LiliaError, Result};

use crate::ffi;

#[derive(Debug)]
pub struct NativeStatement {
    raw: *mut ffi::sqlite3_stmt,
}

unsafe impl Send for NativeStatement {}

impl NativeStatement {
    /// Create a statement from a raw SQLite statement pointer.
    ///
    /// # Safety
    /// The caller must ensure `raw` is a valid, non-null statement handle.
    pub unsafe fn from_raw(raw: *mut ffi::sqlite3_stmt) -> Self {
        Self { raw }
    }

    #[inline]
    pub fn raw(&self) -> *mut ffi::sqlite3_stmt {
        self.raw
    }

    #[inline]
    pub fn reset(&mut self) -> Result<()> {
        let rc = unsafe { ffi::sqlite3_reset(self.raw) };
        if rc == ffi::SQLITE_OK {
            Ok(())
        } else {
            Err(LiliaError::new(
                ErrorCode::Storage,
                format!("failed to reset statement: rc={rc}"),
                false,
            ))
        }
    }

    #[inline]
    pub fn clear_bindings(&mut self) -> Result<()> {
        let rc = unsafe { ffi::sqlite3_clear_bindings(self.raw) };
        if rc == ffi::SQLITE_OK {
            Ok(())
        } else {
            Err(LiliaError::new(
                ErrorCode::Storage,
                format!("failed to clear bindings: rc={rc}"),
                false,
            ))
        }
    }

    /// Bind a UTF-8 text parameter using zero-copy `SQLITE_STATIC`.
    /// The slice must live at least until `step()` or `reset()` finishes.
    #[inline]
    pub fn bind_text_static(&mut self, col: i32, val: &str) -> Result<()> {
        let rc = unsafe {
            ffi::sqlite3_bind_text64(
                self.raw,
                col as c_int,
                val.as_ptr().cast::<c_char>(),
                val.len() as u64,
                ffi::SQLITE_STATIC,
                ffi::SQLITE_UTF8,
            )
        };
        if rc == ffi::SQLITE_OK {
            Ok(())
        } else {
            Err(LiliaError::new(
                ErrorCode::Storage,
                format!("failed to bind text static col={col}: rc={rc}"),
                false,
            ))
        }
    }

    /// Bind a byte blob parameter using zero-copy `SQLITE_STATIC`.
    /// The slice must live at least until `step()` or `reset()` finishes.
    #[inline]
    pub fn bind_blob_static(&mut self, col: i32, val: &[u8]) -> Result<()> {
        let rc = unsafe {
            ffi::sqlite3_bind_blob64(
                self.raw,
                col as c_int,
                val.as_ptr().cast::<c_void>(),
                val.len() as u64,
                ffi::SQLITE_STATIC,
            )
        };
        if rc == ffi::SQLITE_OK {
            Ok(())
        } else {
            Err(LiliaError::new(
                ErrorCode::Storage,
                format!("failed to bind blob static col={col}: rc={rc}"),
                false,
            ))
        }
    }

    #[inline]
    pub fn bind_int64(&mut self, col: i32, val: i64) -> Result<()> {
        let rc = unsafe { ffi::sqlite3_bind_int64(self.raw, col as c_int, val) };
        if rc == ffi::SQLITE_OK {
            Ok(())
        } else {
            Err(LiliaError::new(
                ErrorCode::Storage,
                format!("failed to bind int64 col={col}: rc={rc}"),
                false,
            ))
        }
    }

    #[inline]
    pub fn bind_null(&mut self, col: i32) -> Result<()> {
        let rc = unsafe { ffi::sqlite3_bind_null(self.raw, col as c_int) };
        if rc == ffi::SQLITE_OK {
            Ok(())
        } else {
            Err(LiliaError::new(
                ErrorCode::Storage,
                format!("failed to bind null col={col}: rc={rc}"),
                false,
            ))
        }
    }

    /// Step the statement. Returns `Ok(true)` for SQLITE_ROW, `Ok(false)` for SQLITE_DONE.
    #[inline]
    pub fn step(&mut self) -> Result<bool> {
        let rc = unsafe { ffi::sqlite3_step(self.raw) };
        match rc {
            ffi::SQLITE_ROW => Ok(true),
            ffi::SQLITE_DONE => Ok(false),
            _ => Err(LiliaError::new(
                ErrorCode::Storage,
                format!("sqlite statement step failed with rc={rc}"),
                false,
            )),
        }
    }

    #[inline]
    pub fn column_int64(&self, col: i32) -> i64 {
        unsafe { ffi::sqlite3_column_int64(self.raw, col as c_int) }
    }

    #[inline]
    pub fn column_blob(&self, col: i32) -> &[u8] {
        let ptr = unsafe { ffi::sqlite3_column_blob(self.raw, col as c_int) };
        let len = unsafe { ffi::sqlite3_column_bytes(self.raw, col as c_int) };
        if ptr.is_null() || len <= 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(ptr.cast::<u8>(), len as usize) }
        }
    }

    #[inline]
    pub fn column_text(&self, col: i32) -> &str {
        let ptr = unsafe { ffi::sqlite3_column_text(self.raw, col as c_int) };
        let len = unsafe { ffi::sqlite3_column_bytes(self.raw, col as c_int) };
        if ptr.is_null() || len <= 0 {
            ""
        } else {
            let bytes = unsafe { slice::from_raw_parts(ptr, len as usize) };
            str::from_utf8(bytes).unwrap_or("")
        }
    }

    #[inline]
    pub fn column_is_null(&self, col: i32) -> bool {
        let t = unsafe { ffi::sqlite3_column_type(self.raw, col as c_int) };
        t == ffi::SQLITE_NULL
    }
}

impl Drop for NativeStatement {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe {
                ffi::sqlite3_finalize(self.raw);
            }
            self.raw = ptr::null_mut();
        }
    }
}
