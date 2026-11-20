//! The set of sorted runs, versioned: readers hold an immutable [`Version`] while flushes and
//! compactions install new ones through the manifest.

pub mod edit;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;

use lilia_io::{OpenMode, SyncLevel, Vfs};

use self::edit::{NewFile, VersionEdit};
use crate::error::{corruption, Result};
use crate::files::{self, TableStore};
use crate::journal::{self, JournalWriter};
use crate::key;
use crate::memtable::Found;
use crate::options::LEVELS;

/// A sorted run of a version. Its file is deleted once it is obsolete and no version or iterator
/// still refers to it.
#[derive(Debug)]
pub struct FileMeta {
    pub number: u64,
    pub size: u64,
    pub smallest: Vec<u8>,
    pub largest: Vec<u8>,
    /// Taken by a running compaction.
    pub compacting: AtomicBool,
    obsolete: AtomicBool,
    store: Arc<TableStore>,
}

impl FileMeta {
    pub fn new(file: &NewFile, store: &Arc<TableStore>) -> Self {
        Self {
            number: file.number,
            size: file.size,
            smallest: file.smallest.clone(),
            largest: file.largest.clone(),
            compacting: AtomicBool::new(false),
            obsolete: AtomicBool::new(false),
            store: Arc::clone(store),
        }
    }

    pub fn smallest_user(&self) -> &[u8] {
        key::user(&self.smallest)
    }

    pub fn largest_user(&self) -> &[u8] {
        key::user(&self.largest)
    }

    /// Whether the run's user-key range intersects `[low, high]`.
    pub fn overlaps(&self, low: &[u8], high: &[u8]) -> bool {
        self.smallest_user() <= high && self.largest_user() >= low
    }

    pub fn mark_obsolete(&self) {
        self.obsolete.store(true, AtomicOrdering::Release);
    }
}

impl Drop for FileMeta {
    fn drop(&mut self) {
        if self.obsolete.load(AtomicOrdering::Acquire) {
            // A failed deletion leaves an orphan file; opening the database deletes orphans.
            drop(self.store.remove(self.number));
        }
    }
}

/// One immutable state of the sorted runs. Level 0 is ordered newest first and its runs may
/// overlap; deeper levels are ordered by key and do not overlap.
#[derive(Debug, Default)]
pub struct Version {
    pub levels: [Vec<Arc<FileMeta>>; LEVELS],
}

impl Version {
    /// The newest version of `user` visible at the sequence of `lookup`.
    pub fn get(&self, store: &TableStore, user: &[u8], lookup: &[u8]) -> Result<Option<Found>> {
        let probe = |file: &FileMeta| -> Result<Option<Found>> {
            let Some((found, value)) = store.get(file.number, file.size)?.get(lookup, user)? else { return Ok(None) };
            Ok(Some(Found { kind: key::kind(&found)?, sequence: key::sequence(&found), value }))
        };
        for file in self.levels[0].iter().filter(|f| f.overlaps(user, user)) {
            if let Some(found) = probe(file)? {
                return Ok(Some(found));
            }
        }
        for level in &self.levels[1..] {
            let at = level.partition_point(|f| f.largest_user() < user);
            if let Some(file) = level.get(at).filter(|f| f.smallest_user() <= user) {
                if let Some(found) = probe(file)? {
                    return Ok(Some(found));
                }
            }
        }
        Ok(None)
    }

    pub fn level_bytes(&self, level: usize) -> u64 {
        self.levels[level].iter().map(|f| f.size).sum()
    }

    /// Runs of `level` whose user-key range intersects `[low, high]`.
    pub fn overlapping(&self, level: usize, low: &[u8], high: &[u8]) -> Vec<Arc<FileMeta>> {
        self.levels[level].iter().filter(|f| f.overlaps(low, high)).cloned().collect()
    }

