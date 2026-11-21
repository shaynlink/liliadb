//! Geo collections: geofences (polygons found by the points they contain) and GPS tracks
//! (points by track and time, found by H3 cell). See [`store`] for the keyspaces.

mod handle;
mod polygon;
mod store;

use lilia_core::GpsPoint;

pub use handle::{Geo, TrackPoint};
pub use polygon::Polygon;
pub use store::Fence;
pub(crate) use store::{fence_key, point_key};

use self::store::Effect;
use crate::error::{corruption, invalid, Result};
use crate::keys::{self, validate_name};
use crate::records::RecordSource;
use crate::txn::View;
use crate::write::overlay::Changes;
use crate::write::{Model, Planner};

/// A geo write, for a [`crate::Batch`]; see [`Geo`] for each operation.
#[derive(Debug, Clone, PartialEq)]
pub enum GeoWrite {
    PutFence {
        collection: String,
        id: String,
        fence: Fence,
    },
    RemoveFence {
        collection: String,
        id: String,
    },
    Record {
        collection: String,
        track: String,
        points: Vec<GpsPoint>,
    },
    /// Deletes the points of `track` with timestamps in `[start, end]`.
    DeleteTrack {
        collection: String,
        track: String,
        start: i64,
        end: i64,
    },
}

impl GeoWrite {
    fn collection(&self) -> &str {
        match self {
            Self::PutFence { collection, .. }
            | Self::RemoveFence { collection, .. }
            | Self::Record { collection, .. }
            | Self::DeleteTrack { collection, .. } => collection,
        }
    }

    fn validate(&self) -> Result<()> {
        validate_name("collection", self.collection())?;
        match self {
            Self::PutFence { id, .. } | Self::RemoveFence { id, .. } => {
                validate_name("fence id", id)
            }
            Self::Record { track, points, .. } => {
                validate_name("track", track)?;
                points.iter().try_for_each(store::validate_point)
            }
            Self::DeleteTrack {
                track, start, end, ..
            } => {
                validate_name("track", track)?;
                if start > end {
                    return Err(invalid(
                        "a deleted time range must not end before it starts",
                    ));
                }
                Ok(())
            }
        }
    }

    fn effect(&self, source: &mut impl RecordSource) -> Result<Effect> {
        match self {
            Self::PutFence {
                collection,
                id,
                fence,
            } => store::put_fence(source, collection, id, fence),
            Self::RemoveFence { collection, id } => store::remove_fence(source, collection, id),
            Self::Record {
                collection,
                track,
                points,
            } => store::record(source, collection, track, points),
            Self::DeleteTrack {
                collection,
                track,
                start,
                end,
            } => store::delete_track(source, collection, track, *start, *end),
        }
    }
}

/// Ledger identity of the record at `rest` (a key after `collection\0`) of keyspace `tag`.
pub(crate) fn identity(tag: u8, rest: &[u8]) -> Option<(Model, String)> {
    match tag {
        keys::GEO_FENCE => Some((Model::GeoFence, String::from_utf8(rest.to_vec()).ok()?)),
        keys::GEO_POINT => Some((Model::GeoPoint, store::point_identity(rest)?)),
        _ => None,
    }
}

impl Planner<'_> {
    /// Plans one geo write.
    ///
    /// # Errors
    ///
    /// Invalid arguments, records that do not decode, engine read errors.
    pub(crate) fn geo(&mut self, write: &GeoWrite) -> Result<()> {
        write.validate()?;
        let effect = write.effect(self)?;
        let collection = write.collection();
        let mut record_op = None;
        for change in effect.changes {
            if change.record {
                let tag = change.key.first().copied().unwrap_or_default();
                let rest = keys::suffix(&change.key, &keys::collection(tag, collection));
                let (model, id) =
                    identity(tag, rest).ok_or_else(|| corruption("unreadable geo record key"))?;
                record_op =
                    Some(self.write_record(&change.key, change.value, (model, collection, id)));
            } else {
                self.op(change.key, change.value);
            }
        }
        self.result(record_op, effect.deleted);
        Ok(())
    }
}

/// The records a geo write changes in a transaction's view; see [`crate::write::overlay`].
///
/// # Errors
///
/// As [`Planner::geo`].
pub(crate) fn overlay(write: &GeoWrite, view: &View<'_>) -> Result<(Changes, bool)> {
    write.validate()?;
    let effect = write.effect(&mut &*view)?;
    Ok((
        effect
            .changes
            .into_iter()
            .map(|c| (c.key, c.value))
            .collect(),
        effect.deleted,
    ))
}
