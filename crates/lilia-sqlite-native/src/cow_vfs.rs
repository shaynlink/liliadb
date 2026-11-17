#![allow(
    unsafe_code,
    non_snake_case,
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

use lilia_core::{ErrorCode, LiliaError, Result};

use crate::ffi;

pub const COW_VFS_NAME: &str = "lilia-cow";

/// In-memory state tracking Copy-on-Write modifications for a specific database branch.
pub struct BranchVfsState {
    pub branch_id: String,
    pub parent_path: PathBuf,
    pub parent_file: Option<File>,
    pub parent_bytes: Option<Vec<u8>>,
    pub page_size: usize,
    pub page_overlay: RwLock<HashMap<u64, Vec<u8>>>,
    pub file_size: AtomicU64,
    pub journal_buffer: RwLock<Vec<u8>>,
    pub open_connections: AtomicUsize,
}

impl std::fmt::Debug for BranchVfsState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BranchVfsState")
            .field("branch_id", &self.branch_id)
            .field("parent_path", &self.parent_path)
            .field("file_size", &self.file_size.load(Ordering::Relaxed))
            .field("dirty_pages", &self.page_overlay.read().unwrap().len())
            .finish()
    }
}

impl BranchVfsState {
    /// Create a new branch state pointing to a parent database file or in-memory buffer.
    pub fn new(branch_id: String, parent_path: impl AsRef<Path>, parent_bytes: Option<Vec<u8>>) -> Result<Arc<Self>> {
        let parent_path = parent_path.as_ref().to_path_buf();
        let (parent_file, initial_size, detected_page_size) = if let Some(ref bytes) = parent_bytes {
            let pgsz = if bytes.len() >= 18 {
                let sz = u16::from_be_bytes([bytes[16], bytes[17]]) as usize;
                if sz == 1 { 65536 } else if sz >= 512 { sz } else { 4096 }
            } else {
                4096
            };
            (None, bytes.len() as u64, pgsz)
        } else if parent_path.exists() {
            let file = File::open(&parent_path).map_err(|e| {
                LiliaError::new(ErrorCode::Io, format!("failed to open parent db for branch: {e}"), false)
            })?;
            let len = file.metadata().map(|m| m.len()).unwrap_or(0);
            let mut header = [0u8; 18];
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileExt;
                let _ = file.read_exact_at(&mut header, 0);
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::FileExt;
                let _ = file.seek_read(&mut header, 0);
            }
            let pgsz = {
                let sz = u16::from_be_bytes([header[16], header[17]]) as usize;
                if sz == 1 { 65536 } else if sz >= 512 { sz } else { 4096 }
            };
            (Some(file), len, pgsz)
        } else {
            (None, 0, 4096)
        };

