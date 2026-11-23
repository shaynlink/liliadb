//! The Gorilla codec of time-series points: timestamps as deltas of deltas, values as XORs
//! with the previous value, packed bit by bit.

use super::TimeSeriesPoint;
use crate::{ErrorCode, LiliaError, Result};

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
    writer.write_bits(t0.cast_unsigned(), 64);
    writer.write_bits(v0.to_bits(), 64);

    if points.len() == 1 {
        return Ok(writer.finish());
    }

    // Second point: store initial delta D0. Deltas wrap, as in the decoder, so any timestamps
    // round-trip.
    let mut prev_t = points[1].timestamp;
    let mut prev_delta = prev_t.wrapping_sub(t0);
    writer.write_bits(prev_delta.cast_unsigned(), 64);

    let mut prev_val_bits = points[0].value.to_bits();
    let mut prev_leading = 0u8;
    let mut prev_trailing = 0u8;
    let mut has_prev_window = false;

    // Encode value for second point
    encode_float_xor(
        &mut writer,
        points[1].value.to_bits(),
        prev_val_bits,
        &mut prev_leading,
        &mut prev_trailing,
        &mut has_prev_window,
    );
    prev_val_bits = points[1].value.to_bits();

    // Encode subsequent points (i >= 2)
    for p in &points[2..] {
        // --- 1. Delta-of-Delta Timestamp Compression ---
        let delta = p.timestamp.wrapping_sub(prev_t);
        let dod = delta.wrapping_sub(prev_delta);

        if dod == 0 {
            writer.write_bit(false); // 0
        } else if (-63..=64).contains(&dod) {
            writer.write_bits(0b10, 2);
            writer.write_bits((dod + 63).cast_unsigned(), 7);
        } else if (-255..=256).contains(&dod) {
            writer.write_bits(0b110, 3);
            writer.write_bits((dod + 255).cast_unsigned(), 9);
        } else if (-2047..=2048).contains(&dod) {
            writer.write_bits(0b1110, 4);
            writer.write_bits((dod + 2047).cast_unsigned(), 12);
        } else if let Ok(dod_i32) = i32::try_from(dod) {
            writer.write_bits(0b11110, 5);
            writer.write_bits(u64::from(dod_i32.cast_unsigned()), 32);
        } else {
            writer.write_bits(0b11111, 5);
            writer.write_bits(dod.cast_unsigned(), 64);
        }

        prev_t = p.timestamp;
        prev_delta = delta;

        // --- 2. Floating-Point XOR Compression ---
        encode_float_xor(
            &mut writer,
            p.value.to_bits(),
            prev_val_bits,
            &mut prev_leading,
            &mut prev_trailing,
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
    prev_leading: &mut u8,
    prev_trailing: &mut u8,
    has_prev_window: &mut bool,
) {
    let xor = curr_bits ^ prev_bits;
    if xor == 0 {
        writer.write_bit(false); // 0
        return;
    }

    writer.write_bit(true); // 1

    #[allow(
        clippy::cast_possible_truncation,
        reason = "a u64 has at most 64 leading or trailing zeros"
    )]
    let (mut leading, trailing) = (xor.leading_zeros() as u8, xor.trailing_zeros() as u8);

    if leading > 31 {
        leading = 31;
    }

    if *has_prev_window && leading >= *prev_leading && trailing >= *prev_trailing {
        writer.write_bit(false); // 0: reuse previous window
        let len = 64 - *prev_leading - *prev_trailing;
        let meaningful = (xor >> *prev_trailing) & mask_for_length(len);
        writer.write_bits(meaningful, len);
    } else {
        writer.write_bit(true); // 1: new window
        writer.write_bits(u64::from(leading), 5); // 5 bits for leading zeros
        let len = 64 - leading - trailing;
        writer.write_bits(u64::from(len - 1), 6); // 6 bits for (length - 1)
        let meaningful = (xor >> trailing) & mask_for_length(len);
        writer.write_bits(meaningful, len);

        *prev_leading = leading;
        *prev_trailing = trailing;
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
    let count = usize::try_from(read_bits(&mut reader, 32, "missing point count")?)
        .map_err(|_| corrupt_payload("point count exceeds the address space"))?;
    if count == 0 {
        return Ok(Vec::new());
    }

    let mut points = Vec::with_capacity(count);
    let mut curr_t = read_bits(&mut reader, 64, "truncated t0")?.cast_signed();
    let mut curr_v_bits = read_bits(&mut reader, 64, "truncated v0")?;
    points.push(TimeSeriesPoint::new(curr_t, f64::from_bits(curr_v_bits)));
    if count == 1 {
        return Ok(points);
    }

    // The second point carries the initial delta; later ones a delta of delta. Corrupted
    // payloads may overflow: the arithmetic wraps instead of panicking.
    let mut prev_delta = read_bits(&mut reader, 64, "truncated delta0")?.cast_signed();
    let mut prev_leading = 0u8;
    let mut prev_trailing = 0u8;
    for index in 1..count {
        if index > 1 {
            prev_delta = prev_delta.wrapping_add(read_delta_of_delta(&mut reader)?);
        }
        curr_t = curr_t.wrapping_add(prev_delta);
        curr_v_bits = decode_float_xor(
            &mut reader,
            curr_v_bits,
            &mut prev_leading,
            &mut prev_trailing,
        )?;
        points.push(TimeSeriesPoint::new(curr_t, f64::from_bits(curr_v_bits)));
    }
    Ok(points)
}

