//! The database: opening and recovery, and the public API.

mod maintenance;
mod read;
mod write;

use std::collections::BTreeMap;
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use bytes::Bytes;
use lilia_io::{LockGuard, NativeVfs, OpenMode, SyncLevel, Vfs};
use parking_lot::{Condvar, Mutex, RwLock};

pub use self::read::{Change, Entry, RangeIter, Snapshot, Versioned};
use crate::batch::{self, WriteBatch};
use crate::commit::GroupCommit;
use crate::compaction::{Retention, RunSink};
use crate::error::{corruption, Error, Result};
use crate::files::{self, FileKind, TableStore};
use crate::iter::InternalIterator;
use crate::journal::{self, JournalWriter};
use crate::memtable::Memtable;
use crate::options::{Options, LEVELS};
use crate::table::cache::BlockCache;
use crate::version::edit::VersionEdit;
use crate::version::{Version, VersionSet};

/// What readers see: the active memtable, the memtables being flushed (newest first), and the
/// sorted runs. Replaced as a whole, never mutated.
#[derive(Debug)]
pub(crate) struct SuperVersion {
    pub mem: Arc<Memtable>,
    pub imm: Vec<Arc<Memtable>>,
    pub version: Arc<Version>,
}

impl SuperVersion {
    /// The newest version of `user` visible at `sequence`: memtables first, then sorted runs.
    /// The search key is built once and shared by every level.
    pub fn lookup(&self, user: &[u8], sequence: u64) -> Result<Option<crate::memtable::Found>> {
        let target = crate::key::InternalKey(Bytes::from(crate::key::lookup(user, sequence)));
        for memtable in std::iter::once(&self.mem).chain(&self.imm) {
            if let Some(found) = memtable.get(user, &target) {
                return Ok(Some(found));
            }
        }
        self.version.get(user, &target.0)
    }
}

#[derive(Debug, Default)]
pub(crate) struct WorkState {
    pub shutdown: bool,
    /// First write or background failure; writes are refused from then on.
    pub error: Option<Error>,
}

#[derive(Debug)]
pub(crate) struct Inner {
    pub dir: PathBuf,
    pub vfs: Arc<dyn Vfs>,
    pub options: Options,
    pub current: RwLock<Arc<SuperVersion>>,
    pub versions: Mutex<VersionSet>,
    pub journal: Mutex<JournalWriter>,
    pub queue: GroupCommit<WriteBatch, Result<u64>>,
    /// Newest sequence number visible to readers.
    pub visible: AtomicU64,
    /// What readers keep from compaction.
    pub pins: Mutex<Pins>,
    pub work: Mutex<WorkState>,
    /// Wakes the flush and compaction threads.
    pub work_ready: Condvar,
    /// Wakes writers stalled on a full memtable queue or too many level-0 runs, and `flush`.
    pub progress: Condvar,
    /// Asks the next commit group to switch memtables even if the active one is not full.
    pub switch_requested: AtomicBool,
    _lock: Box<dyn LockGuard>,
}

/// The sequence numbers whose state compactions must keep readable.
#[derive(Debug, Default)]
pub(crate) struct Pins {
    /// Live snapshots: sequence → count.
    pub snapshots: BTreeMap<u64, usize>,
    /// With [`Options::retain_history`], the history horizon: every version visible at a later
    /// sequence is kept.
    pub history: Option<u64>,
}

impl Pins {
    /// Oldest sequence whose state is kept, given the newest visible one.
    pub fn oldest(&self, visible: u64) -> u64 {
        let snapshot = self.snapshots.keys().next().copied().unwrap_or(visible);
        snapshot.min(self.history.unwrap_or(visible)).min(visible)
    }
}

impl Inner {
    /// The error that disabled writes, if any.
    pub fn failure(&self) -> Option<Error> {
        self.work.lock().error.clone()
    }

