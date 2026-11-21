//! Undo and redo of commits, read back from the history window ([`crate::history`]).
//!
//! With [`super::Options::undo_depth`], each commit also writes an undo entry: the keys it writes.
//! Undoing the newest commit restores those keys to their state just before it, as read from the
//! engine's history, in a commit of its own; that commit writes a redo entry, which restores the
//! state the undone commit had left. A new commit clears the redo entries, and a catalog change
//! (an index built or dropped, a schema changed) clears every entry: index entries written
//! outside commits would not match the documents an undo restores. An undo or redo fails with
//! [`Error::Conflict`] when a key no longer holds what its commit left, and with
//! [`Error::HistoryUnavailable`] once the state it restores has left the history window.
//!
//! Entries are not ledger records; the commits that undo or redo are ledger commits like any
//! other. Undo is global: it undoes the newest commit of the database, whoever wrote it.

use std::collections::BTreeSet;
use std::ops::Bound;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use bytes::Bytes;
use lilia_engine::Db;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::Database;
use crate::error::{corruption, invalid, Error, Result};
use crate::history;
use crate::keys;
use crate::record::now_ms;
use crate::write::{Delta, Expected, Plan};

const FORMAT: u8 = 1;
const UNDO: u8 = b'u';
const REDO: u8 = b'r';
/// Least time between two trims of the entries by writers.
const TRIM_INTERVAL_MS: i64 = 1_000;

/// A commit that can be undone or redone.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct UndoStep {
    /// The version of the commit's first write.
    pub version: u64,
    /// The keys it wrote: records and the index entries derived from them.
    pub keys: usize,
}

/// An undo or redo entry, as stored.
#[derive(Debug)]
struct Entry {
    key: Bytes,
    /// Versions written by the commit the entry belongs to, entry included.
    written: (u64, u64),
    /// The version whose state the entry restores.
    restore_at: u64,
    keys: Vec<Bytes>,
}

impl Entry {
    fn step(&self) -> UndoStep {
        UndoStep {
            version: self.written.0,
            keys: self.keys.len(),
        }
    }
}

/// The entry that restores `keys` to the state at `restore_at` (the version before the entry's
/// own when `None`); `after` writes of its commit follow it.
fn encode(after: usize, restore_at: Option<u64>, keys: &[Bytes]) -> Bytes {
    let mut out = vec![FORMAT];
    out.extend_from_slice(&(after as u64).to_be_bytes());
    out.extend_from_slice(&restore_at.unwrap_or(u64::MAX).to_be_bytes());
    for key in keys {
        out.extend_from_slice(&u32::try_from(key.len()).unwrap_or(u32::MAX).to_be_bytes());
        out.extend_from_slice(key);
    }
    out.into()
}

fn decode(key: Bytes, version: u64, raw: &[u8]) -> Result<Entry> {
    let unreadable = || corruption("unreadable undo entry");
    let number = |at: usize| -> Result<u64> {
        raw.get(at..at + 8)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_be_bytes)
            .ok_or_else(unreadable)
    };
    if raw.first() != Some(&FORMAT) {
        return Err(unreadable());
    }
    let after = number(1)?;
    let restore_at = match number(9)? {
        u64::MAX => version.saturating_sub(1),
        at => at,
    };
    let mut keys = Vec::new();
    let mut at = 17;
    while at < raw.len() {
        let len = raw
            .get(at..at + 4)
            .and_then(|b| b.try_into().ok())
            .map(u32::from_be_bytes)
            .ok_or_else(unreadable)? as usize;
        let bytes = raw.get(at + 4..at + 4 + len).ok_or_else(unreadable)?;
        keys.push(Bytes::copy_from_slice(bytes));
        at += 4 + len;
    }
    Ok(Entry {
        key,
        written: (version, version + after),
        restore_at,
        keys,
    })
}

/// A fresh key for an entry of `kind`.
fn entry_key(kind: u8) -> Bytes {
    let mut key = vec![keys::UNDO, kind];
    key.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    key.into()
}

/// The entries of `kind`, newest first.
fn entries(engine: &Db, kind: u8) -> Result<Vec<Entry>> {
    let prefix = [keys::UNDO, kind];
    let end = keys::prefix_end(&prefix).unwrap_or_default();
    let mut found = Vec::new();
    for item in engine.range(Bound::Included(&prefix[..]), Bound::Excluded(&end))? {
        let item = item?;
        found.push(decode(item.key, item.version, &item.value)?);
    }
    found.sort_by_key(|e| std::cmp::Reverse(e.written.0));
    Ok(found)
}

