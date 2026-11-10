#![allow(unsafe_code)]

use std::ffi::CString;
use std::ptr;

use lilia_core::{ErrorCode, LiliaError, Result};

use crate::connection::NativeConnection;
use crate::ffi;

pub struct NativeBackup {
    raw: *mut ffi::sqlite3_backup,
    dest_raw: *mut ffi::sqlite3,
}

impl std::fmt::Debug for NativeBackup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeBackup")
            .field("raw", &self.raw)
            .field("dest_raw", &self.dest_raw)
            .finish()
    }
}

unsafe impl Send for NativeBackup {}

impl NativeBackup {
    /// Initialize an online backup from `source` to `dest`.
    pub fn new(dest: &NativeConnection, source: &NativeConnection) -> Result<Self> {
        let main = CString::new("main").map_err(|e| {
            LiliaError::new(ErrorCode::Storage, format!("invalid cstring: {e}"), false)
        })?;

        let raw = unsafe {
            ffi::sqlite3_backup_init(dest.raw(), main.as_ptr(), source.raw(), main.as_ptr())
        };

        if raw.is_null() {
            let err = unsafe {
                std::ffi::CStr::from_ptr(ffi::sqlite3_errmsg(dest.raw())).to_string_lossy()
            };
            return Err(LiliaError::new(
                ErrorCode::Storage,
                format!("failed to initialize sqlite backup: {err}"),
                false,
            ));
        }

        Ok(Self {
            raw,
            dest_raw: dest.raw(),
        })
    }

    /// Step the backup by `pages` (-1 means copy all remaining pages in one call).
    /// Returns `Ok(true)` if backup is completed (SQLITE_DONE), `Ok(false)` if more pages remain (SQLITE_OK).
    pub fn step(&mut self, pages: i32) -> Result<bool> {
        let rc = unsafe { ffi::sqlite3_backup_step(self.raw, pages) };
        match rc {
            ffi::SQLITE_DONE => Ok(true),
            ffi::SQLITE_OK => Ok(false),
            ffi::SQLITE_BUSY | ffi::SQLITE_LOCKED => Err(LiliaError::new(
                ErrorCode::Timeout,
                "sqlite backup step locked or busy",
                true,
            )),
            _ => {
                let err = unsafe {
                    std::ffi::CStr::from_ptr(ffi::sqlite3_errmsg(self.dest_raw)).to_string_lossy()
                };
                Err(LiliaError::new(
                    ErrorCode::Storage,
                    format!("sqlite backup step failed: {err} (rc={rc})"),
                    false,
                ))
            }
        }
    }

    #[inline]
    pub fn remaining(&self) -> i32 {
        unsafe { ffi::sqlite3_backup_remaining(self.raw) }
    }

    #[inline]
    pub fn pagecount(&self) -> i32 {
        unsafe { ffi::sqlite3_backup_pagecount(self.raw) }
    }
}

impl Drop for NativeBackup {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe {
                ffi::sqlite3_backup_finish(self.raw);
            }
            self.raw = ptr::null_mut();
        }
    }
}