    /// Records the first failure, refuses writes from then on, and wakes everyone waiting.
    pub fn fail(&self, error: &Error) {
        let mut work = self.work.lock();
        if work.error.is_none() {
            work.error = Some(Error::Poisoned(error.to_string()));
        }
        drop(work);
        self.work_ready.notify_all();
        self.progress.notify_all();
    }

    /// Wakes waiters of [`Inner::progress`]. Taking the lock first means a waiter is either
    /// already waiting or has not yet checked its condition: no wakeup is lost.
    pub fn signal_progress(&self) {
        let _work = self.work.lock();
        self.progress.notify_all();
    }

    /// Wakes the background threads, without losing the wakeup (see [`Inner::signal_progress`]).
    pub fn signal_work(&self) {
        let _work = self.work.lock();
        self.work_ready.notify_all();
    }

    pub fn super_version(&self) -> Arc<SuperVersion> {
        Arc::clone(&self.current.read())
    }

    /// Oldest sequence number some reader may still read at. Read under the pins' lock, so a
    /// snapshot taken at a past sequence ([`Snapshot::at`]) is either counted here or taken at a
    /// sequence no older than this.
    pub fn smallest_snapshot(&self) -> u64 {
        let pins = self.pins.lock();
        pins.oldest(self.visible.load(Ordering::Acquire))
    }

    fn sync_journal(&self) -> Result<()> {
        self.journal.lock().sync(SyncLevel::Full).map_err(Error::from)
    }
}

/// Per-level shape of the database, for monitoring and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stats {
    /// `(runs, bytes)` per level.
    pub levels: Vec<(usize, u64)>,
    pub memtable_bytes: usize,
    pub immutable_memtables: usize,
    pub sequence: u64,
}

/// An open database. Cheap to share across threads by reference; every method takes `&self`.
#[derive(Debug)]
pub struct Db {
    inner: Arc<Inner>,
    threads: Vec<JoinHandle<()>>,
    closed: bool,
}

/// Deletes a file that is no longer referenced. A failure leaves an orphan that the next open
/// deletes, so it is not an error for the caller.
pub(crate) fn remove_unreferenced(vfs: &dyn Vfs, path: &Path) {
    vfs.remove_file(path).unwrap_or_default();
}

impl Db {
    /// Opens (creating if allowed) the database in `path` on the native file system.
    ///
    /// # Errors
    ///
    /// Invalid options, I/O errors, another handle holding the directory lock, or stored data
    /// that fails its checksums ([`Error::Corruption`]).
    pub fn open(path: impl AsRef<Path>, options: Options) -> Result<Self> {
        Self::open_with(Arc::new(NativeVfs), path, options)
    }

