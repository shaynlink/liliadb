//! The browser's origin private file system (OPFS) as a pool of slots ([`crate::pool`]).
//!
//! OPFS files are read and written synchronously only through `FileSystemSyncAccessHandle`s,
//! which exist in dedicated workers and are opened asynchronously. [`OpfsPool::open`] opens a
//! pool of slot files once; the [`PoolVfs`] it serves is synchronous from then on. A `pool` file
//! records how many slots the directory holds, so a pool that grew ([`OpfsPool::grow`]) opens
//! whole again. An open handle excludes every other tab from the file: one tab at a time opens a
//! database.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use js_sys::Promise;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetDirectoryOptions,
    FileSystemGetFileOptions, FileSystemReadWriteOptions, FileSystemSyncAccessHandle,
    WorkerGlobalScope,
};

use crate::pool::{PoolVfs, Slot};

fn js_error(what: &str, error: &JsValue) -> io::Error {
    io::Error::other(format!("{what}: {error:?}"))
}

async fn resolve<T: JsCast>(promise: Promise, what: &str) -> io::Result<T> {
    JsFuture::from(promise)
        .await
        .map_err(|e| js_error(what, &e))?
        .dyn_into::<T>()
        .map_err(|e| js_error(what, &e))
}

/// A JavaScript object owned by the single thread of a `wasm32` program without atomics.
#[derive(Debug)]
struct Local<T>(T);

// SAFETY: without the `atomics` target feature a `wasm32` program has exactly one thread, so the
// wrapped object is never reached from another thread.
#[cfg(not(target_feature = "atomics"))]
#[allow(
    unsafe_code,
    reason = "a `wasm32` program without atomics has one thread"
)]
unsafe impl<T> Send for Local<T> {}

// SAFETY: as for `Send`.
#[cfg(not(target_feature = "atomics"))]
#[allow(
    unsafe_code,
    reason = "a `wasm32` program without atomics has one thread"
)]
unsafe impl<T> Sync for Local<T> {}

/// A slot file of the pool.
#[derive(Debug)]
struct OpfsSlot(Local<FileSystemSyncAccessHandle>);

impl Drop for OpfsSlot {
    /// Releases the file: until its handle is closed, no other handle (in this worker or in
    /// another tab) can open it.
    fn drop(&mut self) {
        self.0 .0.close();
    }
}

fn at(offset: u64) -> FileSystemReadWriteOptions {
    let options = FileSystemReadWriteOptions::new();
    #[allow(
        clippy::cast_precision_loss,
        reason = "OPFS offsets are JavaScript numbers; files stay far below 2^53 bytes"
    )]
    options.set_at(offset as f64);
    options
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "OPFS reports byte counts as JavaScript numbers, whole and non-negative, below 2^53"
)]
fn bytes(count: f64) -> u64 {
    count as u64
}

impl Slot for OpfsSlot {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let read = self
            .0
             .0
            .read_with_u8_array_and_options(buf, &at(offset))
            .map_err(|e| js_error("read", &e))?;
        usize::try_from(bytes(read)).map_err(io::Error::other)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<usize> {
        let written = self
            .0
             .0
            .write_with_u8_array_and_options(buf, &at(offset))
            .map_err(|e| js_error("write", &e))?;
        usize::try_from(bytes(written)).map_err(io::Error::other)
    }

    fn flush(&self) -> io::Result<()> {
        self.0 .0.flush().map_err(|e| js_error("flush", &e))
    }

    fn size(&self) -> io::Result<u64> {
        self.0
             .0
            .get_size()
            .map(bytes)
            .map_err(|e| js_error("size", &e))
    }

    fn truncate(&self, len: u64) -> io::Result<()> {
        #[allow(clippy::cast_precision_loss, reason = "as for offsets")]
        let len = len as f64;
        self.0
             .0
            .truncate_with_f64(len)
            .map_err(|e| js_error("truncate", &e))
    }
}

