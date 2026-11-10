#![allow(
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use lilia_core::{GeofenceMatch, GpsPoint, Result};

use crate::storage::validate_name;
use crate::Database;

impl Database {
    /// Registers a polygonal geofence into the spatial R*Tree index (`_lilia_geofences`).
    ///
    /// # Arguments
    /// * `id` - Unique identifier for the geofence (e.g. "central_park")
    /// * `name` - Human-readable label (e.g. "Central Park Track")
    /// * `category` - Classification tag (e.g. "running", "hazard", "home")
    /// * `polygon_json` - Valid geopoly JSON coordinate array `[[lon1, lat1], [lon2, lat2], ...]`
    /// * `metadata` - Optional JSON metadata
    pub fn geo_add_fence(
        &self,
        id: &str,
        name: &str,
        category: &str,
        polygon_json: &str,
        metadata: Option<&str>,
    ) -> Result<()> {
        validate_name("id", id)?;
        let writer = self.lock_writer()?;

        // If a geofence with this id exists, delete it first to keep id unique
        let mut del_stmt = writer.prepare("DELETE FROM _lilia_geofences WHERE id = ?1;")?;
        del_stmt.bind_text_static(1, id)?;
        del_stmt.step()?;
        del_stmt.reset()?;

        let mut stmt = writer.prepare(
            "INSERT INTO _lilia_geofences(_shape, id, name, category, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5);",
        )?;

        stmt.bind_text_static(1, polygon_json)?;
        stmt.bind_text_static(2, id)?;
        stmt.bind_text_static(3, name)?;
        stmt.bind_text_static(4, category)?;
        if let Some(meta) = metadata {
            stmt.bind_text_static(5, meta)?;
        } else {
            stmt.bind_null(5)?;
        }

        stmt.step()?;
        stmt.reset()?;
        Ok(())
    }

    /// Removes a geofence by ID. Returns `true` if a zone was deleted.
    pub fn geo_remove_fence(&self, id: &str) -> Result<bool> {
        validate_name("id", id)?;
        let writer = self.lock_writer()?;
        let mut stmt = writer.prepare("DELETE FROM _lilia_geofences WHERE id = ?1;")?;
        stmt.bind_text_static(1, id)?;
        stmt.step()?;
        stmt.reset()?;
        Ok(writer.changes() > 0)
    }

    /// Evaluates if a coordinate `(lon, lat)` is contained inside an ad-hoc polygon.
    /// Uses SQLite's native `geopoly_contains_point` function directly.
    pub fn geo_contains_point(&self, polygon_json: &str, lon: f64, lat: f64) -> Result<bool> {
        let conn = self.read_lock()?;
        let mut stmt = conn.prepare("SELECT geopoly_contains_point(?1, ?2, ?3);")?;
        stmt.bind_text_static(1, polygon_json)?;
        stmt.bind_double(2, lon)?;
        stmt.bind_double(3, lat)?;

        let is_inside = if stmt.step()? {
            stmt.column_int64(0) > 0
        } else {
            false
        };

        stmt.reset()?;
        Ok(is_inside)
    }

    /// Queries all registered geofences that contain the given point `(lon, lat)`.
    /// Accelerated by SQLite's native R*Tree spatial index on `_lilia_geofences`.
    pub fn geo_find_fences_for_point(&self, lon: f64, lat: f64) -> Result<Vec<GeofenceMatch>> {
        let conn = self.read_lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, category, metadata
             FROM _lilia_geofences
             WHERE geopoly_contains_point(_shape, ?1, ?2);",
        )?;

        stmt.bind_double(1, lon)?;
        stmt.bind_double(2, lat)?;

        let mut matches = Vec::new();
        while stmt.step()? {
            let id = stmt.column_text(0).to_string();
            let name = stmt.column_text(1).to_string();
            let category = stmt.column_text(2).to_string();
            let metadata = if stmt.column_is_null(3) {
                None
            } else {
                Some(stmt.column_text(3).to_string())
            };

            matches.push(GeofenceMatch {
                id,
                name,
                category,
                metadata,
            });
        }