    /// Opens the database in `path` on `vfs`.
    ///
    /// # Errors
    ///
    /// As [`Db::open`].
    pub fn open_with(vfs: Arc<dyn Vfs>, path: impl AsRef<Path>, options: Options) -> Result<Self> {
        options.validate()?;
        let dir = path.as_ref().to_path_buf();
        if !vfs.exists(&dir)? {
            if !options.create_if_missing {
                return Err(Error::InvalidArgument(format!("{} does not exist", dir.display())));
            }
            vfs.create_dir_all(&dir)?;
        }
        let lock = vfs.lock(&dir.join(files::LOCK))?;
        let store = Arc::new(TableStore::new(Arc::clone(&vfs), dir.clone(), Arc::new(BlockCache::new(options.block_cache_bytes))));
        let sync = options.durability.metadata_sync();
        let mut versions = match VersionSet::recover(Arc::clone(&vfs), dir.clone(), Arc::clone(&store), sync)? {
            Some(set) => set,
            None if options.create_if_missing => VersionSet::empty(Arc::clone(&vfs), dir.clone(), Arc::clone(&store), sync),
            None => return Err(Error::InvalidArgument(format!("no database in {}", dir.display()))),
        };

        let names = vfs.list_dir(&dir)?;
        let mut journals: Vec<u64> = Vec::new();
        for kind in names.iter().filter_map(|n| files::parse(n)) {
            let (FileKind::Journal(n) | FileKind::Table(n) | FileKind::Manifest(n) | FileKind::Temporary(n)) = kind;
            versions.bump_file_number(n);
            if let FileKind::Journal(n) = kind {
                if n >= versions.log_number {
                    journals.push(n);
                }
            }
        }
        journals.sort_unstable();
        let last_sequence = replay(&vfs, &dir, &options, &store, &mut versions, &journals)?;

        let journal_number = versions.new_file_number();
        let journal_file = vfs.open(&files::journal(&dir, journal_number), OpenMode::CreateNew)?;
        journal_file.preallocate(options.memtable_bytes as u64 + (1 << 20))?;
        versions.write_snapshot(journal_number, last_sequence)?;
        sweep(vfs.as_ref(), &dir, &names, &versions)?;
        for level in &versions.current.levels {
            for file in level {
                if !vfs.exists(&files::table(&dir, file.number))? {
                    return Err(corruption(format!("table {} listed by the manifest is missing", file.number)));
                }
            }
        }

        let super_version = SuperVersion {
            mem: Arc::new(Memtable::new(journal_number, options.memtable_bytes)),
            imm: Vec::new(),
            version: Arc::clone(&versions.current),
        };
        let history = options.retain_history.then_some(0);
        let inner = Arc::new(Inner {
            dir,
            vfs,
            options,
            current: RwLock::new(Arc::new(super_version)),
            versions: Mutex::new(versions),
            journal: Mutex::new(JournalWriter::new(journal_file)),
            queue: GroupCommit::default(),
            visible: AtomicU64::new(last_sequence),
            pins: Mutex::new(Pins { snapshots: BTreeMap::new(), history }),
            work: Mutex::new(WorkState::default()),
            work_ready: Condvar::new(),
            progress: Condvar::new(),
            switch_requested: AtomicBool::new(false),
            _lock: lock,
        });
        let threads = maintenance::start(&inner)?;
        Ok(Self { inner, threads, closed: false })
    }

    /// Commits `batch` atomically. Returns the sequence number of its first write; write `i` of
    /// the batch gets version `first + i`. Fails with [`Error::Conflict`] when an expectation of
    /// the batch does not hold, and then writes nothing.
    ///
    /// # Errors
    ///
    /// [`Error::Conflict`] for a failed expectation; [`Error::Poisoned`] once a journal write,
    /// sync or background job has failed; [`Error::Io`] for the failure itself.
    pub fn write(&self, batch: WriteBatch) -> Result<u64> {
        self.inner.write(batch)
    }

    /// Writes one key; see [`Db::write`].
    ///
    /// # Errors
    ///
    /// As [`Db::write`].
    pub fn put(&self, key: impl Into<Bytes>, value: impl Into<Bytes>) -> Result<u64> {
        let mut batch = WriteBatch::new();
        batch.put(key, value);
        self.write(batch)
    }

    /// Deletes one key; see [`Db::write`].
    ///
    /// # Errors
    ///
    /// As [`Db::write`].
    pub fn delete(&self, key: impl Into<Bytes>) -> Result<u64> {
        let mut batch = WriteBatch::new();
        batch.delete(key);
        self.write(batch)
    }

    /// The newest value of `key`.
    ///
    /// # Errors
    ///
    /// I/O errors and checksum failures of the sorted runs read.
    pub fn get(&self, key: &[u8]) -> Result<Option<Bytes>> {
        Ok(self.get_versioned(key)?.map(|v| v.value))
    }

    /// The newest value of `key` and its version.
    ///
    /// # Errors
    ///
    /// As [`Db::get`].
    pub fn get_versioned(&self, key: &[u8]) -> Result<Option<Versioned>> {
        self.inner.get_at(key, self.inner.visible.load(Ordering::Acquire))
    }

    /// Sequence number of the newest visible write (0 for an empty database). The next batch
    /// commits at `last_sequence() + 1` unless another one commits first.
    #[must_use]
    pub fn last_sequence(&self) -> u64 {
        self.inner.visible.load(Ordering::Acquire)
    }

