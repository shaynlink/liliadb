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

/// Operation type of a row change in a changeset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChangeOp {
    Insert,
    Update,
    Delete,
}

/// A parsed change for a single row within a changeset.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RowChange {
    pub table: String,
    pub op: ChangeOp,
    pub is_indirect: bool,
    pub old_values: Vec<Option<serde_json::Value>>,
    pub new_values: Vec<Option<serde_json::Value>>,
}

unsafe fn sqlite3_value_to_json(val: *mut ffi::sqlite3_value) -> serde_json::Value {
    if val.is_null() {
        return serde_json::Value::Null;
    }
    match ffi::sqlite3_value_type(val) {
        ffi::SQLITE_INTEGER => serde_json::Value::Number(ffi::sqlite3_value_int64(val).into()),
        ffi::SQLITE_FLOAT => {
            let num = ffi::sqlite3_value_double(val);
            serde_json::Number::from_f64(num)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null)
        }
        ffi::SQLITE_TEXT => {
            let text = ffi::sqlite3_value_text(val);
            let len = ffi::sqlite3_value_bytes(val);
            if text.is_null() || len <= 0 {
                serde_json::Value::String(String::new())
            } else {
                let slice = slice::from_raw_parts(text, len as usize);
                let s = String::from_utf8_lossy(slice).into_owned();
                serde_json::Value::String(s)
            }
        }
        ffi::SQLITE_BLOB => {
            let blob = ffi::sqlite3_value_blob(val);
            let len = ffi::sqlite3_value_bytes(val);
            if blob.is_null() || len <= 0 {
                serde_json::Value::String(String::new())
            } else {
                let slice = slice::from_raw_parts(blob.cast::<u8>(), len as usize);
                if slice.starts_with(b"LBJ\x01") {
                    if let Ok(doc) = lilia_core::json::BinaryDoc::from_bytes(slice.to_vec()) {
                        return doc.to_json_value();
                    }
                }
                if let Ok(s) = std::str::from_utf8(slice) {
                    if let Ok(json_val) = serde_json::from_str(s) {
                        return json_val;
                    }
                    serde_json::Value::String(s.to_string())
                } else {
                    let mut hex = String::with_capacity(slice.len() * 2);
                    for b in slice {
                        use std::fmt::Write;
                        let _ = write!(&mut hex, "{b:02x}");
                    }
                    serde_json::Value::String(hex)
                }
            }
        }
        _ => serde_json::Value::Null,
    }
}

/// Safe iterator over individual row modifications in an SQLite binary changeset.
pub struct ChangesetIterator {
    raw: *mut ffi::sqlite3_changeset_iter,
}

unsafe impl Send for ChangesetIterator {}

impl Drop for ChangesetIterator {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe {
                ffi::sqlite3changeset_finalize(self.raw);
            }
        }
    }
}

