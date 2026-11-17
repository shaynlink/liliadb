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
    /// Attached database that sharded-table statement slots currently target.
    /// `None` leaves slot SQL unqualified (resolved by SQLite's schema search order).
    write_schema: Option<String>,
    /// Statement slots prepared against an attached schema, keyed by its alias.
    schema_slots: std::collections::HashMap<String, Box<StatementSlots>>,
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
            write_schema: None,
            schema_slots: std::collections::HashMap::new(),
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

    /// Execute a raw SQL script (e.g. for migrations or pragmas), retrying via `unlock_notify`
    /// on shared-cache lock contention.
    pub fn exec(&self, sql: &str) -> Result<()> {
        let c_sql = CString::new(sql).map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("null byte in sql: {e}"),
                false,
            )
        })?;
        loop {
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
            if rc == ffi::SQLITE_OK {
                return Ok(());
            }
            if is_shared_cache_locked(self.raw, rc) {
                if !err_msg.is_null() {
                    unsafe { ffi::sqlite3_free(err_msg.cast()) };
                }
                wait_for_unlock_notify(self.raw)?;
                continue;
            }
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
    }

    /// Prepare a persistent statement, transparently waiting via `unlock_notify` on shared-cache locks.
    pub fn prepare(&self, sql: &str) -> Result<NativeStatement> {
        let c_sql = CString::new(sql).map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("null byte in sql: {e}"),
                false,
            )
        })?;
        loop {
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
            if rc == ffi::SQLITE_OK && !stmt.is_null() {
                return Ok(unsafe { NativeStatement::from_raw(stmt) });
            }
            if is_shared_cache_locked(self.raw, rc) {
                wait_for_unlock_notify(self.raw)?;
                continue;
            }
            let err = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(self.raw)).to_string_lossy() };
            return Err(ffi::sqlite_code_to_lilia(
                rc,
                format!("sqlite prepare failed: {err} (rc={rc})"),
            ));
        }
    }

    /// Prepares `sql`, refusing any statement that inserts into, updates or deletes from one of
    /// `protected_tables`. SQLite consults the authorizer only while compiling, so it is installed
    /// for the duration of the call and always cleared before returning.
    pub fn prepare_denying_writes(
        &self,
        sql: &str,
        protected_tables: &[&str],
    ) -> Result<NativeStatement> {
        unsafe extern "C" fn authorize(
            context: *mut std::ffi::c_void,
            action: c_int,
            table: *const c_char,
            _arg2: *const c_char,
            _database: *const c_char,
            _trigger: *const c_char,
        ) -> c_int {
            if !matches!(
                action,
                ffi::SQLITE_INSERT | ffi::SQLITE_UPDATE | ffi::SQLITE_DELETE
            ) || table.is_null()
            {
                return ffi::SQLITE_OK;
            }
            // SAFETY: `context` points to the `&[&str]` owned by the enclosing call, which
            // outlives every authorizer invocation (compilation happens inside that call).
            let protected = unsafe { &*context.cast::<&[&str]>() };
            let name = unsafe { CStr::from_ptr(table) }.to_string_lossy();
            if protected.iter().any(|candidate| *candidate == name) {
                ffi::SQLITE_DENY
            } else {
                ffi::SQLITE_OK
            }
        }

        let mut context: &[&str] = protected_tables;
        let context_ptr = ptr::addr_of_mut!(context).cast::<std::ffi::c_void>();
        let installed =
            unsafe { ffi::sqlite3_set_authorizer(self.raw, Some(authorize), context_ptr) };
        if installed != ffi::SQLITE_OK {
            return Err(ffi::sqlite_code_to_lilia(
                installed,
                "failed to install the SQL write guard",
            ));
        }
        let prepared = self.prepare(sql);
        unsafe { ffi::sqlite3_set_authorizer(self.raw, None, ptr::null_mut()) };
        prepared.map_err(|error| {
            if error.code == ErrorCode::Unsupported {
                LiliaError::new(
                    ErrorCode::Unsupported,
                    format!(
                        "direct SQL writes to {} are disabled while integrity contracts are declared; use batch operations",
                        protected_tables.join(", ")
                    ),
                    false,
                )
            } else {
                error
            }
        })
    }

    /// Ends the read of every cached slot statement.
    ///
    /// A statement stepped but not reset keeps its read transaction open, even after the
    /// enclosing transaction commits. That pins a WAL snapshot: checkpoints still copy frames but
    /// the WAL can never restart from its beginning, so it grows without bound.
    pub fn reset_statements(&mut self) {
        self.slots.reset_all();
        for slots in self.schema_slots.values_mut() {
            slots.reset_all();
        }
    }

    /// Retrieve or lazily prepare a statement from a pre-defined slot.
    #[inline]
    pub fn slot(&mut self, slot: StmtSlot) -> Result<&mut NativeStatement> {
        if slot.sharded_table().is_some() {
            if let Some(schema) = self.write_schema.clone() {
                let missing = self
                    .schema_slots
                    .get_mut(&schema)
                    .is_none_or(|slots| slots.get_mut(slot).is_none());
                if missing {
                    let stmt = self.prepare(&slot.sql_in_schema(&schema))?;
                    self.schema_slots
                        .entry(schema.clone())
                        .or_insert_with(|| Box::new(StatementSlots::empty()))
                        .set(slot, stmt);
                }
                return Ok(self
                    .schema_slots
                    .get_mut(&schema)
                    .and_then(|slots| slots.get_mut(slot))
                    .expect("schema slot must be populated"));
            }
        }
        if self.slots.get_mut(slot).is_none() {
            let stmt = self.prepare(slot.sql())?;
            self.slots.set(slot, stmt);
        }
        Ok(self.slots.get_mut(slot).expect("slot must be populated"))
    }

    /// Directs sharded-table statement slots (KV and JSON) at the attached
    /// database `schema`, or back to unqualified resolution with `None`.
    pub fn set_write_schema(&mut self, schema: Option<&str>) {
        self.write_schema = schema.map(str::to_owned);
    }

    #[must_use]
    pub fn write_schema(&self) -> Option<&str> {
        self.write_schema.as_deref()
    }

    /// Finalizes statements prepared against `schema`; required before it is detached.
    pub fn forget_schema_slots(&mut self, schema: &str) {
        self.schema_slots.remove(schema);
        if self.write_schema.as_deref() == Some(schema) {
            self.write_schema = None;
        }
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
    pub fn savepoint(&self, name: &str) -> Result<()> {
        validate_savepoint_name(name)?;
        self.exec(&format!("SAVEPOINT \"{name}\";"))
    }

    #[inline]
    pub fn release_savepoint(&self, name: &str) -> Result<()> {
        validate_savepoint_name(name)?;
        self.exec(&format!("RELEASE SAVEPOINT \"{name}\";"))
    }

    #[inline]
    pub fn rollback_to_savepoint(&self, name: &str) -> Result<()> {
        validate_savepoint_name(name)?;
        self.exec(&format!("ROLLBACK TO SAVEPOINT \"{name}\";"))
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

    /// Whether no transaction is open on this connection (SQLite autocommit mode).
    #[inline]
    #[must_use]
    pub fn is_autocommit(&self) -> bool {
        unsafe { ffi::sqlite3_get_autocommit(self.raw) != 0 }
    }

    /// Rows inserted, updated or deleted through this connection since it was opened.
    #[inline]
    #[must_use]
    pub fn total_changes(&self) -> i64 {
        unsafe { ffi::sqlite3_total_changes64(self.raw) }
    }

    #[inline]
    pub fn last_insert_rowid(&self) -> i64 {
        unsafe { ffi::sqlite3_last_insert_rowid(self.raw) }
    }

    /// Attach an auxiliary SQLite database (such as shared-cache `_lilia_mem`) under `alias`.
    pub fn attach_database(&self, alias: &str, uri: &str) -> Result<()> {
        if alias.is_empty() || !alias.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "invalid attach database alias",
                false,
            ));
        }
        let esc_uri = uri.replace('\'', "''");
        self.exec(&format!("ATTACH DATABASE '{esc_uri}' AS {alias};"))
    }

    /// Detach an auxiliary SQLite database previously attached under `alias`.
    pub fn detach_database(&self, alias: &str) -> Result<()> {
        if alias.is_empty() || !alias.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "invalid detach database alias",
                false,
            ));
        }
        self.exec(&format!("DETACH DATABASE {alias};"))
    }

    /// Register an eponymous virtual table module projecting external/in-memory stores into SQL.
    pub fn register_vtab_module(
        &self,
        name: &str,
        provider: std::sync::Arc<dyn crate::vtab::VirtualTableProvider>,
    ) -> Result<()> {
        unsafe { crate::vtab::register_eponymous_module(self.raw, name, provider) }
    }

    /// Register a virtual table factory supporting dynamic construction and arguments (`CREATE VIRTUAL TABLE ... USING`).
    pub fn register_vtab_factory(
        &self,
        name: &str,
        factory: std::sync::Arc<dyn crate::vtab::VirtualTableFactory>,
    ) -> Result<()> {
        unsafe { crate::vtab::register_module_factory(self.raw, name, factory) }
    }
}

