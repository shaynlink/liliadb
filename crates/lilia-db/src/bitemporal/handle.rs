//! The bitemporal handle of a session or transaction.

use std::ops::Bound;

use lilia_core::{BitemporalEntry, BitemporalInterval, BitemporalPoint, BITEMPORAL_INFINITY};
use serde_json::{json, Value};

use super::store::{decode, entity_prefix, key_times};
use super::BitemporalWrite;
use crate::batch::{Outcome, Write};
use crate::context::{guard, Context, Reader};
use crate::error::{corruption, Result};
use crate::keys::{self, validate_name};
use crate::read::{Scan, Source};

/// Bitemporal space `name` of a session or transaction: entities by id, each a JSON value over
/// valid-time intervals, recorded at transaction times. Times are milliseconds since the epoch;
/// an interval is half-open, `[from, to)`.
///
/// Reads answer what was known at a transaction time (`known_at`, now by default) about the
/// fact at a valid time (`valid_at`). An entity is readable when the read policies of the space
/// admit the document `{"space", "id"}`.
#[derive(Debug)]
pub struct Bitemporal<'c, C: Context> {
    context: &'c C,
    name: String,
}

impl<'c, C: Context> Bitemporal<'c, C> {
    pub(crate) fn new(context: &'c C, name: &str) -> Self {
        Self {
            context,
            name: name.to_owned(),
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    fn submit(&self, write: BitemporalWrite) -> Result<Outcome> {
        let mut outcomes = self.context.submit(vec![Write::Bitemporal(write)])?;
        outcomes
            .pop()
            .ok_or_else(|| corruption("a bitemporal write produced no outcome"))
    }

    /// Records that entity `id` is `value` over `valid`, now. What it overlaps is superseded,
    /// cut around `valid`; adjacent intervals of an equal value are joined.
    ///
    /// # Errors
    ///
    /// Invalid names, an empty interval; as [`crate::Session::commit`].
    pub fn put(&self, id: &str, value: &Value, valid: BitemporalInterval) -> Result<Outcome> {
        self.write_put(id, value, valid, None)
    }

    /// As [`Bitemporal::put`], recorded at transaction time `recorded_at` (to load an earlier
    /// record of facts), which must not be before the entity's last change.
    ///
    /// # Errors
    ///
    /// A transaction time before the entity's last change; as [`Bitemporal::put`].
    pub fn put_recorded_at(
        &self,
        id: &str,
        value: &Value,
        valid: BitemporalInterval,
        recorded_at: i64,
    ) -> Result<Outcome> {
        self.write_put(id, value, valid, Some(recorded_at))
    }

    fn write_put(
        &self,
        id: &str,
        value: &Value,
        valid: BitemporalInterval,
        recorded_at: Option<i64>,
    ) -> Result<Outcome> {
        self.submit(BitemporalWrite::Put {
            space: self.name.clone(),
            id: id.to_owned(),
            value: value.clone(),
            valid,
            recorded_at,
        })
    }

    /// Retracts what is known of entity `id` over `valid`, or over all valid time, now; `true`
    /// if something was known there.
    ///
    /// # Errors
    ///
    /// As [`Bitemporal::put`].
    pub fn delete(&self, id: &str, valid: Option<BitemporalInterval>) -> Result<bool> {
        self.write_delete(id, valid, None)
    }

    /// As [`Bitemporal::delete`], recorded at transaction time `recorded_at`.
    ///
    /// # Errors
    ///
    /// As [`Bitemporal::put_recorded_at`].
    pub fn delete_recorded_at(
        &self,
        id: &str,
        valid: Option<BitemporalInterval>,
        recorded_at: i64,
    ) -> Result<bool> {
        self.write_delete(id, valid, Some(recorded_at))
    }

    fn write_delete(
        &self,
        id: &str,
        valid: Option<BitemporalInterval>,
        recorded_at: Option<i64>,
    ) -> Result<bool> {
        Ok(self
            .submit(BitemporalWrite::Delete {
                space: self.name.clone(),
                id: id.to_owned(),
                valid,
                recorded_at,
            })?
            .deleted)
    }

    /// Entity `id` at valid time `valid_at`, as known now.
    ///
    /// # Errors
    ///
    /// Invalid names, records that do not decode, engine errors.
    pub fn get(
        &self,
        id: &str,
        valid_at: impl Into<BitemporalPoint>,
    ) -> Result<Option<BitemporalEntry>> {
        let valid_at = valid_at.into().as_ms();
        let reader = self.context.reader(true);
        Ok(at_valid(self.timeline_in(&reader, id, None)?, valid_at))
    }

    /// Entity `id` at valid time `valid_at`, as known at transaction time `known_at`.
    ///
    /// # Errors
    ///
    /// As [`Bitemporal::get`].
    pub fn get_as_of(
        &self,
        id: &str,
        known_at: impl Into<BitemporalPoint>,
        valid_at: impl Into<BitemporalPoint>,
    ) -> Result<Option<BitemporalEntry>> {
        let valid_at = valid_at.into().as_ms();
        let reader = self.context.reader(true);
        let known_at = Some(known_at.into().as_ms());
        Ok(at_valid(self.timeline_in(&reader, id, known_at)?, valid_at))
    }

    /// Every interval of entity `id` as known now, by valid time.
    ///
    /// # Errors
    ///
    /// As [`Bitemporal::get`].
    pub fn timeline(&self, id: &str) -> Result<Vec<BitemporalEntry>> {
        self.timeline_in(&self.context.reader(true), id, None)
    }

    /// Every interval of entity `id` as known at transaction time `known_at`, by valid time.
    ///
    /// # Errors
    ///
    /// As [`Bitemporal::get`].
    pub fn timeline_as_of(
        &self,
        id: &str,
        known_at: impl Into<BitemporalPoint>,
    ) -> Result<Vec<BitemporalEntry>> {
        let known_at = Some(known_at.into().as_ms());
        self.timeline_in(&self.context.reader(true), id, known_at)
    }

    /// Everything ever recorded about entity `id`, superseded or not, by transaction time then
    /// valid time.
    ///
    /// # Errors
    ///
    /// As [`Bitemporal::get`].
    pub fn history(&self, id: &str) -> Result<Vec<BitemporalEntry>> {
        let reader = self.context.reader(true);
        if !self.readable(id)? {
            return Ok(Vec::new());
        }
        let mut entries = self.slices(&reader, keys::BT_CURRENT, id, &[])?;
        entries.extend(self.slices(&reader, keys::BT_PAST, id, &[])?);
        entries.sort_by_key(|e| (e.tt_from, e.vt_from, e.tt_to));
        Ok(entries)
    }

    /// The entities with a value at valid time `valid_at`, as known now, in id order: `scan`
    /// bounds their ids and counts the entities returned.
    ///
    /// # Errors
    ///
    /// As [`Bitemporal::get`].
    pub fn scan(
        &self,
        valid_at: impl Into<BitemporalPoint>,
        scan: &Scan,
    ) -> Result<Vec<BitemporalEntry>> {
        self.scan_known(None, valid_at.into().as_ms(), scan)
    }

    /// As [`Bitemporal::scan`], as known at transaction time `known_at`.
    ///
    /// # Errors
    ///
    /// As [`Bitemporal::get`].
    pub fn scan_as_of(
        &self,
        known_at: impl Into<BitemporalPoint>,
        valid_at: impl Into<BitemporalPoint>,
        scan: &Scan,
    ) -> Result<Vec<BitemporalEntry>> {
        self.scan_known(Some(known_at.into().as_ms()), valid_at.into().as_ms(), scan)
    }

    fn scan_known(
        &self,
        known_at: Option<i64>,
        valid_at: i64,
        scan: &Scan,
    ) -> Result<Vec<BitemporalEntry>> {
        validate_name("space", &self.name)?;
        let reader = self.context.reader(true);
        let tags: &[u8] = match known_at {
            None => &[keys::BT_CURRENT],
            Some(_) => &[keys::BT_CURRENT, keys::BT_PAST],
        };
        let mut found = Vec::new();
        let mut after = scan.after.clone();
        while scan.limit.is_none_or(|limit| found.len() < limit) {
            let mut next: Option<String> = None;
            for &tag in tags {
                if let Some(id) = next_id(&reader, tag, &self.name, scan, after.as_deref())? {
                    next = Some(next.map_or(id.clone(), |n: String| n.min(id)));
                }
            }
            let Some(id) = next else { break };
            found.extend(at_valid(
                self.timeline_in(&reader, &id, known_at)?,
                valid_at,
            ));
            after = Some(id.into_bytes());
        }
        Ok(found)
    }

    /// The intervals of entity `id` known at `known_at` (now when `None`), by valid time.
    fn timeline_in(
        &self,
        reader: &Reader<'_>,
        id: &str,
        known_at: Option<i64>,
    ) -> Result<Vec<BitemporalEntry>> {
        if !self.readable(id)? {
            return Ok(Vec::new());
        }
        let mut entries = self.slices(reader, keys::BT_CURRENT, id, &[])?;
        if let Some(tt) = known_at {
            entries.retain(|e| e.tt_from <= tt);
            // Past slices still known at `tt`: superseded after it.
            let after = keys::ordered(tt.saturating_add(1));
            if tt < i64::MAX {
                let past = self.slices(reader, keys::BT_PAST, id, &after)?;
                entries.extend(past.into_iter().filter(|e| e.tt_from <= tt));
            }
        }
        entries.sort_by_key(|e| e.vt_from);
        Ok(entries)
    }

    fn readable(&self, id: &str) -> Result<bool> {
        validate_name("space", &self.name)?;
        validate_name("entity id", id)?;
        Ok(guard(self.context, &self.name)
            .related_hit(None, || json!({ "space": self.name, "id": id })))
    }

    /// The slices of entity `id` in keyspace `tag` from the key continuing with `from`.
    fn slices(
        &self,
        reader: &Reader<'_>,
        tag: u8,
        id: &str,
        from: &[u8],
    ) -> Result<Vec<BitemporalEntry>> {
        let prefix = entity_prefix(tag, &self.name, id);
        let mut start = prefix.clone();
        start.extend_from_slice(from);
        let end = keys::prefix_end(&prefix).unwrap_or_default();
        let mut entries = Vec::new();
        for item in reader.range(Bound::Included(&start), Bound::Excluded(&end))? {
            let item = item?;
            reader.observe(&item);
            let (vt_from, tt_to) = key_times(keys::suffix(&item.key, &prefix))
                .ok_or_else(|| corruption("unreadable bitemporal slice key"))?;
            let slice = decode(vt_from, &item.value)?;
            entries.push(BitemporalEntry {
                space: self.name.clone(),
                id: id.to_owned(),
                vt_from: slice.vt_from,
                vt_to: slice.vt_to,
                tt_from: slice.tt_from,
                tt_to: tt_to.unwrap_or(BITEMPORAL_INFINITY),
                value: slice.value,
                version: item.version,
            });
        }
        Ok(entries)
    }
}

/// The interval of `timeline` that holds at valid time `valid_at`.
fn at_valid(timeline: Vec<BitemporalEntry>, valid_at: i64) -> Option<BitemporalEntry> {
    timeline
        .into_iter()
        .find(|e| e.vt_from <= valid_at && valid_at < e.vt_to)
}

/// The first entity id of keyspace `tag` of `space` that `scan` admits after `after`.
fn next_id(
    reader: &Reader<'_>,
    tag: u8,
    space: &str,
    scan: &Scan,
    after: Option<&[u8]>,
) -> Result<Option<String>> {
    let base = keys::collection(tag, space);
    let mut first = base.clone();
    first.extend_from_slice(&scan.prefix);
    let end = keys::prefix_end(&first).unwrap_or_default();
    if let Some(after) = after {
        // Every key of entity `after` continues with 0x00; ids hold no control character.
        let mut resume = base.clone();
        resume.extend_from_slice(after);
        resume.push(1);
        first = first.max(resume);
    }
    let Some(item) = reader
        .range(Bound::Included(&first), Bound::Excluded(&end))?
        .next()
        .transpose()?
    else {
        return Ok(None);
    };
    let rest = keys::suffix(&item.key, &base);
    let split = rest
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| corruption("bitemporal key without an entity id"))?;
    String::from_utf8(rest[..split].to_vec())
        .map(Some)
        .map_err(|_| corruption("bitemporal entity id is not UTF-8"))
}
