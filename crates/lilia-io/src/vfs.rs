//! The file-system interface the storage engine is written against.

use std::fmt;
use std::io;
use std::path::Path;
use std::sync::Arc;

/// How far a [`File::sync`] pushes data before returning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncLevel {
    /// Data survives power loss once the call returns: `F_FULLFSYNC` on Apple platforms,
    /// `fdatasync` on Linux and Android, `FlushFileBuffers` on Windows.
    Full,
    /// Writes reach the drive in order, but may still sit in its volatile cache: a crash keeps
    /// the file consistent (no later write without the earlier ones) but may lose the most recent
    /// writes. `F_BARRIERFSYNC` on Apple platforms; elsewhere the same as [`SyncLevel::Full`].
    Barrier,
}

/// How [`Vfs::open`] opens a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    /// An existing file, read only.
    Read,
    /// An existing file, read and write.
    ReadWrite,
    /// A new file, read and write; fails if the path exists.
    CreateNew,
}

/// An open file. Positional reads and writes only: there is no shared cursor, so one file can be
/// read by many threads while one thread appends to it.
pub trait File: Send + Sync + fmt::Debug {
    /// Reads up to `buf.len()` bytes at `offset`; returns the number read (0 at end of file).
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;

    /// Writes all of `buf` at `offset`, extending the file if needed.
    fn write_all_at(&self, buf: &[u8], offset: u64) -> io::Result<()>;

    /// Makes the written data durable to `level`.
    fn sync(&self, level: SyncLevel) -> io::Result<()>;

    /// Current length in bytes.
    fn len(&self) -> io::Result<u64>;

    /// Truncates or extends (with zeros) to `len` bytes.
    fn set_len(&self, len: u64) -> io::Result<()>;

    /// Reserves storage for the first `len` bytes without changing the file length, so later
    /// appends do not allocate. Best effort: a platform without the primitive does nothing.
    fn preallocate(&self, len: u64) -> io::Result<()>;

    /// Reads exactly `buf.len()` bytes at `offset`.
    fn read_exact_at(&self, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
        while !buf.is_empty() {
            match self.read_at(buf, offset) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "read past end of file")),
                Ok(n) => {
                    buf = &mut buf[n..];
                    offset += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Whether the file is empty.
    fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }
}

/// Held while a database directory is open; releases the lock when dropped.
pub trait LockGuard: Send + Sync + fmt::Debug {}

/// A file system.
pub trait Vfs: Send + Sync + fmt::Debug {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Arc<dyn File>>;
    fn exists(&self, path: &Path) -> io::Result<bool>;
    fn remove_file(&self, path: &Path) -> io::Result<()>;
    /// Atomically replaces `to` with `from`.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Names of the entries of `dir` (not paths), in no particular order.
    fn list_dir(&self, dir: &Path) -> io::Result<Vec<String>>;
    fn create_dir_all(&self, dir: &Path) -> io::Result<()>;
    /// Makes the creations, renames and removals of entries of `dir` durable.
    fn sync_dir(&self, dir: &Path) -> io::Result<()>;
    /// Takes an exclusive lock on `path` (created if missing) for the life of the guard; fails
    /// with [`io::ErrorKind::WouldBlock`] if another holder has it.
    fn lock(&self, path: &Path) -> io::Result<Box<dyn LockGuard>>;
}