impl ChangesetIterator {
    /// Create a new iterator over a binary changeset buffer.
    pub fn new(changeset: &[u8]) -> Result<Self> {
        if changeset.is_empty() {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "empty changeset cannot be parsed",
                false,
            ));
        }

        let mut raw = ptr::null_mut();
        let rc = unsafe {
            ffi::sqlite3changeset_start(
                &mut raw,
                changeset.len() as c_int,
                changeset.as_ptr().cast::<c_void>(),
            )
        };

        if rc != ffi::SQLITE_OK || raw.is_null() {
            return Err(LiliaError::new(
                ErrorCode::Storage,
                format!("failed to start changeset iterator: rc={rc}"),
                false,
            ));
        }

        Ok(Self { raw })
    }

    /// Advance to and return the next row change, or `None` if finished.
    pub fn next_change(&mut self) -> Result<Option<RowChange>> {
        let rc = unsafe { ffi::sqlite3changeset_next(self.raw) };
        if rc == ffi::SQLITE_DONE {
            return Ok(None);
        }
        if rc != ffi::SQLITE_ROW {
            return Err(LiliaError::new(
                ErrorCode::Storage,
                format!("changeset iterator next failed: rc={rc}"),
                false,
            ));
        }

        let mut z_tab: *const c_char = ptr::null();
        let mut n_col: c_int = 0;
        let mut op: c_int = 0;
        let mut b_indirect: c_int = 0;

        let rc = unsafe {
            ffi::sqlite3changeset_op(self.raw, &mut z_tab, &mut n_col, &mut op, &mut b_indirect)
        };
        if rc != ffi::SQLITE_OK {
            return Err(LiliaError::new(
                ErrorCode::Storage,
                format!("failed to get changeset op: rc={rc}"),
                false,
            ));
        }

        let table = if z_tab.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(z_tab).to_string_lossy().into_owned() }
        };

        let change_op = match op {
            ffi::SQLITE_INSERT => ChangeOp::Insert,
            ffi::SQLITE_UPDATE => ChangeOp::Update,
            ffi::SQLITE_DELETE => ChangeOp::Delete,
            other => {
                return Err(LiliaError::new(
                    ErrorCode::Storage,
                    format!("unknown changeset op: {other}"),
                    false,
                ));
            }
        };

        let col_count = n_col.max(0) as usize;
        let mut old_values = Vec::with_capacity(col_count);
        let mut new_values = Vec::with_capacity(col_count);

        match change_op {
            ChangeOp::Insert => {
                for i in 0..n_col {
                    let mut p_val: *mut ffi::sqlite3_value = ptr::null_mut();
                    let rc = unsafe { ffi::sqlite3changeset_new(self.raw, i, &mut p_val) };
                    if rc == ffi::SQLITE_OK && !p_val.is_null() {
                        let val = unsafe { sqlite3_value_to_json(p_val) };
                        new_values.push(Some(val));
                    } else {
                        new_values.push(None);
                    }
                }
            }
            ChangeOp::Delete => {
                for i in 0..n_col {
                    let mut p_val: *mut ffi::sqlite3_value = ptr::null_mut();
                    let rc = unsafe { ffi::sqlite3changeset_old(self.raw, i, &mut p_val) };
                    if rc == ffi::SQLITE_OK && !p_val.is_null() {
                        let val = unsafe { sqlite3_value_to_json(p_val) };
                        old_values.push(Some(val));
                    } else {
                        old_values.push(None);
                    }
                }
            }
            ChangeOp::Update => {
                for i in 0..n_col {
                    let mut p_old: *mut ffi::sqlite3_value = ptr::null_mut();
                    let rc = unsafe { ffi::sqlite3changeset_old(self.raw, i, &mut p_old) };
                    if rc == ffi::SQLITE_OK && !p_old.is_null() {
                        let val = unsafe { sqlite3_value_to_json(p_old) };
                        old_values.push(Some(val));
                    } else {
                        old_values.push(None);
                    }

                    let mut p_new: *mut ffi::sqlite3_value = ptr::null_mut();
                    let rc = unsafe { ffi::sqlite3changeset_new(self.raw, i, &mut p_new) };
                    if rc == ffi::SQLITE_OK && !p_new.is_null() {
                        let val = unsafe { sqlite3_value_to_json(p_new) };
                        new_values.push(Some(val));
                    } else {
                        new_values.push(None);
                    }
                }
            }
        }

        Ok(Some(RowChange {
            table,
            op: change_op,
            is_indirect: b_indirect != 0,
            old_values,
            new_values,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_changeset_iterator_captures_insert_update_delete() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("changeset_test.db");
        let mut conn = NativeConnection::open_file(&db_path).unwrap();
        conn.exec(
            "CREATE TABLE tasks (id TEXT PRIMARY KEY, title TEXT, status TEXT, priority INT);",
        )
        .unwrap();

        let session = NativeSession::create(&conn, "main").unwrap();
        session.attach_all().unwrap();

        let tx = conn.transaction().unwrap();
        tx.exec("INSERT INTO tasks VALUES ('T1', 'Design', 'pending', 1);")
            .unwrap();
        tx.exec("INSERT INTO tasks VALUES ('T2', 'Build', 'draft', 2);")
            .unwrap();
        tx.commit().unwrap();

        let cs = session.changeset().unwrap();
        assert!(!cs.is_empty());

        let mut iter = ChangesetIterator::new(&cs).unwrap();
        let mut changes = Vec::new();
        while let Some(change) = iter.next_change().unwrap() {
            changes.push(change);
        }

        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].table, "tasks");
        assert_eq!(changes[0].op, ChangeOp::Insert);

        let t1 = changes
            .iter()
            .find(|c| c.new_values[0] == Some(serde_json::json!("T1")))
            .unwrap();
        assert_eq!(t1.new_values[2], Some(serde_json::json!("pending")));

        let t2 = changes
            .iter()
            .find(|c| c.new_values[0] == Some(serde_json::json!("T2")))
            .unwrap();
        assert_eq!(t2.new_values[2], Some(serde_json::json!("draft")));

        // Now test UPDATE and DELETE
        let session2 = NativeSession::create(&conn, "main").unwrap();
        session2.attach_all().unwrap();

        let tx2 = conn.transaction().unwrap();
        tx2.exec("UPDATE tasks SET status = 'pending' WHERE id = 'T2';")
            .unwrap();
        tx2.exec("DELETE FROM tasks WHERE id = 'T1';").unwrap();
        tx2.commit().unwrap();

        let cs2 = session2.changeset().unwrap();
        let mut iter2 = ChangesetIterator::new(&cs2).unwrap();
        let mut changes2 = Vec::new();
        while let Some(change) = iter2.next_change().unwrap() {
            changes2.push(change);
        }

        assert_eq!(changes2.len(), 2);
        // One update, one delete
        let update_change = changes2.iter().find(|c| c.op == ChangeOp::Update).unwrap();
        assert_eq!(
            update_change.old_values[2],
            Some(serde_json::json!("draft"))
        );
        assert_eq!(
            update_change.new_values[2],
            Some(serde_json::json!("pending"))
        );

        let delete_change = changes2.iter().find(|c| c.op == ChangeOp::Delete).unwrap();
        assert_eq!(delete_change.old_values[0], Some(serde_json::json!("T1")));
    }
}
