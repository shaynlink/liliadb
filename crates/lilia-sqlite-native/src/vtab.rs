#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;
use std::slice;
use std::sync::Arc;

use lilia_core::{ErrorCode, LiliaError, Result};

use crate::ffi;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintOp {
    Eq,
    Gt,
    Le,
    Lt,
    Ge,
    Like,
    Other,
}

impl ConstraintOp {
    fn from_sqlite(op: u8) -> Self {
        match op {
            ffi::SQLITE_INDEX_CONSTRAINT_EQ => Self::Eq,
            ffi::SQLITE_INDEX_CONSTRAINT_GT => Self::Gt,
            ffi::SQLITE_INDEX_CONSTRAINT_LE => Self::Le,
            ffi::SQLITE_INDEX_CONSTRAINT_LT => Self::Lt,
            ffi::SQLITE_INDEX_CONSTRAINT_GE => Self::Ge,
            ffi::SQLITE_INDEX_CONSTRAINT_LIKE => Self::Like,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct IndexConstraint {
    pub column: i32,
    pub op: ConstraintOp,
    pub usable: bool,
}

#[derive(Debug, Clone)]
pub struct IndexPlan {
    pub idx_num: i32,
    pub idx_str: Option<String>,
    /// Matches constraint index in `constraints` -> `(argv_index (1-based), omit)`.
    pub constraint_usage: Vec<(usize, i32, bool)>,
    pub estimated_cost: f64,
    pub estimated_rows: i64,
    pub unique_scan: bool,
}

impl Default for IndexPlan {
    fn default() -> Self {
        Self {
            idx_num: 0,
            idx_str: None,
            constraint_usage: Vec::new(),
            estimated_cost: 1000.0,
            estimated_rows: 1000,
            unique_scan: false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum VTabValue {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Blob(Vec<u8>),
}

pub trait VirtualCursor: Send {
    fn filter(&mut self, idx_num: i32, idx_str: Option<&str>, args: &[VTabValue]) -> Result<()>;
    fn next(&mut self) -> Result<()>;
    fn eof(&self) -> bool;
    fn column(&self, col: i32) -> Result<VTabValue>;
    fn rowid(&self) -> i64;
}

pub trait VirtualTableProvider: Send + Sync {
    fn schema_ddl(&self) -> &str;
    fn best_index(&self, constraints: &[IndexConstraint]) -> IndexPlan;
    fn open_cursor(&self) -> Result<Box<dyn VirtualCursor>>;
    fn on_begin(&self) -> Result<()> {
        Ok(())
    }
    fn on_sync(&self) -> Result<()> {
        Ok(())
    }
    fn on_commit(&self) -> Result<()> {
        Ok(())
    }
    fn on_rollback(&self) -> Result<()> {
        Ok(())
    }
}

#[repr(C)]
struct LiliaVTab {
    base: ffi::sqlite3_vtab,
    provider: Arc<dyn VirtualTableProvider>,
}

#[repr(C)]
struct LiliaVTabCursor {
    base: ffi::sqlite3_vtab_cursor,
    cursor: Box<dyn VirtualCursor>,
}

unsafe fn sqlite_alloc_cstr(msg: &str) -> *mut c_char {
    let sanitized: Vec<u8> = msg
        .as_bytes()
        .iter()
        .map(|&b| if b == 0 { b' ' } else { b })
        .collect();
    let len = sanitized.len();
    let ptr = ffi::sqlite3_malloc64((len + 1) as u64).cast::<u8>();
    if ptr.is_null() {
        return ptr::null_mut();
    }
    ptr::copy_nonoverlapping(sanitized.as_ptr(), ptr, len);
    *ptr.add(len) = 0;
    ptr.cast::<c_char>()
}

unsafe fn set_out_err(pz_err: *mut *mut c_char, msg: &str) {
    if !pz_err.is_null() {
        *pz_err = sqlite_alloc_cstr(msg);
    }
}

unsafe fn set_vtab_err(p_vtab: *mut ffi::sqlite3_vtab, msg: &str) {
    if !p_vtab.is_null() {
        if !(*p_vtab).zErrMsg.is_null() {
            ffi::sqlite3_free((*p_vtab).zErrMsg.cast());
        }
        (*p_vtab).zErrMsg = sqlite_alloc_cstr(msg);
    }
}

unsafe fn read_sqlite_value(val: *mut ffi::sqlite3_value) -> VTabValue {
    if val.is_null() {
        return VTabValue::Null;
    }
    match ffi::sqlite3_value_type(val) {
        ffi::SQLITE_INTEGER => VTabValue::Int(ffi::sqlite3_value_int64(val)),
        ffi::SQLITE_FLOAT => VTabValue::Float(ffi::sqlite3_value_double(val)),
        ffi::SQLITE_TEXT => {
            let ptr = ffi::sqlite3_value_text(val);
            let len = ffi::sqlite3_value_bytes(val);
            if ptr.is_null() || len <= 0 {
                VTabValue::Text(String::new())
            } else {
                let bytes = slice::from_raw_parts(ptr, len as usize);
                VTabValue::Text(String::from_utf8_lossy(bytes).into_owned())
            }
        }
        ffi::SQLITE_BLOB => {
            let ptr = ffi::sqlite3_value_blob(val);
            let len = ffi::sqlite3_value_bytes(val);
            if ptr.is_null() || len <= 0 {
                VTabValue::Blob(Vec::new())
            } else {
                let bytes = slice::from_raw_parts(ptr.cast::<u8>(), len as usize);
                VTabValue::Blob(bytes.to_vec())
            }
        }
        _ => VTabValue::Null,
    }
}

unsafe fn write_sqlite_result(ctx: *mut ffi::sqlite3_context, val: VTabValue) {
    match val {
        VTabValue::Null => ffi::sqlite3_result_null(ctx),
        VTabValue::Int(v) => ffi::sqlite3_result_int64(ctx, v),
        VTabValue::Float(v) => ffi::sqlite3_result_double(ctx, v),
        VTabValue::Text(s) => {
            ffi::sqlite3_result_text64(
                ctx,
                s.as_ptr().cast::<c_char>(),
                s.len() as u64,
                ffi::sqlite_transient(),
                ffi::SQLITE_UTF8,
            );
        }
        VTabValue::Blob(b) => {
            ffi::sqlite3_result_blob64(
                ctx,
                b.as_ptr().cast::<c_void>(),
                b.len() as u64,
                ffi::sqlite_transient(),
            );
        }
    }
}

unsafe extern "C" fn vtab_connect(
    db: *mut ffi::sqlite3,
    p_aux: *mut c_void,
    _argc: c_int,
    _argv: *const *const c_char,
    pp_vtab: *mut *mut ffi::sqlite3_vtab,
    pz_err: *mut *mut c_char,
) -> c_int {
    if p_aux.is_null() || pp_vtab.is_null() {
        return ffi::SQLITE_ERROR;
    }
    let provider = Arc::clone(&*p_aux.cast::<Arc<dyn VirtualTableProvider>>());
    let Ok(c_ddl) = CString::new(provider.schema_ddl()) else {
        set_out_err(pz_err, "virtual table DDL contains null byte");
        return ffi::SQLITE_ERROR;
    };
    let rc = ffi::sqlite3_declare_vtab(db, c_ddl.as_ptr());
    if rc != ffi::SQLITE_OK {
        set_out_err(pz_err, &format!("sqlite3_declare_vtab failed: rc={rc}"));
        return rc;
    }
    let vtab = Box::new(LiliaVTab {
        base: ffi::sqlite3_vtab {
            pModule: ptr::null(),
            nRef: 0,
            zErrMsg: ptr::null_mut(),
        },
        provider,
    });
    *pp_vtab = Box::into_raw(vtab).cast::<ffi::sqlite3_vtab>();
    ffi::SQLITE_OK
}

unsafe extern "C" fn vtab_disconnect(p_vtab: *mut ffi::sqlite3_vtab) -> c_int {
    if !p_vtab.is_null() {
        drop(Box::from_raw(p_vtab.cast::<LiliaVTab>()));
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn vtab_best_index(
    p_vtab: *mut ffi::sqlite3_vtab,
    p_info: *mut ffi::sqlite3_index_info,
) -> c_int {
    if p_vtab.is_null() || p_info.is_null() {
        return ffi::SQLITE_ERROR;
    }
    let vtab = &*(p_vtab.cast::<LiliaVTab>());
    let info = &mut *p_info;
    let n_constraints = if info.nConstraint > 0 {
        info.nConstraint as usize
    } else {
        0
    };
    let mut constraints = Vec::with_capacity(n_constraints);
    if n_constraints > 0 && !info.aConstraint.is_null() {
        let raw_constraints = slice::from_raw_parts(info.aConstraint, n_constraints);
        for c in raw_constraints {
            constraints.push(IndexConstraint {
                column: c.iColumn,
                op: ConstraintOp::from_sqlite(c.op),
                usable: c.usable != 0,
            });
        }
    }

    let plan = vtab.provider.best_index(&constraints);
    info.idxNum = plan.idx_num;
    info.estimatedCost = plan.estimated_cost;
    info.estimatedRows = plan.estimated_rows;
    if plan.unique_scan {
        info.idxFlags |= ffi::SQLITE_INDEX_SCAN_UNIQUE;
    }
    if let Some(idx_str) = plan.idx_str {
        info.idxStr = sqlite_alloc_cstr(&idx_str);
        info.needToFreeIdxStr = 1;
    }
    if n_constraints > 0 && !info.aConstraintUsage.is_null() {
        let usage = slice::from_raw_parts_mut(info.aConstraintUsage, n_constraints);
        for (idx, argv_index, omit) in plan.constraint_usage {
            if let Some(slot) = usage.get_mut(idx) {
                slot.argvIndex = argv_index;
                slot.omit = u8::from(omit);
            }
        }
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn vtab_open(
    p_vtab: *mut ffi::sqlite3_vtab,
    pp_cursor: *mut *mut ffi::sqlite3_vtab_cursor,
) -> c_int {
    if p_vtab.is_null() || pp_cursor.is_null() {
        return ffi::SQLITE_ERROR;
    }
    let vtab = &*(p_vtab.cast::<LiliaVTab>());
    match vtab.provider.open_cursor() {
        Ok(cursor) => {
            let boxed = Box::new(LiliaVTabCursor {
                base: ffi::sqlite3_vtab_cursor { pVtab: p_vtab },
                cursor,
            });
            *pp_cursor = Box::into_raw(boxed).cast::<ffi::sqlite3_vtab_cursor>();
            ffi::SQLITE_OK
        }
        Err(err) => {
            set_vtab_err(p_vtab, &err.to_string());
            ffi::SQLITE_ERROR
        }
    }
}

unsafe extern "C" fn vtab_close(p_cursor: *mut ffi::sqlite3_vtab_cursor) -> c_int {
    if !p_cursor.is_null() {
        drop(Box::from_raw(p_cursor.cast::<LiliaVTabCursor>()));
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn vtab_filter(
    p_cursor: *mut ffi::sqlite3_vtab_cursor,
    idx_num: c_int,
    idx_str: *const c_char,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) -> c_int {
    if p_cursor.is_null() {
        return ffi::SQLITE_ERROR;
    }
    let cur = &mut *(p_cursor.cast::<LiliaVTabCursor>());
    let idx_str_opt = if idx_str.is_null() {
        None
    } else {
        CStr::from_ptr(idx_str).to_str().ok()
    };
    let n = if argc > 0 { argc as usize } else { 0 };
    let mut args = Vec::with_capacity(n);
    if n > 0 && !argv.is_null() {
        let raw_args = slice::from_raw_parts(argv, n);
        for &val_ptr in raw_args {
            args.push(read_sqlite_value(val_ptr));
        }
    }
    match cur.cursor.filter(idx_num, idx_str_opt, &args) {
        Ok(()) => ffi::SQLITE_OK,
        Err(err) => {
            set_vtab_err(cur.base.pVtab, &err.to_string());
            ffi::SQLITE_ERROR
        }
    }
}

unsafe extern "C" fn vtab_next(p_cursor: *mut ffi::sqlite3_vtab_cursor) -> c_int {
    if p_cursor.is_null() {
        return ffi::SQLITE_ERROR;
    }
    let cur = &mut *(p_cursor.cast::<LiliaVTabCursor>());
    match cur.cursor.next() {
        Ok(()) => ffi::SQLITE_OK,
        Err(err) => {
            set_vtab_err(cur.base.pVtab, &err.to_string());
            ffi::SQLITE_ERROR
        }
    }
}

unsafe extern "C" fn vtab_eof(p_cursor: *mut ffi::sqlite3_vtab_cursor) -> c_int {
    if p_cursor.is_null() {
        return 1;
    }
    let cur = &*(p_cursor.cast::<LiliaVTabCursor>());
    c_int::from(cur.cursor.eof())
}

unsafe extern "C" fn vtab_column(
    p_cursor: *mut ffi::sqlite3_vtab_cursor,
    ctx: *mut ffi::sqlite3_context,
    col: c_int,
) -> c_int {
    if p_cursor.is_null() || ctx.is_null() {
        return ffi::SQLITE_ERROR;
    }
    let cur = &*(p_cursor.cast::<LiliaVTabCursor>());
    match cur.cursor.column(col) {
        Ok(val) => {
            write_sqlite_result(ctx, val);
            ffi::SQLITE_OK
        }
        Err(err) => {
            set_vtab_err(cur.base.pVtab, &err.to_string());
            ffi::SQLITE_ERROR
        }
    }
}

unsafe extern "C" fn vtab_rowid(
    p_cursor: *mut ffi::sqlite3_vtab_cursor,
    p_rowid: *mut i64,
) -> c_int {
    if p_cursor.is_null() || p_rowid.is_null() {
        return ffi::SQLITE_ERROR;
    }
    let cur = &*(p_cursor.cast::<LiliaVTabCursor>());
    *p_rowid = cur.cursor.rowid();
    ffi::SQLITE_OK
}

unsafe extern "C" fn vtab_begin(p_vtab: *mut ffi::sqlite3_vtab) -> c_int {
    if p_vtab.is_null() {
        return ffi::SQLITE_OK;
    }
    let vtab = &*(p_vtab.cast::<LiliaVTab>());
    match vtab.provider.on_begin() {
        Ok(()) => ffi::SQLITE_OK,
        Err(err) => {
            set_vtab_err(p_vtab, &err.to_string());
            ffi::SQLITE_ERROR
        }
    }
}

unsafe extern "C" fn vtab_sync(p_vtab: *mut ffi::sqlite3_vtab) -> c_int {
    if p_vtab.is_null() {
        return ffi::SQLITE_OK;
    }
    let vtab = &*(p_vtab.cast::<LiliaVTab>());
    match vtab.provider.on_sync() {
        Ok(()) => ffi::SQLITE_OK,
        Err(err) => {
            set_vtab_err(p_vtab, &err.to_string());
            ffi::SQLITE_ERROR
        }
    }
}

unsafe extern "C" fn vtab_commit(p_vtab: *mut ffi::sqlite3_vtab) -> c_int {
    if p_vtab.is_null() {
        return ffi::SQLITE_OK;
    }
    let vtab = &*(p_vtab.cast::<LiliaVTab>());
    match vtab.provider.on_commit() {
        Ok(()) => ffi::SQLITE_OK,
        Err(err) => {
            set_vtab_err(p_vtab, &err.to_string());
            ffi::SQLITE_ERROR
        }
    }
}

unsafe extern "C" fn vtab_rollback(p_vtab: *mut ffi::sqlite3_vtab) -> c_int {
    if p_vtab.is_null() {
        return ffi::SQLITE_OK;
    }
    let vtab = &*(p_vtab.cast::<LiliaVTab>());
    match vtab.provider.on_rollback() {
        Ok(()) => ffi::SQLITE_OK,
        Err(err) => {
            set_vtab_err(p_vtab, &err.to_string());
            ffi::SQLITE_ERROR
        }
    }
}

unsafe extern "C" fn destroy_provider(p_aux: *mut c_void) {
    if !p_aux.is_null() {
        drop(Box::from_raw(
            p_aux.cast::<Arc<dyn VirtualTableProvider>>(),
        ));
    }
}

static LILIA_EPONYMOUS_MODULE: ffi::sqlite3_module = ffi::sqlite3_module {
    iVersion: 2,
    xCreate: None,
    xConnect: Some(vtab_connect),
    xBestIndex: Some(vtab_best_index),
    xDisconnect: Some(vtab_disconnect),
    xDestroy: Some(vtab_disconnect),
    xOpen: Some(vtab_open),
    xClose: Some(vtab_close),
    xFilter: Some(vtab_filter),
    xNext: Some(vtab_next),
    xEof: Some(vtab_eof),
    xColumn: Some(vtab_column),
    xRowid: Some(vtab_rowid),
    xUpdate: None,
    xBegin: Some(vtab_begin),
    xSync: Some(vtab_sync),
    xCommit: Some(vtab_commit),
    xRollback: Some(vtab_rollback),
    xFindFunction: None,
    xRename: None,
    xSavepoint: None,
    xRelease: None,
    xRollbackTo: None,
    xShadowName: None,
    xIntegrity: None,
};

pub(crate) unsafe fn register_eponymous_module(
    db: *mut ffi::sqlite3,
    name: &str,
    provider: Arc<dyn VirtualTableProvider>,
) -> Result<()> {
    let c_name = CString::new(name).map_err(|e| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("null byte in vtab module name: {e}"),
            false,
        )
    })?;
    let client_ptr = Box::into_raw(Box::new(provider));
    let rc = ffi::sqlite3_create_module_v2(
        db,
        c_name.as_ptr(),
        &LILIA_EPONYMOUS_MODULE,
        client_ptr.cast::<c_void>(),
        Some(destroy_provider),
    );
    if rc != ffi::SQLITE_OK {
        // NOTE: SQLite invokes destroy_provider automatically on failure of sqlite3_create_module_v2.
        return Err(ffi::sqlite_code_to_lilia(
            rc,
            format!("sqlite3_create_module_v2({name}) failed: rc={rc}"),
        ));
    }
    Ok(())
}
