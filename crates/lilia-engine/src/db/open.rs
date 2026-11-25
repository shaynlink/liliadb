//! Opening and recovery: the directory lock, the manifest, journal replay, and the sweep of
//! files left by an interrupted flush or compaction.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

use lilia_io::{OpenMode, Vfs};
use parking_lot::{Condvar, Mutex, RwLock};

use super::{maintenance, Db, Inner, Pins, SuperVersion, WorkState};
use crate::batch;
use crate::commit::GroupCommit;
use crate::compaction::{Retention, RunSink};
use crate::error::{corruption, Error, Result};
use crate::files::{self, FileKind, TableStore};
use crate::iter::InternalIterator;
use crate::journal::{self, JournalWriter};
use crate::memtable::Memtable;
use crate::options::Options;
use crate::table::cache::BlockCache;
use crate::version::edit::VersionEdit;
use crate::version::VersionSet;

impl Db {
    /// Opens (creating if allowed) the database in `path` on the native file system.
    ///
    /// # Errors
    ///
    /// Invalid options, I/O errors, another handle holding the directory lock, or stored data
    /// that fails its checksums ([`Error::Corruption`]).
    #[cfg(any(unix, windows))]
    pub fn open(path: impl AsRef<Path>, options: Options) -> Result<Self> {
        Self::open_with(Arc::new(lilia_io::NativeVfs), path, options)
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
                return Err(Error::InvalidArgument(format!(
                    "{} does not exist",
                    dir.display()
                )));
            }
            vfs.create_dir_all(&dir)?;
        }
        let lock = vfs.lock(&dir.join(files::LOCK))?;
        let store = Arc::new(TableStore::new(
            Arc::clone(&vfs),
            dir.clone(),
            Arc::new(BlockCache::new(options.block_cache_bytes)),
        ));
        let sync = options.durability.metadata_sync();
        let mut versions =
            match VersionSet::recover(Arc::clone(&vfs), dir.clone(), Arc::clone(&store), sync)? {
                Some(set) => set,
                None if options.create_if_missing => {
                    VersionSet::empty(Arc::clone(&vfs), dir.clone(), Arc::clone(&store), sync)
                }
                None => {
                    return Err(Error::InvalidArgument(format!(
                        "no database in {}",
                        dir.display()
                    )))
                }
            };

        let names = vfs.list_dir(&dir)?;
        let mut journals: Vec<u64> = Vec::new();
        for kind in names.iter().filter_map(|n| files::parse(n)) {
            let (FileKind::Journal(n)
            | FileKind::Table(n)
            | FileKind::Manifest(n)
            | FileKind::Temporary(n)) = kind;
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
                    return Err(corruption(format!(
                        "table {} listed by the manifest is missing",
                        file.number
                    )));
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
            pins: Mutex::new(Pins {
                snapshots: BTreeMap::new(),
                history,
            }),
            work: Mutex::new(WorkState::default()),
            work_ready: Condvar::new(),
            progress: Condvar::new(),
            switch_requested: AtomicBool::new(false),
            _lock: lock,
        });
        let threads = maintenance::start(&inner)?;
        Ok(Self {
            inner,
            threads,
            closed: false,
        })
    }
}

/// Replays the live journals into a memtable and writes it as a level-0 run; returns the last
/// sequence number. Replay stops at the first damaged record: everything after it in time was
/// never acknowledged as durable.
fn replay(
    vfs: &Arc<dyn Vfs>,
    dir: &Path,
    options: &Options,
    store: &Arc<TableStore>,
    versions: &mut VersionSet,
    journals: &[u64],
) -> Result<u64> {
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
    let mut sink = RunSink {
        vfs: vfs.as_ref(),
        dir,
        options,
        sync: options.durability.metadata_sync(),
        level: 0,
        allocate: &mut allocate,
    };
    // With history, an open keeps every version: the horizon starts at the oldest data.
    let smallest_snapshot = if options.retain_history {
        0
    } else {
        last_sequence
    };
    let added = sink.write(
        &mut iter,
        &Retention {
            smallest_snapshot,
            tombstones: None,
        },
    )?;
    let edit = VersionEdit {
        added,
        ..VersionEdit::default()
    };
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
