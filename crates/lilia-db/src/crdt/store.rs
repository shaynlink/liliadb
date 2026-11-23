//! CRDT records and what each write changes.
//!
//! Every object (`space\0 id\0`) is a set of ledger records, so that concurrent writers of one
//! object rarely touch the same record:
//!
//! - a PN-counter has one record per actor (`actor`: its increment and decrement totals), so
//!   actors never contend;
//! - an OR-set has one record per entry (element escaped, tag): live (empty) or tombstoned (the
//!   removal time); an add writes a fresh tag, so it never contends;
//! - an LWW document has one record per field (`path`): its value, clock and deletion mark.

use std::collections::{BTreeMap, BTreeSet};

use bytes::Bytes;
use lilia_core::crdt::{LogicalClock, LwwClock, LwwField, OrSet, PNCounter};
use serde_json::Value;

use crate::error::{corruption, invalid, Result};
use crate::keys;
use crate::records::RecordSource;

const FIELD_FORMAT: u8 = 1;

/// Prefix of the records of object `id` of `space` in keyspace `tag`.
pub fn object_prefix(tag: u8, space: &str, id: &str) -> Vec<u8> {
    let mut key = keys::collection(tag, space);
    key.extend_from_slice(id.as_bytes());
    key.push(0);
    key
}

/// The end of the key range of the keys that start with `prefix`.
pub fn end_of(prefix: &[u8]) -> Vec<u8> {
    keys::prefix_end(prefix).unwrap_or_default()
}

pub fn counter_key(space: &str, id: &str, actor: &str) -> Bytes {
    let mut key = object_prefix(keys::CRDT_COUNTER, space, id);
    key.extend_from_slice(actor.as_bytes());
    key.into()
}

/// Prefix of the entries of `element` in set `id`.
pub fn element_prefix(space: &str, id: &str, element: &str) -> Vec<u8> {
    let mut key = object_prefix(keys::CRDT_SET, space, id);
    keys::push_escaped(&mut key, element.as_bytes());
    key
}

pub fn entry_key(space: &str, id: &str, element: &str, tag: &str) -> Bytes {
    let mut key = element_prefix(space, id, element);
    key.extend_from_slice(tag.as_bytes());
    key.into()
}

pub fn field_key(space: &str, id: &str, path: &str) -> Bytes {
    let mut key = object_prefix(keys::CRDT_FIELD, space, id);
    key.extend_from_slice(path.as_bytes());
    key.into()
}

/// Element and tag of a set entry, from its key after the set's prefix.
pub fn entry_parts(rest: &[u8]) -> Option<(String, String)> {
    let (element, tag) = keys::split_escaped(rest)?;
    Some((
        String::from_utf8(element).ok()?,
        String::from_utf8(tag.to_vec()).ok()?,
    ))
}

/// `path` as a JSON pointer: `/`, or `/`-separated non-empty keys (a leading `/` is added).
///
/// # Errors
///
/// [`crate::Error::InvalidArgument`] for an empty key (`//`, a trailing `/`).
pub fn normalize_path(path: &str) -> Result<String> {
    let path = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    if path != "/" && (path.ends_with('/') || path.contains("//")) {
        return Err(invalid(format!(
            "field path {path:?} has an empty key: a path is `/` or `/`-separated keys"
        )));
    }
    Ok(path)
}

pub fn encode_counter(increments: u64, decrements: u64) -> Bytes {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&increments.to_be_bytes());
    out.extend_from_slice(&decrements.to_be_bytes());
    out.into()
}

/// Increment and decrement totals of a counter record.
///
/// # Errors
///
/// [`crate::Error::Corruption`] for a record of another length.
pub fn decode_counter(raw: &[u8]) -> Result<(u64, u64)> {
    let total = |part: Option<&[u8]>| {
        part.and_then(|p| p.try_into().ok())
            .map(u64::from_be_bytes)
            .ok_or_else(|| corruption("unreadable CRDT counter record"))
    };
    if raw.len() != 16 {
        return Err(corruption("unreadable CRDT counter record"));
    }
    Ok((total(raw.get(..8))?, total(raw.get(8..))?))
}

/// A set entry: live (`None`) or tombstoned at a time.
pub fn encode_entry(tombstone: Option<i64>) -> Bytes {
    tombstone.map_or_else(Bytes::new, |at| Bytes::copy_from_slice(&at.to_be_bytes()))
}

/// # Errors
///
/// [`crate::Error::Corruption`] for a record that is neither empty nor a time.
pub fn decode_entry(raw: &[u8]) -> Result<Option<i64>> {
    if raw.is_empty() {
        return Ok(None);
    }
    raw.try_into()
        .map(|bytes| Some(i64::from_be_bytes(bytes)))
        .map_err(|_| corruption("unreadable CRDT set entry"))
}

