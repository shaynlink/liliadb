#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, CStr, CString};
use std::path::Path;
use std::ptr;

use lilia_core::{ErrorCode, LiliaError, Result};

use crate::ffi;
use crate::slots::{StatementSlots, StmtSlot};
use crate::statement::NativeStatement;

pub struct NativeConnection {
    raw: *mut ffi::sqlite3,
    slots: StatementSlots,
}

unsafe impl Send for NativeConnection {}

#[derive(Debug, Clone, Copy)]
pub struct InterruptHandle {
    raw: *mut ffi::sqlite3,
}

unsafe impl Send for InterruptHandle {}
unsafe impl Sync for InterruptHandle {}

impl InterruptHandle {
    pub fn interrupt(&self) {
        if !self.raw.is_null() {
            unsafe {
                ffi::sqlite3_interrupt(self.raw);
            }
        }
    }
}

impl NativeConnection {
    /// Open a new SQLite connection with high-performance parameters.
    pub fn open(
        path: &Path,
        synchronous: &str,
        mmap_size: u64,
        busy_timeout_ms: i32,
    ) -> Result<Self> {
        let path_str = path.to_str().ok_or_else(|| {
            LiliaError::new(ErrorCode::InvalidInput, "invalid path encoding", false)
        })?;

        let c_path = CString::new(path_str).map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("null byte in path: {e}"),
                false,
            )
        })?;

        let mut raw = ptr::null_mut();
        let flags = ffi::SQLITE_OPEN_READWRITE
            | ffi::SQLITE_OPEN_CREATE
            | ffi::SQLITE_OPEN_URI
            | ffi::SQLITE_OPEN_NOMUTEX;

        let rc = unsafe { ffi::sqlite3_open_v2(c_path.as_ptr(), &mut raw, flags, ptr::null()) };
        if rc != ffi::SQLITE_OK || raw.is_null() {
            let msg = if raw.is_null() {
                format!("failed to allocate sqlite handle: rc={rc}")
            } else {
                let err = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(raw)).to_string_lossy() };
                let err_msg = format!("failed to open sqlite db: {err} (rc={rc})");
                unsafe { ffi::sqlite3_close_v2(raw) };
                err_msg
            };
            return Err(LiliaError::new(ErrorCode::Storage, msg, false));
        }

        unsafe {
            ffi::sqlite3_busy_timeout(raw, busy_timeout_ms);
            ffi::sqlite3_db_config(
                raw,
                ffi::SQLITE_DBCONFIG_DEFENSIVE,
                1,
                ptr::null_mut::<c_int>(),
            );
        }

        let conn = Self {
            raw,
            slots: StatementSlots::empty(),
        };

        // Configure PRAGMAs
        conn.exec("PRAGMA journal_mode = WAL;")?;
        conn.exec(&format!("PRAGMA synchronous = {synchronous};"))?;
        conn.exec("PRAGMA temp_store = MEMORY;")?;
        conn.exec("PRAGMA cache_size = -65536;")?;
        if mmap_size > 0 {
            conn.exec(&format!("PRAGMA mmap_size = {mmap_size};"))?;
        }

        Ok(conn)
    }

    #[inline]
    pub fn raw(&self) -> *mut ffi::sqlite3 {
        self.raw
    }

    #[inline]
    pub fn interrupt_handle(&self) -> InterruptHandle {
        InterruptHandle { raw: self.raw }
    }

    /// Execute a raw SQL script (e.g. for migrations or pragmas).
    pub fn exec(&self, sql: &str) -> Result<()> {
        let c_sql = CString::new(sql).map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("null byte in sql: {e}"),
                false,
            )
        })?;
        let mut err_msg: *mut c_char = ptr::null_mut();
        let rc = unsafe {
            ffi::sqlite3_exec(
                self.raw,
                c_sql.as_ptr(),
                None,
                ptr::null_mut(),
                &mut err_msg,
            )
        };
        if rc != ffi::SQLITE_OK {
            let detail = if err_msg.is_null() {
                format!("rc={rc}")
            } else {
                let msg = unsafe { CStr::from_ptr(err_msg).to_string_lossy().into_owned() };
                unsafe { ffi::sqlite3_free(err_msg.cast()) };
                msg
            };
            return Err(LiliaError::new(
                ErrorCode::Storage,
                format!("sqlite_exec failed: {detail}"),
                false,
            ));
        }
        Ok(())
    }

    /// Prepare a persistent statement.
    pub fn prepare(&self, sql: &str) -> Result<NativeStatement> {
        let c_sql = CString::new(sql).map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("null byte in sql: {e}"),
                false,
            )
        })?;
        let mut stmt = ptr::null_mut();
        let rc = unsafe {
            ffi::sqlite3_prepare_v3(
                self.raw,
                c_sql.as_ptr(),
                -1,
                ffi::SQLITE_PREPARE_PERSISTENT,
                &mut stmt,
                ptr::null_mut(),
            )
        };
        if rc != ffi::SQLITE_OK || stmt.is_null() {
            let err = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(self.raw)).to_string_lossy() };
            return Err(LiliaError::new(
                ErrorCode::Storage,
                format!("sqlite prepare failed: {err} (rc={rc})"),
                false,
            ));
        }
        Ok(unsafe { NativeStatement::from_raw(stmt) })
    }

    /// Retrieve or lazily prepare a statement from a pre-defined slot.
    #[inline]
    pub fn slot(&mut self, slot: StmtSlot) -> Result<&mut NativeStatement> {
        if self.slots.get_mut(slot).is_none() {
            let stmt = self.prepare(slot.sql())?;
            self.slots.set(slot, stmt);
        }
        Ok(self.slots.get_mut(slot).expect("slot must be populated"))
    }

    #[inline]
    pub fn begin_immediate(&self) -> Result<()> {
        self.exec("BEGIN IMMEDIATE;")
    }

    #[inline]
    pub fn commit(&self) -> Result<()> {
        self.exec("COMMIT;")
    }

    #[inline]
    pub fn rollback(&self) -> Result<()> {
        self.exec("ROLLBACK;")
    }

    #[inline]
    pub fn changes(&self) -> i64 {
        unsafe { ffi::sqlite3_changes64(self.raw) }
    }

    #[inline]
    pub fn last_insert_rowid(&self) -> i64 {
        unsafe { ffi::sqlite3_last_insert_rowid(self.raw) }
    }
}

impl Drop for NativeConnection {
    fn drop(&mut self) {
        // Drop all pre-compiled statements first before closing the database handle
        self.slots = StatementSlots::empty();
        if !self.raw.is_null() {
            unsafe {
                ffi::sqlite3_close_v2(self.raw);
            }
            self.raw = ptr::null_mut();
        }
    }
}