        Ok(Arc::new(Self {
            branch_id,
            parent_path,
            parent_file,
            parent_bytes,
            page_size: detected_page_size,
            page_overlay: RwLock::new(HashMap::new()),
            file_size: AtomicU64::new(initial_size),
            journal_buffer: RwLock::new(Vec::new()),
            open_connections: AtomicUsize::new(0),
        }))
    }

    /// Number of modified pages stored in the Copy-on-Write overlay.
    pub fn dirty_page_count(&self) -> usize {
        self.page_overlay.read().unwrap().len()
    }

    /// Total bytes held in memory by the dirty page overlay.
    pub fn memory_bytes(&self) -> usize {
        self.page_overlay.read().unwrap().len() * self.page_size
    }

    /// Read data from the Copy-on-Write branch view.
    pub fn read_bytes(&self, buf: &mut [u8], offset: u64) -> Result<()> {
        let amt = buf.len();
        if amt == 0 {
            return Ok(());
        }

        let page_size = self.page_size as u64;
        let overlay = self.page_overlay.read().unwrap();

        let page_start = offset / page_size;
        let page_end = (offset + amt as u64 - 1) / page_size;

        for pg in page_start..=page_end {
            let pg_byte_start = pg * page_size;
            let chunk_start = offset.max(pg_byte_start);
            let chunk_end = (offset + amt as u64).min(pg_byte_start + page_size);
            let chunk_len = (chunk_end - chunk_start) as usize;

            let dest_offset = (chunk_start - offset) as usize;
            let pg_internal_offset = (chunk_start - pg_byte_start) as usize;

            if let Some(dirty_page) = overlay.get(&pg) {
                let src_slice = &dirty_page[pg_internal_offset..pg_internal_offset + chunk_len];
                buf[dest_offset..dest_offset + chunk_len].copy_from_slice(src_slice);
            } else if let Some(ref file) = self.parent_file {
                let target = &mut buf[dest_offset..dest_offset + chunk_len];
                #[cfg(unix)]
                {
                    use std::os::unix::fs::FileExt;
                    if let Err(e) = file.read_exact_at(target, chunk_start) {
                        if e.kind() == std::io::ErrorKind::UnexpectedEof {
                            target.fill(0);
                        } else {
                            return Err(LiliaError::new(ErrorCode::Io, format!("branch pread error: {e}"), false));
                        }
                    }
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::FileExt;
                    if let Err(e) = file.seek_read(target, chunk_start) {
                        target.fill(0);
                    }
                }
                #[cfg(not(any(unix, windows)))]
                {
                    target.fill(0);
                }
            } else if let Some(ref bytes) = self.parent_bytes {
                let target = &mut buf[dest_offset..dest_offset + chunk_len];
                let cs = chunk_start as usize;
                let ce = chunk_end as usize;
                if cs < bytes.len() {
                    let avail = ce.min(bytes.len());
                    let copy_len = avail.saturating_sub(cs);
                    target[..copy_len].copy_from_slice(&bytes[cs..avail]);
                    if copy_len < chunk_len {
                        target[copy_len..].fill(0);
                    }
                } else {
                    target.fill(0);
                }
            } else {
                buf[dest_offset..dest_offset + chunk_len].fill(0);
            }
        }

        // Force rollback journal format (1, 1) instead of WAL (2, 2)
        // so branch connections operate with in-memory rollback journal without requiring WAL shared memory.
        if offset <= 18 && (offset + amt as u64) > 18 {
            buf[(18 - offset) as usize] = 1;
        }
        if offset <= 19 && (offset + amt as u64) > 19 {
            buf[(19 - offset) as usize] = 1;
        }

        Ok(())
    }

    /// Write data into the Copy-on-Write overlay.
    pub fn write_bytes(&self, buf: &[u8], offset: u64) -> Result<()> {
        let amt = buf.len();
        if amt == 0 {
            return Ok(());
        }

        let page_size = self.page_size as u64;
        let mut overlay = self.page_overlay.write().unwrap();

        let page_start = offset / page_size;
        let page_end = (offset + amt as u64 - 1) / page_size;

        for pg in page_start..=page_end {
            let pg_byte_start = pg * page_size;
            let chunk_start = offset.max(pg_byte_start);
            let chunk_end = (offset + amt as u64).min(pg_byte_start + page_size);
            let chunk_len = (chunk_end - chunk_start) as usize;

            let src_offset = (chunk_start - offset) as usize;
            let pg_internal_offset = (chunk_start - pg_byte_start) as usize;

            let page_buf = overlay.entry(pg).or_insert_with(|| {
                let mut base_page = vec![0u8; self.page_size];
                if let Some(ref file) = self.parent_file {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::FileExt;
                        let _ = file.read_exact_at(&mut base_page, pg_byte_start);
                    }
                    #[cfg(windows)]
                    {
                        use std::os::windows::fs::FileExt;
                        let _ = file.seek_read(&mut base_page, pg_byte_start);
                    }
                } else if let Some(ref bytes) = self.parent_bytes {
                    let ps = pg_byte_start as usize;
                    if ps < bytes.len() {
                        let pe = (ps + self.page_size).min(bytes.len());
                        base_page[..pe - ps].copy_from_slice(&bytes[ps..pe]);
                    }
                }
                base_page
            });

            page_buf[pg_internal_offset..pg_internal_offset + chunk_len]
                .copy_from_slice(&buf[src_offset..src_offset + chunk_len]);
        }

        let new_size = offset + amt as u64;
        self.file_size.fetch_max(new_size, Ordering::AcqRel);

        Ok(())
    }

    /// Truncate the virtual file size.
    pub fn truncate(&self, size: u64) {
        self.file_size.store(size, Ordering::Release);
        let page_size = self.page_size as u64;
        let mut overlay = self.page_overlay.write().unwrap();
        overlay.retain(|&pg, _| pg * page_size < size);
    }
}