/// # Errors
///
/// [`crate::Error::InvalidArgument`] for a value JSON cannot hold.
pub fn encode_field(field: &LwwField) -> Result<Bytes> {
    let mut out = vec![FIELD_FORMAT];
    serde_json::to_writer(&mut out, field).map_err(|e| invalid(format!("field value: {e}")))?;
    Ok(out.into())
}

/// # Errors
///
/// [`crate::Error::Corruption`] for a record that does not decode.
pub fn decode_field(raw: &[u8]) -> Result<LwwField> {
    match raw.split_first() {
        Some((&FIELD_FORMAT, json)) => serde_json::from_slice(json)
            .map_err(|e| corruption(format!("unreadable CRDT field: {e}"))),
        _ => Err(corruption("unreadable CRDT field")),
    }
}

/// The records a CRDT write changes, and whether it removed a live element or field.
#[derive(Debug, Default)]
pub struct Effect {
    pub changes: Vec<(Bytes, Option<Bytes>)>,
    pub deleted: bool,
}

/// Adds `up` to the increments of `actor` in counter `id`, and `down` to its decrements.
pub fn count(
    source: &mut impl RecordSource,
    (space, id): (&str, &str),
    actor: &str,
    up: u64,
    down: u64,
) -> Result<Effect> {
    let mut effect = Effect::default();
    if up == 0 && down == 0 {
        return Ok(effect);
    }
    let key = counter_key(space, id, actor);
    let (p, n) = source
        .read(&key)?
        .map(|raw| decode_counter(&raw))
        .transpose()?
        .unwrap_or_default();
    let value = encode_counter(p.saturating_add(up), n.saturating_add(down));
    effect.changes.push((key, Some(value)));
    Ok(effect)
}

/// Joins `remote` into counter `id`: each actor's totals become the larger of the two.
pub fn merge_counter(
    source: &mut impl RecordSource,
    (space, id): (&str, &str),
    remote: &PNCounter,
) -> Result<Effect> {
    let mut effect = Effect::default();
    let actors: BTreeSet<&String> = remote.p.keys().chain(remote.n.keys()).collect();
    for actor in actors {
        let key = counter_key(space, id, actor);
        let ours = source
            .read(&key)?
            .map(|raw| decode_counter(&raw))
            .transpose()?;
        let (p, n) = ours.unwrap_or_default();
        let merged = (
            p.max(remote.p.get(actor).copied().unwrap_or_default()),
            n.max(remote.n.get(actor).copied().unwrap_or_default()),
        );
        if ours != Some(merged) {
            effect
                .changes
                .push((key, Some(encode_counter(merged.0, merged.1))));
        }
    }
    Ok(effect)
}

/// Adds `element` to set `id` under `tag`; a tag already there (live or not) is kept as it is.
pub fn add(
    source: &mut impl RecordSource,
    (space, id): (&str, &str),
    element: &str,
    tag: &str,
) -> Result<Effect> {
    let key = entry_key(space, id, element, tag);
    let mut effect = Effect::default();
    if source.read(&key)?.is_none() {
        effect.changes.push((key, Some(encode_entry(None))));
    }
    Ok(effect)
}

/// Tombstones every entry of `element` seen in set `id` at `at` (an entry already tombstoned
/// keeps the later time). An entry added concurrently is not seen: the add wins.
pub fn remove(
    source: &mut impl RecordSource,
    (space, id): (&str, &str),
    element: &str,
    at: i64,
) -> Result<Effect> {
    let mut effect = Effect::default();
    let start = element_prefix(space, id, element);
    for (key, raw) in source.live(&start, &end_of(&start))? {
        let ours = decode_entry(&raw)?;
        effect.deleted |= ours.is_none();
        let merged = ours.max(Some(at));
        if ours != merged {
            effect.changes.push((key, Some(encode_entry(merged))));
        }
    }
    Ok(effect)
}

/// Joins `remote` into set `id`: the union of the entries, an entry tombstoned on either side
/// tombstoned at the later time. A remote tombstone whose entry `remote` no longer has applies
/// to the local entry of its tag.
pub fn merge_set(
    source: &mut impl RecordSource,
    (space, id): (&str, &str),
    remote: &OrSet<String>,
) -> Result<Effect> {
    let mut effect = Effect::default();
    let mut seen = BTreeSet::new();
    for (element, tags) in &remote.entries {
        for tag in tags {
            seen.insert(tag.as_str());
            let key = entry_key(space, id, element, tag);
            let ours = source
                .read(&key)?
                .map(|raw| decode_entry(&raw))
                .transpose()?;
            let theirs = remote.tombstones.get(tag).copied();
            let merged = ours.map_or(theirs, |ours| ours.max(theirs));
            if ours != Some(merged) {
                effect.changes.push((key, Some(encode_entry(merged))));
            }
        }
    }
    let orphans: BTreeMap<&str, i64> = remote
        .tombstones
        .iter()
        .filter(|(tag, _)| !seen.contains(tag.as_str()))
        .map(|(tag, &at)| (tag.as_str(), at))
        .collect();
    if orphans.is_empty() {
        return Ok(effect);
    }
    let start = object_prefix(keys::CRDT_SET, space, id);
    for (key, raw) in source.live(&start, &end_of(&start))? {
        let (_, tag) = entry_parts(keys::suffix(&key, &start))
            .ok_or_else(|| corruption("unreadable CRDT set entry key"))?;
        if let Some(&at) = orphans.get(tag.as_str()) {
            let ours = decode_entry(&raw)?;
            let merged = ours.max(Some(at));
            if ours != merged {
                effect.changes.push((key, Some(encode_entry(merged))));
            }
        }
    }
    Ok(effect)
}

