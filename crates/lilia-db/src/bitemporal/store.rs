//! Bitemporal records and what each write changes.
//!
//! An entity (`space\0 id\0`) is a set of slices, each a value over a valid-time interval
//! `[vt_from, vt_to)` recorded at a transaction time `tt_from`:
//!
//! - the current slices (`vt_from`): what the database knows now, never overlapping in valid
//!   time;
//! - the past slices (`tt_to`, `vt_from`): slices superseded at transaction time `tt_to`, kept
//!   for good, so the entity can be read as known at any earlier time;
//! - the entity's state: the transaction time of its last change, which later writes never go
//!   before.
//!
//! Every slice is a ledger record; the state is bookkeeping, not a record.

use std::collections::BTreeMap;

use bytes::Bytes;
use serde_json::Value;

use crate::error::{corruption, invalid, Result};
use crate::keys;
use crate::records::RecordSource;

const FORMAT: u8 = 1;

/// A value over a valid-time interval, recorded at a transaction time.
#[derive(Debug, Clone, PartialEq)]
pub struct Slice {
    pub vt_from: i64,
    pub vt_to: i64,
    pub tt_from: i64,
    pub value: Value,
}

/// Prefix of the records of entity `id` of `space` in keyspace `tag`.
pub fn entity_prefix(tag: u8, space: &str, id: &str) -> Vec<u8> {
    let mut key = keys::collection(tag, space);
    key.extend_from_slice(id.as_bytes());
    key.push(0);
    key
}

pub fn current_key(space: &str, id: &str, vt_from: i64) -> Bytes {
    let mut key = entity_prefix(keys::BT_CURRENT, space, id);
    key.extend_from_slice(&keys::ordered(vt_from));
    key.into()
}

pub fn state_key(space: &str, id: &str) -> Bytes {
    let mut key = keys::collection(keys::BT_STATE, space);
    key.extend_from_slice(id.as_bytes());
    key.into()
}

pub fn past_key(space: &str, id: &str, tt_to: i64, vt_from: i64) -> Bytes {
    let mut key = entity_prefix(keys::BT_PAST, space, id);
    key.extend_from_slice(&keys::ordered(tt_to));
    key.extend_from_slice(&keys::ordered(vt_from));
    key.into()
}

/// The times of a key after its entity prefix: `vt_from` for a current slice, `tt_to` and
/// `vt_from` for a past one.
pub fn key_times(rest: &[u8]) -> Option<(i64, Option<i64>)> {
    match rest.len() {
        8 => Some((keys::from_ordered(rest)?, None)),
        16 => Some((
            keys::from_ordered(&rest[8..])?,
            Some(keys::from_ordered(&rest[..8])?),
        )),
        _ => None,
    }
}

/// # Errors
///
/// [`crate::Error::InvalidArgument`] for a value JSON cannot hold.
pub fn encode(slice: &Slice) -> Result<Bytes> {
    let mut out = vec![FORMAT];
    out.extend_from_slice(&slice.vt_to.to_be_bytes());
    out.extend_from_slice(&slice.tt_from.to_be_bytes());
    serde_json::to_writer(&mut out, &slice.value)
        .map_err(|e| invalid(format!("bitemporal value: {e}")))?;
    Ok(out.into())
}

/// The slice starting at `vt_from` stored as `raw`.
///
/// # Errors
///
/// [`crate::Error::Corruption`] for a record that does not decode.
pub fn decode(vt_from: i64, raw: &[u8]) -> Result<Slice> {
    let unreadable = || corruption("unreadable bitemporal slice");
    let (&format, rest) = raw.split_first().ok_or_else(unreadable)?;
    if format != FORMAT || rest.len() < 16 {
        return Err(unreadable());
    }
    let time = |at: usize| -> Result<i64> {
        rest.get(at..at + 8)
            .and_then(|b| b.try_into().ok())
            .map(i64::from_be_bytes)
            .ok_or_else(unreadable)
    };
    Ok(Slice {
        vt_from,
        vt_to: time(0)?,
        tt_from: time(8)?,
        value: serde_json::from_slice(&rest[16..])
            .map_err(|e| corruption(format!("unreadable bitemporal value: {e}")))?,
    })
}

/// The records a bitemporal write changes, the entity state it leaves, and whether it removed
/// knowledge.
#[derive(Debug, Default)]
pub struct Effect {
    pub changes: Vec<(Bytes, Option<Bytes>)>,
    pub state: Option<(Bytes, Bytes)>,
    pub deleted: bool,
}

/// When a write is recorded: an explicit transaction time, or now.
#[derive(Debug, Clone, Copy)]
pub struct Recorded {
    pub at: i64,
    pub explicit: bool,
}

