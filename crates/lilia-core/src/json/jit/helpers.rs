#![allow(unsafe_code)]

use crate::json::binary::{read_value, BinaryValueRef};

/// Reference to a contiguous BinaryDoc byte buffer in memory.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DocRef {
    pub ptr: *const u8,
    pub len: usize,
}

#[inline(always)]
unsafe fn extract_val<'a>(data: &'a [u8], path: &str) -> Option<BinaryValueRef<'a>> {
    if data.len() < 8 {
        return None;
    }
    let offset_bytes: [u8; 4] = data.get(4..8)?.try_into().ok()?;
    let offset = u32::from_le_bytes(offset_bytes) as usize;
    let root = read_value(data, offset)?;
    if !path.contains('.') && !path.contains('/') {
        if let BinaryValueRef::Object(obj) = root {
            return obj.get(path);
        }
    }
    root.extract(path)
}

/// Extracts a numeric value (f64 or i64 promoted to f64) by path.
///
/// Returns 1 if present and numeric, 0 otherwise.
///
/// # Safety
///
/// If pointers are non-null:
/// - `doc_ptr` must point to at least `doc_len` readable bytes.
/// - `path_ptr` must point to at least `path_len` valid UTF-8 bytes.
/// - `out_val` must point to a writable `f64`.
#[no_mangle]
pub unsafe extern "C" fn lilia_jit_extract_f64(
    doc_ptr: *const u8,
    doc_len: usize,
    path_ptr: *const u8,
    path_len: usize,
    out_val: *mut f64,
) -> u8 {
    if doc_ptr.is_null() || path_ptr.is_null() || out_val.is_null() || doc_len < 8 {
        return 0;
    }
    let data = std::slice::from_raw_parts(doc_ptr, doc_len);
    let path = std::str::from_utf8_unchecked(std::slice::from_raw_parts(path_ptr, path_len));

    if let Some(val) = extract_val(data, path) {
        if let Some(f) = val.as_f64() {
            *out_val = f;
            return 1;
        }
        if let Some(i) = val.as_i64() {
            *out_val = i as f64;
            return 1;
        }
    }
    0
}

/// Extracts an integer value (i64) by path.
///
/// Returns 1 if present and integer (or exact float integer), 0 otherwise.
///
/// # Safety
///
/// If pointers are non-null:
/// - `doc_ptr` must point to at least `doc_len` readable bytes.
/// - `path_ptr` must point to at least `path_len` valid UTF-8 bytes.
/// - `out_val` must point to a writable `i64`.
#[no_mangle]
pub unsafe extern "C" fn lilia_jit_extract_i64(
    doc_ptr: *const u8,
    doc_len: usize,
    path_ptr: *const u8,
    path_len: usize,
    out_val: *mut i64,
) -> u8 {
    if doc_ptr.is_null() || path_ptr.is_null() || out_val.is_null() || doc_len < 8 {
        return 0;
    }
    let data = std::slice::from_raw_parts(doc_ptr, doc_len);
    let path = std::str::from_utf8_unchecked(std::slice::from_raw_parts(path_ptr, path_len));

    if let Some(val) = extract_val(data, path) {
        if let Some(i) = val.as_i64() {
            *out_val = i;
            return 1;
        }
        if let Some(f) = val.as_f64() {
            if let Some(i) = crate::json::binary::exact_int(f) {
                *out_val = i;
                return 1;
            }
        }
    }
    0
}

/// Extracts a boolean value by path.
///
/// Returns 1 if present and boolean, 0 otherwise.
///
/// # Safety
///
/// If pointers are non-null:
/// - `doc_ptr` must point to at least `doc_len` readable bytes.
/// - `path_ptr` must point to at least `path_len` valid UTF-8 bytes.
/// - `out_val` must point to a writable `u8`.
#[no_mangle]
pub unsafe extern "C" fn lilia_jit_extract_bool(
    doc_ptr: *const u8,
    doc_len: usize,
    path_ptr: *const u8,
    path_len: usize,
    out_val: *mut u8,
) -> u8 {
    if doc_ptr.is_null() || path_ptr.is_null() || out_val.is_null() || doc_len < 8 {
        return 0;
    }
    let data = std::slice::from_raw_parts(doc_ptr, doc_len);
    let path = std::str::from_utf8_unchecked(std::slice::from_raw_parts(path_ptr, path_len));

    if let Some(val) = extract_val(data, path) {
        if let Some(b) = val.as_bool() {
            *out_val = if b { 1 } else { 0 };
            return 1;
        }
    }
    0
}

