//! A file system over a fixed pool of storage slots, for platforms where files can only be
//! opened asynchronously (the browser's origin private file system): the slots are opened once,
//! up front, and every [`Vfs`] operation after that is synchronous.
//!
//! Each slot starts with a header of [`HEADER_BYTES`] naming the path it holds; the file's data
//! follows. Creating, renaming and removing a file rewrite headers only. A header carries a
//! generation, so if a crash interrupts a rename and two slots claim one path, the newer claim
//! wins when the pool is opened again. A removed file keeps its slot until its last open handle
//! is dropped, so readers of a removed file keep their data, as on POSIX.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use crate::vfs::{File, LockGuard, OpenMode, SyncLevel, Vfs};

/// Bytes reserved at the start of every slot for its header.
pub const HEADER_BYTES: u64 = 4096;
const MAGIC: &[u8; 4] = b"LPS1";
/// Magic, generation (8 bytes), path length (2 bytes).
const HEADER_FIXED: usize = 14;

/// One storage slot: a file of the pool, read and written at byte offsets.
pub trait Slot: Send + Sync + fmt::Debug {
    /// Reads up to `buf.len()` bytes at `offset`; returns the number read (0 at the end).
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;
    /// Writes `buf` at `offset`, extending the slot if needed; returns the number written.
    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<usize>;
    /// Makes the writes durable.
    fn flush(&self) -> io::Result<()>;
    fn size(&self) -> io::Result<u64>;
    /// Truncates or extends (with zeros) to `len` bytes.
    fn truncate(&self, len: u64) -> io::Result<()>;
}

/// A slot in memory: for tests, and for databases that need no persistence.
#[derive(Debug, Default)]
pub struct MemSlot {
    data: Mutex<Vec<u8>>,
}

impl Slot for MemSlot {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let data = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(data.len());
        let n = buf.len().min(data.len() - start);
        buf[..n].copy_from_slice(&data[start..start + n]);
        Ok(n)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<usize> {
        let mut data = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        let start = usize::try_from(offset).map_err(|_| too_large())?;
        let end = start.checked_add(buf.len()).ok_or_else(too_large)?;
        if data.len() < end {
            data.resize(end, 0);
        }
        data[start..end].copy_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&self) -> io::Result<()> {
        Ok(())
    }

    fn size(&self) -> io::Result<u64> {
        Ok(self
            .data
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len() as u64)
    }

    fn truncate(&self, len: u64) -> io::Result<()> {
        let len = usize::try_from(len).map_err(|_| too_large())?;
        self.data
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .resize(len, 0);
        Ok(())
    }
}

fn too_large() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "offset beyond the address space",
    )
}

fn not_found(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("{} does not exist", path.display()),
    )
}

/// What a slot's header says.
fn read_header(slot: &dyn Slot) -> io::Result<Option<(u64, PathBuf)>> {
    let mut fixed = [0u8; HEADER_FIXED];
    let n = slot.read_at(&mut fixed, 0)?;
    if n < HEADER_FIXED || &fixed[..4] != MAGIC {
        return Ok(None);
    }
    let generation = u64::from_le_bytes(fixed[4..12].try_into().unwrap_or_default());
    let len = usize::from(u16::from_le_bytes([fixed[12], fixed[13]]));
    if len == 0 {
        return Ok(None);
    }
    let mut path = vec![0u8; len];
    if slot.read_at(&mut path, HEADER_FIXED as u64)? < len {
        return Ok(None);
    }
    let path = String::from_utf8(path)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a slot header is not UTF-8"))?;
    Ok(Some((generation, PathBuf::from(path))))
}

/// Writes the header claiming `path` (or none) at `generation`, durably.
fn write_header(slot: &dyn Slot, generation: u64, path: Option<&Path>) -> io::Result<()> {
    let name = path.map_or_else(String::new, |p| p.to_string_lossy().into_owned());
    let len = u16::try_from(name.len())
        .ok()
        .filter(|&len| (HEADER_FIXED + usize::from(len)) as u64 <= HEADER_BYTES)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path too long for a slot"))?;
    let mut header = Vec::with_capacity(HEADER_FIXED + name.len());
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&generation.to_le_bytes());
    header.extend_from_slice(&len.to_le_bytes());
    header.extend_from_slice(name.as_bytes());
    write_all(slot, &header, 0)?;
    slot.flush()
}

