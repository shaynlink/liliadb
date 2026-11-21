#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::similar_names,
    clippy::if_not_else
)]

use serde::{Deserialize, Serialize};

mod gorilla;

pub use gorilla::{compress_timeseries, decompress_timeseries, BitReader, BitWriter};

/// A single timestamped floating-point observation for time-series.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct TimeSeriesStats {
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub sum: f64,
    pub count: u64,
}

/// Aggregation functions for downsampling high-frequency series into wider buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
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
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
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

/// Aggregates a slice of points into fixed-width buckets using the given aggregation method.
/// The timestamp of each downsampled point is aligned to the start of the bucket:
/// `(timestamp / bucket_ms) * bucket_ms`.
#[must_use]
pub fn downsample_points(
    points: &[TimeSeriesPoint],
    bucket_ms: i64,
    agg: DownsampleAggregation,
) -> Vec<TimeSeriesPoint> {
    if points.is_empty() || bucket_ms <= 0 {
        return points.to_vec();
    }

    let mut sorted_points = points.to_vec();
    sorted_points.sort_by_key(|p| p.timestamp);

    let mut downsampled = Vec::new();
    let mut current_bucket: Option<i64> = None;
    let mut bucket_min = f64::INFINITY;
    let mut bucket_max = f64::NEG_INFINITY;
    let mut bucket_sum = 0.0;
    let mut bucket_first = 0.0;
    let mut bucket_last = 0.0;
    let mut bucket_count = 0u64;

    for p in sorted_points {
        let b = (p.timestamp.div_euclid(bucket_ms)) * bucket_ms;
        if Some(b) != current_bucket {
            if let Some(prev_b) = current_bucket {
                if bucket_count > 0 {
                    let val = match agg {
                        DownsampleAggregation::Avg => bucket_sum / (bucket_count as f64),
                        DownsampleAggregation::Min => bucket_min,
                        DownsampleAggregation::Max => bucket_max,
                        DownsampleAggregation::Sum => bucket_sum,
                        DownsampleAggregation::First => bucket_first,
                        DownsampleAggregation::Last => bucket_last,
                        DownsampleAggregation::Count => bucket_count as f64,
                    };
                    downsampled.push(TimeSeriesPoint::new(prev_b, val));
                }
            }
            current_bucket = Some(b);
            bucket_min = p.value;
            bucket_max = p.value;
            bucket_sum = p.value;
            bucket_first = p.value;
            bucket_last = p.value;
            bucket_count = 1;
        } else {
            if p.value < bucket_min {
                bucket_min = p.value;
            }
            if p.value > bucket_max {
                bucket_max = p.value;
            }
            bucket_sum += p.value;
            bucket_last = p.value;
            bucket_count += 1;
        }
    }

    if let Some(last_b) = current_bucket {
        if bucket_count > 0 {
            let val = match agg {
                DownsampleAggregation::Avg => bucket_sum / (bucket_count as f64),
                DownsampleAggregation::Min => bucket_min,
                DownsampleAggregation::Max => bucket_max,
                DownsampleAggregation::Sum => bucket_sum,
                DownsampleAggregation::First => bucket_first,
                DownsampleAggregation::Last => bucket_last,
                DownsampleAggregation::Count => bucket_count as f64,
            };
            downsampled.push(TimeSeriesPoint::new(last_b, val));
        }
    }

    downsampled
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

    #[test]
    fn test_downsample_aggregations() {
        // 6 points across two 60-second buckets:
        // Bucket 0..60000: timestamps 10_000, 20_000, 30_000 with values 10.0, 20.0, 30.0
        // Bucket 60000..120000: timestamps 70_000, 80_000, 90_000 with values 40.0, 50.0, 60.0
        let points = vec![
            TimeSeriesPoint::new(10_000, 10.0),
            TimeSeriesPoint::new(20_000, 20.0),
            TimeSeriesPoint::new(30_000, 30.0),
            TimeSeriesPoint::new(70_000, 40.0),
            TimeSeriesPoint::new(80_000, 50.0),
            TimeSeriesPoint::new(90_000, 60.0),
        ];

        let bucket_ms = 60_000;

        // Avg
        let avg = downsample_points(&points, bucket_ms, DownsampleAggregation::Avg);
        assert_eq!(avg.len(), 2);
        assert_eq!(avg[0], TimeSeriesPoint::new(0, 20.0));
        assert_eq!(avg[1], TimeSeriesPoint::new(60_000, 50.0));

        // Min
        let min = downsample_points(&points, bucket_ms, DownsampleAggregation::Min);
        assert_eq!(min[0], TimeSeriesPoint::new(0, 10.0));
        assert_eq!(min[1], TimeSeriesPoint::new(60_000, 40.0));

        // Max
        let max = downsample_points(&points, bucket_ms, DownsampleAggregation::Max);
        assert_eq!(max[0], TimeSeriesPoint::new(0, 30.0));
        assert_eq!(max[1], TimeSeriesPoint::new(60_000, 60.0));

        // Sum
        let sum = downsample_points(&points, bucket_ms, DownsampleAggregation::Sum);
        assert_eq!(sum[0], TimeSeriesPoint::new(0, 60.0));
        assert_eq!(sum[1], TimeSeriesPoint::new(60_000, 150.0));

        // First
        let first = downsample_points(&points, bucket_ms, DownsampleAggregation::First);
        assert_eq!(first[0], TimeSeriesPoint::new(0, 10.0));
        assert_eq!(first[1], TimeSeriesPoint::new(60_000, 40.0));

        // Last
        let last = downsample_points(&points, bucket_ms, DownsampleAggregation::Last);
        assert_eq!(last[0], TimeSeriesPoint::new(0, 30.0));
        assert_eq!(last[1], TimeSeriesPoint::new(60_000, 60.0));

        // Count
        let count = downsample_points(&points, bucket_ms, DownsampleAggregation::Count);
        assert_eq!(count[0], TimeSeriesPoint::new(0, 3.0));
        assert_eq!(count[1], TimeSeriesPoint::new(60_000, 3.0));
    }
}