/// Extracts a borrowed string slice (pointer + length) by path.
///
/// Returns 1 if present and string, 0 otherwise.
///
/// # Safety
///
/// If pointers are non-null:
/// - `doc_ptr` must point to at least `doc_len` readable bytes.
/// - `path_ptr` must point to at least `path_len` valid UTF-8 bytes.
/// - `out_str_ptr` and `out_str_len` must point to writable memory.
#[no_mangle]
pub unsafe extern "C" fn lilia_jit_extract_str(
    doc_ptr: *const u8,
    doc_len: usize,
    path_ptr: *const u8,
    path_len: usize,
    out_str_ptr: *mut *const u8,
    out_str_len: *mut usize,
) -> u8 {
    if doc_ptr.is_null()
        || path_ptr.is_null()
        || out_str_ptr.is_null()
        || out_str_len.is_null()
        || doc_len < 8
    {
        return 0;
    }
    let data = std::slice::from_raw_parts(doc_ptr, doc_len);
    let path = std::str::from_utf8_unchecked(std::slice::from_raw_parts(path_ptr, path_len));

    if let Some(val) = extract_val(data, path) {
        if let Some(s) = val.as_str() {
            *out_str_ptr = s.as_ptr();
            *out_str_len = s.len();
            return 1;
        }
    }
    0
}

/// Checks if a field is present and non-null.
///
/// Returns 1 if present and not null, 0 if null or missing.
///
/// # Safety
///
/// If pointers are non-null:
/// - `doc_ptr` must point to at least `doc_len` readable bytes.
/// - `path_ptr` must point to at least `path_len` valid UTF-8 bytes.
#[no_mangle]
pub unsafe extern "C" fn lilia_jit_is_present_and_not_null(
    doc_ptr: *const u8,
    doc_len: usize,
    path_ptr: *const u8,
    path_len: usize,
) -> u8 {
    if doc_ptr.is_null() || path_ptr.is_null() || doc_len < 8 {
        return 0;
    }
    let data = std::slice::from_raw_parts(doc_ptr, doc_len);
    let path = std::str::from_utf8_unchecked(std::slice::from_raw_parts(path_ptr, path_len));

    if let Some(val) = extract_val(data, path) {
        if !val.is_null() {
            return 1;
        }
    }
    0
}

/// Compares two memory buffers for byte equality.
///
/// Returns 0 if equal, non-zero if different.
///
/// # Safety
///
/// If pointers are non-null and `len > 0`:
/// - `p1` and `p2` must point to at least `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn lilia_jit_memcmp(p1: *const u8, p2: *const u8, len: usize) -> i32 {
    if p1 == p2 || len == 0 {
        return 0;
    }
    if p1.is_null() || p2.is_null() {
        return -1;
    }
    let s1 = std::slice::from_raw_parts(p1, len);
    let s2 = std::slice::from_raw_parts(p2, len);
    if s1 == s2 {
        0
    } else {
        1
    }
}

/// Compares two UTF-8 string slices lexicographically.
///
/// Returns -1 if s1 < s2, 0 if s1 == s2, 1 if s1 > s2.
///
/// # Safety
///
/// If pointers are non-null:
/// - `p1` must point to at least `l1` valid UTF-8 bytes.
/// - `p2` must point to at least `l2` valid UTF-8 bytes.
#[no_mangle]
pub unsafe extern "C" fn lilia_jit_strcmp(
    p1: *const u8,
    l1: usize,
    p2: *const u8,
    l2: usize,
) -> i32 {
    let s1 = if p1.is_null() || l1 == 0 {
        ""
    } else {
        std::str::from_utf8_unchecked(std::slice::from_raw_parts(p1, l1))
    };
    let s2 = if p2.is_null() || l2 == 0 {
        ""
    } else {
        std::str::from_utf8_unchecked(std::slice::from_raw_parts(p2, l2))
    };

    match s1.cmp(s2) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}