#[inline]
pub(crate) fn is_shared_cache_locked(db: *mut ffi::sqlite3, rc: c_int) -> bool {
    if db.is_null() {
        return false;
    }
    rc == ffi::SQLITE_LOCKED_SHAREDCACHE
        || ((rc & 0xFF) == ffi::SQLITE_LOCKED
            && unsafe { ffi::sqlite3_extended_errcode(db) } == ffi::SQLITE_LOCKED_SHAREDCACHE)
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn wait_for_unlock_notify(db: *mut ffi::sqlite3) -> Result<()> {
    struct NotifyState {
        fired: std::sync::Mutex<bool>,
        cv: std::sync::Condvar,
    }

    unsafe extern "C" fn unlock_notify_cb(ap_arg: *mut *mut std::ffi::c_void, n_arg: c_int) {
        if ap_arg.is_null() || n_arg <= 0 {
            return;
        }
        let args = std::slice::from_raw_parts(ap_arg, n_arg as usize);
        for &ptr in args {
            if !ptr.is_null() {
                let state = &*(ptr.cast::<NotifyState>());
                let mut guard = state
                    .fired
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *guard = true;
                state.cv.notify_all();
            }
        }
    }

    let state = NotifyState {
        fired: std::sync::Mutex::new(false),
        cv: std::sync::Condvar::new(),
    };
    let state_ptr: *const NotifyState = &state;
    let rc = unsafe {
        ffi::sqlite3_unlock_notify(
            db,
            Some(unlock_notify_cb),
            state_ptr.cast::<std::ffi::c_void>().cast_mut(),
        )
    };
    if rc != ffi::SQLITE_OK {
        return Err(ffi::sqlite_code_to_lilia(
            rc,
            format!("sqlite3_unlock_notify failed: rc={rc}"),
        ));
    }
    let mut guard = state
        .fired
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while !*guard {
        guard = state
            .cv
            .wait(guard)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    Ok(())
}

fn validate_savepoint_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 255
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid savepoint name '{name}': must contain 1..=255 alphanumeric or underscore characters"),
            false,
        ));
    }
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn wait_for_unlock_notify(_db: *mut ffi::sqlite3) -> Result<()> {
    Ok(())
}

impl Drop for NativeConnection {
    fn drop(&mut self) {
        // Drop all pre-compiled statements first before closing the database handle
        self.slots = StatementSlots::empty();
        self.schema_slots.clear();
        if !self.raw.is_null() {
            unsafe {
                ffi::sqlite3_close_v2(self.raw);
            }
            self.raw = ptr::null_mut();
        }
    }
}
