use serde::{Deserialize, Serialize};

/// Earth radius in meters according to WGS84 mean radius.
pub const EARTH_RADIUS_METERS: f64 = 6_371_000.0;

/// A GPS location fix recorded during an outdoor activity (running, walking, etc.).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpsPoint {
    pub timestamp: i64,
    pub latitude: f64,
    pub longitude: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub altitude: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accuracy: Option<f64>,
}

impl GpsPoint {
    #[must_use]
    pub const fn new(timestamp: i64, latitude: f64, longitude: f64) -> Self {
        Self {
            timestamp,
            latitude,
            longitude,
            altitude: None,
            speed: None,
            accuracy: None,
        }
    }

    #[must_use]
    pub const fn with_details(
        timestamp: i64,
        latitude: f64,
        longitude: f64,
        altitude: Option<f64>,
        speed: Option<f64>,
        accuracy: Option<f64>,
    ) -> Self {
        Self {
            timestamp,
            latitude,
            longitude,
            altitude,
            speed,
            accuracy,
        }
    }
}

/// A matched geofence polygonal zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeofenceMatch {
    pub id: String,
    pub name: String,
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,
}

/// Calculates great-circle distance between two GPS coordinates in meters
/// using the Haversine formula.
#[must_use]
pub fn haversine_distance_meters(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let d_lat = (lat2 - lat1).to_radians();
    let d_lon = (lon2 - lon1).to_radians();

    let lat1_rad = lat1.to_radians();
    let lat2_rad = lat2.to_radians();

    let a =
        (d_lat / 2.0).sin().powi(2) + lat1_rad.cos() * lat2_rad.cos() * (d_lon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());

    EARTH_RADIUS_METERS * c
}

/// Generates a closed polygonal zone approximating a circle around a center coordinate.
/// Coordinates in geopoly format are Cartesian `[Longitude, Latitude]`.
///
/// # Arguments
/// * `center_lon` - Longitude of the center point (degrees)
/// * `center_lat` - Latitude of the center point (degrees)
/// * `radius_meters` - Radius of the circle in meters
/// * `n_vertices` - Number of vertices for the polygon approximation (clamped to 4..=128)
#[must_use]
pub fn circle_to_geopoly_json(
    center_lon: f64,
    center_lat: f64,
    radius_meters: f64,
    n_vertices: usize,
) -> String {
    let vertices = n_vertices.clamp(4, 128);

    // Approximate meter to degree conversion
    // 1 deg latitude ~ 111,320 m
    // 1 deg longitude ~ 111,320 m * cos(lat)
    let lat_rad = center_lat.to_radians();
    let cos_lat = lat_rad.cos().max(1e-6);

    let d_lat_deg = radius_meters / 111_320.0;
    let d_lon_deg = radius_meters / (111_320.0 * cos_lat);

    let mut points = Vec::with_capacity(vertices + 1);

    for i in 0..vertices {
        #[allow(clippy::cast_precision_loss)]
        let angle = (i as f64) * (2.0 * std::f64::consts::PI / (vertices as f64));
        let lon = center_lon + d_lon_deg * angle.cos();
        let lat = center_lat + d_lat_deg * angle.sin();
        points.push((lon, lat));
    }

    // Close the polygon by repeating the first vertex
    if let Some(&first) = points.first() {
        points.push(first);
    }

    coords_to_geopoly_json(&points)
}

/// Formats a list of `(longitude, latitude)` coordinate pairs into a geopoly-compatible JSON string.
#[must_use]
pub fn coords_to_geopoly_json(coords: &[(f64, f64)]) -> String {
    use std::fmt::Write;
    let mut json = String::from("[");
    for (i, &(lon, lat)) in coords.iter().enumerate() {
        if i > 0 {
            json.push_str(", ");
        }
        let _ = write!(json, "[{lon:.7}, {lat:.7}]");
    }
    json.push(']');
    json
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_haversine_distance() {
        // Paris Eiffel Tower (48.8584° N, 2.2945° E) to Arc de Triomphe (48.8738° N, 2.2950° E)
        let dist = haversine_distance_meters(48.8584, 2.2945, 48.8738, 2.2950);
        // Distance is ~1.71 km (1712 meters)
        assert!(
            (dist - 1712.0).abs() < 50.0,
            "distance should be ~1712m, got {dist:.1}m"
        );

        // Distance between identical points is 0
        let zero = haversine_distance_meters(48.8584, 2.2945, 48.8584, 2.2945);
        assert!(zero.abs() < 1e-6);
    }

    #[test]
    fn test_circle_to_geopoly_json() {
        let json = circle_to_geopoly_json(2.2945, 48.8584, 500.0, 8);
        assert!(json.starts_with('['));
        assert!(json.ends_with(']'));

        // Parse JSON to verify structure
        let parsed: Vec<Vec<f64>> = serde_json::from_str(&json).unwrap();
        // 8 vertices + 1 closed point = 9 points
        assert_eq!(parsed.len(), 9);
        assert_eq!(parsed[0], parsed[8], "polygon must be closed");

        // Verify distance of points from center is approximately radius (500m)
        for pt in &parsed {
            let dist = haversine_distance_meters(48.8584, 2.2945, pt[1], pt[0]);
            assert!(
                (dist - 500.0).abs() < 25.0,
                "radius distance should be ~500m, got {dist:.1}m"
            );
        }
    }
}
