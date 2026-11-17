#![allow(
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use std::str::FromStr;

use h3o::{CellIndex, LatLng, Resolution};
use serde::{Deserialize, Serialize};

use crate::{ErrorCode, LiliaError, Result};

/// Default H3 resolution for urban tracking, delivery, and mobility (~100m hexagon diameter).
pub const DEFAULT_H3_RESOLUTION: u8 = 9;

/// A point indexed in both spatial (H3) and temporal dimensions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpatioTemporalPoint {
    pub entity_id: String,
    pub h3_index: u64,
    pub timestamp: i64,
    pub latitude: f64,
    pub longitude: f64,
}

impl SpatioTemporalPoint {
    /// Creates a new spatio-temporal point by discretizing coordinates into an H3 index.
    pub fn new(
        entity_id: impl Into<String>,
        latitude: f64,
        longitude: f64,
        timestamp: i64,
        resolution: u8,
    ) -> Result<Self> {
        let h3_index = lat_lng_to_cell(latitude, longitude, resolution)?;
        Ok(Self {
            entity_id: entity_id.into(),
            h3_index,
            timestamp,
            latitude,
            longitude,
        })
    }
}

/// Discretizes a temporal timestamp (in milliseconds or seconds) into a uniform bucket.
#[must_use]
pub const fn time_bucket(timestamp: i64, bucket_size: i64) -> i64 {
    if bucket_size <= 0 {
        return timestamp;
    }
    (timestamp / bucket_size) * bucket_size
}

/// Converts GPS latitude and longitude (in degrees) to a 64-bit unsigned H3 cell index.
pub fn lat_lng_to_cell(lat: f64, lng: f64, resolution: u8) -> Result<u64> {
    let res = Resolution::try_from(resolution).map_err(|e| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid H3 resolution {resolution}: {e}"),
            false,
        )
    })?;
    let coord = LatLng::new(lat, lng).map_err(|e| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid GPS coordinates ({lat}, {lng}): {e}"),
            false,
        )
    })?;
    Ok(u64::from(coord.to_cell(res)))
}

/// Converts a 64-bit H3 cell index to the centroid coordinates `(latitude, longitude)` in degrees.
pub fn cell_to_lat_lng(cell: u64) -> Result<(f64, f64)> {
    let cell_idx = CellIndex::try_from(cell).map_err(|e| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid H3 cell {cell:#x}: {e}"),
            false,
        )
    })?;
    let lat_lng = LatLng::from(cell_idx);
    Ok((lat_lng.lat(), lat_lng.lng()))
}

/// Retrieves all neighbor cells within grid distance `k` (filled-in hexagonal disk).
/// Radius k=0 returns the cell itself; k=1 returns 7 cells; k=2 returns 19 cells, etc.
pub fn k_ring(cell: u64, k: u32) -> Result<Vec<u64>> {
    let cell_idx = CellIndex::try_from(cell).map_err(|e| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid H3 cell {cell:#x}: {e}"),
            false,
        )
    })?;
    let cells: Vec<CellIndex> = cell_idx.grid_disk(k);
    Ok(cells.into_iter().map(u64::from).collect())
}

/// Retrieves all neighbor cells within grid distance `k` along with their exact hop distance.
pub fn k_ring_distances(cell: u64, k: u32) -> Result<Vec<(u64, u32)>> {
    let cell_idx = CellIndex::try_from(cell).map_err(|e| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid H3 cell {cell:#x}: {e}"),
            false,
        )
    })?;
    let items: Vec<(CellIndex, u32)> = cell_idx.grid_disk_distances(k);
    Ok(items
        .into_iter()
        .map(|(c, dist)| (u64::from(c), dist))
        .collect())
}

/// Computes the grid distance (topological step count) between two H3 cells of the same resolution.
pub fn grid_distance(origin: u64, destination: u64) -> Result<i64> {
    let o = CellIndex::try_from(origin).map_err(|e| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid origin H3 cell {origin:#x}: {e}"),
            false,
        )
    })?;
    let d = CellIndex::try_from(destination).map_err(|e| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid destination H3 cell {destination:#x}: {e}"),
            false,
        )
    })?;
    o.grid_distance(d)
        .map(i64::from)
        .map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("failed to compute grid distance: {e}"),
                false,
            )
        })
}

