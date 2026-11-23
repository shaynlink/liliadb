use serde::{Deserialize, Serialize};

mod gorilla;

pub use gorilla::{compress_timeseries, decompress_timeseries, BitReader, BitWriter};

/// A single timestamped floating-point observation for time-series.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct TimeSeriesPoint {
    pub timestamp: i64,
    pub value: f64,
}

impl TimeSeriesPoint {
    #[must_use]
    pub const fn new(timestamp: i64, value: f64) -> Self {
        Self { timestamp, value }
    }
}

/// Aggregated statistical summary for a time-series range.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct TimeSeriesStats {
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub sum: f64,
    pub count: u64,
}

/// Aggregation functions for downsampling high-frequency series into wider buckets.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    Default,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq))]
pub enum DownsampleAggregation {
    #[default]
    Avg,
    Min,
    Max,
    Sum,
    First,
    Last,
    Count,
}

/// A declarative rule within a time-series lifecycle policy.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct TimeSeriesRetentionRule {
    /// Target bucket resolution in milliseconds (e.g. 0 for raw, `60_000` for 1 minute, `3_600_000` for 1 hour).
    pub resolution_ms: i64,
    /// Duration in milliseconds to retain data at this resolution tier.
    /// `None` indicates indefinite retention.
    pub retention_ms: Option<i64>,
    /// Aggregation method used when downsampling to this resolution tier.
    pub aggregation: DownsampleAggregation,
}

impl TimeSeriesRetentionRule {
    #[must_use]
    pub const fn new(
        resolution_ms: i64,
        retention_ms: Option<i64>,
        aggregation: DownsampleAggregation,
    ) -> Self {
        Self {
            resolution_ms,
            retention_ms,
            aggregation,
        }
    }
}

/// Declarative lifecycle policy for time-series streams.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct TimeSeriesLifecyclePolicy {
    /// Series ID or pattern (e.g. "`heart_rate`", "sensor:*", or "*" for global default).
    pub series_pattern: String,
    /// Ordered retention and downsampling tiers.
    pub rules: Vec<TimeSeriesRetentionRule>,
}

impl TimeSeriesLifecyclePolicy {
    #[must_use]
    pub fn new(series_pattern: impl Into<String>, rules: Vec<TimeSeriesRetentionRule>) -> Self {
        Self {
            series_pattern: series_pattern.into(),
            rules,
        }
    }
}

/// Statistics returned after applying time-series lifecycle retention and downsampling.
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq))]
pub struct TimeSeriesRetentionStats {
    pub series_processed: u64,
    pub downsampled_points_created: u64,
    pub chunks_pruned: u64,
    pub bytes_reclaimed_approx: u64,
}

/// Matches a glob-like pattern against a series ID.
/// Supports `*` as catch-all, `prefix:*`, or exact equality.
#[must_use]
pub fn pattern_matches(pattern: &str, series_id: &str) -> bool {
    if pattern == "*" || pattern.is_empty() {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return series_id.starts_with(prefix);
    }
    pattern == series_id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pattern_matches() {
        assert!(pattern_matches("*", "any_series"));
        assert!(pattern_matches("", "any_series"));
        assert!(pattern_matches("sensor:*", "sensor:temp"));
        assert!(pattern_matches("sensor:*", "sensor:hr"));
        assert!(!pattern_matches("sensor:*", "device:123"));
        assert!(pattern_matches("exact_match", "exact_match"));
        assert!(!pattern_matches("exact_match", "other"));
    }
}
