use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use crate::error::{Result, ShmError};

/// Backend configuration for shared memory allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShmBackend {
    /// File-backed shared memory. Fully compatible with:
    /// - iOS App Groups shared directory (`group.com.example.app`)
    /// - Android App internal storage sandbox
    /// - macOS, Linux, and Windows shared paths.
    FileBacked(PathBuf),

    /// POSIX named shared memory (`/dev/shm` on Linux, `shm_open` on macOS/Linux).
    PosixNamed(String),

    /// Automatically derived from database path (`<db_path>.shm`).
    Auto(PathBuf),
}

impl ShmBackend {
    pub fn resolve_path(&self) -> PathBuf {
        match self {
            Self::FileBacked(path) => path.clone(),
            Self::PosixNamed(name) => {
                #[cfg(target_os = "linux")]
                {
                    PathBuf::from("/dev/shm").join(name.trim_start_matches('/'))
                }
                #[cfg(not(target_os = "linux"))]
                {
                    std::env::temp_dir().join(format!("lilia_shm_{}", name.trim_start_matches('/')))
                }
            }
            Self::Auto(db_path) => {
                let mut path = db_path.as_os_str().to_os_string();
                path.push(".shm");
                PathBuf::from(path)
            }
        }
    }
}

/// Low-level memory-mapped shared memory segment.
pub struct ShmSegment {
    mmap: memmap2::MmapRaw,
    ptr: *mut u8,
    len: usize,
    path: PathBuf,
    is_writer: bool,
}

// Safety: ShmSegment accesses raw memory-mapped bytes that are coordinated
// across threads and processes via atomic instructions (Seqlocks and Epochs).
unsafe impl Send for ShmSegment {}
unsafe impl Sync for ShmSegment {}

impl std::fmt::Debug for ShmSegment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShmSegment")
            .field("path", &self.path)
            .field("len", &self.len)
            .field("is_writer", &self.is_writer)
            .finish()
    }
}

impl ShmSegment {
    /// Create or truncate a shared memory segment for write and read access.
    pub fn create_or_open(backend: &ShmBackend, capacity_bytes: usize) -> Result<Self> {
        let path = backend.resolve_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;

        let metadata = file.metadata()?;
        if metadata.len() < capacity_bytes as u64 {
            file.set_len(capacity_bytes as u64)?;
        }

        let mmap = memmap2::MmapRaw::map_raw(&file)?;
        let ptr = mmap.as_mut_ptr();
        let len = mmap.len();

        Ok(Self {
            mmap,
            ptr,
            len,
            path,
            is_writer: true,
        })
    }

    /// Open an existing shared memory segment in read-only / shared read mode.
    pub fn open_read(backend: &ShmBackend) -> Result<Self> {
        let path = backend.resolve_path();
        if !path.exists() {
            return Err(ShmError::InvalidParameter(format!(
                "SHM segment file does not exist: {}",
                path.display()
            )));
        }

        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        let metadata = file.metadata()?;
        let len = metadata.len() as usize;

        let mmap = memmap2::MmapRaw::map_raw(&file)?;
        let ptr = mmap.as_mut_ptr();

        Ok(Self {
            mmap,
            ptr,
            len,
            path,
            is_writer: false,
        })
    }

    #[inline]
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Flush mapped modifications to underlying storage.
    pub fn flush(&self) -> Result<()> {
        self.mmap.flush().map_err(ShmError::from)
    }
}