/// The current slices of entity `id`, by valid time.
fn current(source: &mut impl RecordSource, space: &str, id: &str) -> Result<Vec<Slice>> {
    let prefix = entity_prefix(keys::BT_CURRENT, space, id);
    let end = keys::prefix_end(&prefix).unwrap_or_default();
    source
        .live(&prefix, &end)?
        .into_iter()
        .map(|(key, raw)| {
            let vt_from = keys::from_ordered(keys::suffix(&key, &prefix))
                .ok_or_else(|| corruption("unreadable bitemporal slice key"))?;
            decode(vt_from, &raw)
        })
        .collect()
}

/// The transaction time of the entity's last change.
fn last_change(source: &mut impl RecordSource, space: &str, id: &str) -> Result<Option<i64>> {
    source
        .read(&state_key(space, id))?
        .map(|raw| {
            raw.as_ref()
                .try_into()
                .map(i64::from_be_bytes)
                .map_err(|_| corruption("unreadable bitemporal entity state"))
        })
        .transpose()
}

/// The transaction time of a write to entity `id`: it never goes back, so a write recorded now
/// is recorded at the entity's last change if that is later.
fn transaction_time(
    source: &mut impl RecordSource,
    (space, id): (&str, &str),
    recorded: Recorded,
) -> Result<i64> {
    let latest = last_change(source, space, id)?.unwrap_or(i64::MIN);
    if recorded.at >= latest {
        Ok(recorded.at)
    } else if recorded.explicit {
        Err(invalid(format!(
            "transaction time {} is before the entity's last change, at {latest}",
            recorded.at
        )))
    } else {
        Ok(latest)
    }
}

/// Replaces the `closed` current slices with `opened` ones at transaction time `tt`, keeping the
/// closed ones as past slices (unless they were current for no time at all), and records `tt`
/// as the entity's last change.
fn supersede(
    (space, id): (&str, &str),
    closed: &[&Slice],
    opened: &[Slice],
    (tt, deleted): (i64, bool),
) -> Result<Effect> {
    let mut changes = Vec::new();
    let mut current: BTreeMap<Bytes, Option<Bytes>> = BTreeMap::new();
    for slice in closed {
        if tt > slice.tt_from {
            changes.push((past_key(space, id, tt, slice.vt_from), Some(encode(slice)?)));
        }
        current.insert(current_key(space, id, slice.vt_from), None);
    }
    for slice in opened {
        let slice = Slice {
            tt_from: tt,
            ..slice.clone()
        };
        current.insert(current_key(space, id, slice.vt_from), Some(encode(&slice)?));
    }
    changes.extend(current);
    Ok(Effect {
        changes,
        state: Some((
            state_key(space, id),
            Bytes::copy_from_slice(&tt.to_be_bytes()),
        )),
        deleted,
    })
}

/// Records `value` over valid time `[from, to)`: the slices it overlaps are cut around it, and
/// adjacent slices of an equal value are joined.
pub fn put(
    source: &mut impl RecordSource,
    entity: (&str, &str),
    value: &Value,
    (from, to): (i64, i64),
    recorded: Recorded,
) -> Result<Effect> {
    let slices = current(source, entity.0, entity.1)?;
    let tt = transaction_time(source, entity, recorded)?;
    let mut closed: Vec<&Slice> = slices
        .iter()
        .filter(|s| s.vt_from < to && s.vt_to > from)
        .collect();
    let mut opened = Vec::new();
    if let Some(first) = closed.first().filter(|s| s.vt_from < from) {
        opened.push(Slice {
            vt_to: from,
            ..(*first).clone()
        });
    }
    opened.push(Slice {
        vt_from: from,
        vt_to: to,
        tt_from: tt,
        value: value.clone(),
    });
    if let Some(last) = closed.last().filter(|s| s.vt_to > to) {
        opened.push(Slice {
            vt_from: to,
            ..(*last).clone()
        });
    }
    if let Some(left) = slices
        .iter()
        .find(|s| s.vt_to == opened[0].vt_from && s.value == opened[0].value && s.vt_to <= from)
    {
        opened[0].vt_from = left.vt_from;
        closed.insert(0, left);
    }
    let last = opened.len() - 1;
    if let Some(right) = slices.iter().find(|s| {
        s.vt_from == opened[last].vt_to && s.value == opened[last].value && s.vt_from >= to
    }) {
        opened[last].vt_to = right.vt_to;
        closed.push(right);
    }
    let mut joined: Vec<Slice> = Vec::with_capacity(opened.len());
    for slice in opened {
        match joined.last_mut() {
            Some(previous) if previous.vt_to == slice.vt_from && previous.value == slice.value => {
                previous.vt_to = slice.vt_to;
            }
            _ => joined.push(slice),
        }
    }
    supersede(entity, &closed, &joined, (tt, false))
}

