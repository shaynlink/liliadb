//! Choosing and executing compactions, and writing sorted runs from an iterator (also used by
//! memtable flushes).

use std::path::Path;
use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::Arc;

use lilia_io::{OpenMode, SyncLevel, Vfs};

use crate::error::Result;
use crate::files;
use crate::iter::InternalIterator;
use crate::key::{self, Kind};
use crate::options::{Options, LEVELS};
use crate::table::TableBuilder;
use crate::version::edit::NewFile;
use crate::version::{FileMeta, Version};

/// Runs of `level` and the overlapping runs of `level + 1`, merged into `level + 1`.
#[derive(Debug)]
pub struct Compaction {
    pub level: usize,
    pub inputs: [Vec<Arc<FileMeta>>; 2],
}

impl Compaction {
    /// Moving the single input down a level rewrites nothing.
    pub fn is_trivial_move(&self) -> bool {
        self.level > 0 && self.inputs[0].len() == 1 && self.inputs[1].is_empty()
    }

    pub fn all_inputs(&self) -> impl Iterator<Item = &Arc<FileMeta>> {
        self.inputs.iter().flatten()
    }

    /// Lets other compactions take the inputs again.
    pub fn release(&self) {
        for file in self.all_inputs() {
            file.compacting.store(false, AtomicOrdering::Release);
        }
    }

    fn user_range(files: &[Arc<FileMeta>]) -> Option<(Vec<u8>, Vec<u8>)> {
        let low = files.iter().map(|f| f.smallest_user()).min()?.to_vec();
        let high = files.iter().map(|f| f.largest_user()).max()?.to_vec();
        Some((low, high))
    }
}

fn free(files: &[Arc<FileMeta>]) -> bool {
    files.iter().all(|f| !f.compacting.load(AtomicOrdering::Acquire))
}

/// Picks the most urgent compaction whose runs no other compaction holds, and marks its runs as
/// taken. Level 0 goes first once it has `l0_compaction_trigger` runs; then the deeper level that
/// most exceeds its size budget.
#[allow(clippy::cast_precision_loss)] // reason: scores only rank levels; 52 bits of precision are plenty.
pub fn pick(version: &Version, options: &Options) -> Option<Compaction> {
    let mut candidates: Vec<(f64, usize)> = Vec::new();
    if version.levels[0].len() >= options.l0_compaction_trigger {
        candidates.push((version.levels[0].len() as f64 / options.l0_compaction_trigger as f64, 0));
    }
    for level in 1..LEVELS - 1 {
        let score = version.level_bytes(level) as f64 / options.level_bytes(level) as f64;
        if score >= 1.0 {
            candidates.push((score, level));
        }
    }
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (_, level) in candidates {
        let chosen = if level == 0 {
            let inputs = version.levels[0].clone();
            free(&inputs).then_some(inputs)
        } else {
            version.levels[level].iter().find(|f| {
                !f.compacting.load(AtomicOrdering::Acquire)
                    && free(&version.overlapping(level + 1, f.smallest_user(), f.largest_user()))
            }).map(|f| vec![Arc::clone(f)])
        };
        let Some(lower) = chosen else { continue };
        let Some((low, high)) = Compaction::user_range(&lower) else { continue };
        let upper = version.overlapping(level + 1, &low, &high);
        if !free(&upper) {
            continue;
        }
        let compaction = Compaction { level, inputs: [lower, upper] };
        for file in compaction.all_inputs() {
            file.compacting.store(true, AtomicOrdering::Release);
        }
        return Some(compaction);
    }
    None
}

/// What a merge may drop.
#[derive(Debug)]
pub struct Retention<'a> {
    /// No reader can see a version older than the newest one at or below this sequence.
    pub smallest_snapshot: u64,
    /// When set, tombstones of keys absent from these deeper levels are dropped.
    pub tombstones: Option<(&'a Version, usize)>,
}

impl Retention<'_> {
    fn is_base_level(&self, user: &[u8]) -> bool {
        self.tombstones.is_some_and(|(version, deepest_output)| {
            version.levels[deepest_output + 1..].iter().all(|level| level.iter().all(|f| !f.overlaps(user, user)))
        })
    }
}

/// Where new runs go and how they are built.
pub struct RunSink<'a> {
    pub vfs: &'a dyn Vfs,
    pub dir: &'a Path,
    pub options: &'a Options,
    pub sync: SyncLevel,
    pub level: usize,
    /// Allocates file numbers from the version set.
    pub allocate: &'a mut dyn FnMut() -> u64,
}

impl std::fmt::Debug for RunSink<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunSink").field("dir", &self.dir).field("level", &self.level).finish_non_exhaustive()
    }
}