        stmt.reset()?;
        Ok(matches)
    }

    /// Records GPS fixes into `_lilia_gps_points` and checks whether the latest
    /// fix falls within any active geofence.
    pub fn geo_record_gps_points(
        &self,
        track_id: &str,
        points: &[GpsPoint],
    ) -> Result<Vec<GeofenceMatch>> {
        validate_name("track_id", track_id)?;
        if points.is_empty() {
            return Ok(Vec::new());
        }

        let writer = self.lock_writer()?;
        writer.exec("BEGIN IMMEDIATE;")?;

        let mut stmt = writer.prepare(
            "INSERT OR REPLACE INTO _lilia_gps_points(
                track_id, timestamp, latitude, longitude, altitude, speed, accuracy
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7);",
        )?;

        for p in points {
            stmt.reset()?;
            stmt.bind_text_static(1, track_id)?;
            stmt.bind_int64(2, p.timestamp)?;
            stmt.bind_double(3, p.latitude)?;
            stmt.bind_double(4, p.longitude)?;

            if let Some(alt) = p.altitude {
                stmt.bind_double(5, alt)?;
            } else {
                stmt.bind_null(5)?;
            }

            if let Some(spd) = p.speed {
                stmt.bind_double(6, spd)?;
            } else {
                stmt.bind_null(6)?;
            }

            if let Some(acc) = p.accuracy {
                stmt.bind_double(7, acc)?;
            } else {
                stmt.bind_null(7)?;
            }

            stmt.step()?;
        }

        stmt.reset()?;
        writer.exec("COMMIT;")?;

        // Evaluate fences against the most recent point
        let latest = &points[points.len() - 1];
        self.geo_find_fences_for_point(latest.longitude, latest.latitude)
    }

    /// Retrieves recorded GPS fixes for a track within an optional time window.
    pub fn geo_get_track_points(
        &self,
        track_id: &str,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<Vec<GpsPoint>> {
        validate_name("track_id", track_id)?;
        let conn = self.read_lock()?;

        let mut stmt = conn.prepare(
            "SELECT timestamp, latitude, longitude, altitude, speed, accuracy
             FROM _lilia_gps_points
             WHERE track_id = ?1
               AND (?2 IS NULL OR timestamp >= ?2)
               AND (?3 IS NULL OR timestamp <= ?3)
             ORDER BY timestamp ASC;",
        )?;

        stmt.bind_text_static(1, track_id)?;
        if let Some(st) = start_time {
            stmt.bind_int64(2, st)?;
        } else {
            stmt.bind_null(2)?;
        }
        if let Some(et) = end_time {
            stmt.bind_int64(3, et)?;
        } else {
            stmt.bind_null(3)?;
        }

        let mut points = Vec::new();
        while stmt.step()? {
            let timestamp = stmt.column_int64(0);
            let latitude = stmt.column_double(1);
            let longitude = stmt.column_double(2);
            let altitude = if stmt.column_is_null(3) {
                None
            } else {
                Some(stmt.column_double(3))
            };
            let speed = if stmt.column_is_null(4) {
                None
            } else {
                Some(stmt.column_double(4))
            };
            let accuracy = if stmt.column_is_null(5) {
                None
            } else {
                Some(stmt.column_double(5))
            };

            points.push(GpsPoint {
                timestamp,
                latitude,
                longitude,
                altitude,
                speed,
                accuracy,
            });
        }

        stmt.reset()?;
        Ok(points)
    }

    /// Computes total cumulative distance (in meters) along a track using
    /// Haversine distance and SQL window functions.
    pub fn geo_track_distance_meters(&self, track_id: &str) -> Result<f64> {
        validate_name("track_id", track_id)?;
        let conn = self.read_lock()?;
        let mut stmt = conn.prepare(
            "WITH ordered AS (
                SELECT latitude, longitude,
                       LAG(latitude) OVER (ORDER BY timestamp) AS prev_lat,
                       LAG(longitude) OVER (ORDER BY timestamp) AS prev_lon
                FROM _lilia_gps_points
                WHERE track_id = ?1
             )
             SELECT COALESCE(SUM(lilia_geo_distance(prev_lat, prev_lon, latitude, longitude)), 0.0)
             FROM ordered
             WHERE prev_lat IS NOT NULL;",
        )?;

        stmt.bind_text_static(1, track_id)?;

        let dist = if stmt.step()? {
            stmt.column_double(0)
        } else {
            0.0
        };

        stmt.reset()?;
        Ok(dist)
    }
}