    /// The version that results from applying `edit`.
    pub fn apply(&self, edit: &VersionEdit, store: &Arc<TableStore>) -> Self {
        let mut next = Self::default();
        for (level, files) in self.levels.iter().enumerate() {
            next.levels[level] =
                files.iter().filter(|f| !edit.deleted.contains(&(level, f.number))).cloned().collect();
        }
        for file in &edit.added {
            next.levels[file.level].push(Arc::new(FileMeta::new(file, store)));
        }
        next.levels[0].sort_by_key(|f| std::cmp::Reverse(f.number));
        for level in &mut next.levels[1..] {
            level.sort_by(|a, b| key::compare(&a.smallest, &b.smallest));
        }
        next
    }

    /// Deeper levels must stay sorted and non-overlapping, or lookups miss data.
    pub fn check_levels(&self) -> Result<()> {
        for (level, files) in self.levels.iter().enumerate().skip(1) {
            for pair in files.windows(2) {
                if pair[0].largest_user() >= pair[1].smallest_user() {
                    return Err(corruption(format!(
                        "level {level}: runs {} and {} overlap ({:?}..{:?} vs {:?}..{:?})",
                        pair[0].number,
                        pair[1].number,
                        String::from_utf8_lossy(pair[0].smallest_user()),
                        String::from_utf8_lossy(pair[0].largest_user()),
                        String::from_utf8_lossy(pair[1].smallest_user()),
                        String::from_utf8_lossy(pair[1].largest_user())
                    )));
                }
            }
        }
        Ok(())
    }

    /// Every run as manifest records, for a manifest snapshot.
    fn files(&self) -> Vec<NewFile> {
        let mut files = Vec::new();
        for (level, runs) in self.levels.iter().enumerate() {
            for f in runs {
                files.push(NewFile { level, number: f.number, size: f.size, smallest: f.smallest.clone(), largest: f.largest.clone() });
            }
        }
        files
    }
}

/// The current version, the manifest that records it, and the file-number counter.
#[derive(Debug)]
pub struct VersionSet {
    vfs: Arc<dyn Vfs>,
    dir: PathBuf,
    store: Arc<TableStore>,
    sync: SyncLevel,
    pub current: Arc<Version>,
    manifest: Option<(u64, JournalWriter)>,
    pub next_file: u64,
    pub log_number: u64,
    /// Last sequence number recorded in the manifest.
    pub last_sequence: u64,
}

impl VersionSet {
    pub fn empty(vfs: Arc<dyn Vfs>, dir: PathBuf, store: Arc<TableStore>, sync: SyncLevel) -> Self {
        Self { vfs, dir, store, sync, current: Arc::new(Version::default()), manifest: None, next_file: 1, log_number: 0, last_sequence: 0 }
    }

    /// Loads the state recorded by the manifest that `CURRENT` names; `None` without `CURRENT`.
    pub fn recover(vfs: Arc<dyn Vfs>, dir: PathBuf, store: Arc<TableStore>, sync: SyncLevel) -> Result<Option<Self>> {
        let current = dir.join(files::CURRENT);
        if !vfs.exists(&current)? {
            return Ok(None);
        }
        let file = vfs.open(&current, OpenMode::Read)?;
        let mut name = vec![0u8; usize::try_from(file.len()?).map_err(|_| corruption("CURRENT too large"))?];
        file.read_exact_at(&mut name, 0)?;
        let name = String::from_utf8(name).map_err(|_| corruption("CURRENT is not UTF-8"))?;
        let name = name.strip_suffix('\n').ok_or_else(|| corruption("CURRENT is incomplete"))?;
        let Some(files::FileKind::Manifest(_)) = files::parse(name) else {
            return Err(corruption(format!("CURRENT names {name:?}, not a manifest")));
        };
        let manifest = vfs.open(&dir.join(name), OpenMode::Read)?;
        // A torn last record is an edit that was never acknowledged: its files are orphans and
        // its journals are still live, so the state before it is complete.
        let recovered = journal::read_all(manifest.as_ref())?;
        let mut set = Self::empty(vfs, dir, store, sync);
        let mut files: BTreeMap<(usize, u64), NewFile> = BTreeMap::new();
        let mut saw_next_file = false;
        for record in &recovered.records {
            let edit = VersionEdit::decode(record)?;
            for deleted in &edit.deleted {
                files.remove(deleted);
            }
            for added in edit.added {
                files.insert((added.level, added.number), added);
            }
            if let Some(n) = edit.log_number {
                set.log_number = n;
            }
            if let Some(n) = edit.next_file {
                set.next_file = n;
                saw_next_file = true;
            }
            if let Some(n) = edit.last_sequence {
                set.last_sequence = n;
            }
        }
        if !saw_next_file {
            return Err(corruption(format!("{name} records no file counter")));
        }
        let edit = VersionEdit { added: files.into_values().collect(), ..VersionEdit::default() };
        set.current = Arc::new(Version::default().apply(&edit, &set.store));
        Ok(Some(set))
    }