static BRANCH_REGISTRY: LazyLock<RwLock<HashMap<String, Arc<BranchVfsState>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Register a branch VFS state so SQLite can open it with `vfs=lilia-cow&branch_id=...`.
pub fn register_branch_state(state: Arc<BranchVfsState>) {
    ensure_cow_vfs_registered();
    let mut reg = BRANCH_REGISTRY.write().unwrap();
    reg.insert(state.branch_id.clone(), state);
}

/// Retrieve a registered branch VFS state.
#[must_use]
pub fn get_branch_state(branch_id: &str) -> Option<Arc<BranchVfsState>> {
    let reg = BRANCH_REGISTRY.read().unwrap();
    reg.get(branch_id).cloned()
}

/// Unregister a branch VFS state upon merge or drop.
pub fn unregister_branch_state(branch_id: &str) {
    let mut reg = BRANCH_REGISTRY.write().unwrap();
    reg.remove(branch_id);
}

#[repr(C)]
struct CowFile {
    base: ffi::sqlite3_file,
    state_ptr: *const BranchVfsState,
    is_journal: bool,
}

static COW_IO_METHODS: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    iVersion: 1,
    xClose: Some(cow_xClose),
    xRead: Some(cow_xRead),
    xWrite: Some(cow_xWrite),
    xTruncate: Some(cow_xTruncate),
    xSync: Some(cow_xSync),
    xFileSize: Some(cow_xFileSize),
    xLock: Some(cow_xLock),
    xUnlock: Some(cow_xUnlock),
    xCheckReservedLock: Some(cow_xCheckReservedLock),
    xFileControl: Some(cow_xFileControl),
    xSectorSize: Some(cow_xSectorSize),
    xDeviceCharacteristics: Some(cow_xDeviceCharacteristics),
    xShmMap: None,
    xShmLock: None,
    xShmBarrier: None,
    xShmUnmap: None,
    xFetch: None,
    xUnfetch: None,
};

