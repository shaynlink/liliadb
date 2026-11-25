//! The operating system's file system.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;

use crate::vfs::{File, LockGuard, OpenMode, SyncLevel, Vfs};

/// The native file system, with the strongest sync primitive of each platform.
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeVfs;

#[derive(Debug)]
struct NativeFile(fs::File);

#[derive(Debug)]
struct NativeLock(fs::File);

impl LockGuard for NativeLock {}

impl Drop for NativeLock {
    fn drop(&mut self) {
        // Closing the descriptor releases the lock anyway; an explicit unlock failure changes
        // nothing about that, so it is not reported.
        drop(self.0.unlock());
    }
}

impl Vfs for NativeVfs {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Arc<dyn File>> {
        let mut options = fs::OpenOptions::new();
        match mode {
            OpenMode::Read => options.read(true),
            OpenMode::ReadWrite => options.read(true).write(true),
            OpenMode::CreateNew => options.read(true).write(true).create_new(true),
        };
        Ok(Arc::new(NativeFile(options.open(path)?)))
    }

    fn exists(&self, path: &Path) -> io::Result<bool> {
        path.try_exists()
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn list_dir(&self, dir: &Path) -> io::Result<Vec<String>> {
        fs::read_dir(dir)?
            .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect()
    }

    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        fs::create_dir_all(dir)
    }

    #[cfg(unix)]
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        fs::File::open(dir)?.sync_all()
    }

    /// NTFS journals directory metadata itself, and Windows cannot open a directory for flushing
    /// without backup semantics; renames and creations are durable once the file data is.
    #[cfg(not(unix))]
    fn sync_dir(&self, _dir: &Path) -> io::Result<()> {
        Ok(())
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn LockGuard>> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Box::new(NativeLock(file))),
            Err(fs::TryLockError::WouldBlock) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("{} is locked by another process", path.display()),
            )),
            Err(fs::TryLockError::Error(e)) => Err(e),
        }
    }
}

impl File for NativeFile {
    #[cfg(unix)]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(&self.0, buf, offset)
    }

    #[cfg(windows)]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        std::os::windows::fs::FileExt::seek_read(&self.0, buf, offset)
    }

    #[cfg(unix)]
    fn write_all_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        std::os::unix::fs::FileExt::write_all_at(&self.0, buf, offset)
    }

    #[cfg(windows)]
    fn write_all_at(&self, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
        while !buf.is_empty() {
            match std::os::windows::fs::FileExt::seek_write(&self.0, buf, offset) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "failed to write whole buffer",
                    ))
                }
                Ok(n) => {
                    buf = &buf[n..];
                    offset += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn sync(&self, level: SyncLevel) -> io::Result<()> {
        match level {
            // The standard library issues `F_FULLFSYNC` for `sync_data` on Apple platforms,
            // `fdatasync` on Linux and `FlushFileBuffers` on Windows.
            SyncLevel::Full => self.0.sync_data(),
            SyncLevel::Barrier => barrier(&self.0),
        }
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self.0.metadata()?.len())
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.0.set_len(len)
    }

    fn preallocate(&self, len: u64) -> io::Result<()> {
        preallocate(&self.0, len)
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[allow(
    unsafe_code,
    reason = "`fcntl` on a descriptor owned by `file`, no pointers involved"
)]
fn barrier(file: &fs::File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: the descriptor is open for the whole call; F_BARRIERFSYNC takes no argument.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_BARRIERFSYNC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn barrier(file: &fs::File) -> io::Result<()> {
    file.sync_data()
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[allow(
    unsafe_code,
    reason = "`fcntl(F_PREALLOCATE)` with a local, fully initialised `fstore_t`"
)]
fn preallocate(file: &fs::File, len: u64) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let current = file.metadata()?.len();
    if len <= current {
        return Ok(());
    }
    let mut store = libc::fstore_t {
        fst_flags: libc::F_ALLOCATECONTIG | libc::F_ALLOCATEALL,
        fst_posmode: libc::F_PEOFPOSMODE,
        fst_offset: 0,
        fst_length: libc::off_t::try_from(len - current).map_err(io::Error::other)?,
        fst_bytesalloc: 0,
    };
    // SAFETY: `store` lives for the whole call and matches the layout F_PREALLOCATE expects.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PREALLOCATE, &raw mut store) } == -1 {
        // Contiguous space may be unavailable; any placement is still better than none.
        store.fst_flags = libc::F_ALLOCATEALL;
        // SAFETY: as above.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PREALLOCATE, &raw mut store) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[allow(
    unsafe_code,
    reason = "`fallocate` on a descriptor owned by `file`, no pointers involved"
)]
fn preallocate(file: &fs::File, len: u64) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let len = libc::off_t::try_from(len).map_err(io::Error::other)?;
    // SAFETY: the descriptor is open for the whole call.
    if unsafe { libc::fallocate(file.as_raw_fd(), libc::FALLOC_FL_KEEP_SIZE, 0, len) } == -1 {
        let error = io::Error::last_os_error();
        // File systems without `fallocate` (tmpfs on old kernels, some FUSE) keep working.
        return if error.raw_os_error() == Some(libc::EOPNOTSUPP) {
            Ok(())
        } else {
            Err(error)
        };
    }
    Ok(())
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android"
)))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the same signature as the platforms that preallocate, which can fail"
)]
fn preallocate(_file: &fs::File, _len: u64) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_sync_read_and_lock() {
        let dir = tempfile::tempdir().unwrap();
        let vfs = NativeVfs;
        let path = dir.path().join("f");
        let file = vfs.open(&path, OpenMode::CreateNew).unwrap();
        file.preallocate(1 << 20).unwrap();
        assert_eq!(
            file.len().unwrap(),
            0,
            "preallocation must not change the length"
        );
        file.write_all_at(b"hello", 3).unwrap();
        file.sync(SyncLevel::Barrier).unwrap();
        file.sync(SyncLevel::Full).unwrap();
        let mut buf = [0u8; 8];
        file.read_exact_at(&mut buf, 0).unwrap();
        assert_eq!(&buf, b"\0\0\0hello");
        assert!(vfs.open(&path, OpenMode::CreateNew).is_err());

        let lock = vfs.lock(&dir.path().join("LOCK")).unwrap();
        assert_eq!(
            vfs.lock(&dir.path().join("LOCK")).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(lock);
        vfs.lock(&dir.path().join("LOCK")).unwrap();

        vfs.rename(&path, &dir.path().join("g")).unwrap();
        vfs.sync_dir(dir.path()).unwrap();
        let mut names = vfs.list_dir(dir.path()).unwrap();
        names.sort();
        assert_eq!(names, ["LOCK", "g"]);
    }
}
