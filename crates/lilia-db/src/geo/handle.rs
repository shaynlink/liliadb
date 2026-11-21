//! The geo handle of a session or transaction.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Bound;

use lilia_core::geo::haversine_distance_meters;
use lilia_core::h3::{child_range, k_ring, lat_lng_to_cell, resolution};
use lilia_core::{GeofenceMatch, GpsPoint};
use serde_json::json;

use super::polygon::validate_position;
use super::store::{
    cell_entry_parts, cell_prefix, decode_point, fence_key, from_ordered, grid_cells_at, point_key,
    track_prefix, Fence, CELL_RESOLUTION,
};
use super::GeoWrite;
use crate::batch::{Outcome, Write};
use crate::context::{guard, Context};
use crate::error::{corruption, invalid, Result};
use crate::guard::Guard;
use crate::keys::{self, validate_name};
use crate::read::Source;

/// A GPS point and its track.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackPoint {
    pub track: String,
    pub point: GpsPoint,
}

/// Geo collection `name` of a session or transaction: geofences and GPS tracks.
///
/// A fence is readable when the read policies of the collection admit the document
/// `{"collection", "fence", "category"}`, a track's points when they admit
/// `{"collection", "track"}`. Time ranges are inclusive.
#[derive(Debug)]
pub struct Geo<'c, C: Context> {
    context: &'c C,
    name: String,
}

impl<'c, C: Context> Geo<'c, C> {
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

    fn submit(&self, write: GeoWrite) -> Result<Outcome> {
        let mut outcomes = self.context.submit(vec![Write::Geo(write)])?;
        outcomes
            .pop()
            .ok_or_else(|| corruption("a geo write produced no outcome"))
    }

    /// Stores fence `id`, replacing the previous one.
    ///
    /// # Errors
    ///
    /// Invalid names; as [`crate::Session::commit`].
    pub fn put_fence(&self, id: &str, fence: &Fence) -> Result<Outcome> {
        self.submit(GeoWrite::PutFence {
            collection: self.name.clone(),
            id: id.to_owned(),
            fence: fence.clone(),
        })
    }

    /// Removes fence `id`; `true` if it existed.
    ///
    /// # Errors
    ///
    /// As [`Geo::put_fence`].
    pub fn remove_fence(&self, id: &str) -> Result<bool> {
        Ok(self
            .submit(GeoWrite::RemoveFence {
                collection: self.name.clone(),
                id: id.to_owned(),
            })?
            .deleted)
    }

    /// Fence `id`, if the caller may read it.
    ///
    /// # Errors
    ///
    /// Invalid names, records that do not decode, engine errors.
    pub fn fence(&self, id: &str) -> Result<Option<Fence>> {
        validate_name("collection", &self.name)?;
        validate_name("fence id", id)?;
        let reader = self.context.reader(false);
        let guard = guard(self.context, &self.name);
        let Some(item) = reader.get(&fence_key(&self.name, id))? else {
            return Ok(None);
        };
        let fence = Fence::decode(&item.value)?;
        Ok(self.fence_allowed(&guard, id, &fence).then_some(fence))
    }

    /// The fences that contain `(lon, lat)` (boundary included), by id.
    ///
    /// # Errors
    ///
    /// An invalid name or position; as [`Geo::fence`].
    pub fn fences_at(&self, lon: f64, lat: f64) -> Result<Vec<GeofenceMatch>> {
        validate_name("collection", &self.name)?;
        validate_position(lon, lat)?;
        let reader = self.context.reader(true);
        let guard = guard(self.context, &self.name);
        let base = keys::collection(keys::GEO_GRID, &self.name);
        let mut ids = BTreeSet::new();
        for cell in grid_cells_at(&self.name, lon, lat) {
            let end = keys::prefix_end(&cell).unwrap_or_default();
            for item in reader.range(Bound::Included(&cell), Bound::Excluded(&end))? {
                let item = item?;
                reader.observe(&item);
                let id = keys::suffix(&item.key, &base)
                    .get(9..)
                    .and_then(|id| std::str::from_utf8(id).ok())
                    .ok_or_else(|| corruption("unreadable fence grid entry"))?;
                ids.insert(id.to_owned());
            }
        }
        let mut matches = Vec::new();
        for id in ids {
            let Some(item) = reader.get(&fence_key(&self.name, &id))? else {
                continue;
            };
            let fence = Fence::decode(&item.value)?;
            if fence.polygon.contains(lon, lat) && self.fence_allowed(&guard, &id, &fence) {
                matches.push(GeofenceMatch {
                    id,
                    name: fence.name,
                    category: fence.category,
                    metadata: fence.metadata,
                });
            }
        }
        Ok(matches)
    }

    /// Records `points` in `track` (replacing the points of the same timestamps; among `points`,
    /// the last of a timestamp wins), then returns the fences containing the newest of them.
    /// A point without an H3 index gets the cell of the default resolution.
    ///
    /// # Errors
    ///
    /// Invalid names, positions or readings; as [`Geo::put_fence`].
    pub fn record(&self, track: &str, points: &[GpsPoint]) -> Result<Vec<GeofenceMatch>> {
        self.submit(GeoWrite::Record {
            collection: self.name.clone(),
            track: track.to_owned(),
            points: points.to_vec(),
        })?;
        let newest = points
            .iter()
            .reduce(|a, b| if b.timestamp >= a.timestamp { b } else { a });
        match newest {
            Some(point) => self.fences_at(point.longitude, point.latitude),
            None => Ok(Vec::new()),
        }
    }

