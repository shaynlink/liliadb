#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::Once;

use crate::ffi;

static INIT_SQLITE: Once = Once::new();

unsafe extern "C" fn sqlite_log_callback(
    _user_data: *mut c_void,
    err_code: c_int,
    msg: *const c_char,
) {
    let msg_str = if msg.is_null() {
        "<empty sqlite log message>"
    } else {
        CStr::from_ptr(msg)
            .to_str()
            .unwrap_or("<invalid utf-8 sqlite log message>")
    };

    let primary = err_code & 0xFF;
    match primary {
        ffi::SQLITE_ERROR | ffi::SQLITE_CORRUPT | ffi::SQLITE_NOTADB | ffi::SQLITE_IOERR => {
            tracing::error!(target: "sqlite", err_code = err_code, "{msg_str}");
        }
        ffi::SQLITE_BUSY | ffi::SQLITE_LOCKED | ffi::SQLITE_FULL => {
            tracing::warn!(target: "sqlite", err_code = err_code, "{msg_str}");
        }
        _ => {
            tracing::info!(target: "sqlite", err_code = err_code, "{msg_str}");
        }
    }
}

/// Initialize SQLite global process configuration (logger, auto-extensions).
/// Safe to call multiple times; execution is guarded by [`std::sync::Once`].
pub fn ensure_sqlite_initialized() {
    INIT_SQLITE.call_once(|| {
        unsafe {
            // Configure SQLite global warning/error log hook
            ffi::sqlite3_config(
                ffi::SQLITE_CONFIG_LOG,
                sqlite_log_callback as unsafe extern "C" fn(*mut c_void, c_int, *const c_char),
                std::ptr::null_mut::<c_void>(),
            );

            // Register auto-extensions (series, decimal)
            let series_ptr = ffi::sqlite3_series_init as *const ();
            let decimal_ptr = ffi::sqlite3_decimal_init as *const ();
            ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(),
            >(series_ptr)));
            ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(),
            >(decimal_ptr)));
        }
    });
}