impl RunSink<'_> {
    fn start(&mut self) -> Result<(u64, TableBuilder)> {
        let number = (self.allocate)();
        let file = self.vfs.open(&files::table(self.dir, number), OpenMode::CreateNew)?;
        Ok((number, TableBuilder::new(file, self.options.block_bytes, self.options.bloom_bits_per_key)))
    }

    fn finish(&self, number: u64, builder: TableBuilder, out: &mut Vec<NewFile>) -> Result<()> {
        let info = builder.finish(self.sync)?;
        out.push(NewFile { level: self.level, number, size: info.size, smallest: info.smallest, largest: info.largest });
        Ok(())
    }

    /// Writes the entries of `iter` (from its current position to its end) into runs of about
    /// `target_file_bytes`, dropping what `retention` allows. Versions of one user key always
    /// land in the same run. Returns the runs written, all synced.
    pub fn write(&mut self, iter: &mut dyn InternalIterator, retention: &Retention<'_>) -> Result<Vec<NewFile>> {
        let mut out = Vec::new();
        let mut current: Option<(u64, TableBuilder)> = None;
        let mut last_user: Option<Vec<u8>> = None;
        let mut last_sequence_for_key = u64::MAX;
        while iter.valid() {
            let entry = iter.key();
            let (user, sequence, kind) = (key::user(entry), key::sequence(entry), key::kind(entry)?);
            let new_user = last_user.as_deref() != Some(user);
            if new_user {
                last_sequence_for_key = u64::MAX;
            }
            // Hidden: a newer version of this key is visible to every reader.
            let hidden = last_sequence_for_key <= retention.smallest_snapshot;
            let dead_tombstone = kind == Kind::Delete && sequence <= retention.smallest_snapshot && retention.is_base_level(user);
            last_sequence_for_key = sequence;
            if !hidden && !dead_tombstone {
                if new_user {
                    if let Some((number, builder)) = current.take_if(|(_, b)| b.estimated_size() >= self.options.target_file_bytes) {
                        self.finish(number, builder, &mut out)?;
                    }
                }
                if current.is_none() {
                    current = Some(self.start()?);
                }
                if let Some((_, builder)) = current.as_mut() {
                    builder.add(entry, iter.value())?;
                }
            }
            if new_user {
                last_user = Some(user.to_vec());
            }
            iter.next()?;
        }
        if let Some((number, builder)) = current {
            if builder.entries() > 0 {
                self.finish(number, builder, &mut out)?;
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use lilia_io::MemVfs;

    use super::*;
    use crate::iter::testing::VecIter;
    use crate::key::encode;

    fn run(entries: Vec<(Vec<u8>, Vec<u8>)>, smallest_snapshot: u64, drop_tombstones: bool) -> Vec<(Vec<u8>, u64)> {
        let vfs = MemVfs::new(1);
        let dir = Path::new("/c");
        vfs.create_dir_all(dir).unwrap();
        let options = Options { target_file_bytes: 1 << 20, ..Options::for_hardware(&lilia_io::HardwareProfile::minimal()) };
        let mut next = 10;
        let mut allocate = || {
            next += 1;
            next
        };
        let mut sink = RunSink { vfs: &vfs, dir, options: &options, sync: SyncLevel::Full, level: 1, allocate: &mut allocate };
        let empty = Version::default();
        let retention = Retention { smallest_snapshot, tombstones: drop_tombstones.then_some((&empty, 1)) };
        let mut iter = VecIter::new(entries);
        iter.seek_to_first().unwrap();
        let outputs = sink.write(&mut iter, &retention).unwrap();
        let store = Arc::new(crate::files::TableStore::new(Arc::new(vfs.clone()), dir.to_path_buf(), Arc::new(crate::table::cache::BlockCache::new(0))));
        let mut kept = Vec::new();
        for file in outputs {
            let table = store.open(file.number, file.size).unwrap();
            let mut it = table.iter();
            it.seek_to_first().unwrap();
            while it.valid() {
                kept.push((key::user(it.key()).to_vec(), key::sequence(it.key())));
                it.next().unwrap();
            }
        }
        kept
    }

    #[test]
    fn keeps_versions_visible_to_snapshots_and_drops_dead_tombstones() {
        let entries = vec![
            (encode(b"a", 9, Kind::Put), b"a9".to_vec()),
            (encode(b"a", 5, Kind::Put), b"a5".to_vec()),
            (encode(b"a", 2, Kind::Put), b"a2".to_vec()),
            (encode(b"b", 4, Kind::Delete), vec![]),
            (encode(b"b", 1, Kind::Put), b"b1".to_vec()),
        ];
        // A snapshot at 6 still sees a5; a2 and b1 are hidden from everyone.
        assert_eq!(run(entries.clone(), 6, false), [(b"a".to_vec(), 9), (b"a".to_vec(), 5), (b"b".to_vec(), 4)]);
        // No snapshot older than 9: only the newest versions remain, and the tombstone goes.
        assert_eq!(run(entries.clone(), 9, true), [(b"a".to_vec(), 9)]);
        // A tombstone newer than the oldest snapshot stays.
        assert_eq!(run(entries, 3, true), [(b"a".to_vec(), 9), (b"a".to_vec(), 5), (b"a".to_vec(), 2), (b"b".to_vec(), 4), (b"b".to_vec(), 1)]);
    }
}
