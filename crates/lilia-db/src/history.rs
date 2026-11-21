//! History: the versions kept for a window of time, reads in the past, and the revisions of a
//! record.
//!
//! With a [`crate::Options::history_retention`], the engine keeps every version written within
//! the window ([`lilia_engine::Options::retain_history`]), so a session can read the database as
//! it was at a version or a time ([`crate::Session::at_version`], [`crate::Session::at_time`])
//! and list the versions of a record ([`crate::Session::json_history`]).
//!
//! Times map to versions through time marks: a commit planned in a millisecond later than the
//! last mark writes one (`CLOCK` keyspace) as its first write, so the mark's version is the
//! commit's first. The state at a time is the state before the first mark after it, so it is
//! exact to the millisecond of planning:
//! commits planned within the same millisecond as the time asked may fall on either side.
//!
//! At most once a second, the next writer moves the history horizon to the start of the window
//! and deletes the marks before it; [`crate::Database::trim_history`] does it on demand.

use std::ops::Bound;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use lilia_core::{JsonEntry, KvEntry};
use lilia_engine::{Db, WriteBatch};
use parking_lot::Mutex;

use crate::db::MAINTENANCE_CHUNK;
use crate::error::{Error, Result};
use crate::guard::Guard;
use crate::keys;
use crate::record::{decode_doc, decode_kv};
use crate::write::Plan;

/// Least time between two moves of the horizon by writers.
const TRIM_INTERVAL_MS: i64 = 1_000;

/// One version of a document: `doc` is `None` where the document was deleted.
#[derive(Debug, Clone)]
pub struct JsonRevision {
    pub version: u64,
    pub doc: Option<JsonEntry>,
}

/// One version of a key-value record: `entry` is `None` where it was deleted. An entry keeps its
/// expiry, whether or not it has passed.
#[derive(Debug, Clone)]
pub struct KvRevision {
    pub version: u64,
    pub entry: Option<KvEntry>,
}

/// The history window of a database.
#[derive(Debug)]
pub(crate) struct History {
    /// Length of the window; 0 when the database keeps no history.
    retention_ms: i64,
    /// Time of the newest mark committed.
    last_mark: AtomicI64,
    /// When the horizon last moved.
    last_trim: AtomicI64,
    /// Start of the window at the last move: reads at earlier times are refused.
    start: AtomicI64,
    trimming: Mutex<()>,
}