unsafe extern "C" fn cow_xClose(p_file: *mut ffi::sqlite3_file) -> c_int {
    let file = p_file.cast::<CowFile>();
    if !file.is_null() && !(*file).state_ptr.is_null() {
        let state = Arc::from_raw((*file).state_ptr);
        state.open_connections.fetch_sub(1, Ordering::Relaxed);
        (*file).state_ptr = ptr::null();
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn cow_xRead(
    p_file: *mut ffi::sqlite3_file,
    z_buf: *mut c_void,
    i_amt: c_int,
    i_ofst: i64,
) -> c_int {
    if z_buf.is_null() || i_amt <= 0 {
        return ffi::SQLITE_OK;
    }
    let file = p_file.cast::<CowFile>();
    if file.is_null() || (*file).state_ptr.is_null() {
        return ffi::SQLITE_IOERR;
    }
    let state = &*(*file).state_ptr;

    let target = std::slice::from_raw_parts_mut(z_buf.cast::<u8>(), i_amt as usize);

    if (*file).is_journal {
        let j = state.journal_buffer.read().unwrap();
        let off = i_ofst as usize;
        let amt = i_amt as usize;
        if off < j.len() {
            let avail = (j.len() - off).min(amt);
            target[..avail].copy_from_slice(&j[off..off + avail]);
            if avail < amt {
                target[avail..].fill(0);
                return ffi::SQLITE_IOERR_SHORT_READ;
            }
            ffi::SQLITE_OK
        } else {
            target.fill(0);
            ffi::SQLITE_IOERR_SHORT_READ
        }
    } else {
        let off = i_ofst as u64;
        let current_size = state.file_size.load(Ordering::Acquire);
        if off >= current_size {
            target.fill(0);
            return ffi::SQLITE_IOERR_SHORT_READ;
        }

        match state.read_bytes(target, off) {
            Ok(()) => {
                if off + i_amt as u64 > current_size {
                    ffi::SQLITE_IOERR_SHORT_READ
                } else {
                    ffi::SQLITE_OK
                }
            }
            Err(_) => ffi::SQLITE_IOERR,
        }
    }
}

unsafe extern "C" fn cow_xWrite(
    p_file: *mut ffi::sqlite3_file,
    z_buf: *const c_void,
    i_amt: c_int,
    i_ofst: i64,
) -> c_int {
    if z_buf.is_null() || i_amt <= 0 {
        return ffi::SQLITE_OK;
    }
    let file = p_file.cast::<CowFile>();
    if file.is_null() || (*file).state_ptr.is_null() {
        return ffi::SQLITE_IOERR;
    }
    let state = &*(*file).state_ptr;

    let src = std::slice::from_raw_parts(z_buf.cast::<u8>(), i_amt as usize);

    if (*file).is_journal {
        let mut j = state.journal_buffer.write().unwrap();
        let off = i_ofst as usize;
        let req_len = off + i_amt as usize;
        if req_len > j.len() {
            j.resize(req_len, 0);
        }
        j[off..req_len].copy_from_slice(src);
        ffi::SQLITE_OK
    } else {
        match state.write_bytes(src, i_ofst as u64) {
            Ok(()) => ffi::SQLITE_OK,
            Err(_) => ffi::SQLITE_IOERR,
        }
    }
}

unsafe extern "C" fn cow_xTruncate(p_file: *mut ffi::sqlite3_file, size: i64) -> c_int {
    let file = p_file.cast::<CowFile>();
    if file.is_null() || (*file).state_ptr.is_null() {
        return ffi::SQLITE_IOERR;
    }
    let state = &*(*file).state_ptr;

    if (*file).is_journal {
        let mut j = state.journal_buffer.write().unwrap();
        j.truncate(size as usize);
    } else {
        state.truncate(size as u64);
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn cow_xSync(_p_file: *mut ffi::sqlite3_file, _flags: c_int) -> c_int {
    ffi::SQLITE_OK
}

unsafe extern "C" fn cow_xFileSize(p_file: *mut ffi::sqlite3_file, p_size: *mut i64) -> c_int {
    if p_size.is_null() {
        return ffi::SQLITE_MISUSE;
    }
    let file = p_file.cast::<CowFile>();
    if file.is_null() || (*file).state_ptr.is_null() {
        return ffi::SQLITE_IOERR;
    }
    let state = &*(*file).state_ptr;

    if (*file).is_journal {
        *p_size = state.journal_buffer.read().unwrap().len() as i64;
    } else {
        *p_size = state.file_size.load(Ordering::Acquire) as i64;
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn cow_xLock(_p_file: *mut ffi::sqlite3_file, _lock: c_int) -> c_int {
    ffi::SQLITE_OK
}

unsafe extern "C" fn cow_xUnlock(_p_file: *mut ffi::sqlite3_file, _lock: c_int) -> c_int {
    ffi::SQLITE_OK
}

unsafe extern "C" fn cow_xCheckReservedLock(_p_file: *mut ffi::sqlite3_file, p_res_out: *mut c_int) -> c_int {
    if !p_res_out.is_null() {
        *p_res_out = 0;
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn cow_xFileControl(_p_file: *mut ffi::sqlite3_file, _op: c_int, _p_arg: *mut c_void) -> c_int {
    ffi::SQLITE_NOTFOUND
}

unsafe extern "C" fn cow_xSectorSize(_p_file: *mut ffi::sqlite3_file) -> c_int {
    4096
}

unsafe extern "C" fn cow_xDeviceCharacteristics(_p_file: *mut ffi::sqlite3_file) -> c_int {
    ffi::SQLITE_IOCAP_POWERSAFE_OVERWRITE | ffi::SQLITE_IOCAP_UNDELETABLE_WHEN_OPEN
}

// VFS Callbacks

unsafe extern "C" fn cow_xOpen(
    _p_vfs: *mut ffi::sqlite3_vfs,
    z_name: *const c_char,
    p_file: *mut ffi::sqlite3_file,
    flags: c_int,
    p_out_flags: *mut c_int,
) -> c_int {
    if p_file.is_null() {
        return ffi::SQLITE_MISUSE;
    }

    let is_journal = (flags
        & (ffi::SQLITE_OPEN_MAIN_JOURNAL
            | ffi::SQLITE_OPEN_TEMP_JOURNAL
            | ffi::SQLITE_OPEN_SUBJOURNAL
            | ffi::SQLITE_OPEN_SUPER_JOURNAL
            | ffi::SQLITE_OPEN_TEMP_DB
            | ffi::SQLITE_OPEN_TRANSIENT_DB
            | ffi::SQLITE_OPEN_WAL))
        != 0;

    let state = if !z_name.is_null() {
        let param_key = CString::new("branch_id").unwrap();
        let param_val = ffi::sqlite3_uri_parameter(z_name, param_key.as_ptr());
        if !param_val.is_null() {
            let id = CStr::from_ptr(param_val).to_string_lossy();
            get_branch_state(&id)
        } else {
            let name_str = CStr::from_ptr(z_name).to_string_lossy();
            if let Some(pos) = name_str.find("branch_id=") {
                let rest = &name_str[pos + 10..];
                let id = rest.split('&').next().unwrap_or(rest);
                get_branch_state(id)
            } else {
                let stripped = name_str
                    .strip_suffix("-journal")
                    .or_else(|| name_str.strip_suffix("-wal"))
                    .or_else(|| name_str.strip_suffix("-shm"))
                    .unwrap_or(&name_str);
                let base_name = Path::new(stripped)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| stripped.to_string());
                get_branch_state(&base_name)
            }
        }
    } else {
        None
    };

    let state = match state {
        Some(s) => s,
        None if is_journal => {
            // Anonymous in-memory temporary journal or scratch file
            BranchVfsState::new(format!("temp_{:p}", p_file), "", Some(Vec::new())).unwrap()
        }
        None => return ffi::SQLITE_CANTOPEN,
    };

    let cow_file = p_file.cast::<CowFile>();
    (*cow_file).base.pMethods = &COW_IO_METHODS;
    (*cow_file).state_ptr = Arc::into_raw(state);
    (*cow_file).is_journal = is_journal;

    let state_ref = &*(*cow_file).state_ptr;
    state_ref.open_connections.fetch_add(1, Ordering::Relaxed);

    if !p_out_flags.is_null() {
        *p_out_flags = flags;
    }

    ffi::SQLITE_OK
}

unsafe extern "C" fn cow_xDelete(_p_vfs: *mut ffi::sqlite3_vfs, _z_name: *const c_char, _sync_dir: c_int) -> c_int {
    ffi::SQLITE_OK
}

unsafe extern "C" fn cow_xAccess(
    _p_vfs: *mut ffi::sqlite3_vfs,
    z_name: *const c_char,
    flags: c_int,
    p_res_out: *mut c_int,
) -> c_int {
    if p_res_out.is_null() {
        return ffi::SQLITE_MISUSE;
    }

    if z_name.is_null() {
        *p_res_out = 0;
        return ffi::SQLITE_OK;
    }

    let name_str = CStr::from_ptr(z_name).to_string_lossy();

    if flags == ffi::SQLITE_ACCESS_EXISTS {
        if name_str.ends_with("-journal") || name_str.ends_with("-wal") || name_str.ends_with("-shm") {
            let stripped = name_str
                .strip_suffix("-journal")
                .or_else(|| name_str.strip_suffix("-wal"))
                .or_else(|| name_str.strip_suffix("-shm"))
                .unwrap_or(&name_str);
            let base_name = Path::new(stripped)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| stripped.to_string());

            if let Some(state) = get_branch_state(&base_name) {
                *p_res_out = i32::from(!state.journal_buffer.read().unwrap().is_empty());
            } else {
                *p_res_out = 0;
            }
        } else {
            *p_res_out = 1;
        }
    } else {
        *p_res_out = 1;
    }

    ffi::SQLITE_OK
}

unsafe extern "C" fn cow_xFullPathname(
    _p_vfs: *mut ffi::sqlite3_vfs,
    z_name: *const c_char,
    n_out: c_int,
    z_out: *mut c_char,
) -> c_int {
    if z_name.is_null() || z_out.is_null() || n_out <= 0 {
        return ffi::SQLITE_MISUSE;
    }
    let src = CStr::from_ptr(z_name).to_bytes_with_nul();
    let copy_len = src.len().min(n_out as usize);
    ptr::copy_nonoverlapping(src.as_ptr().cast::<c_char>(), z_out, copy_len);
    *z_out.add(copy_len - 1) = 0;
    ffi::SQLITE_OK
}

unsafe extern "C" fn cow_xRandomness(_p_vfs: *mut ffi::sqlite3_vfs, n_byte: c_int, z_out: *mut c_char) -> c_int {
    let dflt = ffi::sqlite3_vfs_find(ptr::null());
    if !dflt.is_null() {
        if let Some(func) = (*dflt).xRandomness {
            return func(dflt, n_byte, z_out);
        }
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn cow_xSleep(_p_vfs: *mut ffi::sqlite3_vfs, microseconds: c_int) -> c_int {
    std::thread::sleep(std::time::Duration::from_micros(microseconds as u64));
    microseconds
}

unsafe extern "C" fn cow_xCurrentTime(_p_vfs: *mut ffi::sqlite3_vfs, p_time: *mut f64) -> c_int {
    let dflt = ffi::sqlite3_vfs_find(ptr::null());
    if !dflt.is_null() {
        if let Some(func) = (*dflt).xCurrentTime {
            return func(dflt, p_time);
        }
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn cow_xCurrentTimeInt64(_p_vfs: *mut ffi::sqlite3_vfs, p_time: *mut i64) -> c_int {
    let dflt = ffi::sqlite3_vfs_find(ptr::null());
    if !dflt.is_null() {
        if let Some(func) = (*dflt).xCurrentTimeInt64 {
            return func(dflt, p_time);
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    *p_time = (now + 210866760000000) * 86400000;
    ffi::SQLITE_OK
}

static COW_VFS_NAME_C: LazyLock<CString> = LazyLock::new(|| CString::new(COW_VFS_NAME).unwrap());

static mut COW_VFS: ffi::sqlite3_vfs = ffi::sqlite3_vfs {
    iVersion: 2,
    szOsFile: std::mem::size_of::<CowFile>() as c_int,
    mxPathname: 1024,
    pNext: ptr::null_mut(),
    zName: ptr::null(),
    pAppData: ptr::null_mut(),
    xOpen: Some(cow_xOpen),
    xDelete: Some(cow_xDelete),
    xAccess: Some(cow_xAccess),
    xFullPathname: Some(cow_xFullPathname),
    xDlOpen: None,
    xDlError: None,
    xDlSym: None,
    xDlClose: None,
    xRandomness: Some(cow_xRandomness),
    xSleep: Some(cow_xSleep),
    xCurrentTime: Some(cow_xCurrentTime),
    xGetLastError: None,
    xCurrentTimeInt64: Some(cow_xCurrentTimeInt64),
    xSetSystemCall: None,
    xGetSystemCall: None,
    xNextSystemCall: None,
};

static VFS_INIT: std::sync::Once = std::sync::Once::new();

/// Ensure the Copy-on-Write VFS is registered in the SQLite runtime.
pub fn ensure_cow_vfs_registered() {
    VFS_INIT.call_once(|| unsafe {
        crate::logging::ensure_sqlite_initialized();
        COW_VFS.zName = COW_VFS_NAME_C.as_ptr();
        let rc = ffi::sqlite3_vfs_register(ptr::addr_of_mut!(COW_VFS), 0);
        assert_eq!(rc, ffi::SQLITE_OK, "failed to register lilia-cow VFS in SQLite");
    });
}