/// Retrieves the parent cell at a coarser resolution (`parent_res < current_res`).
pub fn cell_to_parent(cell: u64, parent_res: u8) -> Result<u64> {
    let cell_idx = CellIndex::try_from(cell).map_err(|e| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid H3 cell {cell:#x}: {e}"),
            false,
        )
    })?;
    let target_res = Resolution::try_from(parent_res).map_err(|e| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid parent resolution {parent_res}: {e}"),
            false,
        )
    })?;
    cell_idx
        .parent(target_res)
        .map(u64::from)
        .ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!(
                    "target resolution {parent_res} must be <= cell resolution {:?}",
                    cell_idx.resolution()
                ),
                false,
            )
        })
}

/// Retrieves all child cells at a finer resolution (`child_res > current_res`).
pub fn cell_to_children(cell: u64, child_res: u8) -> Result<Vec<u64>> {
    let cell_idx = CellIndex::try_from(cell).map_err(|e| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid H3 cell {cell:#x}: {e}"),
            false,
        )
    })?;
    let target_res = Resolution::try_from(child_res).map_err(|e| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid child resolution {child_res}: {e}"),
            false,
        )
    })?;
    let parent_res = cell_idx.resolution();
    if target_res < parent_res {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            format!("child resolution ({child_res}) must be >= cell resolution ({parent_res:?})"),
            false,
        ));
    }
    Ok(cell_idx.children(target_res).map(u64::from).collect())
}

/// Computes in O(1) the minimum and maximum child cell 64-bit integer values `[h3_min, h3_max]`.
/// This interval can be directly queried via a B-Tree range scan:
/// `WHERE h3_index BETWEEN :h3_min AND :h3_max`.
pub fn child_range(cell: u64, child_res: u8) -> Result<(u64, u64)> {
    let cell_idx = CellIndex::try_from(cell).map_err(|e| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid H3 cell {cell:#x}: {e}"),
            false,
        )
    })?;
    let _ = Resolution::try_from(child_res).map_err(|e| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid target resolution {child_res}: {e}"),
            false,
        )
    })?;
    let parent_res_val = u8::from(cell_idx.resolution());
    if child_res < parent_res_val {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            format!("child resolution ({child_res}) must be >= parent resolution ({parent_res_val})"),
            false,
        ));
    }
    if child_res == parent_res_val {
        return Ok((cell, cell));
    }

    // Direct bitwise O(1) range calculation:
    // Update the resolution field (bits 55..52)
    let raw = u64::from(cell_idx);
    let base = (raw & !(0xFu64 << 52)) | ((u64::from(child_res)) << 52);

    let mut min_val = base;
    let mut max_val = base;

    for r in (parent_res_val + 1)..=child_res {
        let shift = (15 - r) * 3;
        // In the parent, these slots were 7 (0b111).
        // Clear them first:
        min_val &= !(0x7u64 << shift);
        max_val &= !(0x7u64 << shift);
        // Min child takes digit 0:
        // (min_val slot is already 0)
        // Max child takes digit 6 (0b110):
        max_val |= 0x6u64 << shift;
    }

    Ok((min_val, max_val))
}

/// Retrieves the polygon boundary coordinates `[(longitude, latitude), ...]` outlining the cell.
/// Output is ordered for compatibility with geopoly and `GeoJSON`.
pub fn cell_to_boundary(cell: u64) -> Result<Vec<(f64, f64)>> {
    let cell_idx = CellIndex::try_from(cell).map_err(|e| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid H3 cell {cell:#x}: {e}"),
            false,
        )
    })?;
    let boundary = cell_idx.boundary();
    let mut coords: Vec<(f64, f64)> = boundary.iter().map(|v| (v.lng(), v.lat())).collect();
    if let Some(&first) = coords.first() {
        coords.push(first);
    }
    Ok(coords)
}

/// Formats a 64-bit integer H3 cell into a 15-character lowercase hexadecimal string.
#[must_use]
pub fn cell_to_string(cell: u64) -> String {
    format!("{cell:x}")
}

/// Parses an H3 hexadecimal string representation into a 64-bit integer.
pub fn string_to_cell(s: &str) -> Result<u64> {
    let trimmed = s.trim();
    let cell_idx = CellIndex::from_str(trimmed).map_err(|e| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid H3 string '{trimmed}': {e}"),
            false,
        )
    })?;
    Ok(u64::from(cell_idx))
}

/// Checks whether a 64-bit integer represents a valid H3 cell.
#[must_use]
pub fn is_valid(cell: u64) -> bool {
    CellIndex::try_from(cell).is_ok()
}

