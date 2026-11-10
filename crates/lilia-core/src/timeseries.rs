#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::similar_names,
    clippy::if_not_else
)]

use serde::{Deserialize, Serialize};

use crate::{ErrorCode, LiliaError, Result};

/// A single timestamped floating-point observation for time-series.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TimeSeriesStats {
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub sum: f64,
    pub count: u64,
}

/// Aggregation functions for downsampling high-frequency series into wider buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
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
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TimeSeriesRetentionRule {
    /// Target bucket resolution in milliseconds (e.g. 0 for raw, 60_000 for 1 minute, 3_600_000 for 1 hour).
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeSeriesLifecyclePolicy {
    /// Series ID or pattern (e.g. "heart_rate", "sensor:*", or "*" for global default).
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
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

/// Bit-level writer for packing variable-length bit sequences.
#[derive(Debug, Default)]
pub struct BitWriter {
    buffer: Vec<u8>,
    current_byte: u8,
    bit_count: u8,
}

impl BitWriter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buffer: Vec::with_capacity(capacity),
            current_byte: 0,
            bit_count: 0,
        }
    }

    pub fn write_bit(&mut self, bit: bool) {
        if bit {
            self.current_byte |= 1 << (7 - self.bit_count);
        }
        self.bit_count += 1;
        if self.bit_count == 8 {
            self.buffer.push(self.current_byte);
            self.current_byte = 0;
            self.bit_count = 0;
        }
    }

    pub fn write_bits(&mut self, value: u64, num_bits: u8) {
        if num_bits == 0 {
            return;
        }
        for i in (0..num_bits).rev() {
            let bit = ((value >> i) & 1) != 0;
            self.write_bit(bit);
        }
    }

    #[must_use]
    pub fn finish(mut self) -> Vec<u8> {
        if self.bit_count > 0 {
            self.buffer.push(self.current_byte);
        }
        self.buffer
    }
}

/// Bit-level reader for unpacking variable-length bit sequences.
#[derive(Debug)]
pub struct BitReader<'a> {
    bytes: &'a [u8],
    byte_index: usize,
    bit_index: u8,
}

impl<'a> BitReader<'a> {
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            byte_index: 0,
            bit_index: 0,
        }
    }

    pub fn read_bit(&mut self) -> Option<bool> {
        if self.byte_index >= self.bytes.len() {
            return None;
        }
        let bit = (self.bytes[self.byte_index] >> (7 - self.bit_index)) & 1 != 0;
        self.bit_index += 1;
        if self.bit_index == 8 {
            self.bit_index = 0;
            self.byte_index += 1;
        }
        Some(bit)
    }

    pub fn read_bits(&mut self, num_bits: u8) -> Option<u64> {
        if num_bits == 0 {
            return Some(0);
        }
        let mut value = 0u64;
        for _ in 0..num_bits {
            let bit = self.read_bit()?;
            value = (value << 1) | u64::from(bit);
        }
        Some(value)
    }
}

