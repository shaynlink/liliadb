//! Conflict-free replicated data types: PN-counters, add-wins OR-sets and LWW JSON documents,
//! by id in CRDT spaces. Each object can be merged with the state of a remote replica (the
//! `lilia_core::crdt` types), and its local writes commute with concurrent ones: concurrent
//! writers of one object only conflict on the same actor's counter totals, the same set
//! entries or the same document fields. See [`store`] for the records.

mod handle;
mod store;

use lilia_core::crdt::{LwwJsonDocument, OrSet, PNCounter};
use serde_json::Value;

pub use handle::{Crdt, CrdtCounter, CrdtDoc, CrdtSet};
pub(crate) use store::{counter_key, entry_key, field_key, normalize_path};

use self::store::Effect;
use crate::error::{corruption, invalid, Result};
use crate::keys::{self, validate_name, ID_SEPARATOR};
use crate::records::RecordSource;
use crate::txn::View;
use crate::write::overlay::Changes;
use crate::write::{Model, Planner};

/// A CRDT write, for a [`crate::Batch`]; see [`CrdtCounter`], [`CrdtSet`] and [`CrdtDoc`] for
/// each operation. Times are in milliseconds.
#[derive(Debug, Clone, PartialEq)]
pub enum CrdtWrite {
    /// Adds `increment` to the increments of `actor` in counter `id`, `decrement` to its
    /// decrements.
    Count {
        space: String,
        id: String,
        actor: String,
        increment: u64,
        decrement: u64,
    },
    CounterMerge {
        space: String,
        id: String,
        state: PNCounter,
    },
    /// Adds `element` under `tag`, which must be unique: see [`CrdtWrite::set_add`].
    SetAdd {
        space: String,
        id: String,
        element: String,
        tag: String,
    },
    SetRemove {
        space: String,
        id: String,
        element: String,
        timestamp_ms: i64,
    },
    SetMerge {
        space: String,
        id: String,
        state: OrSet<String>,
    },
    /// Deletes the entries tombstoned before `cutoff_ms`.
    SetPrune {
        space: String,
        id: String,
        cutoff_ms: i64,
    },
    DocSet {
        space: String,
        id: String,
        actor: String,
        path: String,
        value: Value,
        timestamp_ms: i64,
    },
    DocDelete {
        space: String,
        id: String,
        actor: String,
        path: String,
        timestamp_ms: i64,
    },
    DocMerge {
        space: String,
        id: String,
        state: LwwJsonDocument,
    },
}

impl CrdtWrite {
    /// An add of `element` by `actor` to set `id`, under a new tag (`actor:` and a random
    /// UUID).
    #[must_use]
    pub fn set_add(space: &str, id: &str, actor: &str, element: &str) -> Self {
        Self::SetAdd {
            space: space.to_owned(),
            id: id.to_owned(),
            element: element.to_owned(),
            tag: format!("{actor}:{}", uuid::Uuid::new_v4()),
        }
    }

    fn object(&self) -> (&str, &str) {
        match self {
            Self::Count { space, id, .. }
            | Self::CounterMerge { space, id, .. }
            | Self::SetAdd { space, id, .. }
            | Self::SetRemove { space, id, .. }
            | Self::SetMerge { space, id, .. }
            | Self::SetPrune { space, id, .. }
            | Self::DocSet { space, id, .. }
            | Self::DocDelete { space, id, .. }
            | Self::DocMerge { space, id, .. } => (space, id),
        }
    }

    /// Checks the write and computes its effect.
    fn effect(&self, source: &mut impl RecordSource) -> Result<Effect> {
        let object = self.object();
        validate_name("space", object.0)?;
        validate_name("CRDT id", object.1)?;
        match self {
            Self::Count {
                actor,
                increment,
                decrement,
                ..
            } => {
                validate_name("actor", actor)?;
                store::count(source, object, actor, *increment, *decrement)
            }
            Self::CounterMerge { state, .. } => {
                for actor in state.p.keys().chain(state.n.keys()) {
                    validate_name("actor", actor)?;
                }
                store::merge_counter(source, object, state)
            }
            Self::SetAdd { element, tag, .. } => store::add(source, object, element, tag),
            Self::SetRemove {
                element,
                timestamp_ms,
                ..
            } => store::remove(source, object, element, *timestamp_ms),
            Self::SetMerge { state, .. } => store::merge_set(source, object, state),
            Self::SetPrune { cutoff_ms, .. } => store::prune(source, object, *cutoff_ms),
            Self::DocSet {
                actor,
                path,
                value,
                timestamp_ms,
                ..
            } => {
                validate_name("actor", actor)?;
                let path = normalize_path(path)?;
                store::set_field(source, object, &path, value, (actor, *timestamp_ms))
            }
            Self::DocDelete {
                actor,
                path,
                timestamp_ms,
                ..
            } => {
                validate_name("actor", actor)?;
                let path = normalize_path(path)?;
                store::delete_field(source, object, &path, (actor, *timestamp_ms))
            }
            Self::DocMerge { state, .. } => {
                for path in state.fields.keys() {
                    if normalize_path(path)? != *path {
                        return Err(invalid(format!(
                            "merged field path {path:?} does not start with `/`"
                        )));
                    }
                }
                store::merge_doc(source, object, state.fields.iter())
            }
        }
    }
}

/// Ledger identity of the record at `rest` (a key after `space\0`) of keyspace `tag`: the
/// object id and the record's actor, element and tag, or path, separated by
/// [`ID_SEPARATOR`].
pub(crate) fn identity(tag: u8, rest: &[u8]) -> Option<(Model, String)> {
    let split = rest.iter().position(|&b| b == 0)?;
    let id = std::str::from_utf8(&rest[..split]).ok()?;
    let record = &rest[split + 1..];
    match tag {
        keys::CRDT_COUNTER => Some((
            Model::CrdtCounter,
            format!("{id}{ID_SEPARATOR}{}", std::str::from_utf8(record).ok()?),
        )),
        keys::CRDT_SET => {
            let (element, entry_tag) = store::entry_parts(record)?;
            Some((
                Model::CrdtSet,
                format!("{id}{ID_SEPARATOR}{element}{ID_SEPARATOR}{entry_tag}"),
            ))
        }
        keys::CRDT_FIELD => Some((
            Model::CrdtDoc,
            format!("{id}{ID_SEPARATOR}{}", std::str::from_utf8(record).ok()?),
        )),
        _ => None,
    }
}

impl Planner<'_> {
    /// Plans one CRDT write.
    ///
    /// # Errors
    ///
    /// Invalid names, paths or merged states, records that do not decode, engine read errors.
    pub(crate) fn crdt(&mut self, write: &CrdtWrite) -> Result<()> {
        let effect = write.effect(self)?;
        let space = write.object().0;
        let mut record_op = None;
        for (key, value) in effect.changes {
            let tag = key.first().copied().unwrap_or_default();
            let rest = keys::suffix(&key, &keys::collection(tag, space));
            let (model, id) =
                identity(tag, rest).ok_or_else(|| corruption("unreadable CRDT record key"))?;
            record_op = Some(self.write_record(&key, value, (model, space, id)));
        }
        self.result(record_op, effect.deleted);
        Ok(())
    }
}

/// The records a CRDT write changes in a transaction's view; see [`crate::write::overlay`].
///
/// # Errors
///
/// As [`Planner::crdt`].
pub(crate) fn overlay(write: &CrdtWrite, view: &View<'_>) -> Result<(Changes, bool)> {
    let effect = write.effect(&mut &*view)?;
    Ok((effect.changes, effect.deleted))
}