fn corrupt_payload(what: &str) -> LiliaError {
    LiliaError::new(
        ErrorCode::InvalidInput,
        format!("corrupted timeseries payload: {what}"),
        false,
    )
}

fn read_bits(reader: &mut BitReader<'_>, width: u8, what: &str) -> Result<u64> {
    reader.read_bits(width).ok_or_else(|| corrupt_payload(what))
}

fn read_flag(reader: &mut BitReader<'_>, what: &str) -> Result<bool> {
    reader.read_bit().ok_or_else(|| corrupt_payload(what))
}

/// Reads a timestamp delta of delta: `0` for zero, then the prefixes `10`, `110` and `1110`
/// followed by 7, 9 and 12 biased bits, `11110` by a 32-bit and `11111` by a 64-bit value.
fn read_delta_of_delta(reader: &mut BitReader<'_>) -> Result<i64> {
    const BIASED: [(u8, i64); 3] = [(7, 63), (9, 255), (12, 2047)];
    if !read_flag(reader, "truncated dod bit")? {
        return Ok(0);
    }
    for (width, bias) in BIASED {
        if !read_flag(reader, "truncated dod tag")? {
            let bits = read_bits(reader, width, &format!("truncated {width}-bit dod"))?;
            return Ok(bits.cast_signed() - bias);
        }
    }
    if read_flag(reader, "truncated dod tag")? {
        Ok(read_bits(reader, 64, "truncated 64-bit dod")?.cast_signed())
    } else {
        let bits = read_bits(reader, 32, "truncated 32-bit dod")?;
        let low = u32::try_from(bits).map_err(|_| corrupt_payload("oversized 32-bit dod"))?;
        Ok(i64::from(low.cast_signed()))
    }
}

#[inline]
fn decode_float_xor(
    reader: &mut BitReader<'_>,
    prev_bits: u64,
    prev_leading: &mut u8,
    prev_trailing: &mut u8,
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

    let (len, trailing) = if is_new_window {
        let leading = reader.read_bits(5).ok_or_else(|| {
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

        #[allow(
            clippy::cast_possible_truncation,
            reason = "the fields are 5 and 6 bits wide"
        )]
        let (leading, len) = (leading as u8, (len_raw as u8) + 1);
        let trailing = 64 - leading - len;

        *prev_leading = leading;
        *prev_trailing = trailing;
        (len, trailing)
    } else {
        let leading = *prev_leading;
        let trailing = *prev_trailing;
        let len = 64 - leading - trailing;
        (len, trailing)
    };

    let meaningful = reader.read_bits(len).ok_or_else(|| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            "corrupted timeseries payload: truncated meaningful bits",
            false,
        )
    })?;

    let xor = meaningful << trailing;
    Ok(prev_bits ^ xor)
}

#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    reason = "test sizes and sample indices are far below 2^52"
)]
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
        assert_eq!(comp, b"");
        let decomp = decompress_timeseries(&comp).unwrap();
        assert_eq!(decomp, empty);

        let single = [TimeSeriesPoint::new(1_700_000_000_000, 72.5)];
        let comp = compress_timeseries(&single).unwrap();
        let decomp = decompress_timeseries(&comp).unwrap();
        assert_eq!(decomp, single);
    }

    #[test]
    fn extreme_timestamps_round_trip() {
        let points = [
            TimeSeriesPoint::new(i64::MIN, 1.0),
            TimeSeriesPoint::new(-1, 2.0),
            TimeSeriesPoint::new(i64::MAX - 1, 3.0),
            TimeSeriesPoint::new(i64::MAX, -4.0),
        ];
        let comp = compress_timeseries(&points).unwrap();
        assert_eq!(decompress_timeseries(&comp).unwrap(), points);
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
}
