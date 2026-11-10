#![allow(non_camel_case_types, non_snake_case, unsafe_code)]

use std::ffi::{c_char, c_int, c_uchar, c_uint, c_void};

use lilia_core::{ErrorCode, LiliaError};

pub enum sqlite3 {}
pub enum sqlite3_stmt {}
pub enum sqlite3_backup {}
pub enum sqlite3_context {}
pub enum sqlite3_value {}
pub enum sqlite3_session {}
pub enum sqlite3_changeset_iter {}

pub type sqlite3_destructor_type = Option<unsafe extern "C" fn(*mut c_void)>;

pub const SQLITE_STATIC: sqlite3_destructor_type = None;

#[inline]
pub fn sqlite_transient() -> sqlite3_destructor_type {
    unsafe { std::mem::transmute(-1isize) }
}

pub const SQLITE_UTF8: u8 = 1;

// Result codes
pub const SQLITE_OK: c_int = 0;
pub const SQLITE_ERROR: c_int = 1;
pub const SQLITE_BUSY: c_int = 5;
pub const SQLITE_LOCKED: c_int = 6;
pub const SQLITE_INTERRUPT: c_int = 9;
pub const SQLITE_IOERR: c_int = 10;
pub const SQLITE_CORRUPT: c_int = 11;
pub const SQLITE_FULL: c_int = 13;
pub const SQLITE_MISUSE: c_int = 21;
pub const SQLITE_NOTADB: c_int = 26;
pub const SQLITE_ROW: c_int = 100;
pub const SQLITE_DONE: c_int = 101;

// Open flags
pub const SQLITE_OPEN_READONLY: c_int = 0x00000001;
pub const SQLITE_OPEN_READWRITE: c_int = 0x00000002;
pub const SQLITE_OPEN_CREATE: c_int = 0x00000004;
pub const SQLITE_OPEN_URI: c_int = 0x00000040;
pub const SQLITE_OPEN_MEMORY: c_int = 0x00000080;
pub const SQLITE_OPEN_NOMUTEX: c_int = 0x00008000;

// Prepare flags
pub const SQLITE_PREPARE_PERSISTENT: c_uint = 0x01;

// Column datatypes
pub const SQLITE_INTEGER: c_int = 1;
pub const SQLITE_FLOAT: c_int = 2;
pub const SQLITE_TEXT: c_int = 3;
pub const SQLITE_BLOB: c_int = 4;
pub const SQLITE_NULL: c_int = 5;

// Global Config ops
pub const SQLITE_CONFIG_LOG: c_int = 16;

// DB Config ops
pub const SQLITE_DBCONFIG_DEFENSIVE: c_int = 1010;

// Carray data types
pub const CARRAY_INT32: c_int = 0;
pub const CARRAY_INT64: c_int = 1;
pub const CARRAY_DOUBLE: c_int = 2;
pub const CARRAY_TEXT: c_int = 3;
pub const CARRAY_BLOB: c_int = 4;

// Changeset conflict actions
pub const SQLITE_CHANGESET_OMIT: c_int = 0;
pub const SQLITE_CHANGESET_REPLACE: c_int = 1;
pub const SQLITE_CHANGESET_ABORT: c_int = 2;

// Checkpoint modes
pub const SQLITE_CHECKPOINT_PASSIVE: c_int = 0;
pub const SQLITE_CHECKPOINT_FULL: c_int = 1;
pub const SQLITE_CHECKPOINT_RESTART: c_int = 2;
pub const SQLITE_CHECKPOINT_TRUNCATE: c_int = 3;

// Function flags
pub const SQLITE_DETERMINISTIC: c_int = 0x000000800;