/// Returns the resolution (0 to 15) of an H3 cell.
pub fn resolution(cell: u64) -> Result<u8> {
    let cell_idx = CellIndex::try_from(cell).map_err(|e| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid H3 cell {cell:#x}: {e}"),
            false,
        )
    })?;
    Ok(u8::from(cell_idx.resolution()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lat_lng_to_cell_and_back() {
        // Eiffel Tower coordinates: 48.8584, 2.2945
        let cell = lat_lng_to_cell(48.8584, 2.2945, 9).unwrap();
        assert!(is_valid(cell));
        assert_eq!(resolution(cell).unwrap(), 9);

        // Convert back to centroid
        let (lat, lng) = cell_to_lat_lng(cell).unwrap();
        let dist = crate::geo::haversine_distance_meters(48.8584, 2.2945, lat, lng);
        // Centroid must be within hexagon radius (~100m)
        assert!(dist < 120.0, "centroid distance was {dist}m");
    }

    #[test]
    fn test_k_ring_sizes() {
        let cell = lat_lng_to_cell(48.8584, 2.2945, 9).unwrap();

        let ring_0 = k_ring(cell, 0).unwrap();
        assert_eq!(ring_0.len(), 1);
        assert_eq!(ring_0[0], cell);

        let ring_1 = k_ring(cell, 1).unwrap();
        assert_eq!(ring_1.len(), 7); // Center + 6 direct neighbors

        let ring_2 = k_ring(cell, 2).unwrap();
        assert_eq!(ring_2.len(), 19); // Center + 6 + 12

        let distances = k_ring_distances(cell, 1).unwrap();
        assert_eq!(distances.len(), 7);
        let center_dists: Vec<_> = distances.iter().filter(|(c, _)| *c == cell).collect();
        assert_eq!(center_dists.len(), 1);
        assert_eq!(center_dists[0].1, 0);
    }

    #[test]
    fn test_grid_distance() {
        let cell_a = lat_lng_to_cell(48.8584, 2.2945, 9).unwrap();
        let neighbors = k_ring(cell_a, 1).unwrap();
        let cell_b = neighbors[1];

        assert_eq!(grid_distance(cell_a, cell_a).unwrap(), 0);
        assert_eq!(grid_distance(cell_a, cell_b).unwrap(), 1);
    }

    #[test]
    fn test_child_range_bitwise_exactness() {
        // Test parent resolution 7 to child resolution 9
        let parent = lat_lng_to_cell(48.8584, 2.2945, 7).unwrap();
        let (min_child, max_child) = child_range(parent, 9).unwrap();

        assert!(min_child <= max_child);
        assert!(is_valid(min_child));
        assert!(is_valid(max_child));
        assert_eq!(resolution(min_child).unwrap(), 9);
        assert_eq!(resolution(max_child).unwrap(), 9);

        // Verify with actual enumerated children:
        let children = cell_to_children(parent, 9).unwrap();
        assert_eq!(children.len(), 49); // 7^2 = 49 children
        let actual_min = *children.iter().min().unwrap();
        let actual_max = *children.iter().max().unwrap();

        assert_eq!(min_child, actual_min, "min_child bitwise must match actual min");
        assert_eq!(max_child, actual_max, "max_child bitwise must match actual max");

        for child in children {
            assert!(
                child >= min_child && child <= max_child,
                "child {child:#x} must be within [{min_child:#x}, {max_child:#x}]"
            );
        }
    }

    #[test]
    fn test_hex_string_roundtrip() {
        let cell = lat_lng_to_cell(48.8584, 2.2945, 8).unwrap();
        let s = cell_to_string(cell);
        let parsed = string_to_cell(&s).unwrap();
        assert_eq!(cell, parsed);
    }

    #[test]
    fn test_boundary_polygon() {
        let cell = lat_lng_to_cell(48.8584, 2.2945, 9).unwrap();
        let boundary = cell_to_boundary(cell).unwrap();
        // Hexagon: 6 vertices + 1 closed point = 7 points
        assert!(boundary.len() == 7 || boundary.len() == 6);
        assert_eq!(boundary.first(), boundary.last());
    }

    #[test]
    fn test_spatio_temporal_point() {
        let pt = SpatioTemporalPoint::new("courier_42", 48.8584, 2.2945, 1_700_000_000_000, 9).unwrap();
        assert_eq!(pt.entity_id, "courier_42");
        assert!(is_valid(pt.h3_index));
        assert_eq!(time_bucket(pt.timestamp, 60_000), 1_699_999_980_000);
    }
}