    /// A consistent view of the database as of now; reads through it ignore later writes.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot::new(&self.inner)
    }

    /// A view of the database as it was after the write of `sequence`. Its state must still be
    /// kept: `sequence` at or after [`Db::oldest_sequence`].
    ///
    /// # Errors
    ///
    /// [`Error::HistoryUnavailable`] for an older sequence; [`Error::InvalidArgument`] for one
    /// after the newest visible write.
    pub fn snapshot_at(&self, sequence: u64) -> Result<Snapshot> {
        Snapshot::at(&self.inner, sequence)
    }

    /// Oldest sequence number whose state is still kept: the history horizon
    /// ([`Options::retain_history`]), or else the oldest live snapshot, or else the newest write.
    #[must_use]
    pub fn oldest_sequence(&self) -> u64 {
        self.inner.smallest_snapshot()
    }

    /// Moves the history horizon forward to `sequence` (clamped to the newest visible write):
    /// compactions may then drop the versions only a read before it would see. A horizon never
    /// moves back.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgument`] when the database keeps no history.
    pub fn set_history_horizon(&self, sequence: u64) -> Result<()> {
        let mut pins = self.inner.pins.lock();
        let Some(horizon) = pins.history.as_mut() else {
            return Err(Error::InvalidArgument("the database keeps no history (Options::retain_history)".into()));
        };
        *horizon = (*horizon).max(sequence.min(self.inner.visible.load(Ordering::Acquire)));
        Ok(())
    }

    /// The versions of `key` still kept, newest first: every write at or after
    /// [`Db::oldest_sequence`] and the one that was current then. A deletion has no value.
    ///
    /// # Errors
    ///
    /// As [`Db::get`].
    pub fn history(&self, key: &[u8]) -> Result<Vec<Change>> {
        self.inner.history(key)
    }

    /// The value of `key` as `snapshot` sees it.
    ///
    /// # Errors
    ///
    /// As [`Db::get`].
    pub fn get_at(&self, snapshot: &Snapshot, key: &[u8]) -> Result<Option<Versioned>> {
        self.inner.get_at(key, snapshot.sequence())
    }

    /// Entries with keys in `[start, end]` (as bounded), in key order, as of now.
    ///
    /// # Errors
    ///
    /// I/O errors opening the sorted runs; the iterator reports later read errors itself.
    pub fn range(&self, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<RangeIter> {
        RangeIter::new(&self.inner, self.inner.visible.load(Ordering::Acquire), start, end)
    }

    /// Entries with keys in `[start, end]` as `snapshot` sees them.
    ///
    /// # Errors
    ///
    /// As [`Db::range`].
    pub fn range_at(&self, snapshot: &Snapshot, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<RangeIter> {
        RangeIter::new(&self.inner, snapshot.sequence(), start, end)
    }

    /// Writes the active memtable to a sorted run and waits until every memtable is flushed.
    ///
    /// # Errors
    ///
    /// The failure that stopped the flush thread, if any.
    pub fn flush(&self) -> Result<()> {
        self.inner.switch_requested.store(true, Ordering::Release);
        self.inner.write(WriteBatch::new())?;
        let mut work = self.inner.work.lock();
        loop {
            if let Some(error) = &work.error {
                return Err(error.clone());
            }
            if self.inner.super_version().imm.is_empty() && !self.inner.switch_requested.load(Ordering::Acquire) {
                return Ok(());
            }
            self.inner.progress.wait(&mut work);
        }
    }

    /// Waits until no compaction is due (every level within budget, level 0 below its trigger).
    ///
    /// # Errors
    ///
    /// The failure that stopped a compaction thread, if any.
    pub fn wait_for_compactions(&self) -> Result<()> {
        let mut work = self.inner.work.lock();
        loop {
            if let Some(error) = &work.error {
                return Err(error.clone());
            }
            let version = Arc::clone(&self.inner.super_version().version);
            if !maintenance::compaction_due(&version, &self.inner.options) {
                return Ok(());
            }
            self.inner.work_ready.notify_all();
            self.inner.progress.wait(&mut work);
        }
    }

    #[must_use]
    pub fn stats(&self) -> Stats {
        let current = self.inner.super_version();
        Stats {
            levels: (0..LEVELS).map(|l| (current.version.levels[l].len(), current.version.level_bytes(l))).collect(),
            memtable_bytes: current.mem.approximate_bytes(),
            immutable_memtables: current.imm.len(),
            sequence: self.inner.visible.load(Ordering::Acquire),
        }
    }

    /// Stops the background threads and makes every acknowledged write durable.
    ///
    /// # Errors
    ///
    /// The failure that disabled writes, if any, or the final journal sync's.
    pub fn close(mut self) -> Result<()> {
        self.shutdown()
    }

    fn shutdown(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.inner.work.lock().shutdown = true;
        self.inner.work_ready.notify_all();
        self.inner.progress.notify_all();
        for thread in self.threads.drain(..) {
            thread.join().map_err(|_| corruption("a background thread panicked"))?;
        }
        match self.inner.failure() {
            Some(error) => Err(error),
            None => self.inner.sync_journal(),
        }
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        // `close` reports these errors; dropping without closing has nobody to report them to.
        drop(self.shutdown());
    }
}

/// Replays the live journals into a memtable and writes it as a level-0 run; returns the last
/// sequence number. Replay stops at the first damaged record: everything after it in time was
/// never acknowledged as durable.
fn replay(vfs: &Arc<dyn Vfs>, dir: &Path, options: &Options, store: &Arc<TableStore>, versions: &mut VersionSet, journals: &[u64]) -> Result<u64> {
    let memtable = Arc::new(Memtable::new(0, options.memtable_bytes));
    let mut last_sequence = versions.last_sequence;
    for &number in journals {
        let file = vfs.open(&files::journal(dir, number), OpenMode::Read)?;
        let recovered = journal::read_all(file.as_ref())?;
        for record in &recovered.records {
            let (first, ops) = batch::decode(record)?;
            for (i, op) in ops.iter().enumerate() {
                memtable.insert(first + i as u64, op);
            }
            if let Some(last) = (ops.len() as u64).checked_sub(1) {
                last_sequence = last_sequence.max(first + last);
            }
        }
        if recovered.torn {
            break;
        }
    }
    if memtable.is_empty() {
        return Ok(last_sequence);
    }
    let mut iter = memtable.iter();
    iter.seek_to_first()?;
    let mut allocate = || versions.new_file_number();
    let mut sink = RunSink { vfs: vfs.as_ref(), dir, options, sync: options.durability.metadata_sync(), level: 0, allocate: &mut allocate };
    // With history, an open keeps every version: the horizon starts at the oldest data.
    let smallest_snapshot = if options.retain_history { 0 } else { last_sequence };
    let added = sink.write(&mut iter, &Retention { smallest_snapshot, tombstones: None })?;
    let edit = VersionEdit { added, ..VersionEdit::default() };
    versions.current = Arc::new(versions.current.apply(&edit, store));
    Ok(last_sequence)
}

/// Deletes files no longer referenced: old journals, manifests and temporaries, and runs that
/// are not part of the current version (left by an interrupted flush or compaction).
fn sweep(vfs: &dyn Vfs, dir: &Path, names: &[String], versions: &VersionSet) -> Result<()> {
    let live = versions.live_tables();
    let manifest = versions.manifest_number();
    for name in names {
        let obsolete = match files::parse(name) {
            Some(FileKind::Journal(n)) => n < versions.log_number,
            Some(FileKind::Table(n)) => !live.contains(&n),
            Some(FileKind::Manifest(n)) => Some(n) != manifest,
            Some(FileKind::Temporary(_)) => true,
            None => false,
        };
        if obsolete {
            vfs.remove_file(&dir.join(name))?;
        }
    }
    vfs.sync_dir(dir)?;
    Ok(())
}
