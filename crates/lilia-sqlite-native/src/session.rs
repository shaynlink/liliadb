#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;
use std::slice;

use lilia_core::{ErrorCode, LiliaError, Result};

use crate::connection::NativeConnection;
use crate::ffi;

/// Conflict resolution strategy when applying a changeset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictAction {
    /// Omit conflicting change and proceed.
    Omit,
    /// Overwrite destination row with conflicting change.
    #[default]
    Replace,
    /// Abort changeset application immediately.
    Abort,
}

impl ConflictAction {
    #[must_use]
    pub fn to_sqlite_code(self) -> c_int {
        match self {
            Self::Omit => ffi::SQLITE_CHANGESET_OMIT,
            Self::Replace => ffi::SQLITE_CHANGESET_REPLACE,
            Self::Abort => ffi::SQLITE_CHANGESET_ABORT,
        }
    }
}

/// Safe RAII wrapper around SQLite's `sqlite3_session`.
/// Tracks database changes at the VDBE layer and serializes them into compact changesets.
pub struct NativeSession {
    raw: *mut ffi::sqlite3_session,
    filter: Option<Box<Box<dyn Fn(&str) -> bool + Send>>>,
}

impl std::fmt::Debug for NativeSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeSession")
            .field("raw", &self.raw)
            .finish_non_exhaustive()
    }
}

unsafe impl Send for NativeSession {}

impl Drop for NativeSession {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe {
                ffi::sqlite3session_delete(self.raw);
            }
        }
    }
}

unsafe extern "C" fn table_filter_trampoline(p_ctx: *mut c_void, z_tab: *const c_char) -> c_int {
    if p_ctx.is_null() || z_tab.is_null() {
        return 1;
    }
    let filter = &*(p_ctx.cast::<Box<dyn Fn(&str) -> bool + Send>>());
    let Ok(name) = CStr::from_ptr(z_tab).to_str() else {
        return 1;
    };
    if filter(name) {
        1
    } else {
        0
    }
}

unsafe extern "C" fn default_conflict_callback(
    p_ctx: *mut c_void,
    _e_conflict: c_int,
    _p_iter: *mut ffi::sqlite3_changeset_iter,
) -> c_int {
    if p_ctx.is_null() {
        ffi::SQLITE_CHANGESET_REPLACE
    } else {
        let action = *(p_ctx.cast::<ConflictAction>());
        action.to_sqlite_code()
    }
}

