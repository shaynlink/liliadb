//! In-memory file system with a power-cut model, for tests and deterministic crash simulation.
//!
//! Every file keeps its current bytes and the bytes made durable by its last sync; every
//! directory keeps its current entries and the entries made durable by its last directory sync.
//! [`MemVfs::crash`] cuts the power: every later operation fails until [`MemVfs::restart`], which
//! rebuilds the file system from durable state only, optionally keeping a torn prefix of the
//! unsynced tail of each file, as a real drive may.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use crate::vfs::{File, LockGuard, OpenMode, SyncLevel, Vfs};

/// What survives [`MemVfs::restart`] beyond the durable state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrashOptions {
    /// Keep a random prefix of each file's unsynced appended bytes.
    pub keep_unsynced_prefix: bool,
    /// Overwrite the last kept unsynced bytes (up to 512) with garbage, like a torn sector.
    pub tear_last_sector: bool,
}

impl CrashOptions {
    /// Only synced data survives.
    pub const CLEAN: Self = Self { keep_unsynced_prefix: false, tear_last_sector: false };
    /// Unsynced tails may partially survive, with a torn last sector.
    pub const TORN: Self = Self { keep_unsynced_prefix: true, tear_last_sector: true };
}

#[derive(Debug, Default)]
struct Contents {
    current: Vec<u8>,
    durable: Vec<u8>,
}

#[derive(Debug, Default)]
struct State {
    /// Current namespace: path → file.
    files: BTreeMap<PathBuf, Arc<RwLock<Contents>>>,
    /// Namespace as of each directory's last sync.
    durable_files: BTreeMap<PathBuf, Arc<RwLock<Contents>>>,
    dirs: BTreeSet<PathBuf>,
    locks: BTreeSet<PathBuf>,
    powered_off: bool,
    /// Incremented by every restart; handles opened before it stay unusable.
    generation: u64,
    rng: u64,
    /// Probability, in parts per million, that a write or sync fails with an I/O error.
    fault_ppm: u32,
}

impl State {
    fn next_random(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn check(&mut self) -> io::Result<()> {
        if self.powered_off {
            return Err(io::Error::other("simulated power loss"));
        }
        Ok(())
    }

    /// Fails a mutating operation with the configured probability.
    fn maybe_fail(&mut self, what: &str) -> io::Result<()> {
        self.check()?;
        if self.fault_ppm > 0 && self.next_random() % 1_000_000 < u64::from(self.fault_ppm) {
            return Err(io::Error::other(format!("injected I/O error on {what}")));
        }
        Ok(())
    }
}

/// In-memory file system modelling durability; see the module documentation.
#[derive(Debug, Clone, Default)]
pub struct MemVfs {
    state: Arc<Mutex<State>>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn not_found(path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{} does not exist", path.display()))
}

impl MemVfs {
    /// An empty file system; `seed` drives fault injection and torn tails deterministically.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        let vfs = Self::default();
        lock(&vfs.state).rng = seed;
        vfs
    }

    /// Every write or sync fails with this probability, in parts per million.
    pub fn set_fault_ppm(&self, ppm: u32) {
        lock(&self.state).fault_ppm = ppm;
    }

    /// Cuts the power: every later operation on this file system or its open files fails.
    pub fn crash(&self) {
        lock(&self.state).powered_off = true;
    }

    /// Powers the file system back on with what survived the crash. Open files from before the
    /// crash stay unusable; locks are released.
    pub fn restart(&self, options: CrashOptions) {
        let mut state = lock(&self.state);
        let survivors: Vec<(PathBuf, Arc<RwLock<Contents>>)> =
            state.durable_files.iter().map(|(p, c)| (p.clone(), Arc::clone(c))).collect();
        let mut files = BTreeMap::new();
        for (path, contents) in survivors {
            let old = contents.read().unwrap_or_else(PoisonError::into_inner);
            let mut data = old.durable.clone();
            if options.keep_unsynced_prefix && old.current.len() > data.len() && old.current.starts_with(&data) {
                let tail = &old.current[data.len()..];
                let keep = usize::try_from(state.next_random() % (tail.len() as u64 + 1)).unwrap_or(0);
                let start = data.len();
                data.extend_from_slice(&tail[..keep]);
                if options.tear_last_sector && keep > 0 {
                    let torn = keep.min(512);
                    for byte in &mut data[start + keep - torn..] {
                        *byte = state.next_random().to_le_bytes()[0];
                    }
                }
            }
            files.insert(path, Arc::new(RwLock::new(Contents { current: data.clone(), durable: data })));
        }
        state.durable_files.clone_from(&files);
        state.files = files;
        state.locks.clear();
        state.powered_off = false;
        state.generation += 1;
    }

