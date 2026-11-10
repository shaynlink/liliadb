#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, CStr, CString};
use std::path::Path;
use std::ptr;

use lilia_core::{ErrorCode, LiliaError, Result};

use crate::ffi;
use crate::function::{NativeContext, NativeValue};
use crate::slots::{StatementSlots, StmtSlot};
use crate::statement::NativeStatement;
use crate::transaction::NativeTransaction;

pub struct NativeConnection {
    raw: *mut ffi::sqlite3,
    slots: StatementSlots,
}

impl std::fmt::Debug for NativeConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeConnection")
            .field("raw", &self.raw)
            .finish()
    }
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
    /// Open a new SQLite connection with default parameters for utilities and testing.
    pub fn open_file(path: impl AsRef<Path>) -> Result<Self> {
        Self::open(path.as_ref(), "NORMAL", 0, 5000)
    }

    /// Open a new SQLite connection with high-performance parameters.
    pub fn open(
        path: &Path,
        synchronous: &str,
        mmap_size: u64,
        busy_timeout_ms: i32,
    ) -> Result<Self> {
        Self::open_with_config(path, synchronous, mmap_size, busy_timeout_ms, -65536)
    }

    /// Open a new SQLite connection with explicit cache and durability parameters.
    pub fn open_with_config(
        path: &Path,
        synchronous: &str,
        mmap_size: u64,
        busy_timeout_ms: i32,
        cache_size_kib: i32,
    ) -> Result<Self> {
        crate::logging::ensure_sqlite_initialized();

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
        conn.exec(&format!("PRAGMA cache_size = {cache_size_kib};"))?;
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
            return Err(ffi::sqlite_code_to_lilia(
                rc,
                format!("sqlite_exec failed: {detail} (rc={rc})"),
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
            return Err(ffi::sqlite_code_to_lilia(
                rc,
                format!("sqlite prepare failed: {err} (rc={rc})"),
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
    pub fn transaction(&mut self) -> Result<NativeTransaction<'_>> {
        NativeTransaction::new(self)
    }

    #[inline]
    pub fn pragma_update(&self, name: &str, val: impl std::fmt::Display) -> Result<()> {
        let sql = if name.trim_start().to_uppercase().starts_with("PRAGMA ") {
            format!("{name} = {val};")
        } else {
            format!("PRAGMA {name} = {val};")
        };
        self.exec(&sql)
    }

    pub fn query_int(&self, sql: &str) -> Result<i64> {
        let mut stmt = self.prepare(sql)?;
        if stmt.step()? {
            Ok(stmt.column_int64(0))
        } else {
            Ok(0)
        }
    }

    pub fn pragma_query_int(&self, pragma: &str) -> Result<i64> {
        let trimmed = pragma.trim_start();
        let sql = if trimmed.to_uppercase().starts_with("PRAGMA ")
            || trimmed.to_uppercase().starts_with("SELECT ")
        {
            pragma.to_string()
        } else {
            format!("PRAGMA {pragma};")
        };
        self.query_int(&sql)
    }

    pub fn wal_checkpoint(&self, mode: c_int) -> Result<(i32, i32, i32)> {
        let mut log = 0;
        let mut ckpt = 0;
        let rc = unsafe {
            ffi::sqlite3_wal_checkpoint_v2(self.raw, ptr::null(), mode, &mut log, &mut ckpt)
        };
        if rc == ffi::SQLITE_OK || rc == ffi::SQLITE_BUSY {
            let busy = if rc == ffi::SQLITE_BUSY { 1 } else { 0 };
            Ok((busy, log, ckpt))
        } else {
            let err = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(self.raw)).to_string_lossy() };
            Err(LiliaError::new(
                ErrorCode::Storage,
                format!("wal_checkpoint failed: {err} (rc={rc})"),
                false,
            ))
        }
    }

    pub fn cache_flush(&self) -> Result<()> {
        let rc = unsafe { ffi::sqlite3_db_cacheflush(self.raw) };
        if rc == ffi::SQLITE_OK {
            Ok(())
        } else {
            let err = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(self.raw)).to_string_lossy() };
            Err(LiliaError::new(
                ErrorCode::Storage,
                format!("cache_flush failed: {err} (rc={rc})"),
                false,
            ))
        }
    }

    pub fn register_scalar_function<F>(&self, name: &str, n_arg: i32, f: F) -> Result<()>
    where
        F: Fn(&NativeContext, &[NativeValue]) -> Result<()> + Send + Sync + 'static,
    {
        let c_name = CString::new(name).map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("null byte in function name: {e}"),
                false,
            )
        })?;

        let boxed_f = Box::new(f);
        let app_ptr = Box::into_raw(boxed_f);

        unsafe extern "C" fn x_func<F>(
            ctx: *mut ffi::sqlite3_context,
            argc: c_int,
            argv: *mut *mut ffi::sqlite3_value,
        ) where
            F: Fn(&NativeContext, &[NativeValue]) -> Result<()> + Send + Sync + 'static,
        {
            let user_data = ffi::sqlite3_user_data(ctx);
            let closure = &*(user_data.cast::<F>());
            let n = argc as usize;
            let mut values = Vec::with_capacity(n);
            for i in 0..n {
                let val_ptr = *argv.add(i);
                values.push(NativeValue::from_raw(val_ptr));
            }
            let native_ctx = NativeContext::from_raw(ctx);
            if let Err(e) = closure(&native_ctx, &values) {
                native_ctx.set_result_error(&e.to_string());
            }
        }

        unsafe extern "C" fn x_destroy<F>(p: *mut std::ffi::c_void) {
            if !p.is_null() {
                drop(Box::from_raw(p.cast::<F>()));
            }
        }

        let rc = unsafe {
            ffi::sqlite3_create_function_v2(
                self.raw,
                c_name.as_ptr(),
                n_arg as c_int,
                ffi::SQLITE_UTF8 as c_int | ffi::SQLITE_DETERMINISTIC,
                app_ptr.cast::<std::ffi::c_void>(),
                Some(x_func::<F>),
                None,
                None,
                Some(x_destroy::<F>),
            )
        };

        if rc != ffi::SQLITE_OK {
            unsafe { drop(Box::from_raw(app_ptr)) };
            return Err(LiliaError::new(
                ErrorCode::Storage,
                format!("failed to register scalar function {name}: rc={rc}"),
                false,
            ));
        }

        Ok(())
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