impl History {
    pub fn new(retention: Duration) -> Self {
        Self {
            retention_ms: i64::try_from(retention.as_millis()).unwrap_or(i64::MAX),
            last_mark: AtomicI64::new(i64::MIN),
            last_trim: AtomicI64::new(i64::MIN),
            start: AtomicI64::new(i64::MIN),
            trimming: Mutex::new(()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.retention_ms > 0
    }

    /// Puts first in `plan`, planned at `now`, the time mark of its millisecond if it is the
    /// first.
    pub fn mark(&self, plan: &mut Plan, now: i64) {
        if self.enabled() && !plan.ops.is_empty() && now > self.last_mark.load(Ordering::Acquire) {
            plan.prepend(keys::clock(now), Some(Bytes::new()));
        }
    }

    /// Records that a plan planned at `now` committed.
    pub fn committed(&self, now: i64) {
        self.last_mark.fetch_max(now, Ordering::AcqRel);
    }

    /// Moves the horizon when the last move is older than [`TRIM_INTERVAL_MS`] and no other
    /// writer is moving it.
    ///
    /// # Errors
    ///
    /// As [`History::trim`].
    pub fn maybe_trim(&self, engine: &Db, now: i64) -> Result<()> {
        if !self.enabled()
            || now.saturating_sub(self.last_trim.load(Ordering::Acquire)) < TRIM_INTERVAL_MS
        {
            return Ok(());
        }
        let Some(_trimming) = self.trimming.try_lock() else {
            return Ok(());
        };
        self.trim_locked(engine, now)
    }

    /// Moves the horizon to the start of the window ending at `now`, and deletes the time marks
    /// before it.
    ///
    /// # Errors
    ///
    /// Engine errors.
    pub fn trim(&self, engine: &Db, now: i64) -> Result<()> {
        if !self.enabled() {
            return Ok(());
        }
        let _trimming = self.trimming.lock();
        self.trim_locked(engine, now)
    }

    fn trim_locked(&self, engine: &Db, now: i64) -> Result<()> {
        let start = now.saturating_sub(self.retention_ms);
        let horizon = match first_mark_from(engine, start)? {
            Some(version) => version.saturating_sub(1),
            None => engine.last_sequence(),
        };
        engine.set_history_horizon(horizon)?;
        self.start.fetch_max(start, Ordering::AcqRel);
        self.last_trim.store(now, Ordering::Release);
        let first = [keys::CLOCK];
        let end = keys::clock(start);
        loop {
            let mut batch = WriteBatch::new();
            for entry in engine
                .range(Bound::Included(&first[..]), Bound::Excluded(&end))?
                .take(MAINTENANCE_CHUNK)
            {
                batch.delete(entry?.key);
            }
            if batch.is_empty() {
                return Ok(());
            }
            engine.write(batch)?;
        }
    }

    /// The version current at `time_ms`: the newest commit planned at or before it.
    ///
    /// # Errors
    ///
    /// [`Error::HistoryUnavailable`] without history or for a time before the window; engine
    /// errors.
    pub fn version_at(&self, engine: &Db, time_ms: i64) -> Result<u64> {
        if !self.enabled() {
            return Err(Error::HistoryUnavailable(
                "the database keeps no history (Options::history_retention)".into(),
            ));
        }
        let start = self.start.load(Ordering::Acquire);
        if time_ms < start {
            return Err(Error::HistoryUnavailable(format!(
                "the history kept starts at {start} ms; asked for {time_ms} ms"
            )));
        }
        Ok(match first_mark_from(engine, time_ms.saturating_add(1))? {
            Some(version) => version.saturating_sub(1),
            None => engine.last_sequence(),
        })
    }
}

/// Version of the first time mark at or after `time_ms`.
fn first_mark_from(engine: &Db, time_ms: i64) -> Result<Option<u64>> {
    let start = keys::clock(time_ms);
    let end = [keys::CLOCK + 1];
    match engine
        .range(Bound::Included(&start), Bound::Excluded(&end[..]))?
        .next()
    {
        Some(entry) => Ok(Some(entry?.version)),
        None => Ok(None),
    }
}

/// The versions of the record at `key` kept, up to version `upto`, oldest first; a deletion
/// before the first version kept is left out.
fn changes(engine: &Db, key: &[u8], upto: u64) -> Result<Vec<lilia_engine::Change>> {
    let mut changes: Vec<_> = engine
        .history(key)?
        .into_iter()
        .filter(|c| c.version <= upto)
        .collect();
    changes.reverse();
    let first = changes
        .iter()
        .position(|c| c.value.is_some())
        .unwrap_or(changes.len());
    Ok(changes.split_off(first))
}

/// The revisions of document `id` the caller may read, masked; a deletion is listed after a
/// revision the caller may read.
///
/// # Errors
///
/// Records that do not decode, engine errors.
pub(crate) fn json_revisions(
    engine: &Db,
    guard: &Guard<'_>,
    (space, id): (&str, &str),
    upto: u64,
) -> Result<Vec<JsonRevision>> {
    let mut revisions: Vec<JsonRevision> = Vec::new();
    for change in changes(engine, &keys::doc(space, id), upto)? {
        let doc = match change.value {
            Some(raw) => {
                let entry = JsonEntry::new(space, id, decode_doc(&raw)?, change.version);
                match guard.doc(entry)? {
                    Some(doc) => Some(doc),
                    None => continue,
                }
            }
            None if revisions.last().is_some_and(|r| r.doc.is_some()) => None,
            None => continue,
        };
        revisions.push(JsonRevision {
            version: change.version,
            doc,
        });
    }
    Ok(revisions)
}

/// The revisions of `key` of `namespace` the caller may read, as [`json_revisions`].
///
/// # Errors
///
/// As [`json_revisions`].
pub(crate) fn kv_revisions(
    engine: &Db,
    guard: &Guard<'_>,
    (namespace, key): (&str, &[u8]),
    upto: u64,
) -> Result<Vec<KvRevision>> {
    let mut revisions: Vec<KvRevision> = Vec::new();
    for change in changes(engine, &keys::kv(namespace, key), upto)? {
        let entry = match change.value {
            Some(raw) => {
                let record = decode_kv(&raw)?;
                let entry = KvEntry {
                    namespace: namespace.to_owned(),
                    key: key.to_vec(),
                    value: record.value.to_vec(),
                    version: change.version,
                    expires_at_ms: record.expires_at_ms,
                };
                match guard.kv(entry) {
                    Some(entry) => Some(entry),
                    None => continue,
                }
            }
            None if revisions.last().is_some_and(|r| r.entry.is_some()) => None,
            None => continue,
        };
        revisions.push(KvRevision {
            version: change.version,
            entry,
        });
    }
    Ok(revisions)
}

/// The caller's error for an engine refusal to read at `version`.
pub(crate) fn unavailable(error: lilia_engine::Error) -> Error {
    match error {
        lilia_engine::Error::HistoryUnavailable { sequence, oldest } => Error::HistoryUnavailable(
            format!("version {sequence} is older than the oldest kept, {oldest}"),
        ),
        lilia_engine::Error::InvalidArgument(reason) => Error::InvalidArgument(reason),
        other => Error::Engine(other),
    }
}
