//! Geo records, their derived index entries, and what each write changes.
//!
//! Keyspaces, each under `collection\0`:
//!
//! - fences (`fence id`): name, category, polygon, metadata; a ledger record;
//! - the fence grid (`level, x, y, fence id`): a fence is listed in the cells of the coarsest
//!   level of a lon/lat grid whose cells are at least as large as its bounds, so in at most
//!   2 × 2 cells; a point finds its candidate fences in one cell per level;
//! - GPS points (`track\0`, timestamp): position and readings; a ledger record;
//! - the point cells (finest H3 cell, timestamp, track): every H3 cell's points are one key
//!   range, the descendants of a coarser cell a contiguous one.

use std::collections::BTreeSet;

use bytes::Bytes;
use lilia_core::h3::{lat_lng_to_cell, DEFAULT_H3_RESOLUTION};
use lilia_core::GpsPoint;
use serde::{Deserialize, Serialize};

use super::polygon::{validate_position, Bounds, Polygon};
use crate::error::{corruption, invalid, Result};
use crate::keys;
use crate::records::RecordSource;

/// Finest level of the fence grid: cells of 360 / 2^24 degrees, about 2.4 m.
const GRID_LEVELS: u8 = 25;
/// Resolution of the point cells: H3's finest.
pub const CELL_RESOLUTION: u8 = 15;
const FORMAT: u8 = 1;

/// A geofence: a named, categorized polygon with optional metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct Fence {
    pub name: String,
    pub category: String,
    pub polygon: Polygon,
    pub metadata: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct StoredFence {
    name: String,
    category: String,
    polygon: Vec<[f64; 2]>,
    #[serde(default)]
    metadata: Option<String>,
}

impl Fence {
    pub(crate) fn encode(&self) -> Result<Bytes> {
        let stored = StoredFence {
            name: self.name.clone(),
            category: self.category.clone(),
            polygon: self
                .polygon
                .vertices()
                .iter()
                .map(|&(x, y)| [x, y])
                .collect(),
            metadata: self.metadata.clone(),
        };
        let mut out = vec![FORMAT];
        serde_json::to_writer(&mut out, &stored).map_err(|e| corruption(e.to_string()))?;
        Ok(out.into())
    }

    pub(crate) fn decode(raw: &[u8]) -> Result<Self> {
        let [FORMAT, json @ ..] = raw else {
            return Err(corruption("fence record with an unknown format"));
        };
        let stored: StoredFence =
            serde_json::from_slice(json).map_err(|e| corruption(format!("fence record: {e}")))?;
        let vertices: Vec<(f64, f64)> = stored.polygon.iter().map(|c| (c[0], c[1])).collect();
        Ok(Self {
            name: stored.name,
            category: stored.category,
            polygon: Polygon::new(&vertices)
                .map_err(|e| corruption(format!("fence record: {e}")))?,
            metadata: stored.metadata,
        })
    }
}

pub fn fence_key(collection: &str, id: &str) -> Bytes {
    let mut key = keys::collection(keys::GEO_FENCE, collection);
    key.extend_from_slice(id.as_bytes());
    key.into()
}

fn cell_size(level: u8) -> f64 {
    360.0 / f64::from(1u32 << level)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "positions are validated: 0 <= cells <= 2^24"
)]
fn grid_cell(lon: f64, lat: f64, level: u8) -> (u32, u32) {
    let size = cell_size(level);
    (
        ((lon + 180.0) / size).floor() as u32,
        ((lat + 90.0) / size).floor() as u32,
    )
}

/// Prefix of the fences listed in one grid cell.
fn grid_prefix(collection: &str, level: u8, (x, y): (u32, u32)) -> Vec<u8> {
    let mut key = keys::collection(keys::GEO_GRID, collection);
    key.push(level);
    key.extend_from_slice(&x.to_be_bytes());
    key.extend_from_slice(&y.to_be_bytes());
    key
}

