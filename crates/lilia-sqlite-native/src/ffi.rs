#![allow(non_camel_case_types, non_snake_case, unsafe_code)]

use std::ffi::{c_char, c_int, c_uchar, c_uint, c_void};

pub enum sqlite3 {}
pub enum sqlite3_stmt {}

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
pub const SQLITE_MISUSE: c_int = 21;
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

// DB Config ops
pub const SQLITE_DBCONFIG_DEFENSIVE: c_int = 1010;

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

    // Metadata & Execution stats
    pub fn sqlite3_changes(db: *mut sqlite3) -> c_int;
    pub fn sqlite3_changes64(db: *mut sqlite3) -> i64;
    pub fn sqlite3_last_insert_rowid(db: *mut sqlite3) -> i64;
    pub fn sqlite3_interrupt(db: *mut sqlite3);

    pub fn sqlite3_db_config(db: *mut sqlite3, op: c_int, ...) -> c_int;
}