    fn file(&self, contents: Arc<RwLock<Contents>>, writable: bool, generation: u64) -> Arc<dyn File> {
        Arc::new(MemHandle { shared: contents, vfs: Arc::clone(&self.state), writable, generation })
    }
}

/// An open file: `shared` is the file's contents in the namespace it was opened from.
#[derive(Debug)]
struct MemHandle {
    shared: Arc<RwLock<Contents>>,
    vfs: Arc<Mutex<State>>,
    writable: bool,
    generation: u64,
}

impl MemHandle {
    /// Fails after a crash, and for handles opened before the last restart.
    fn state(&self) -> io::Result<MutexGuard<'_, State>> {
        let mut state = lock(&self.vfs);
        state.check()?;
        if state.generation != self.generation {
            return Err(io::Error::other("file handle opened before a simulated power loss"));
        }
        Ok(state)
    }

    fn check(&self) -> io::Result<()> {
        self.state().map(drop)
    }
}

impl File for MemHandle {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.check()?;
        let contents = self.shared.read().unwrap_or_else(PoisonError::into_inner);
        let start = usize::try_from(offset).map_err(io::Error::other)?;
        if start >= contents.current.len() {
            return Ok(0);
        }
        let n = buf.len().min(contents.current.len() - start);
        buf[..n].copy_from_slice(&contents.current[start..start + n]);
        Ok(n)
    }

    fn write_all_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        self.state()?.maybe_fail("write")?;
        if !self.writable {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "file opened read-only"));
        }
        let mut contents = self.shared.write().unwrap_or_else(PoisonError::into_inner);
        let start = usize::try_from(offset).map_err(io::Error::other)?;
        let end = start + buf.len();
        if contents.current.len() < end {
            contents.current.resize(end, 0);
        }
        contents.current[start..end].copy_from_slice(buf);
        Ok(())
    }

    fn sync(&self, _level: SyncLevel) -> io::Result<()> {
        self.state()?.maybe_fail("sync")?;
        let mut contents = self.shared.write().unwrap_or_else(PoisonError::into_inner);
        let Contents { current, durable } = &mut *contents;
        durable.clone_from(current);
        Ok(())
    }

    fn len(&self) -> io::Result<u64> {
        self.check()?;
        Ok(self.shared.read().unwrap_or_else(PoisonError::into_inner).current.len() as u64)
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.state()?.maybe_fail("set_len")?;
        let len = usize::try_from(len).map_err(io::Error::other)?;
        self.shared.write().unwrap_or_else(PoisonError::into_inner).current.resize(len, 0);
        Ok(())
    }

    fn preallocate(&self, _len: u64) -> io::Result<()> {
        self.check()
    }
}

#[derive(Debug)]
struct MemLock {
    state: Arc<Mutex<State>>,
    path: PathBuf,
}

impl LockGuard for MemLock {}

impl Drop for MemLock {
    fn drop(&mut self) {
        lock(&self.state).locks.remove(&self.path);
    }
}