/// Compresses a slice of `TimeSeriesPoint` using the Gorilla Delta-of-Delta
/// and float XOR compression algorithms.
///
/// # Errors
/// Returns [`LiliaError`] if the points slice exceeds maximum allowed capacity.
pub fn compress_timeseries(points: &[TimeSeriesPoint]) -> Result<Vec<u8>> {
    if points.is_empty() {
        return Ok(Vec::new());
    }

    let point_count = u32::try_from(points.len()).map_err(|_| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            "too many points in single timeseries chunk",
            false,
        )
    })?;

    // Estimate capacity: typically ~1.5 - 2 bytes per point for regular sensor streams
    let mut writer = BitWriter::with_capacity(points.len() * 2 + 32);

    // Header: point count
    writer.write_bits(u64::from(point_count), 32);

    // First point
    let t0 = points[0].timestamp;
    let v0 = points[0].value;
    writer.write_bits(t0 as u64, 64);
    writer.write_bits(v0.to_bits(), 64);

    if points.len() == 1 {
        return Ok(writer.finish());
    }

    // Second point: store initial delta D0
    let mut prev_t = points[1].timestamp;
    let mut prev_delta = prev_t - t0;
    writer.write_bits(prev_delta as u64, 64);

    let mut prev_val_bits = points[0].value.to_bits();
    let mut prev_lz = 0u8;
    let mut prev_tz = 0u8;
    let mut has_prev_window = false;

    // Encode value for second point
    encode_float_xor(
        &mut writer,
        points[1].value.to_bits(),
        prev_val_bits,
        &mut prev_lz,
        &mut prev_tz,
        &mut has_prev_window,
    );
    prev_val_bits = points[1].value.to_bits();

    // Encode subsequent points (i >= 2)
    for p in &points[2..] {
        // --- 1. Delta-of-Delta Timestamp Compression ---
        let delta = p.timestamp - prev_t;
        let dod = delta - prev_delta;

        if dod == 0 {
            writer.write_bit(false); // 0
        } else if (-63..=64).contains(&dod) {
            writer.write_bits(0b10, 2);
            writer.write_bits((dod + 63) as u64, 7);
        } else if (-255..=256).contains(&dod) {
            writer.write_bits(0b110, 3);
            writer.write_bits((dod + 255) as u64, 9);
        } else if (-2047..=2048).contains(&dod) {
            writer.write_bits(0b1110, 4);
            writer.write_bits((dod + 2047) as u64, 12);
        } else if let Ok(dod_i32) = i32::try_from(dod) {
            writer.write_bits(0b11110, 5);
            writer.write_bits(dod_i32 as u32 as u64, 32);
        } else {
            writer.write_bits(0b11111, 5);
            writer.write_bits(dod as u64, 64);
        }

        prev_t = p.timestamp;
        prev_delta = delta;

        // --- 2. Floating-Point XOR Compression ---
        encode_float_xor(
            &mut writer,
            p.value.to_bits(),
            prev_val_bits,
            &mut prev_lz,
            &mut prev_tz,
            &mut has_prev_window,
        );
        prev_val_bits = p.value.to_bits();
    }

    Ok(writer.finish())
}

#[inline]
fn encode_float_xor(
    writer: &mut BitWriter,
    curr_bits: u64,
    prev_bits: u64,
    prev_lz: &mut u8,
    prev_tz: &mut u8,
    has_prev_window: &mut bool,
) {
    let xor = curr_bits ^ prev_bits;
    if xor == 0 {
        writer.write_bit(false); // 0
        return;
    }

    writer.write_bit(true); // 1

    let mut lz = xor.leading_zeros() as u8;
    let tz = xor.trailing_zeros() as u8;

    if lz > 31 {
        lz = 31;
    }

    if *has_prev_window && lz >= *prev_lz && tz >= *prev_tz {
        writer.write_bit(false); // 0: reuse previous window
        let len = 64 - *prev_lz - *prev_tz;
        let meaningful = (xor >> *prev_tz) & mask_for_length(len);
        writer.write_bits(meaningful, len);
    } else {
        writer.write_bit(true); // 1: new window
        writer.write_bits(u64::from(lz), 5); // 5 bits for leading zeros
        let len = 64 - lz - tz;
        writer.write_bits(u64::from(len - 1), 6); // 6 bits for (length - 1)
        let meaningful = (xor >> tz) & mask_for_length(len);
        writer.write_bits(meaningful, len);

        *prev_lz = lz;
        *prev_tz = tz;
        *has_prev_window = true;
    }
}

#[inline]
const fn mask_for_length(len: u8) -> u64 {
    if len >= 64 {
        u64::MAX
    } else {
        (1u64 << len) - 1
    }
}