/// The grid entries of a fence with bounds `bounds`.
fn grid_entries(collection: &str, id: &str, bounds: Bounds) -> BTreeSet<Bytes> {
    let (x0, y0, x1, y1) = bounds;
    let extent = (x1 - x0).max(y1 - y0);
    let level = (0..GRID_LEVELS)
        .rev()
        .find(|&level| cell_size(level) >= extent)
        .unwrap_or(0);
    let (min, max) = (grid_cell(x0, y0, level), grid_cell(x1, y1, level));
    let mut entries = BTreeSet::new();
    for x in min.0..=max.0 {
        for y in min.1..=max.1 {
            let mut key = grid_prefix(collection, level, (x, y));
            key.extend_from_slice(id.as_bytes());
            entries.insert(key.into());
        }
    }
    entries
}

/// Prefixes of the grid cells that hold every fence whose bounds contain `(lon, lat)`.
pub fn grid_cells_at(collection: &str, lon: f64, lat: f64) -> Vec<Vec<u8>> {
    (0..GRID_LEVELS)
        .map(|level| grid_prefix(collection, level, grid_cell(lon, lat, level)))
        .collect()
}

/// Prefix of the points of `track`.
pub fn track_prefix(collection: &str, track: &str) -> Vec<u8> {
    let mut key = keys::collection(keys::GEO_POINT, collection);
    key.extend_from_slice(track.as_bytes());
    key.push(0);
    key
}

pub fn point_key(collection: &str, track: &str, timestamp: i64) -> Bytes {
    let mut key = track_prefix(collection, track);
    key.extend_from_slice(&keys::ordered(timestamp));
    key.into()
}

/// The point-cell prefix of finest cell `cell`.
pub fn cell_prefix(collection: &str, cell: u64) -> Vec<u8> {
    let mut key = keys::collection(keys::GEO_CELL, collection);
    key.extend_from_slice(&cell.to_be_bytes());
    key
}

fn cell_entry(collection: &str, cell: u64, timestamp: i64, track: &str) -> Bytes {
    let mut key = cell_prefix(collection, cell);
    key.extend_from_slice(&keys::ordered(timestamp));
    key.extend_from_slice(track.as_bytes());
    key.into()
}

/// Timestamp and track of a point-cell entry, from the part after its cell.
pub fn cell_entry_parts(rest: &[u8]) -> Option<(i64, &str)> {
    let (timestamp, track) = rest.split_at_checked(8)?;
    Some((
        keys::from_ordered(timestamp)?,
        std::str::from_utf8(track).ok()?,
    ))
}

/// Ledger identity of a point (`track\0` timestamp): `track@timestamp`.
pub fn point_identity(rest: &[u8]) -> Option<String> {
    let split = rest.len().checked_sub(9)?;
    let (track, tail) = rest.split_at(split);
    let [0, timestamp @ ..] = tail else {
        return None;
    };
    Some(format!(
        "{}@{}",
        std::str::from_utf8(track).ok()?,
        keys::from_ordered(timestamp)?
    ))
}

fn h3(lat: f64, lon: f64, resolution: u8) -> Result<u64> {
    lat_lng_to_cell(lat, lon, resolution).map_err(|e| invalid(e.to_string()))
}

/// Checks a GPS point: a valid position, finite readings, a valid H3 cell if it has one.
///
/// # Errors
///
/// [`crate::Error::InvalidArgument`] naming what is wrong.
pub fn validate_point(point: &GpsPoint) -> Result<()> {
    validate_position(point.longitude, point.latitude)?;
    if [point.altitude, point.speed, point.accuracy]
        .iter()
        .flatten()
        .any(|x| !x.is_finite())
    {
        return Err(invalid("GPS readings must be finite"));
    }
    if point.h3_index.is_some_and(|c| !lilia_core::h3::is_valid(c)) {
        return Err(invalid("a GPS point's H3 index must be a valid cell"));
    }
    Ok(())
}