/// Retracts what is known over valid time `[from, to)`, or over all of it (`None`): the slices
/// it overlaps are cut around it.
pub fn delete(
    source: &mut impl RecordSource,
    entity: (&str, &str),
    valid: Option<(i64, i64)>,
    recorded: Recorded,
) -> Result<Effect> {
    let slices = current(source, entity.0, entity.1)?;
    let (from, to) = valid.unwrap_or((i64::MIN, i64::MAX));
    let closed: Vec<&Slice> = slices
        .iter()
        .filter(|s| s.vt_from < to && s.vt_to > from)
        .collect();
    if closed.is_empty() {
        return Ok(Effect::default());
    }
    let tt = transaction_time(source, entity, recorded)?;
    let mut opened = Vec::new();
    if let Some(first) = closed.first().filter(|s| s.vt_from < from) {
        opened.push(Slice {
            vt_to: from,
            ..(*first).clone()
        });
    }
    if let Some(last) = closed.last().filter(|s| s.vt_to > to) {
        opened.push(Slice {
            vt_from: to,
            ..(*last).clone()
        });
    }
    supersede(entity, &closed, &opened, (tt, true))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const ENTITY: (&str, &str) = ("space", "e");

    #[derive(Default)]
    struct Map(BTreeMap<Bytes, Bytes>);

    impl RecordSource for Map {
        fn read(&mut self, key: &Bytes) -> Result<Option<Bytes>> {
            Ok(self.0.get(key).cloned())
        }

        fn live(&mut self, start: &[u8], end: &[u8]) -> Result<Vec<(Bytes, Bytes)>> {
            Ok(self
                .0
                .range::<[u8], _>((
                    std::ops::Bound::Included(start),
                    std::ops::Bound::Excluded(end),
                ))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect())
        }
    }

    impl Map {
        fn apply(&mut self, effect: Effect) -> bool {
            self.0.extend(effect.state);
            for (key, value) in effect.changes {
                match value {
                    Some(value) => self.0.insert(key, value),
                    None => self.0.remove(&key),
                };
            }
            effect.deleted
        }

        fn put(&mut self, value: &Value, valid: (i64, i64), at: i64) {
            let recorded = Recorded { at, explicit: true };
            let effect = put(self, ENTITY, value, valid, recorded).unwrap();
            self.apply(effect);
        }

        /// The current slices as `(vt_from, vt_to, tt_from, value)`.
        fn now(&mut self) -> Vec<(i64, i64, i64, Value)> {
            current(self, ENTITY.0, ENTITY.1)
                .unwrap()
                .into_iter()
                .map(|s| (s.vt_from, s.vt_to, s.tt_from, s.value))
                .collect()
        }

        fn past(&self) -> usize {
            let prefix = entity_prefix(keys::BT_PAST, ENTITY.0, ENTITY.1);
            self.0.keys().filter(|k| k.starts_with(&prefix)).count()
        }
    }

    #[test]
    fn writes_cut_join_and_keep_what_they_supersede() {
        let mut map = Map::default();
        map.put(&json!(1), (0, 100), 10);
        map.put(&json!(2), (40, 60), 20);
        assert_eq!(
            map.now(),
            [
                (0, 40, 20, json!(1)),
                (40, 60, 20, json!(2)),
                (60, 100, 20, json!(1))
            ]
        );
        assert_eq!(map.past(), 1, "the slice known from 10 to 20");

        // Writing the surrounding value back joins the three slices.
        map.put(&json!(1), (40, 60), 30);
        assert_eq!(map.now(), [(0, 100, 30, json!(1))]);
        assert_eq!(map.past(), 4);

        // Adjacent equal values join; a write in the same transaction time supersedes for no
        // time and leaves no past slice.
        map.put(&json!(1), (100, 150), 30);
        assert_eq!(map.now(), [(0, 150, 30, json!(1))]);
        assert_eq!(map.past(), 4);

        let early = put(
            &mut map,
            ENTITY,
            &json!(9),
            (0, 1),
            Recorded {
                at: 5,
                explicit: true,
            },
        );
        assert!(matches!(early, Err(crate::Error::InvalidArgument(_))));
        let clamped = put(
            &mut map,
            ENTITY,
            &json!(9),
            (0, 1),
            Recorded {
                at: 5,
                explicit: false,
            },
        )
        .unwrap();
        map.apply(clamped);
        assert_eq!(
            map.now()[0],
            (0, 1, 30, json!(9)),
            "recorded at the last change"
        );
    }

    #[test]
    fn deletions_cut_around_the_retracted_interval() {
        let mut map = Map::default();
        map.put(&json!("a"), (0, 50), 10);
        map.put(&json!("b"), (50, 100), 10);
        let recorded = Recorded {
            at: 20,
            explicit: true,
        };
        let effect = delete(&mut map, ENTITY, Some((25, 75)), recorded).unwrap();
        assert!(map.apply(effect));
        assert_eq!(
            map.now(),
            [(0, 25, 20, json!("a")), (75, 100, 20, json!("b"))]
        );
        assert!(
            !delete(&mut map, ENTITY, Some((30, 70)), recorded)
                .unwrap()
                .deleted
        );
        let effect = delete(&mut map, ENTITY, None, recorded).unwrap();
        assert!(map.apply(effect));
        assert_eq!(map.now(), vec![]);
        assert_eq!(map.past(), 2, "the two slices of 10, not those of 20 to 20");
    }
}