/// The undo and redo entries of a database.
#[derive(Debug)]
pub(crate) struct Undo {
    /// Undo entries kept; 0 when commits are not undoable.
    depth: usize,
    /// Whether redo entries may exist, which the next commit clears.
    redo_pending: AtomicBool,
    last_trim: AtomicI64,
    /// Serializes undos and redos.
    running: Mutex<()>,
}

impl Undo {
    /// Undo of `depth` commits, over a database that keeps `history` or not.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgument`] for undo without a history window; engine errors.
    pub fn open(engine: &Db, depth: usize, history: bool) -> Result<Self> {
        if depth > 0 && !history {
            return Err(invalid(
                "undo reads the history: Options::undo_depth needs a history_retention",
            ));
        }
        Ok(Self {
            depth,
            redo_pending: AtomicBool::new(!entries(engine, REDO)?.is_empty()),
            last_trim: AtomicI64::new(i64::MIN),
            running: Mutex::new(()),
        })
    }

    /// Puts first in `plan` its undo entry, and the deletion of the redo entries if there may
    /// be some; returns whether it clears them.
    ///
    /// # Errors
    ///
    /// Engine errors.
    pub fn record(&self, engine: &Db, plan: &mut Plan) -> Result<bool> {
        if self.depth == 0 || plan.ops.is_empty() {
            return Ok(false);
        }
        let clears = self.redo_pending.load(Ordering::Acquire);
        if clears {
            for entry in entries(engine, REDO)? {
                plan.ops.push((entry.key, None));
            }
        }
        let written: BTreeSet<Bytes> = plan
            .ops
            .iter()
            .map(|(key, _)| key.clone())
            .filter(|key| !matches!(key.first(), Some(&keys::UNDO | &keys::CLOCK)))
            .collect();
        let keys: Vec<Bytes> = written.into_iter().collect();
        let entry = encode(plan.ops.len(), None, &keys);
        plan.prepend(entry_key(UNDO), Some(entry));
        Ok(clears)
    }

    /// Adds to `batch`, a catalog change, the deletion of every entry: the entries go with the
    /// change or not at all, whatever a crash interrupts. [`Undo::cleared`] once it commits.
    ///
    /// # Errors
    ///
    /// Engine errors.
    pub fn clear(&self, engine: &Db, batch: &mut lilia_engine::WriteBatch) -> Result<()> {
        if self.depth == 0 {
            return Ok(());
        }
        for kind in [UNDO, REDO] {
            for entry in entries(engine, kind)? {
                batch.delete(entry.key);
            }
        }
        Ok(())
    }

    /// Records that a catalog change cleared the entries.
    pub fn cleared(&self) {
        self.redo_pending.store(false, Ordering::Release);
    }

    /// Records that a commit that cleared the redo entries committed.
    pub fn redo_cleared(&self) {
        self.redo_pending.store(false, Ordering::Release);
    }

    /// Deletes the entries beyond the depth or whose state left the history window, at most
    /// once a second.
    ///
    /// # Errors
    ///
    /// Engine errors.
    pub fn maybe_trim(&self, engine: &Db, now: i64) -> Result<()> {
        if self.depth == 0
            || now.saturating_sub(self.last_trim.load(Ordering::Acquire)) < TRIM_INTERVAL_MS
        {
            return Ok(());
        }
        self.last_trim.store(now, Ordering::Release);
        let oldest = engine.oldest_sequence();
        let mut batch = lilia_engine::WriteBatch::new();
        for kind in [UNDO, REDO] {
            for (i, entry) in entries(engine, kind)?.into_iter().enumerate() {
                if i >= self.depth || entry.restore_at < oldest {
                    batch.delete(entry.key);
                }
            }
        }
        if !batch.is_empty() {
            engine.write(batch)?;
        }
        Ok(())
    }
}