    pub fn new_file_number(&mut self) -> u64 {
        let number = self.next_file;
        self.next_file += 1;
        number
    }

    /// Ensures the counter is above every file found in the directory.
    pub fn bump_file_number(&mut self, seen: u64) {
        self.next_file = self.next_file.max(seen + 1);
    }

    /// Starts a new manifest holding the whole current state, points `CURRENT` at it, and
    /// deletes the previous manifest.
    pub fn write_snapshot(&mut self, log_number: u64, last_sequence: u64) -> Result<()> {
        let number = self.new_file_number();
        let file = self.vfs.open(&files::manifest(&self.dir, number), OpenMode::CreateNew)?;
        let mut writer = JournalWriter::new(file);
        self.log_number = log_number;
        self.last_sequence = last_sequence;
        let snapshot = VersionEdit {
            log_number: Some(log_number),
            next_file: Some(self.next_file),
            last_sequence: Some(last_sequence),
            added: self.current.files(),
            deleted: Vec::new(),
        };
        writer.append([snapshot.encode().as_slice()])?;
        writer.sync(SyncLevel::Full)?;

        let temporary = files::temporary(&self.dir, number);
        let pointer = self.vfs.open(&temporary, OpenMode::CreateNew)?;
        pointer.write_all_at(format!("MANIFEST-{number:06}\n").as_bytes(), 0)?;
        pointer.sync(SyncLevel::Full)?;
        self.vfs.rename(&temporary, &self.dir.join(files::CURRENT))?;
        self.vfs.sync_dir(&self.dir)?;
        if let Some((old, _)) = self.manifest.replace((number, writer)) {
            self.vfs.remove_file(&files::manifest(&self.dir, old))?;
        }
        Ok(())
    }

    /// Records `edit` durably in the manifest, then installs the resulting version.
    pub fn log_and_apply(&mut self, mut edit: VersionEdit) -> Result<Arc<Version>> {
        edit.next_file = Some(self.next_file);
        let (_, writer) = self.manifest.as_mut().ok_or_else(|| corruption("no manifest open for writing"))?;
        writer.append([edit.encode().as_slice()])?;
        writer.sync(self.sync)?;
        if let Some(n) = edit.log_number {
            self.log_number = n;
        }
        if let Some(n) = edit.last_sequence {
            self.last_sequence = self.last_sequence.max(n);
        }
        let next = Arc::new(self.current.apply(&edit, &self.store));
        next.check_levels()?;
        for &(level, number) in &edit.deleted {
            // A run moved to another level keeps its file.
            let moved = edit.added.iter().any(|f| f.number == number);
            if let Some(file) = self.current.levels[level].iter().find(|f| f.number == number).filter(|_| !moved) {
                file.mark_obsolete();
            }
        }
        self.current = Arc::clone(&next);
        Ok(next)
    }

    /// Numbers of every run of the current version.
    pub fn live_tables(&self) -> Vec<u64> {
        self.current.levels.iter().flatten().map(|f| f.number).collect()
    }

    pub fn manifest_number(&self) -> Option<u64> {
        self.manifest.as_ref().map(|(n, _)| *n)
    }
}
