//! Polygons of longitude and latitude, as geofences use them.

use lilia_core::geo::coords_to_geopoly_json;

use crate::error::{invalid, Result};

/// A simple polygon of `(longitude, latitude)` vertices in degrees, its edges straight in that
/// plane (so a polygon cannot cross the antimeridian). The closing vertex is implicit.
#[derive(Debug, Clone, PartialEq)]
pub struct Polygon {
    vertices: Vec<(f64, f64)>,
}

/// Longitude and latitude bounds: `(min_lon, min_lat, max_lon, max_lat)`.
pub type Bounds = (f64, f64, f64, f64);

/// Checks a coordinate.
///
/// # Errors
///
/// [`crate::Error::InvalidArgument`] outside `[-180, 180]` and `[-90, 90]`.
pub fn validate_position(lon: f64, lat: f64) -> Result<()> {
    if !(-180.0..=180.0).contains(&lon) || !(-90.0..=90.0).contains(&lat) {
        return Err(invalid(format!(
            "({lon}, {lat}) is not a longitude in [-180, 180] and a latitude in [-90, 90]"
        )));
    }
    Ok(())
}

impl Polygon {
    /// A polygon of `vertices`; a last vertex equal to the first closes it and is dropped.
    ///
    /// # Errors
    ///
    /// [`crate::Error::InvalidArgument`] for fewer than 3 vertices, a vertex out of range, or
    /// no area.
    pub fn new(vertices: &[(f64, f64)]) -> Result<Self> {
        let mut vertices = vertices.to_vec();
        if vertices.len() > 1 && vertices.first() == vertices.last() {
            vertices.pop();
        }
        if vertices.len() < 3 {
            return Err(invalid("a polygon needs at least 3 vertices"));
        }
        for &(lon, lat) in &vertices {
            validate_position(lon, lat)?;
        }
        let polygon = Self { vertices };
        if polygon.area() == 0.0 {
            return Err(invalid("a polygon must enclose an area"));
        }
        Ok(polygon)
    }

    /// A polygon from geopoly JSON: `[[lon, lat], ...]`.
    ///
    /// # Errors
    ///
    /// [`crate::Error::InvalidArgument`] for JSON of another shape; as [`Polygon::new`].
    pub fn parse(json: &str) -> Result<Self> {
        let coordinates: Vec<[f64; 2]> =
            serde_json::from_str(json).map_err(|e| invalid(format!("polygon JSON: {e}")))?;
        Self::new(&coordinates.iter().map(|c| (c[0], c[1])).collect::<Vec<_>>())
    }

    /// The polygon as geopoly JSON, closed.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut closed = self.vertices.clone();
        closed.extend(self.vertices.first().copied());
        coords_to_geopoly_json(&closed)
    }

    #[must_use]
    pub fn vertices(&self) -> &[(f64, f64)] {
        &self.vertices
    }

    /// Area in square degrees (the shoelace formula).
    fn area(&self) -> f64 {
        let n = self.vertices.len();
        let twice: f64 = (0..n)
            .map(|i| {
                let (a, b) = (self.vertices[i], self.vertices[(i + 1) % n]);
                a.0 * b.1 - b.0 * a.1
            })
            .sum();
        (twice / 2.0).abs()
    }

    #[must_use]
    pub fn bounds(&self) -> Bounds {
        self.vertices.iter().fold(
            (
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ),
            |(x0, y0, x1, y1), &(x, y)| (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
        )
    }

    /// Whether `(lon, lat)` is inside the polygon or on its boundary (even-odd rule).
    #[must_use]
    pub fn contains(&self, lon: f64, lat: f64) -> bool {
        let n = self.vertices.len();
        let mut inside = false;
        for i in 0..n {
            let (a, b) = (self.vertices[i], self.vertices[(i + 1) % n]);
            if on_segment(a, b, (lon, lat)) {
                return true;
            }
            if (a.1 > lat) != (b.1 > lat) {
                let crossing = a.0 + (lat - a.1) * (b.0 - a.0) / (b.1 - a.1);
                if lon < crossing {
                    inside = !inside;
                }
            }
        }
        inside
    }
}

fn on_segment(a: (f64, f64), b: (f64, f64), p: (f64, f64)) -> bool {
    let cross = (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
    cross.abs() <= 1e-12
        && p.0 >= a.0.min(b.0)
        && p.0 <= a.0.max(b.0)
        && p.1 >= a.1.min(b.1)
        && p.1 <= a.1.max(b.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polygons_validate_and_contain_points() {
        let square =
            Polygon::new(&[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0), (0.0, 0.0)]).unwrap();
        assert_eq!(square.vertices().len(), 4, "the closing vertex is dropped");
        assert!(square.contains(1.0, 1.0));
        assert!(square.contains(2.0, 1.0), "the boundary is inside");
        assert!(square.contains(0.0, 0.0));
        assert!(!square.contains(2.1, 1.0));
        assert!(!square.contains(-0.1, -0.1));
        assert_eq!(square.bounds(), (0.0, 0.0, 2.0, 2.0));
        assert_eq!(Polygon::parse(&square.to_json()).unwrap(), square);

        // A concave "C": the notch is outside.
        let c = Polygon::new(&[
            (0.0, 0.0),
            (3.0, 0.0),
            (3.0, 1.0),
            (1.0, 1.0),
            (1.0, 2.0),
            (3.0, 2.0),
            (3.0, 3.0),
            (0.0, 3.0),
        ])
        .unwrap();
        assert!(c.contains(0.5, 1.5));
        assert!(!c.contains(2.0, 1.5));
        assert!(c.contains(2.0, 0.5));

        assert!(Polygon::new(&[(0.0, 0.0), (1.0, 1.0)]).is_err());
        assert!(
            Polygon::new(&[(0.0, 0.0), (1.0, 1.0), (2.0, 2.0)]).is_err(),
            "no area"
        );
        assert!(Polygon::new(&[(0.0, 0.0), (181.0, 0.0), (0.0, 1.0)]).is_err());
        assert!(Polygon::new(&[(0.0, f64::NAN), (1.0, 0.0), (0.0, 1.0)]).is_err());
        assert!(Polygon::parse("[[0, 0], [1]]").is_err());
    }
}