extern "C" {
    pub fn sqlite3_libversion() -> *const c_char;
    pub fn sqlite3_sourceid() -> *const c_char;

    pub fn sqlite3_open_v2(
        filename: *const c_char,
        ppDb: *mut *mut sqlite3,
        flags: c_int,
        zVfs: *const c_char,
    ) -> c_int;

    pub fn sqlite3_close_v2(db: *mut sqlite3) -> c_int;
    pub fn sqlite3_busy_timeout(db: *mut sqlite3, ms: c_int) -> c_int;
    pub fn sqlite3_errmsg(db: *mut sqlite3) -> *const c_char;
    pub fn sqlite3_errcode(db: *mut sqlite3) -> c_int;
    pub fn sqlite3_extended_errcode(db: *mut sqlite3) -> c_int;

    pub fn sqlite3_exec(
        db: *mut sqlite3,
        sql: *const c_char,
        callback: Option<
            unsafe extern "C" fn(*mut c_void, c_int, *mut *mut c_char, *mut *mut c_char) -> c_int,
        >,
        arg: *mut c_void,
        errmsg: *mut *mut c_char,
    ) -> c_int;

    pub fn sqlite3_free(p: *mut c_void);

    pub fn sqlite3_prepare_v3(
        db: *mut sqlite3,
        zSql: *const c_char,
        nByte: c_int,
        prepFlags: c_uint,
        ppStmt: *mut *mut sqlite3_stmt,
        pzTail: *mut *const c_char,
    ) -> c_int;

    pub fn sqlite3_finalize(pStmt: *mut sqlite3_stmt) -> c_int;
    pub fn sqlite3_step(pStmt: *mut sqlite3_stmt) -> c_int;
    pub fn sqlite3_reset(pStmt: *mut sqlite3_stmt) -> c_int;
    pub fn sqlite3_clear_bindings(pStmt: *mut sqlite3_stmt) -> c_int;

    // Parameter binding
    pub fn sqlite3_bind_blob64(
        pStmt: *mut sqlite3_stmt,
        i: c_int,
        zData: *const c_void,
        nData: u64,
        xDel: sqlite3_destructor_type,
    ) -> c_int;

    pub fn sqlite3_bind_text64(
        pStmt: *mut sqlite3_stmt,
        i: c_int,
        zData: *const c_char,
        nData: u64,
        xDel: sqlite3_destructor_type,
        enc: u8,
    ) -> c_int;

    pub fn sqlite3_bind_int64(pStmt: *mut sqlite3_stmt, i: c_int, iValue: i64) -> c_int;
    pub fn sqlite3_bind_double(pStmt: *mut sqlite3_stmt, i: c_int, rValue: f64) -> c_int;
    pub fn sqlite3_bind_null(pStmt: *mut sqlite3_stmt, i: c_int) -> c_int;

    // Column values
    pub fn sqlite3_column_count(pStmt: *mut sqlite3_stmt) -> c_int;
    pub fn sqlite3_column_type(pStmt: *mut sqlite3_stmt, iCol: c_int) -> c_int;
    pub fn sqlite3_column_int64(pStmt: *mut sqlite3_stmt, iCol: c_int) -> i64;
    pub fn sqlite3_column_double(pStmt: *mut sqlite3_stmt, iCol: c_int) -> f64;
    pub fn sqlite3_column_blob(pStmt: *mut sqlite3_stmt, iCol: c_int) -> *const c_void;
    pub fn sqlite3_column_bytes(pStmt: *mut sqlite3_stmt, iCol: c_int) -> c_int;
    pub fn sqlite3_column_text(pStmt: *mut sqlite3_stmt, iCol: c_int) -> *const c_uchar;
    pub fn sqlite3_column_name(pStmt: *mut sqlite3_stmt, iCol: c_int) -> *const c_char;
    pub fn sqlite3_stmt_readonly(pStmt: *mut sqlite3_stmt) -> c_int;
    pub fn sqlite3_db_handle(pStmt: *mut sqlite3_stmt) -> *mut sqlite3;

    // Metadata & Execution stats
    pub fn sqlite3_changes(db: *mut sqlite3) -> c_int;
    pub fn sqlite3_changes64(db: *mut sqlite3) -> i64;
    pub fn sqlite3_last_insert_rowid(db: *mut sqlite3) -> i64;
    pub fn sqlite3_interrupt(db: *mut sqlite3);
    pub fn sqlite3_db_cacheflush(db: *mut sqlite3) -> c_int;

    pub fn sqlite3_db_config(db: *mut sqlite3, op: c_int, ...) -> c_int;

    // Backup API
    pub fn sqlite3_backup_init(
        pDest: *mut sqlite3,
        zDestName: *const c_char,
        pSource: *mut sqlite3,
        zSourceName: *const c_char,
    ) -> *mut sqlite3_backup;
    pub fn sqlite3_backup_step(p: *mut sqlite3_backup, nPage: c_int) -> c_int;
    pub fn sqlite3_backup_finish(p: *mut sqlite3_backup) -> c_int;
    pub fn sqlite3_backup_remaining(p: *mut sqlite3_backup) -> c_int;
    pub fn sqlite3_backup_pagecount(p: *mut sqlite3_backup) -> c_int;

    // Checkpoint API
    pub fn sqlite3_wal_checkpoint_v2(
        db: *mut sqlite3,
        zDb: *const c_char,
        eMode: c_int,
        pnLog: *mut c_int,
        pnCkpt: *mut c_int,
    ) -> c_int;

    // Scalar function registration
    pub fn sqlite3_create_function_v2(
        db: *mut sqlite3,
        zFunctionName: *const c_char,
        nArg: c_int,
        eTextRep: c_int,
        pApp: *mut c_void,
        xFunc: Option<unsafe extern "C" fn(*mut sqlite3_context, c_int, *mut *mut sqlite3_value)>,
        xStep: Option<unsafe extern "C" fn(*mut sqlite3_context, c_int, *mut *mut sqlite3_value)>,
        xFinal: Option<unsafe extern "C" fn(*mut sqlite3_context)>,
        xDestroy: Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> c_int;

    // Values for scalar functions
    pub fn sqlite3_user_data(pCtx: *mut sqlite3_context) -> *mut c_void;
    pub fn sqlite3_value_type(pVal: *mut sqlite3_value) -> c_int;
    pub fn sqlite3_value_blob(pVal: *mut sqlite3_value) -> *const c_void;
    pub fn sqlite3_value_bytes(pVal: *mut sqlite3_value) -> c_int;
    pub fn sqlite3_value_text(pVal: *mut sqlite3_value) -> *const c_uchar;
    pub fn sqlite3_value_int64(pVal: *mut sqlite3_value) -> i64;
    pub fn sqlite3_value_double(pVal: *mut sqlite3_value) -> f64;

    // Results from scalar functions
    pub fn sqlite3_result_blob64(
        pCtx: *mut sqlite3_context,
        z: *const c_void,
        n: u64,
        xDel: sqlite3_destructor_type,
    );
    pub fn sqlite3_result_text64(
        pCtx: *mut sqlite3_context,
        z: *const c_char,
        n: u64,
        xDel: sqlite3_destructor_type,
        enc: u8,
    );
    pub fn sqlite3_result_int64(pCtx: *mut sqlite3_context, val: i64);
    pub fn sqlite3_result_double(pCtx: *mut sqlite3_context, val: f64);
    pub fn sqlite3_result_null(pCtx: *mut sqlite3_context);
    pub fn sqlite3_result_error(pCtx: *mut sqlite3_context, z: *const c_char, n: c_int);

    // Global config & Extensions
    pub fn sqlite3_config(op: c_int, ...) -> c_int;
    pub fn sqlite3_auto_extension(xEntryPoint: Option<unsafe extern "C" fn()>) -> c_int;

    // Carray
    pub fn sqlite3_carray_bind_v2(
        pStmt: *mut sqlite3_stmt,
        idx: c_int,
        aData: *mut c_void,
        nData: c_int,
        mFlags: c_int,
        xDestroy: sqlite3_destructor_type,
        pDestroy: *mut c_void,
    ) -> c_int;

    // Sessions & Changesets
    pub fn sqlite3session_create(
        db: *mut sqlite3,
        zDb: *const c_char,
        ppSession: *mut *mut sqlite3_session,
    ) -> c_int;
    pub fn sqlite3session_delete(pSession: *mut sqlite3_session);
    pub fn sqlite3session_enable(pSession: *mut sqlite3_session, bEnable: c_int) -> c_int;
    pub fn sqlite3session_attach(pSession: *mut sqlite3_session, zTab: *const c_char) -> c_int;
    pub fn sqlite3session_table_filter(
        pSession: *mut sqlite3_session,
        xFilter: Option<unsafe extern "C" fn(pCtx: *mut c_void, zTab: *const c_char) -> c_int>,
        pCtx: *mut c_void,
    );
    pub fn sqlite3session_isempty(pSession: *mut sqlite3_session) -> c_int;
    pub fn sqlite3session_changeset(
        pSession: *mut sqlite3_session,
        pnChangeset: *mut c_int,
        ppChangeset: *mut *mut c_void,
    ) -> c_int;
    pub fn sqlite3session_patchset(
        pSession: *mut sqlite3_session,
        pnPatchset: *mut c_int,
        ppPatchset: *mut *mut c_void,
    ) -> c_int;
    pub fn sqlite3changeset_apply(
        db: *mut sqlite3,
        nChangeset: c_int,
        pChangeset: *mut c_void,
        xFilter: Option<unsafe extern "C" fn(pCtx: *mut c_void, zTab: *const c_char) -> c_int>,
        xConflict: Option<
            unsafe extern "C" fn(
                pCtx: *mut c_void,
                eConflict: c_int,
                pIter: *mut sqlite3_changeset_iter,
            ) -> c_int,
        >,
        pCtx: *mut c_void,
    ) -> c_int;
    pub fn sqlite3changeset_invert(
        nIn: c_int,
        pIn: *const c_void,
        pnOut: *mut c_int,
        ppOut: *mut *mut c_void,
    ) -> c_int;

    // External extensions entry points
    pub fn sqlite3_series_init(
        db: *mut sqlite3,
        pzErrMsg: *mut *mut c_char,
        pApi: *const c_void,
    ) -> c_int;
    pub fn sqlite3_decimal_init(
        db: *mut sqlite3,
        pzErrMsg: *mut *mut c_char,
        pApi: *const c_void,
    ) -> c_int;
}

/// Convert an SQLite return code to a `LiliaError`.
#[must_use]
pub fn sqlite_code_to_lilia(rc: c_int, msg: impl Into<String>) -> LiliaError {
    let primary = rc & 0xFF;
    let code = match primary {
        SQLITE_BUSY | SQLITE_LOCKED => ErrorCode::Busy,
        SQLITE_CORRUPT | SQLITE_NOTADB => ErrorCode::Corrupt,
        SQLITE_FULL => ErrorCode::DiskFull,
        SQLITE_INTERRUPT => ErrorCode::Timeout,
        _ => ErrorCode::Storage,
    };
    let retryable = code == ErrorCode::Busy || code == ErrorCode::Timeout;
    LiliaError::new(code, msg.into(), retryable)
}