impl Vfs for MemVfs {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Arc<dyn File>> {
        let mut state = lock(&self.state);
        state.check()?;
        let contents = match mode {
            OpenMode::Read | OpenMode::ReadWrite => state.files.get(path).cloned().ok_or_else(|| not_found(path))?,
            OpenMode::CreateNew => {
                state.maybe_fail("create")?;
                if state.files.contains_key(path) {
                    return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} exists", path.display())));
                }
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    if !state.dirs.contains(parent) {
                        return Err(not_found(parent));
                    }
                }
                let contents = Arc::new(RwLock::new(Contents::default()));
                state.files.insert(path.to_path_buf(), Arc::clone(&contents));
                contents
            }
        };
        let generation = state.generation;
        drop(state);
        Ok(self.file(contents, mode != OpenMode::Read, generation))
    }

    fn exists(&self, path: &Path) -> io::Result<bool> {
        let mut state = lock(&self.state);
        state.check()?;
        Ok(state.files.contains_key(path) || state.dirs.contains(path))
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        let mut state = lock(&self.state);
        state.maybe_fail("remove")?;
        state.files.remove(path).map(drop).ok_or_else(|| not_found(path))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut state = lock(&self.state);
        state.maybe_fail("rename")?;
        let contents = state.files.remove(from).ok_or_else(|| not_found(from))?;
        state.files.insert(to.to_path_buf(), contents);
        Ok(())
    }

    fn list_dir(&self, dir: &Path) -> io::Result<Vec<String>> {
        let mut state = lock(&self.state);
        state.check()?;
        if !state.dirs.contains(dir) {
            return Err(not_found(dir));
        }
        Ok(state
            .files
            .keys()
            .filter(|p| p.parent() == Some(dir))
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect())
    }

    /// Directories themselves are always durable: the engine creates its directory once, before
    /// writing anything that has to survive.
    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        let mut state = lock(&self.state);
        state.check()?;
        for ancestor in dir.ancestors().filter(|a| !a.as_os_str().is_empty()) {
            state.dirs.insert(ancestor.to_path_buf());
        }
        Ok(())
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        let mut state = lock(&self.state);
        state.maybe_fail("sync_dir")?;
        state.durable_files.retain(|p, _| p.parent() != Some(dir));
        let current: Vec<(PathBuf, Arc<RwLock<Contents>>)> =
            state.files.iter().filter(|(p, _)| p.parent() == Some(dir)).map(|(p, c)| (p.clone(), Arc::clone(c))).collect();
        state.durable_files.extend(current);
        Ok(())
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn LockGuard>> {
        let mut state = lock(&self.state);
        state.check()?;
        if !state.locks.insert(path.to_path_buf()) {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, format!("{} is locked", path.display())));
        }
        Ok(Box::new(MemLock { state: Arc::clone(&self.state), path: path.to_path_buf() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(vfs: &MemVfs, path: &str, data: &[u8], sync: bool) -> Arc<dyn File> {
        let file = vfs.open(Path::new(path), OpenMode::CreateNew).unwrap();
        file.write_all_at(data, 0).unwrap();
        if sync {
            file.sync(SyncLevel::Full).unwrap();
        }
        file
    }

    fn read(vfs: &MemVfs, path: &str) -> Option<Vec<u8>> {
        let file = vfs.open(Path::new(path), OpenMode::Read).ok()?;
        let mut buf = vec![0; usize::try_from(file.len().unwrap()).unwrap()];
        file.read_exact_at(&mut buf, 0).unwrap();
        Some(buf)
    }

    #[test]
    fn only_synced_data_and_entries_survive_a_clean_crash() {
        let vfs = MemVfs::new(1);
        vfs.create_dir_all(Path::new("/db")).unwrap();
        let a = write(&vfs, "/db/a", b"synced", true);
        write(&vfs, "/db/b", b"never synced", false);
        vfs.sync_dir(Path::new("/db")).unwrap();
        a.write_all_at(b" tail", 6).unwrap();
        write(&vfs, "/db/c", b"synced data, unsynced entry", true);
        vfs.crash();
        assert!(a.write_all_at(b"x", 0).is_err());
        assert!(vfs.open(Path::new("/db/a"), OpenMode::Read).is_err());
        vfs.restart(CrashOptions::CLEAN);
        assert_eq!(read(&vfs, "/db/a").unwrap(), b"synced");
        assert_eq!(read(&vfs, "/db/b").unwrap(), b"");
        assert!(read(&vfs, "/db/c").is_none());
    }

    #[test]
    fn removals_and_renames_need_a_directory_sync() {
        let vfs = MemVfs::new(2);
        vfs.create_dir_all(Path::new("/d")).unwrap();
        write(&vfs, "/d/old", b"1", true);
        vfs.sync_dir(Path::new("/d")).unwrap();
        vfs.rename(Path::new("/d/old"), Path::new("/d/new")).unwrap();
        vfs.crash();
        vfs.restart(CrashOptions::CLEAN);
        assert!(read(&vfs, "/d/old").is_some());
        assert!(read(&vfs, "/d/new").is_none());
    }

    #[test]
    fn torn_tails_keep_a_prefix_of_unsynced_appends() {
        for seed in 0..64 {
            let vfs = MemVfs::new(seed);
            vfs.create_dir_all(Path::new("/t")).unwrap();
            let file = write(&vfs, "/t/log", b"durable", true);
            vfs.sync_dir(Path::new("/t")).unwrap();
            file.write_all_at(&[7u8; 4_000], 7).unwrap();
            vfs.crash();
            vfs.restart(CrashOptions { keep_unsynced_prefix: true, tear_last_sector: false });
            let data = read(&vfs, "/t/log").unwrap();
            assert!(data.starts_with(b"durable") && data.len() <= 4_007);
            assert!(data[7..].iter().all(|&b| b == 7));
        }
    }

    #[test]
    fn injected_faults_fail_writes() {
        let vfs = MemVfs::new(3);
        vfs.create_dir_all(Path::new("/f")).unwrap();
        let file = write(&vfs, "/f/x", b"", false);
        vfs.set_fault_ppm(1_000_000);
        assert!(file.write_all_at(b"a", 0).is_err());
        assert!(file.sync(SyncLevel::Full).is_err());
        vfs.set_fault_ppm(0);
        file.write_all_at(b"a", 0).unwrap();
    }
}