/// Deletes the entries of set `id` tombstoned before `cutoff`.
pub fn prune(
    source: &mut impl RecordSource,
    (space, id): (&str, &str),
    cutoff: i64,
) -> Result<Effect> {
    let mut effect = Effect::default();
    let start = object_prefix(keys::CRDT_SET, space, id);
    for (key, raw) in source.live(&start, &end_of(&start))? {
        if decode_entry(&raw)?.is_some_and(|at| at < cutoff) {
            effect.changes.push((key, None));
            effect.deleted = true;
        }
    }
    Ok(effect)
}

/// The clock of a write by `actor` at `at` that replaces fields with `observed` clocks: later
/// than all of them, so a local write always takes effect, and at `at` when `at` is later.
fn causal<'f>(observed: impl Iterator<Item = &'f LwwClock>, actor: &str, at: i64) -> LwwClock {
    let base = LogicalClock::new(at, 0);
    let clock = match observed.map(|c| c.clock).max() {
        Some(latest) if latest >= base => latest.tick(at),
        _ => base,
    };
    LwwClock::new(clock, actor)
}

/// Sets field `path` (normalized) of document `id` to `value`.
pub fn set_field(
    source: &mut impl RecordSource,
    (space, id): (&str, &str),
    path: &str,
    value: &Value,
    (actor, at): (&str, i64),
) -> Result<Effect> {
    let key = field_key(space, id, path);
    let old = source
        .read(&key)?
        .map(|raw| decode_field(&raw))
        .transpose()?;
    let field = LwwField {
        value: value.clone(),
        clock: causal(old.iter().map(|f| &f.clock), actor, at),
        is_deleted: false,
    };
    let mut effect = Effect::default();
    effect.changes.push((key, Some(encode_field(&field)?)));
    Ok(effect)
}

/// Deletes field `path` (normalized) of document `id` and every field below it, under one
/// clock later than all of theirs. The field is marked deleted even when absent, so an older
/// remote value of it does not come back on a merge.
pub fn delete_field(
    source: &mut impl RecordSource,
    (space, id): (&str, &str),
    path: &str,
    (actor, at): (&str, i64),
) -> Result<Effect> {
    let key = field_key(space, id, path);
    let target = source
        .read(&key)?
        .map(|raw| decode_field(&raw))
        .transpose()?;
    let start = if path == "/" {
        object_prefix(keys::CRDT_FIELD, space, id)
    } else {
        let mut start = key.to_vec();
        start.push(b'/');
        start
    };
    let mut below = Vec::new();
    for (found, raw) in source.live(&start, &end_of(&start))? {
        if found != key {
            below.push((found, decode_field(&raw)?));
        }
    }
    let clock = causal(
        target
            .iter()
            .chain(below.iter().map(|(_, f)| f))
            .map(|f| &f.clock),
        actor,
        at,
    );
    let mut effect = Effect {
        deleted: target
            .iter()
            .chain(below.iter().map(|(_, f)| f))
            .any(|f| !f.is_deleted),
        ..Effect::default()
    };
    let tombstone = encode_field(&LwwField {
        value: Value::Null,
        clock,
        is_deleted: true,
    })?;
    effect.changes.push((key, Some(tombstone.clone())));
    for (found, _) in below {
        effect.changes.push((found, Some(tombstone.clone())));
    }
    Ok(effect)
}

/// Joins `remote` (fields by normalized path) into document `id`: each field takes the value
/// of the later clock.
pub fn merge_doc<'f>(
    source: &mut impl RecordSource,
    (space, id): (&str, &str),
    remote: impl Iterator<Item = (&'f String, &'f LwwField)>,
) -> Result<Effect> {
    let mut effect = Effect::default();
    for (path, field) in remote {
        let key = field_key(space, id, path);
        let ours = source
            .read(&key)?
            .map(|raw| decode_field(&raw))
            .transpose()?;
        if ours.is_none_or(|ours| field.clock > ours.clock) {
            effect.changes.push((key, Some(encode_field(field)?)));
        }
    }
    Ok(effect)
}

#[cfg(test)]
mod tests;