/// Decompresses a binary Gorilla-encoded payload back into points.
///
/// # Errors
/// Returns [`LiliaError`] if the payload is corrupted or truncated.
pub fn decompress_timeseries(bytes: &[u8]) -> Result<Vec<TimeSeriesPoint>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }

    let mut reader = BitReader::new(bytes);

    let point_count = reader.read_bits(32).ok_or_else(|| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            "corrupted timeseries payload: missing point count",
            false,
        )
    })?;

    let count = point_count as usize;
    if count == 0 {
        return Ok(Vec::new());
    }

    let mut points = Vec::with_capacity(count);

    // Read first point
    let t0_raw = reader.read_bits(64).ok_or_else(|| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            "corrupted timeseries payload: truncated t0",
            false,
        )
    })?;
    let mut curr_t = t0_raw as i64;

    let v0_raw = reader.read_bits(64).ok_or_else(|| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            "corrupted timeseries payload: truncated v0",
            false,
        )
    })?;
    let mut curr_v_bits = v0_raw;
    points.push(TimeSeriesPoint::new(curr_t, f64::from_bits(curr_v_bits)));

    if count == 1 {
        return Ok(points);
    }

    // Read initial delta D0
    let d0_raw = reader.read_bits(64).ok_or_else(|| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            "corrupted timeseries payload: truncated delta0",
            false,
        )
    })?;
    let mut prev_delta = d0_raw as i64;
    curr_t += prev_delta;

    let mut prev_lz = 0u8;
    let mut prev_tz = 0u8;

    // Decode value for second point
    curr_v_bits = decode_float_xor(&mut reader, curr_v_bits, &mut prev_lz, &mut prev_tz)?;
    points.push(TimeSeriesPoint::new(curr_t, f64::from_bits(curr_v_bits)));

    // Decode remaining points
    for _ in 2..count {
        // 1. Decode Timestamp Delta-of-Delta
        let first_bit = reader.read_bit().ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "corrupted timeseries payload: truncated dod bit",
                false,
            )
        })?;

        let dod: i64 = if !first_bit {
            0
        } else {
            let second_bit = reader.read_bit().ok_or_else(|| {
                LiliaError::new(
                    ErrorCode::InvalidInput,
                    "corrupted timeseries payload: truncated dod tag",
                    false,
                )
            })?;
            if !second_bit {
                // '10' -> 7 bits
                let bits = reader.read_bits(7).ok_or_else(|| {
                    LiliaError::new(
                        ErrorCode::InvalidInput,
                        "corrupted timeseries payload: truncated 7-bit dod",
                        false,
                    )
                })?;
                (bits as i64) - 63
            } else {
                let third_bit = reader.read_bit().ok_or_else(|| {
                    LiliaError::new(
                        ErrorCode::InvalidInput,
                        "corrupted timeseries payload: truncated dod tag",
                        false,
                    )
                })?;
                if !third_bit {
                    // '110' -> 9 bits
                    let bits = reader.read_bits(9).ok_or_else(|| {
                        LiliaError::new(
                            ErrorCode::InvalidInput,
                            "corrupted timeseries payload: truncated 9-bit dod",
                            false,
                        )
                    })?;
                    (bits as i64) - 255
                } else {
                    let fourth_bit = reader.read_bit().ok_or_else(|| {
                        LiliaError::new(
                            ErrorCode::InvalidInput,
                            "corrupted timeseries payload: truncated dod tag",
                            false,
                        )
                    })?;
                    if !fourth_bit {
                        // '1110' -> 12 bits
                        let bits = reader.read_bits(12).ok_or_else(|| {
                            LiliaError::new(
                                ErrorCode::InvalidInput,
                                "corrupted timeseries payload: truncated 12-bit dod",
                                false,
                            )
                        })?;
                        (bits as i64) - 2047
                    } else {
                        let fifth_bit = reader.read_bit().ok_or_else(|| {
                            LiliaError::new(
                                ErrorCode::InvalidInput,
                                "corrupted timeseries payload: truncated dod tag",
                                false,
                            )
                        })?;
                        if !fifth_bit {
                            // '11110' -> 32 bits
                            let bits = reader.read_bits(32).ok_or_else(|| {
                                LiliaError::new(
                                    ErrorCode::InvalidInput,
                                    "corrupted timeseries payload: truncated 32-bit dod",
                                    false,
                                )
                            })?;
                            i64::from(bits as u32 as i32)
                        } else {
                            // '11111' -> 64 bits
                            let bits = reader.read_bits(64).ok_or_else(|| {
                                LiliaError::new(
                                    ErrorCode::InvalidInput,
                                    "corrupted timeseries payload: truncated 64-bit dod",
                                    false,
                                )
                            })?;
                            bits as i64
                        }
                    }
                }
            }
        };

        let delta = prev_delta + dod;
        curr_t += delta;
        prev_delta = delta;

        // 2. Decode Floating-point XOR
        curr_v_bits = decode_float_xor(&mut reader, curr_v_bits, &mut prev_lz, &mut prev_tz)?;
        points.push(TimeSeriesPoint::new(curr_t, f64::from_bits(curr_v_bits)));
    }

    Ok(points)
}

