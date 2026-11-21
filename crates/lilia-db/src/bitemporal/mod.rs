//! Bitemporal entities: JSON values over valid-time intervals (when a fact holds), recorded at
//! transaction times (when the database learned it). A write supersedes what it overlaps without
//! erasing it, so an entity can be read as it is known now or as it was known at any earlier
//! time, for good: this audit trail does not depend on the history window
//! ([`crate::history`]). The `store` module describes the records.

mod handle;
mod store;

use lilia_core::BitemporalInterval;
use serde_json::Value;

pub use handle::Bitemporal;
pub(crate) use store::current_key;

use self::store::{Effect, Recorded};
use crate::error::{corruption, invalid, Result};
use crate::keys::{self, validate_name, ID_SEPARATOR};
use crate::records::RecordSource;
use crate::txn::View;
use crate::write::overlay::Changes;
use crate::write::{Model, Planner};

/// A bitemporal write, for a [`crate::Batch`]; see [`Bitemporal`] for each operation.
/// `recorded_at` is the transaction time in milliseconds, `None` for the time of the commit.
#[derive(Debug, Clone, PartialEq)]
pub enum BitemporalWrite {
    Put {
        space: String,
        id: String,
        value: Value,
        valid: BitemporalInterval,
        recorded_at: Option<i64>,
    },
    /// Retracts what is known over `valid`, or over all valid time.
    Delete {
        space: String,
        id: String,
        valid: Option<BitemporalInterval>,
        recorded_at: Option<i64>,
    },
}

impl BitemporalWrite {
    fn entity(&self) -> (&str, &str) {
        match self {
            Self::Put { space, id, .. } | Self::Delete { space, id, .. } => (space, id),
        }
    }

    /// Checks the write and computes its effect, recorded now at `now` unless it says when.
    fn effect(&self, source: &mut impl RecordSource, now: i64) -> Result<Effect> {
        let entity = self.entity();
        validate_name("space", entity.0)?;
        validate_name("entity id", entity.1)?;
        let recorded = |at: &Option<i64>| Recorded {
            at: at.unwrap_or(now),
            explicit: at.is_some(),
        };
        match self {
            Self::Put {
                value,
                valid,
                recorded_at,
                ..
            } => {
                let valid = interval(valid)?;
                store::put(source, entity, value, valid, recorded(recorded_at))
            }
            Self::Delete {
                valid, recorded_at, ..
            } => {
                let valid = valid.as_ref().map(interval).transpose()?;
                store::delete(source, entity, valid, recorded(recorded_at))
            }
        }
    }
}

/// A valid-time interval as `(from, to)`.
fn interval(valid: &BitemporalInterval) -> Result<(i64, i64)> {
    if valid.from >= valid.to {
        return Err(invalid(format!(
            "a valid-time interval must not be empty: [{}, {})",
            valid.from, valid.to
        )));
    }
    Ok((valid.from, valid.to))
}

/// Ledger identity of the record at `rest` (a key after `space\0`) of keyspace `tag`: the
/// entity id and the slice's `vt_from`, after its `tt_to` for a past slice.
pub(crate) fn identity(tag: u8, rest: &[u8]) -> Option<(Model, String)> {
    let split = rest.iter().position(|&b| b == 0)?;
    let id = std::str::from_utf8(&rest[..split]).ok()?;
    let (vt_from, tt_to) = store::key_times(&rest[split + 1..])?;
    match (tag, tt_to) {
        (keys::BT_CURRENT, None) => {
            Some((Model::Bitemporal, format!("{id}{ID_SEPARATOR}{vt_from}")))
        }
        (keys::BT_PAST, Some(tt_to)) => Some((
            Model::BitemporalPast,
            format!("{id}{ID_SEPARATOR}{tt_to}{ID_SEPARATOR}{vt_from}"),
        )),
        _ => None,
    }
}

impl Planner<'_> {
    /// Plans one bitemporal write.
    ///
    /// # Errors
    ///
    /// Invalid names or intervals, a transaction time before the entity's last change, records
    /// that do not decode, engine read errors.
    pub(crate) fn bitemporal(&mut self, write: &BitemporalWrite) -> Result<()> {
        let now = self.now_ms();
        let effect = write.effect(self, now)?;
        let space = write.entity().0;
        let mut record_op = None;
        for (key, value) in effect.changes {
            let tag = key.first().copied().unwrap_or_default();
            let rest = keys::suffix(&key, &keys::collection(tag, space));
            let (model, id) = identity(tag, rest)
                .ok_or_else(|| corruption("unreadable bitemporal record key"))?;
            record_op = Some(self.write_record(&key, value, (model, space, id)));
        }
        if let Some((key, value)) = effect.state {
            self.op(key, Some(value));
        }
        self.result(record_op, effect.deleted);
        Ok(())
    }
}

/// The records a bitemporal write changes in a transaction's view; see
/// [`crate::write::overlay`].
///
/// # Errors
///
/// As [`Planner::bitemporal`].
pub(crate) fn overlay(write: &BitemporalWrite, view: &View<'_>) -> Result<(Changes, bool)> {
    let effect = write.effect(&mut &*view, crate::record::now_ms())?;
    let mut changes = effect.changes;
    changes.extend(effect.state.map(|(key, value)| (key, Some(value))));
    Ok((changes, effect.deleted))
}