/// A stored point: its readings, and its H3 index (the default resolution's cell unless it
/// came with one). The timestamp is in the key.
fn encode_point(point: &GpsPoint) -> Result<Bytes> {
    let h3_index = match point.h3_index {
        Some(cell) => cell,
        None => h3(point.latitude, point.longitude, DEFAULT_H3_RESOLUTION)?,
    };
    let readings = [point.altitude, point.speed, point.accuracy];
    let mut flags = 0u8;
    for (bit, reading) in readings.iter().enumerate() {
        if reading.is_some() {
            flags |= 1 << bit;
        }
    }
    let mut out = vec![FORMAT, flags];
    out.extend_from_slice(&point.latitude.to_le_bytes());
    out.extend_from_slice(&point.longitude.to_le_bytes());
    for reading in readings.iter().flatten() {
        out.extend_from_slice(&reading.to_le_bytes());
    }
    out.extend_from_slice(&h3_index.to_le_bytes());
    Ok(out.into())
}

/// # Errors
///
/// [`crate::Error::Corruption`] for a record that does not decode.
pub fn decode_point(timestamp: i64, raw: &[u8]) -> Result<GpsPoint> {
    let bad = || corruption("unreadable GPS point record");
    let [FORMAT, flags, rest @ ..] = raw else {
        return Err(bad());
    };
    let mut words = rest.as_chunks::<8>().0.iter().copied();
    if rest.len() != 8 * (3 + flags.count_ones() as usize) || flags >> 3 != 0 {
        return Err(bad());
    }
    let mut next = || words.next().ok_or_else(bad);
    let latitude = f64::from_le_bytes(next()?);
    let longitude = f64::from_le_bytes(next()?);
    let mut reading = |bit: u8| -> Result<Option<f64>> {
        if flags & (1 << bit) == 0 {
            return Ok(None);
        }
        Ok(Some(f64::from_le_bytes(next()?)))
    };
    let (altitude, speed, accuracy) = (reading(0)?, reading(1)?, reading(2)?);
    Ok(GpsPoint {
        timestamp,
        latitude,
        longitude,
        altitude,
        speed,
        accuracy,
        h3_index: Some(u64::from_le_bytes(next()?)),
    })
}

/// One record or index entry a write changes.
#[derive(Debug)]
pub struct Change {
    pub key: Bytes,
    pub value: Option<Bytes>,
    /// A ledger record (fence or point), not a derived entry.
    pub record: bool,
}

/// What a geo write changes, and whether it deleted a live record.
#[derive(Debug, Default)]
pub struct Effect {
    pub changes: Vec<Change>,
    pub deleted: bool,
}

impl Effect {
    fn set(&mut self, key: Bytes, value: Option<Bytes>, record: bool) {
        self.changes.push(Change { key, value, record });
    }
}

pub fn put_fence(
    source: &mut impl RecordSource,
    collection: &str,
    id: &str,
    fence: &Fence,
) -> Result<Effect> {
    let key = fence_key(collection, id);
    let old = source
        .read(&key)?
        .map(|raw| Fence::decode(&raw))
        .transpose()?;
    let before = old.map_or_default(|f| grid_entries(collection, id, f.polygon.bounds()));
    let after = grid_entries(collection, id, fence.polygon.bounds());
    let mut effect = Effect::default();
    for entry in before.difference(&after) {
        effect.set(entry.clone(), None, false);
    }
    for entry in after.difference(&before) {
        effect.set(entry.clone(), Some(Bytes::new()), false);
    }
    effect.set(key, Some(fence.encode()?), true);
    Ok(effect)
}

pub fn remove_fence(source: &mut impl RecordSource, collection: &str, id: &str) -> Result<Effect> {
    let key = fence_key(collection, id);
    let mut effect = Effect::default();
    let Some(raw) = source.read(&key)? else {
        return Ok(effect);
    };
    for entry in grid_entries(collection, id, Fence::decode(&raw)?.polygon.bounds()) {
        effect.set(entry, None, false);
    }
    effect.set(key, None, true);
    effect.deleted = true;
    Ok(effect)
}

fn finest_cell(point: &GpsPoint) -> Result<u64> {
    h3(point.latitude, point.longitude, CELL_RESOLUTION)
}