impl NativeSession {
    /// Create a new session attached to a database name (typically `"main"`).
    pub fn create(conn: &NativeConnection, db_name: &str) -> Result<Self> {
        let c_db_name = CString::new(db_name).map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("null byte in db_name: {e}"),
                false,
            )
        })?;

        let mut raw = ptr::null_mut();
        let rc = unsafe { ffi::sqlite3session_create(conn.raw(), c_db_name.as_ptr(), &mut raw) };
        if rc != ffi::SQLITE_OK || raw.is_null() {
            let msg = format!("failed to create session: rc={rc}");
            return Err(LiliaError::new(ErrorCode::Storage, msg, false));
        }

        Ok(Self { raw, filter: None })
    }

    /// Set a table filter callback.
    /// When attached tables are tracked, SQLite calls this filter:
    /// returning `true` tracks the table; returning `false` ignores the table.
    pub fn set_table_filter<F>(&mut self, filter: F)
    where
        F: Fn(&str) -> bool + Send + 'static,
    {
        let mut boxed = Box::new(Box::new(filter) as Box<dyn Fn(&str) -> bool + Send>);
        let ctx = ptr::from_mut(&mut *boxed).cast::<c_void>();
        self.filter = Some(boxed);
        unsafe {
            ffi::sqlite3session_table_filter(self.raw, Some(table_filter_trampoline), ctx);
        }
    }

    /// Attach all current and future tables to this session.
    pub fn attach_all(&self) -> Result<()> {
        let rc = unsafe { ffi::sqlite3session_attach(self.raw, ptr::null()) };
        if rc == ffi::SQLITE_OK {
            Ok(())
        } else {
            Err(LiliaError::new(
                ErrorCode::Storage,
                format!("failed to attach all tables to session: rc={rc}"),
                false,
            ))
        }
    }

    /// Attach a specific table by name to this session.
    pub fn attach_table(&self, table: &str) -> Result<()> {
        let c_table = CString::new(table).map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("null byte in table name: {e}"),
                false,
            )
        })?;

        let rc = unsafe { ffi::sqlite3session_attach(self.raw, c_table.as_ptr()) };
        if rc == ffi::SQLITE_OK {
            Ok(())
        } else {
            Err(LiliaError::new(
                ErrorCode::Storage,
                format!("failed to attach table '{table}' to session: rc={rc}"),
                false,
            ))
        }
    }

    /// Returns `true` if the session has recorded no changes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        let rc = unsafe { ffi::sqlite3session_isempty(self.raw) };
        rc != 0
    }

    /// Enable or disable change recording for this session.
    pub fn set_enabled(&self, enabled: bool) -> bool {
        let rc = unsafe { ffi::sqlite3session_enable(self.raw, i32::from(enabled)) };
        rc != 0
    }

    /// Generate a binary changeset containing all recorded changes since the session was created.
    pub fn changeset(&self) -> Result<Vec<u8>> {
        let mut n_bytes: c_int = 0;
        let mut p_buf: *mut c_void = ptr::null_mut();

        let rc = unsafe { ffi::sqlite3session_changeset(self.raw, &mut n_bytes, &mut p_buf) };
        if rc != ffi::SQLITE_OK {
            return Err(LiliaError::new(
                ErrorCode::Storage,
                format!("failed to generate changeset: rc={rc}"),
                false,
            ));
        }

        if p_buf.is_null() || n_bytes <= 0 {
            return Ok(Vec::new());
        }

        let slice = unsafe { slice::from_raw_parts(p_buf.cast::<u8>(), n_bytes as usize) };
        let bytes = slice.to_vec();

        unsafe {
            ffi::sqlite3_free(p_buf);
        }

        Ok(bytes)
    }

    /// Generate a compact binary patchset (containing only primary keys and changed column values).
    pub fn patchset(&self) -> Result<Vec<u8>> {
        let mut n_bytes: c_int = 0;
        let mut p_buf: *mut c_void = ptr::null_mut();

        let rc = unsafe { ffi::sqlite3session_patchset(self.raw, &mut n_bytes, &mut p_buf) };
        if rc != ffi::SQLITE_OK {
            return Err(LiliaError::new(
                ErrorCode::Storage,
                format!("failed to generate patchset: rc={rc}"),
                false,
            ));
        }

        if p_buf.is_null() || n_bytes <= 0 {
            return Ok(Vec::new());
        }

        let slice = unsafe { slice::from_raw_parts(p_buf.cast::<u8>(), n_bytes as usize) };
        let bytes = slice.to_vec();

        unsafe {
            ffi::sqlite3_free(p_buf);
        }

        Ok(bytes)
    }

    /// Invert an existing changeset (converting INSERTs to DELETEs, DELETEs to INSERTs, and swapping UPDATE values).
    pub fn invert_changeset(changeset: &[u8]) -> Result<Vec<u8>> {
        if changeset.is_empty() {
            return Ok(Vec::new());
        }

        let mut n_out: c_int = 0;
        let mut p_out: *mut c_void = ptr::null_mut();

        let rc = unsafe {
            ffi::sqlite3changeset_invert(
                changeset.len() as c_int,
                changeset.as_ptr().cast::<c_void>(),
                &mut n_out,
                &mut p_out,
            )
        };

        if rc != ffi::SQLITE_OK {
            return Err(LiliaError::new(
                ErrorCode::Storage,
                format!("failed to invert changeset: rc={rc}"),
                false,
            ));
        }

        if p_out.is_null() || n_out <= 0 {
            return Ok(Vec::new());
        }

        let slice = unsafe { slice::from_raw_parts(p_out.cast::<u8>(), n_out as usize) };
        let bytes = slice.to_vec();

        unsafe {
            ffi::sqlite3_free(p_out);
        }

        Ok(bytes)
    }

    /// Apply a changeset or patchset to a target connection with a conflict resolution policy.
    pub fn apply(
        conn: &NativeConnection,
        changeset: &[u8],
        conflict_action: ConflictAction,
    ) -> Result<()> {
        if changeset.is_empty() {
            return Ok(());
        }

        let mut action = conflict_action;
        let rc = unsafe {
            ffi::sqlite3changeset_apply(
                conn.raw(),
                changeset.len() as c_int,
                changeset.as_ptr() as *mut c_void,
                None,
                Some(default_conflict_callback),
                ptr::from_mut(&mut action).cast::<c_void>(),
            )
        };

        if rc != ffi::SQLITE_OK {
            let err_msg = unsafe {
                let err = ffi::sqlite3_errmsg(conn.raw());
                if err.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(err).to_string_lossy().into_owned()
                }
            };
            return Err(LiliaError::new(
                ErrorCode::Storage,
                format!("failed to apply changeset: rc={rc} ({err_msg})"),
                false,
            ));
        }

        Ok(())
    }
}