#[inline]
fn decode_float_xor(
    reader: &mut BitReader<'_>,
    prev_bits: u64,
    prev_lz: &mut u8,
    prev_tz: &mut u8,
) -> Result<u64> {
    let bit = reader.read_bit().ok_or_else(|| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            "corrupted timeseries payload: truncated value tag",
            false,
        )
    })?;

    if !bit {
        // Value identical to previous
        return Ok(prev_bits);
    }

    let is_new_window = reader.read_bit().ok_or_else(|| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            "corrupted timeseries payload: truncated window tag",
            false,
        )
    })?;

    let (len, tz) = if is_new_window {
        let lz = reader.read_bits(5).ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "corrupted timeseries payload: truncated lz",
                false,
            )
        })?;
        let len_raw = reader.read_bits(6).ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "corrupted timeseries payload: truncated len",
                false,
            )
        })?;

        let lz = lz as u8;
        let len = (len_raw as u8) + 1;
        let tz = 64 - lz - len;

        *prev_lz = lz;
        *prev_tz = tz;
        (len, tz)
    } else {
        let lz = *prev_lz;
        let tz = *prev_tz;
        let len = 64 - lz - tz;
        (len, tz)
    };

    let meaningful = reader.read_bits(len).ok_or_else(|| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            "corrupted timeseries payload: truncated meaningful bits",
            false,
        )
    })?;

    let xor = meaningful << tz;
    Ok(prev_bits ^ xor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bit_writer_reader_roundtrip() {
        let mut writer = BitWriter::new();
        writer.write_bit(true);
        writer.write_bit(false);
        writer.write_bits(0b1011, 4);
        writer.write_bits(42, 8);
        writer.write_bits(0xDEAD_BEEF_CAFE_BABE, 64);
        let bytes = writer.finish();

        let mut reader = BitReader::new(&bytes);
        assert_eq!(reader.read_bit(), Some(true));
        assert_eq!(reader.read_bit(), Some(false));
        assert_eq!(reader.read_bits(4), Some(0b1011));
        assert_eq!(reader.read_bits(8), Some(42));
        assert_eq!(reader.read_bits(64), Some(0xDEAD_BEEF_CAFE_BABE));
        assert_eq!(reader.read_bit(), Some(false)); // padding bit
    }

    #[test]
    fn test_empty_and_single_point() {
        let empty: [TimeSeriesPoint; 0] = [];
        let comp = compress_timeseries(&empty).unwrap();
        assert!(comp.is_empty());
        let decomp = decompress_timeseries(&comp).unwrap();
        assert!(decomp.is_empty());

        let single = [TimeSeriesPoint::new(1_700_000_000_000, 72.5)];
        let comp = compress_timeseries(&single).unwrap();
        let decomp = decompress_timeseries(&comp).unwrap();
        assert_eq!(decomp, single);
    }

    #[test]
    fn test_heart_rate_compression_and_ratio() {
        // Simulate 1000 seconds (1 Hz) of typical resting & running heart rate
        let mut points = Vec::with_capacity(1000);
        let mut hr = 72.0;
        let start_time = 1_700_000_000_000i64;

        for i in 0..1000 {
            // Slight wander in heart rate (+0.1, -0.1, or steady)
            if i % 10 == 0 {
                hr += 0.5;
            } else if i % 25 == 0 {
                hr -= 0.3;
            }
            points.push(TimeSeriesPoint::new(start_time + (i * 1000), hr));
        }

        let compressed = compress_timeseries(&points).unwrap();
        let decompressed = decompress_timeseries(&compressed).unwrap();

        assert_eq!(points.len(), decompressed.len());
        for (orig, dec) in points.iter().zip(&decompressed) {
            assert_eq!(orig.timestamp, dec.timestamp);
            assert!(
                (orig.value - dec.value).abs() < f64::EPSILON,
                "values must match exact IEEE float bitwise"
            );
        }

        let raw_size = points.len() * 16; // 16 bytes per (timestamp, f64)
        let compressed_size = compressed.len();
        let ratio = (raw_size as f64) / (compressed_size as f64);

        // Expect > 8x to 12x compression ratio on regular 1 Hz sensor
        assert!(
            ratio > 8.0,
            "compression ratio must exceed 8x (got {ratio:.2}x, raw={raw_size}B, comp={compressed_size}B)"
        );
    }

    #[test]
    fn test_accelerometer_50hz_compression() {
        // Simulate 50 Hz accelerometer data (20 ms intervals) with oscillating sine wave
        let mut points = Vec::with_capacity(2500);
        let start_time = 1_700_000_000_000i64;

        for i in 0..2500 {
            let t = start_time + (i * 20); // exactly 20 ms
                                           // Realistic smartwatch IMU discretization (e.g. 0.02 m/s² resolution)
            let raw_sin = (i as f64 * 0.05).sin() * 9.81;
            let val = (raw_sin * 50.0).round() / 50.0;
            points.push(TimeSeriesPoint::new(t, val));
        }

        let compressed = compress_timeseries(&points).unwrap();
        let decompressed = decompress_timeseries(&compressed).unwrap();

        assert_eq!(points.len(), decompressed.len());
        for (orig, dec) in points.iter().zip(&decompressed) {
            assert_eq!(orig.timestamp, dec.timestamp);
            assert_eq!(orig.value.to_bits(), dec.value.to_bits());
        }

        let raw_size = points.len() * 16;
        let compressed_size = compressed.len();
        let ratio = (raw_size as f64) / (compressed_size as f64);
        assert!(
            ratio > 1.8,
            "accelerometer compression ratio should be at least 1.8x (got {ratio:.2}x)"
        );
    }

    #[test]
    fn test_variable_interval_and_large_dod() {
        // Testing irregularly sampled points with large time jumps and special float values
        let points = vec![
            TimeSeriesPoint::new(1_000, 1.0),
            TimeSeriesPoint::new(2_000, 2.0),
            TimeSeriesPoint::new(2_050, 2.5), // small dod
            TimeSeriesPoint::new(10_000, std::f64::consts::PI), // medium dod
            TimeSeriesPoint::new(1_000_000, -100.0), // very large dod (> 32-bit jump)
            TimeSeriesPoint::new(2_000_000, 0.0),
            TimeSeriesPoint::new(3_000_000, f64::INFINITY),
        ];

        let compressed = compress_timeseries(&points).unwrap();
        let decompressed = decompress_timeseries(&compressed).unwrap();

        assert_eq!(points.len(), decompressed.len());
        for (orig, dec) in points.iter().zip(&decompressed) {
            assert_eq!(orig.timestamp, dec.timestamp);
            assert_eq!(orig.value.to_bits(), dec.value.to_bits());
        }
    }

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