/// A pool of slot files in one OPFS directory, and the [`PoolVfs`] over them.
#[derive(Debug)]
pub struct OpfsPool {
    directory: Local<FileSystemDirectoryHandle>,
    /// The `pool` file, holding the number of slots.
    count_file: OpfsSlot,
    slots: AtomicUsize,
    vfs: PoolVfs,
}

impl OpfsPool {
    /// Opens (creating it if needed) the pool of directory `directory` of the origin private file
    /// system, with at least `min_slots` slots.
    ///
    /// # Errors
    ///
    /// Outside a dedicated worker, without OPFS, a slot another tab holds open, slot files whose
    /// headers do not decode.
    pub async fn open(directory: &str, min_slots: usize) -> io::Result<Self> {
        let scope = js_sys::global()
            .dyn_into::<WorkerGlobalScope>()
            .map_err(|_| io::Error::other("OPFS synchronous access needs a dedicated worker"))?;
        let root: FileSystemDirectoryHandle =
            resolve(scope.navigator().storage().get_directory(), "OPFS root").await?;
        let create = FileSystemGetDirectoryOptions::new();
        create.set_create(true);
        let directory: FileSystemDirectoryHandle = resolve(
            root.get_directory_handle_with_options(directory, &create),
            "OPFS directory",
        )
        .await?;
        let count_file = OpfsSlot(Local(sync_handle(&directory, "pool").await?));
        let mut stored = [0u8; 8];
        let stored = match count_file.read_at(&mut stored, 0)? {
            8 => usize::try_from(u64::from_le_bytes(stored)).map_err(io::Error::other)?,
            _ => 0,
        };
        let count = stored.max(min_slots);
        let mut slots: Vec<Arc<dyn Slot>> = Vec::with_capacity(count);
        for index in 0..count {
            slots.push(Arc::new(OpfsSlot(Local(
                sync_handle(&directory, &slot_name(index)).await?,
            ))));
        }
        let pool = Self {
            directory: Local(directory),
            count_file,
            slots: AtomicUsize::new(count),
            vfs: PoolVfs::new(slots)?,
        };
        pool.record(count)?;
        Ok(pool)
    }

    /// The file system over the pool.
    #[must_use]
    pub fn vfs(&self) -> PoolVfs {
        self.vfs.clone()
    }

    /// Adds `count` slots to the pool.
    ///
    /// # Errors
    ///
    /// As [`OpfsPool::open`].
    pub async fn grow(&self, count: usize) -> io::Result<()> {
        let first = self.slots.load(Ordering::Acquire);
        let mut slots: Vec<Arc<dyn Slot>> = Vec::with_capacity(count);
        for index in first..first + count {
            slots.push(Arc::new(OpfsSlot(Local(
                sync_handle(&self.directory.0, &slot_name(index)).await?,
            ))));
        }
        self.vfs.add_slots(slots)?;
        self.slots.store(first + count, Ordering::Release);
        self.record(first + count)
    }

    fn record(&self, count: usize) -> io::Result<()> {
        let count = u64::try_from(count).map_err(io::Error::other)?;
        let written = self.count_file.write_at(&count.to_le_bytes(), 0)?;
        if written != 8 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "the pool count was not written",
            ));
        }
        self.count_file.flush()
    }
}

fn slot_name(index: usize) -> String {
    format!("slot-{index:05}")
}

/// A synchronous access handle on file `name` of `directory`, created if missing.
async fn sync_handle(
    directory: &FileSystemDirectoryHandle,
    name: &str,
) -> io::Result<FileSystemSyncAccessHandle> {
    let create = FileSystemGetFileOptions::new();
    create.set_create(true);
    let file: FileSystemFileHandle = resolve(
        directory.get_file_handle_with_options(name, &create),
        "OPFS file",
    )
    .await?;
    resolve(
        file.create_sync_access_handle(),
        "OPFS synchronous access handle",
    )
    .await
}
