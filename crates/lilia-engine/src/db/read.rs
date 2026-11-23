//! The read path: point lookups, snapshots and range iteration.

use std::cmp::Ordering;
use std::ops::Bound;
use std::sync::Arc;

use bytes::Bytes;

use super::{Inner, SuperVersion};
use crate::error::{Error, Result};
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

/// One write of a key: the value it stored, or `None` for a deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub version: u64,
    pub value: Option<Bytes>,
}

fn visible(found: Option<Found>) -> Option<Versioned> {
    found.filter(|f| f.kind == Kind::Put).map(|f| Versioned {
        value: f.value,
        version: f.sequence,
    })
}

impl Inner {
    pub fn get_at(&self, user: &[u8], sequence: u64) -> Result<Option<Versioned>> {
        let current = self.super_version();
        Ok(visible(current.lookup(user, sequence)?))
    }

    pub fn history(&self, user: &[u8]) -> Result<Vec<Change>> {
        let oldest = self.smallest_snapshot();
        let visible = self.visible.load(std::sync::atomic::Ordering::Acquire);
        let source = self.super_version();
        let mut merged = merged(&source)?;
        merged.seek(&key::lookup(user, visible))?;
        let mut changes = Vec::new();
        while merged.valid() && key::user(merged.key()) == user {
            let internal = merged.key();
            let version = key::sequence(internal);
            let value = (key::kind(internal)? == Kind::Put).then(|| merged.value_bytes());
            changes.push(Change { version, value });
            if version <= oldest {
                break;
            }
            merged.next()?;
        }
        Ok(changes)
    }
}

/// Every memtable and run of `source`, merged in internal key order.
fn merged(source: &SuperVersion) -> Result<MergingIterator> {
    let mut children: Vec<Box<dyn InternalIterator>> = Vec::new();
    for memtable in std::iter::once(&source.mem).chain(&source.imm) {
        children.push(Box::new(memtable.iter()));
    }
    for file in &source.version.levels[0] {
        children.push(Box::new(file.table()?.iter()));
    }
    for level in &source.version.levels[1..] {
        if !level.is_empty() {
            children.push(Box::new(LevelIter {
                files: level.clone(),
                at: 0,
                current: None,
            }));
        }
    }
    Ok(MergingIterator::new(children))
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
        let mut pins = inner.pins.lock();
        // Read under the registry lock so the sequence cannot drop below the oldest snapshot
        // that compaction is honouring.
        let sequence = inner.visible.load(std::sync::atomic::Ordering::Acquire);
        *pins.snapshots.entry(sequence).or_insert(0) += 1;
        Self {
            inner: Arc::clone(inner),
            sequence,
        }
    }

    /// A snapshot at a past `sequence`, if its state is still kept. Checked and registered under
    /// the registry lock: a compaction either counts it or kept every version it can see.
    pub(crate) fn at(inner: &Arc<Inner>, sequence: u64) -> Result<Self> {
        let mut pins = inner.pins.lock();
        let visible = inner.visible.load(std::sync::atomic::Ordering::Acquire);
        if sequence > visible {
            return Err(Error::InvalidArgument(format!(
                "sequence {sequence} is after the newest write, {visible}"
            )));
        }
        let oldest = pins.oldest(visible);
        if sequence < oldest {
            return Err(Error::HistoryUnavailable { sequence, oldest });
        }
        *pins.snapshots.entry(sequence).or_insert(0) += 1;
        Ok(Self {
            inner: Arc::clone(inner),
            sequence,
        })
    }

    /// The last sequence number this snapshot sees.
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        let mut pins = self.inner.pins.lock();
        if let Some(count) = pins.snapshots.get_mut(&self.sequence) {
            *count -= 1;
            if *count == 0 {
                pins.snapshots.remove(&self.sequence);
            }
        }
    }
}

/// Runs of one deeper level, read one after the other: only the run holding the cursor is open.
struct LevelIter {
    files: Vec<Arc<FileMeta>>,
    at: usize,
    current: Option<TableIter>,
}

impl LevelIter {
    /// Positions on run `at` (seeking `target` in it, if given), or on the first later run that
    /// has entries.
    fn open(&mut self, mut at: usize, mut target: Option<&[u8]>) -> Result<()> {
        self.current = None;
        while let Some(file) = self.files.get(at) {
            let mut iter = file.table()?.iter();
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
        let at = self
            .files
            .partition_point(|f| key::compare(&f.largest, target) == Ordering::Less);
        self.open(at, Some(target))
    }

    fn next(&mut self) -> Result<()> {
        let Some(current) = self.current.as_mut() else {
            return Ok(());
        };
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

    fn value_bytes(&self) -> Bytes {
        self.current
            .as_ref()
            .map_or_else(Bytes::new, InternalIterator::value_bytes)
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
        f.debug_struct("RangeIter")
            .field("sequence", &self.sequence)
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

impl RangeIter {
    pub(crate) fn new(
        inner: &Inner,
        sequence: u64,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<Self> {
        let source = inner.super_version();
        let mut merged = merged(&source)?;
        let mut resolved = None;
        match start {
            Bound::Unbounded => merged.seek_to_first()?,
            Bound::Included(k) => merged.seek(&key::lookup(k, sequence))?,
            Bound::Excluded(k) => {
                merged.seek(&key::lookup(k, sequence))?;
                resolved = Some(k.to_vec());
            }
        }
        Ok(Self {
            merged,
            sequence,
            end: end.map(<[u8]>::to_vec),
            resolved,
            done: false,
            _source: source,
        })
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
                value: self.merged.value_bytes(),
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
