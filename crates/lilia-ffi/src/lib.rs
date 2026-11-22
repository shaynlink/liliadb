//! The native SDKs' binding of `LiliaDB`:
//!
//! - a typed `UniFFI` API ([`LiliaDatabase`]), from which the Swift and Kotlin bindings are
//!   generated (`uniffi-bindgen`);
//! - a C ABI ([`c_abi`]) for runtimes that call C directly (Flutter, React Native's JSI), which
//!   takes protocol operations and answers with JS frames.
//!
//! Both run the operations of [`lilia_protocol`] on the database's system session.

// reason: exported functions take their arguments owned, as `UniFFI` passes them, and their
// errors are Swift and Kotlin exceptions, documented by those SDKs.
#![allow(clippy::needless_pass_by_value, clippy::missing_errors_doc)]

uniffi::setup_scaffolding!();

pub mod c_abi;
mod database;
mod records;

pub use database::LiliaDatabase;
pub use records::*;

#[uniffi::export]
pub fn ts_compress_points(points: Vec<FfiTimeSeriesPoint>) -> Result<Vec<u8>, LiliaFfiError> {
    let core_points: Vec<lilia_core::TimeSeriesPoint> =
        points.into_iter().map(Into::into).collect();
    let comp = lilia_core::compress_timeseries(&core_points)?;
    Ok(comp)
}

#[uniffi::export]
pub fn ts_decompress_points(bytes: Vec<u8>) -> Result<Vec<FfiTimeSeriesPoint>, LiliaFfiError> {
    let core_points = lilia_core::decompress_timeseries(&bytes)?;
    Ok(core_points.into_iter().map(Into::into).collect())
}

#[uniffi::export]
#[must_use]
pub fn geo_haversine_distance(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    lilia_core::haversine_distance_meters(lat1, lon1, lat2, lon2)
}

#[uniffi::export]
#[must_use]
pub fn geo_create_circle_polygon(
    center_lon: f64,
    center_lat: f64,
    radius_meters: f64,
    n_vertices: u32,
) -> String {
    lilia_core::circle_to_geopoly_json(center_lon, center_lat, radius_meters, n_vertices as usize)
}