    /// The points of `track` in `[start, end]`, oldest first.
    ///
    /// # Errors
    ///
    /// Invalid names, records that do not decode, engine errors.
    pub fn track(&self, track: &str, start: i64, end: i64) -> Result<Vec<GpsPoint>> {
        validate_name("collection", &self.name)?;
        validate_name("track", track)?;
        if start > end || !self.track_allowed(&guard(self.context, &self.name), track) {
            return Ok(Vec::new());
        }
        let reader = self.context.reader(true);
        let prefix = track_prefix(&self.name, track);
        let first = point_key(&self.name, track, start);
        let mut last = point_key(&self.name, track, end).to_vec();
        last.push(0);
        let mut points = Vec::new();
        for item in reader.range(Bound::Included(&first), Bound::Excluded(&last))? {
            let item = item?;
            reader.observe(&item);
            let timestamp = from_ordered(keys::suffix(&item.key, &prefix))
                .ok_or_else(|| corruption("unreadable GPS point key"))?;
            points.push(decode_point(timestamp, &item.value)?);
        }
        Ok(points)
    }

    /// Deletes the points of `track` in `[start, end]`; `true` if there were some.
    ///
    /// # Errors
    ///
    /// Invalid names, a range that ends before it starts; as [`Geo::put_fence`].
    pub fn delete_track(&self, track: &str, start: i64, end: i64) -> Result<bool> {
        Ok(self
            .submit(GeoWrite::DeleteTrack {
                collection: self.name.clone(),
                track: track.to_owned(),
                start,
                end,
            })?
            .deleted)
    }

    /// The points of every track in `[start, end]` whose H3 cell at the resolution of `center`
    /// is within `k` cells of it, by time then track.
    ///
    /// # Errors
    ///
    /// An invalid name or cell; as [`Geo::track`].
    pub fn near(&self, center: u64, k: u32, start: i64, end: i64) -> Result<Vec<TrackPoint>> {
        validate_name("collection", &self.name)?;
        let invalid_cell = |e: lilia_core::LiliaError| invalid(e.to_string());
        let ring: HashSet<u64> = k_ring(center, k)
            .map_err(invalid_cell)?
            .into_iter()
            .collect();
        let center_resolution = resolution(center).map_err(invalid_cell)?;
        // The finest descendants of a cell (by index) cover its area only approximately, within
        // its neighbors: a coarser query scans one ring more and checks each point's own cell.
        let exact = center_resolution < CELL_RESOLUTION;
        let scanned = if exact {
            k_ring(center, k.saturating_add(1)).map_err(invalid_cell)?
        } else {
            ring.iter().copied().collect()
        };
        let reader = self.context.reader(true);
        let guard = guard(self.context, &self.name);
        let mut allowed: HashMap<String, bool> = HashMap::new();
        let mut found = Vec::new();
        for cell in scanned {
            let (low, high) = if exact {
                child_range(cell, CELL_RESOLUTION).map_err(invalid_cell)?
            } else {
                (cell, cell)
            };
            let first = cell_prefix(&self.name, low);
            let after = keys::prefix_end(&cell_prefix(&self.name, high)).unwrap_or_default();
            for item in reader.range(Bound::Included(&first), Bound::Excluded(&after))? {
                let item = item?;
                reader.observe(&item);
                let (timestamp, track) =
                    cell_entry_parts(item.key.get(first.len()..).unwrap_or_default())
                        .ok_or_else(|| corruption("unreadable GPS cell entry"))?;
                if timestamp < start || timestamp > end {
                    continue;
                }
                let readable = *allowed
                    .entry(track.to_owned())
                    .or_insert_with(|| self.track_allowed(&guard, track));
                if !readable {
                    continue;
                }
                let Some(item) = reader.get(&point_key(&self.name, track, timestamp))? else {
                    continue;
                };
                let point = decode_point(timestamp, &item.value)?;
                let own = lat_lng_to_cell(point.latitude, point.longitude, center_resolution)
                    .map_err(invalid_cell)?;
                if ring.contains(&own) {
                    found.push(TrackPoint {
                        track: track.to_owned(),
                        point,
                    });
                }
            }
        }
        found.sort_by(|a, b| (a.point.timestamp, &a.track).cmp(&(b.point.timestamp, &b.track)));
        Ok(found)
    }

    /// Length of `track` in meters: the great-circle distances between its consecutive points.
    ///
    /// # Errors
    ///
    /// As [`Geo::track`].
    pub fn track_distance(&self, track: &str) -> Result<f64> {
        let points = self.track(track, i64::MIN, i64::MAX)?;
        Ok(points
            .windows(2)
            .map(|w| {
                haversine_distance_meters(
                    w[0].latitude,
                    w[0].longitude,
                    w[1].latitude,
                    w[1].longitude,
                )
            })
            .sum())
    }

    fn fence_allowed(&self, guard: &Guard<'_>, id: &str, fence: &Fence) -> bool {
        guard.related_hit(
            None,
            || json!({ "collection": self.name, "fence": id, "category": fence.category }),
        )
    }

    fn track_allowed(&self, guard: &Guard<'_>, track: &str) -> bool {
        guard.related_hit(None, || json!({ "collection": self.name, "track": track }))
    }
}