/// Stores `points` (each validated) in `track`, the last of several with one timestamp winning.
pub fn record(
    source: &mut impl RecordSource,
    collection: &str,
    track: &str,
    points: &[GpsPoint],
) -> Result<Effect> {
    let mut latest: std::collections::BTreeMap<i64, &GpsPoint> = std::collections::BTreeMap::new();
    for point in points {
        latest.insert(point.timestamp, point);
    }
    let mut effect = Effect::default();
    for (&timestamp, point) in &latest {
        let key = point_key(collection, track, timestamp);
        let cell = finest_cell(point)?;
        let old_cell = source
            .read(&key)?
            .map(|raw| decode_point(timestamp, &raw).and_then(|old| finest_cell(&old)))
            .transpose()?;
        if old_cell != Some(cell) {
            if let Some(old) = old_cell {
                effect.set(cell_entry(collection, old, timestamp, track), None, false);
            }
            effect.set(
                cell_entry(collection, cell, timestamp, track),
                Some(Bytes::new()),
                false,
            );
        }
        effect.set(key, Some(encode_point(point)?), true);
    }
    Ok(effect)
}

/// Deletes the points of `track` with timestamps in `[start, end]`.
pub fn delete_track(
    source: &mut impl RecordSource,
    collection: &str,
    track: &str,
    start: i64,
    end: i64,
) -> Result<Effect> {
    let first = point_key(collection, track, start);
    let mut last = point_key(collection, track, end).to_vec();
    last.push(0);
    let prefix = track_prefix(collection, track);
    let mut effect = Effect::default();
    for (key, raw) in source.live(&first, &last)? {
        let timestamp = keys::from_ordered(keys::suffix(&key, &prefix))
            .ok_or_else(|| corruption("unreadable GPS point key"))?;
        let cell = finest_cell(&decode_point(timestamp, &raw)?)?;
        effect.set(cell_entry(collection, cell, timestamp, track), None, false);
        effect.set(key, None, true);
        effect.deleted = true;
    }
    Ok(effect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_round_trip_and_keys_parse() {
        let mut point = GpsPoint::with_details(5, 48.85, 2.29, Some(35.0), None, Some(4.5));
        let raw = encode_point(&point).unwrap();
        point.h3_index = Some(lat_lng_to_cell(48.85, 2.29, DEFAULT_H3_RESOLUTION).unwrap());
        assert_eq!(decode_point(5, &raw).unwrap(), point);
        assert!(decode_point(5, &raw[..raw.len() - 1]).is_err());

        let key = point_key("c", "run/1", -3);
        let base = keys::collection(keys::GEO_POINT, "c");
        assert_eq!(
            point_identity(keys::suffix(&key, &base)).as_deref(),
            Some("run/1@-3")
        );
        let entry = cell_entry("c", 7, -3, "run/1");
        assert_eq!(
            cell_entry_parts(keys::suffix(&entry, &cell_prefix("c", 7))),
            Some((-3, "run/1"))
        );
    }

    #[test]
    fn fences_sit_in_at_most_four_cells_their_points_reach() {
        let polygon = Polygon::new(&[(2.0, 48.0), (2.5, 48.0), (2.5, 48.4), (2.0, 48.4)]).unwrap();
        let entries = grid_entries("c", "f", polygon.bounds());
        assert!((1..=4).contains(&entries.len()));
        for (lon, lat) in [(2.0, 48.0), (2.25, 48.2), (2.5, 48.4)] {
            let cells = grid_cells_at("c", lon, lat);
            assert!(
                entries
                    .iter()
                    .any(|e| cells.iter().any(|c| e.starts_with(c))),
                "({lon}, {lat}) reaches the fence"
            );
        }
        let world = Polygon::new(&[
            (-180.0, -90.0),
            (180.0, -90.0),
            (180.0, 90.0),
            (-180.0, 90.0),
        ])
        .unwrap();
        assert!(grid_entries("c", "w", world.bounds()).len() <= 4);
    }
}
