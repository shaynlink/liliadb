//! The C ABI, for runtimes that call C directly (Flutter through `dart:ffi`, React Native
//! through JSI): open a database, run operations, close it. An operation goes in as
//! `MessagePack` or JSON ([`RequestEncoding`]); its result comes out as a JS frame
//! ([`lilia_protocol::to_js_frame`]): one JSON text, then the raw bytes it refers to.
//!
//! Memory: a frame from [`lilia_c_execute`] is freed with [`lilia_c_free_bytes`], an error
//! string with [`lilia_c_free_string`]; [`lilia_c_close`] closes a handle and frees it.

// reason: a C ABI receives and returns raw pointers; each function documents what it expects.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, CStr, CString};
use std::time::Duration;

use lilia_core::{ErrorCode, LiliaError};
use lilia_protocol::{
    execute_js, to_js_frame, OpenOptions, RequestEncoding, ResponseValue, SharedDatabase,
};
use serde::Deserialize;

/// An open database, shared by the calls made through it.
#[derive(Debug)]
pub struct LiliaHandle {
    database: SharedDatabase,
}

/// The JSON a database opens with: `path`, then any field of [`OpenOptions`].
#[derive(Deserialize)]
struct Open {
    path: String,
    #[serde(flatten)]
    options: OpenOptions,
}

/// Hands `message` to the caller through `out_err`, when it is not null.
unsafe fn report(out_err: *mut *mut c_char, message: &str) {
    if !out_err.is_null() {
        let message = CString::new(message.replace('\0', " ")).unwrap_or_default();
        // SAFETY: the caller passes either null or a valid pointer to write to.
        unsafe { *out_err = message.into_raw() };
    }
}

/// Hands `frame` to the caller: its length through `out_len`, its bytes as the result.
unsafe fn hand_over(frame: Vec<u8>, out_len: *mut usize) -> *mut u8 {
    let frame = frame.into_boxed_slice();
    if !out_len.is_null() {
        // SAFETY: the caller passes either null or a valid pointer to write to.
        unsafe { *out_len = frame.len() };
    }
    Box::into_raw(frame).cast::<u8>()
}

/// Opens (or creates) the database `options_json` describes (`{"path": ..., "durability":
/// ..., "profile": ..., "ledger": ..., "history_retention_ms": ..., "undo_depth": ...}`, all
/// but `path` optional). Null on failure, with the error's JSON through `out_err`.
///
/// # Safety
///
/// `options_json` is a NUL-terminated UTF-8 string; `out_err` is null or writable.
#[no_mangle]
pub unsafe extern "C" fn lilia_c_open(
    options_json: *const c_char,
    out_err: *mut *mut c_char,
) -> *mut LiliaHandle {
    let opened = (|| {
        if options_json.is_null() {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "no open options",
                false,
            ));
        }
        // SAFETY: the caller passes a NUL-terminated string.
        let json = unsafe { CStr::from_ptr(options_json) }.to_bytes();
        let open: Open = serde_json::from_slice(json).map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("invalid open options: {e}"),
                false,
            )
        })?;
        lilia_protocol::open(open.path, &open.options)
    })();
    match opened {
        Ok(database) => Box::into_raw(Box::new(LiliaHandle {
            database: SharedDatabase::new(database),
        })),
        Err(error) => {
            let json = serde_json::to_string(&error).unwrap_or_else(|_| error.to_string());
            // SAFETY: as documented for `out_err`.
            unsafe { report(out_err, &json) };
            std::ptr::null_mut()
        }
    }
}

/// Runs the operation `request` holds (`len` bytes; `encoding` 0 for `MessagePack`, 1 for
/// JSON) and returns the JS frame of its result, `{ Ok: value }` or `{ Err: error }`, with its
/// length through `out_len`. Null only when the frame cannot be written (past 4 GiB).
///
/// # Safety
///
/// `handle` comes from [`lilia_c_open`] and is not closed; `request` points to `len` readable
/// bytes; `out_len` is writable.
#[no_mangle]
pub unsafe extern "C" fn lilia_c_execute(
    handle: *const LiliaHandle,
    request: *const u8,
    len: usize,
    encoding: c_int,
    out_len: *mut usize,
) -> *mut u8 {
    let refused = |message: &str| {
        to_js_frame(&Err::<ResponseValue, _>(LiliaError::new(
            ErrorCode::InvalidInput,
            message,
            false,
        )))
    };
    let frame = if handle.is_null() || request.is_null() {
        refused("no database handle or no request")
    } else {
        let encoding = match encoding {
            0 => Some(RequestEncoding::MessagePack),
            1 => Some(RequestEncoding::Json),
            _ => None,
        };
        // SAFETY: the caller passes a live handle.
        let handle = unsafe { &*handle };
        match (encoding, handle.database.get()) {
            (None, _) => refused("unknown request encoding"),
            (Some(_), Err(error)) => to_js_frame(&Err::<ResponseValue, _>(error)),
            (Some(encoding), Ok(database)) => {
                // SAFETY: the caller passes `len` readable bytes.
                let request = unsafe { std::slice::from_raw_parts(request, len) };
                execute_js(&database, request, encoding)
            }
        }
    };
    match frame {
        // SAFETY: as documented for `out_len`.
        Ok(frame) => unsafe { hand_over(frame, out_len) },
        Err(_) => std::ptr::null_mut(),
    }
}

/// Closes the database once the calls running on it return, waiting at most `timeout_ms`,
/// and frees the handle. 0 on success; -1 with the error's JSON through `out_err`.
///
/// # Safety
///
/// `handle` comes from [`lilia_c_open`], is closed once, and is not used afterwards;
/// `out_err` is null or writable.
#[no_mangle]
pub unsafe extern "C" fn lilia_c_close(
    handle: *mut LiliaHandle,
    timeout_ms: u64,
    out_err: *mut *mut c_char,
) -> c_int {
    if handle.is_null() {
        return 0;
    }
    // SAFETY: the caller hands the handle back once.
    let handle = unsafe { Box::from_raw(handle) };
    match handle.database.close(Duration::from_millis(timeout_ms)) {
        Ok(()) => 0,
        Err(error) => {
            let json = serde_json::to_string(&error).unwrap_or_else(|_| error.to_string());
            // SAFETY: as documented for `out_err`.
            unsafe { report(out_err, &json) };
            -1
        }
    }
}

/// Frees a frame from [`lilia_c_execute`].
///
/// # Safety
///
/// `bytes` and `len` come from one [`lilia_c_execute`] call and are freed once.
#[no_mangle]
pub unsafe extern "C" fn lilia_c_free_bytes(bytes: *mut u8, len: usize) {
    if !bytes.is_null() {
        // SAFETY: the frame was handed out as a boxed slice of `len` bytes.
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(bytes, len)) });
    }
}

/// Frees an error string from [`lilia_c_open`] or [`lilia_c_close`].
///
/// # Safety
///
/// `message` comes from this library and is freed once.
#[no_mangle]
pub unsafe extern "C" fn lilia_c_free_string(message: *mut c_char) {
    if !message.is_null() {
        // SAFETY: the string was handed out by `CString::into_raw`.
        drop(unsafe { CString::from_raw(message) });
    }
}
