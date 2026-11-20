//! The read path: point lookups, snapshots and range iteration.

use std::cmp::Ordering;
use std::ops::Bound;
use std::sync::Arc;

use bytes::Bytes;

use super::{Inner, SuperVersion};
use crate::error::Result;
use crate::files::TableStore;
use crate::iter::{InternalIterator, MergingIterator};
use crate::key::{self, Kind};
use crate::memtable::Found;
use crate::table::TableIter;
use crate::version::FileMeta;

/// A value and its version (the sequence number of the write that produced it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Versioned {
    pub value: Bytes,
    pub version: u64,
}

/// One entry of a range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: Bytes,
    pub value: Bytes,
    pub version: u64,
}

fn visible(found: Option<Found>) -> Option<Versioned> {
    found.filter(|f| f.kind == Kind::Put).map(|f| Versioned { value: f.value, version: f.sequence })
}

impl Inner {
    pub fn get_at(&self, user: &[u8], sequence: u64) -> Result<Option<Versioned>> {
        let current = self.super_version();
        for memtable in std::iter::once(&current.mem).chain(&current.imm) {
            if let Some(found) = memtable.get(user, sequence) {
                return Ok(visible(Some(found)));
            }
        }
        Ok(visible(current.version.get(&self.store, user, &key::lookup(user, sequence))?))
    }
}

/// A fixed point in the history of the database. While it lives, compactions keep every version
/// it can see.
#[derive(Debug)]
pub struct Snapshot {
    inner: Arc<Inner>,
    sequence: u64,
}

impl Snapshot {
    pub(crate) fn new(inner: &Arc<Inner>) -> Self {
        let mut snapshots = inner.snapshots.lock();
        // Read under the registry lock so the sequence cannot drop below the oldest snapshot
        // that compaction is honouring.
        let sequence = inner.visible.load(std::sync::atomic::Ordering::Acquire);
        *snapshots.entry(sequence).or_insert(0) += 1;
        Self { inner: Arc::clone(inner), sequence }
    }

    /// The last sequence number this snapshot sees.
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        let mut snapshots = self.inner.snapshots.lock();
        if let Some(count) = snapshots.get_mut(&self.sequence) {
            *count -= 1;
            if *count == 0 {
                snapshots.remove(&self.sequence);
            }
        }
    }
}

/// Runs of one deeper level, read one after the other: only the run holding the cursor is open.
struct LevelIter {
    files: Vec<Arc<FileMeta>>,
    store: Arc<TableStore>,
    at: usize,
    current: Option<TableIter>,
}

impl LevelIter {
    /// Positions on run `at` (seeking `target` in it, if given), or on the first later run that
    /// has entries.
    fn open(&mut self, mut at: usize, mut target: Option<&[u8]>) -> Result<()> {
        self.current = None;
        while let Some(file) = self.files.get(at) {
            let mut iter = self.store.get(file.number, file.size)?.iter();
            match target.take() {
                Some(t) => iter.seek(t)?,
                None => iter.seek_to_first()?,
            }
            if iter.valid() {
                self.current = Some(iter);
                break;
            }
            at += 1;
        }
        self.at = at;
        Ok(())
    }
}

impl InternalIterator for LevelIter {
    fn valid(&self) -> bool {
        self.current.as_ref().is_some_and(InternalIterator::valid)
    }

    fn seek_to_first(&mut self) -> Result<()> {
        self.open(0, None)
    }

    fn seek(&mut self, target: &[u8]) -> Result<()> {
        let at = self.files.partition_point(|f| key::compare(&f.largest, target) == Ordering::Less);
        self.open(at, Some(target))
    }

    fn next(&mut self) -> Result<()> {
        let Some(current) = self.current.as_mut() else { return Ok(()) };
        current.next()?;
        if !current.valid() {
            self.open(self.at + 1, None)?;
        }
        Ok(())
    }

    fn key(&self) -> &[u8] {
        self.current.as_ref().map_or(&[], InternalIterator::key)
    }

    fn value(&self) -> &[u8] {
        self.current.as_ref().map_or(&[], InternalIterator::value)
    }
}

/// Entries of a key range as of one sequence number, in key order. Holds the memtables and runs
/// it reads, so concurrent flushes and compactions do not disturb it.
pub struct RangeIter {
    merged: MergingIterator,
    sequence: u64,
    end: Bound<Vec<u8>>,
    /// User key already resolved (its newest visible version was returned or was a tombstone).
    resolved: Option<Vec<u8>>,
    done: bool,
    _source: Arc<SuperVersion>,
}

impl std::fmt::Debug for RangeIter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RangeIter").field("sequence", &self.sequence).field("done", &self.done).finish_non_exhaustive()
    }
}

impl RangeIter {
    pub(crate) fn new(inner: &Inner, sequence: u64, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<Self> {
        let source = inner.super_version();
        let mut children: Vec<Box<dyn InternalIterator>> = Vec::new();
        for memtable in std::iter::once(&source.mem).chain(&source.imm) {
            children.push(Box::new(memtable.iter()));
        }
        for file in &source.version.levels[0] {
            children.push(Box::new(inner.store.get(file.number, file.size)?.iter()));
        }
        for level in &source.version.levels[1..] {
            if !level.is_empty() {
                children.push(Box::new(LevelIter { files: level.clone(), store: Arc::clone(&inner.store), at: 0, current: None }));
            }
        }
        let mut merged = MergingIterator::new(children);
        let mut resolved = None;
        match start {
            Bound::Unbounded => merged.seek_to_first()?,
            Bound::Included(k) => merged.seek(&key::lookup(k, sequence))?,
            Bound::Excluded(k) => {
                merged.seek(&key::lookup(k, sequence))?;
                resolved = Some(k.to_vec());
            }
        }
        Ok(Self { merged, sequence, end: end.map(<[u8]>::to_vec), resolved, done: false, _source: source })
    }

    fn past_end(&self, user: &[u8]) -> bool {
        match &self.end {
            Bound::Unbounded => false,
            Bound::Included(end) => user > end.as_slice(),
            Bound::Excluded(end) => user >= end.as_slice(),
        }
    }

    fn step(&mut self) -> Result<Option<Entry>> {
        while !self.done && self.merged.valid() {
            let internal = self.merged.key();
            let user = key::user(internal);
            if self.resolved.as_deref() == Some(user) || key::sequence(internal) > self.sequence {
                self.merged.next()?;
                continue;
            }
            if self.past_end(user) {
                self.done = true;
                break;
            }
            // First version of this key visible at the snapshot: it decides the key.
            self.resolved = Some(user.to_vec());
            let entry = (key::kind(internal)? == Kind::Put).then(|| Entry {
                key: Bytes::copy_from_slice(user),
                value: Bytes::copy_from_slice(self.merged.value()),
                version: key::sequence(internal),
            });
            self.merged.next()?;
            if entry.is_some() {
                return Ok(entry);
            }
        }
        self.done = true;
        Ok(None)
    }
}

impl Iterator for RangeIter {
    type Item = Result<Entry>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.step() {
            Ok(entry) => entry.map(Ok),
            Err(error) => {
                self.done = true;
                Some(Err(error))
            }
        }
    }
}