fn write_all(slot: &dyn Slot, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        match slot.write_at(buf, offset)? {
            0 => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "a slot accepted no byte",
                ))
            }
            n => {
                buf = &buf[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
struct State {
    /// The slot of each live file.
    paths: HashMap<PathBuf, usize>,
    /// Slots holding no file and no open handle.
    free: Vec<usize>,
    /// Open handles per slot.
    open: Vec<usize>,
    /// Slots of removed files that still have open handles.
    retired: HashSet<usize>,
    dirs: HashSet<PathBuf>,
    locks: HashSet<PathBuf>,
    /// Generation of the newest header written.
    generation: u64,
}

impl State {
    fn add_dirs(&mut self, path: &Path) {
        self.dirs
            .extend(path.ancestors().skip(1).map(Path::to_path_buf));
    }

    /// Releases `slot` once nothing uses it.
    fn release(&mut self, slot: usize) {
        if self.open[slot] == 0 && self.retired.remove(&slot) {
            self.free.push(slot);
        }
    }

    fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }
}

#[derive(Debug)]
struct Shared {
    slots: RwLock<Vec<Arc<dyn Slot>>>,
    state: Mutex<State>,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn slot(&self, index: usize) -> Arc<dyn Slot> {
        let slots = self.slots.read().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(&slots[index])
    }
}

/// A [`Vfs`] over a pool of [`Slot`]s; see the [module documentation](self).
#[derive(Debug, Clone)]
pub struct PoolVfs {
    shared: Arc<Shared>,
}

impl PoolVfs {
    /// The file system the headers of `slots` describe.
    ///
    /// # Errors
    ///
    /// A slot that cannot be read, a header that does not decode.
    pub fn new(slots: Vec<Arc<dyn Slot>>) -> io::Result<Self> {
        let vfs = Self {
            shared: Arc::new(Shared {
                slots: RwLock::new(Vec::new()),
                state: Mutex::new(State::default()),
            }),
        };
        vfs.add_slots(slots)?;
        Ok(vfs)
    }

    /// Adds `slots` to the pool, with whatever files their headers hold.
    ///
    /// # Errors
    ///
    /// As [`PoolVfs::new`].
    pub fn add_slots(&self, slots: Vec<Arc<dyn Slot>>) -> io::Result<()> {
        let mut table = self
            .shared
            .slots
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let mut state = self.shared.state();
        for slot in slots {
            let index = table.len();
            state.open.push(0);
            match read_header(slot.as_ref())? {
                Some((generation, path)) => {
                    state.generation = state.generation.max(generation);
                    match state.paths.get(&path).copied() {
                        // A rename interrupted by a crash: the newer claim wins.
                        Some(other)
                            if read_header(table[other].as_ref())?.map(|h| h.0)
                                > Some(generation) =>
                        {
                            state.free.push(index);
                        }
                        Some(other) => {
                            state.free.push(other);
                            state.paths.insert(path, index);
                        }
                        None => {
                            state.add_dirs(&path);
                            state.paths.insert(path, index);
                        }
                    }
                }
                None => state.free.push(index),
            }
            table.push(slot);
        }
        Ok(())
    }

    /// Slots free for new files.
    #[must_use]
    pub fn free_slots(&self) -> usize {
        self.shared.state().free.len()
    }

    /// Slots in the pool.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.shared
            .slots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn handle(&self, state: &mut State, slot: usize) -> Arc<dyn File> {
        state.open[slot] += 1;
        Arc::new(PoolFile {
            shared: Arc::clone(&self.shared),
            index: slot,
            slot: self.shared.slot(slot),
        })
    }
}

impl Vfs for PoolVfs {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Arc<dyn File>> {
        let mut state = self.shared.state();
        match mode {
            OpenMode::Read | OpenMode::ReadWrite => {
                let slot = *state.paths.get(path).ok_or_else(|| not_found(path))?;
                Ok(self.handle(&mut state, slot))
            }
            OpenMode::CreateNew => {
                if state.paths.contains_key(path) {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("{} exists", path.display()),
                    ));
                }
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    if !state.dirs.contains(parent) {
                        return Err(not_found(parent));
                    }
                }
                let slot = state.free.pop().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::StorageFull,
                        "every slot of the storage pool is in use",
                    )
                })?;
                let storage = self.shared.slot(slot);
                let generation = state.next_generation();
                let claimed = storage
                    .truncate(HEADER_BYTES)
                    .and_then(|()| write_header(storage.as_ref(), generation, Some(path)));
                if let Err(error) = claimed {
                    state.free.push(slot);
                    return Err(error);
                }
                state.paths.insert(path.to_path_buf(), slot);
                Ok(self.handle(&mut state, slot))
            }
        }
    }

    fn exists(&self, path: &Path) -> io::Result<bool> {
        let state = self.shared.state();
        Ok(state.paths.contains_key(path) || state.dirs.contains(path))
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        let mut state = self.shared.state();
        let slot = *state.paths.get(path).ok_or_else(|| not_found(path))?;
        let generation = state.next_generation();
        write_header(self.shared.slot(slot).as_ref(), generation, None)?;
        state.paths.remove(path);
        state.retired.insert(slot);
        state.release(slot);
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut state = self.shared.state();
        let slot = *state.paths.get(from).ok_or_else(|| not_found(from))?;
        // Claim `to` first: a crash before the old claim is cleared leaves two claims, and the
        // newer one wins when the pool opens again.
        let generation = state.next_generation();
        write_header(self.shared.slot(slot).as_ref(), generation, Some(to))?;
        state.paths.remove(from);
        if let Some(replaced) = state.paths.insert(to.to_path_buf(), slot) {
            let generation = state.next_generation();
            write_header(self.shared.slot(replaced).as_ref(), generation, None)?;
            state.retired.insert(replaced);
            state.release(replaced);
        }
        state.add_dirs(to);
        Ok(())
    }

    fn list_dir(&self, dir: &Path) -> io::Result<Vec<String>> {
        let state = self.shared.state();
        if !state.dirs.contains(dir) {
            return Err(not_found(dir));
        }
        Ok(state
            .paths
            .keys()
            .filter(|p| p.parent() == Some(dir))
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect())
    }

    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        let mut state = self.shared.state();
        state.dirs.insert(dir.to_path_buf());
        state.add_dirs(dir);
        Ok(())
    }

    /// Every header is durable once written: there is nothing left to sync.
    fn sync_dir(&self, _dir: &Path) -> io::Result<()> {
        Ok(())
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn LockGuard>> {
        let mut state = self.shared.state();
        if !state.locks.insert(path.to_path_buf()) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("{} is locked", path.display()),
            ));
        }
        Ok(Box::new(PoolLock {
            shared: Arc::clone(&self.shared),
            path: path.to_path_buf(),
        }))
    }
}

#[derive(Debug)]
struct PoolLock {
    shared: Arc<Shared>,
    path: PathBuf,
}

impl LockGuard for PoolLock {}

impl Drop for PoolLock {
    fn drop(&mut self) {
        self.shared.state().locks.remove(&self.path);
    }
}

/// An open file of a [`PoolVfs`]: the data part of its slot.
#[derive(Debug)]
struct PoolFile {
    shared: Arc<Shared>,
    index: usize,
    slot: Arc<dyn Slot>,
}

impl File for PoolFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.slot.read_at(buf, HEADER_BYTES + offset)
    }

    fn write_all_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        write_all(self.slot.as_ref(), buf, HEADER_BYTES + offset)
    }

    fn sync(&self, _level: SyncLevel) -> io::Result<()> {
        self.slot.flush()
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self.slot.size()?.saturating_sub(HEADER_BYTES))
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.slot.truncate(HEADER_BYTES + len)
    }

    /// Slots have no allocation primitive: appends allocate as they go.
    fn preallocate(&self, _len: u64) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PoolFile {
    fn drop(&mut self) {
        let mut state = self.shared.state();
        state.open[self.index] -= 1;
        state.release(self.index);
    }
}

#[cfg(test)]
mod tests;