/// The plan that restores the keys of `entry` and replaces it with an entry of `next` kind that
/// restores the state `entry` undoes (`next_restore_at`, or the state before the plan).
fn restore_plan(
    engine: &Db,
    entry: &Entry,
    next: u8,
    next_restore_at: Option<u64>,
) -> Result<Plan> {
    let past = engine
        .snapshot_at(entry.restore_at)
        .map_err(history::unavailable)?;
    let left = engine
        .snapshot_at(entry.written.1)
        .map_err(history::unavailable)?;
    let mut plan = Plan::default();
    let mut last_record = None;
    for key in &entry.keys {
        let latest = engine
            .history(key)?
            .into_iter()
            .next()
            .ok_or_else(|| corruption("an undo entry names a key never written"))?;
        let left_by_commit = engine.get_at(&left, key)?;
        if latest.value != left_by_commit.as_ref().map(|v| v.value.clone()) {
            return Err(Error::Conflict {
                target: keys::describe(key),
                expected: left_by_commit.map(|v| v.version),
                actual: latest.value.is_some().then_some(latest.version),
            });
        }
        let value = engine.get_at(&past, key)?.map(|v| v.value);
        plan.expected.insert(
            key.clone(),
            Expected {
                version: latest.value.is_some().then_some(latest.version),
                strict: true,
            },
        );
        let op = plan.ops.len();
        if let Some((model, space, id)) = crate::ledger::record_identity(key) {
            plan.deltas.push(Delta {
                model,
                space,
                id,
                op,
                payload: value.as_deref().map(|v| *blake3::hash(v).as_bytes()),
            });
            last_record = Some(op);
        }
        plan.ops.push((key.clone(), value));
    }
    plan.ops.push((entry.key.clone(), None));
    plan.results.push((last_record, false));
    let next_entry = encode(plan.ops.len(), next_restore_at, &entry.keys);
    plan.prepend(entry_key(next), Some(next_entry));
    Ok(plan)
}

/// The collections whose in-memory full-text vocabulary or vector index `keys` touch.
fn cached_collections(keys: &[Bytes]) -> BTreeSet<(u8, String)> {
    keys.iter()
        .filter_map(|key| {
            let (&tag, rest) = key.split_first()?;
            let cached = matches!(
                tag,
                keys::FTS_DOC
                    | keys::FTS_POSTING
                    | keys::FTS_STATS
                    | keys::FTS_DICTIONARY
                    | keys::VECTOR
            );
            let split = rest.iter().position(|&b| b == 0)?;
            let name = std::str::from_utf8(&rest[..split]).ok()?;
            cached.then(|| (tag, name.to_owned()))
        })
        .collect()
}

impl Database {
    /// Undoes the newest commit not undone yet; `None` when there is none.
    ///
    /// # Errors
    ///
    /// [`Error::Conflict`] when a later commit changed a key of it, [`Error::HistoryUnavailable`]
    /// when its previous state left the history window; engine errors.
    pub fn undo(&self) -> Result<Option<UndoStep>> {
        self.step(UNDO, REDO)
    }

    /// Redoes the newest commit undone since the last other commit; `None` when there is none.
    ///
    /// # Errors
    ///
    /// As [`Database::undo`].
    pub fn redo(&self) -> Result<Option<UndoStep>> {
        self.step(REDO, UNDO)
    }

    /// The commits [`Database::undo`] can undo, newest first.
    ///
    /// # Errors
    ///
    /// Engine errors, entries that do not decode.
    pub fn undo_history(&self) -> Result<Vec<UndoStep>> {
        Ok(entries(&self.engine, UNDO)?
            .iter()
            .map(Entry::step)
            .collect())
    }

    fn step(&self, kind: u8, next: u8) -> Result<Option<UndoStep>> {
        let _running = self.undo.running.lock();
        let Some(entry) = entries(&self.engine, kind)?.into_iter().next() else {
            return Ok(None);
        };
        // An undo's redo entry restores the state the undone commit left; a redo's undo entry,
        // the state before the redo.
        let next_restore_at = (kind == UNDO).then_some(entry.written.1);
        self.commit_plan(
            |_| restore_plan(&self.engine, &entry, next, next_restore_at),
            false,
        )?;
        if next == REDO {
            self.undo.redo_pending.store(true, Ordering::Release);
        }
        for (tag, collection) in cached_collections(&entry.keys) {
            if tag == keys::VECTOR {
                self.vector_indexes.evict(&collection);
            } else {
                self.vocabularies.evict(&collection);
            }
        }
        self.undo.maybe_trim(&self.engine, now_ms())?;
        Ok(Some(entry.step()))
    }
}
