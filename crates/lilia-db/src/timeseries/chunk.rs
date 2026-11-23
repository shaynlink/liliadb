//! A chunk of a series: up to [`CHUNK_POINTS`] points with distinct timestamps, in timestamp
//! order, Gorilla-compressed ([`lilia_core::timeseries`]), behind a header of their aggregates
//! so range statistics read the points of the chunks a range only partly covers.
//!
//! Layout: format byte, point count (`u32`), first and last timestamps (`i64`), minimum,
//! maximum and sum (`f64`), all little-endian, then the compressed points.

use bytes::Bytes;
use lilia_core::timeseries::{compress_timeseries, decompress_timeseries};
use lilia_core::{TimeSeriesPoint, TimeSeriesStats};

use crate::error::{corruption, Result};

/// Most points of one chunk: a write rewrites at most this many points per chunk it touches.
pub const CHUNK_POINTS: usize = 1024;
const FORMAT: u8 = 1;
const HEADER: usize = 1 + 4 + 8 * 5;

/// Aggregates of a set of points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    pub count: u64,
    pub first: i64,
    pub last: i64,
    pub min: f64,
    pub max: f64,
    pub sum: f64,
}

impl Summary {
    /// The aggregates of `points`, in timestamp order; `None` when there are none.
    pub fn of(points: &[TimeSeriesPoint]) -> Option<Self> {
        let (head, tail) = (points.first()?, points.last()?);
        let mut summary = Self {
            count: 0,
            first: head.timestamp,
            last: tail.timestamp,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            sum: 0.0,
        };
        for point in points {
            summary.count += 1;
            summary.min = summary.min.min(point.value);
            summary.max = summary.max.max(point.value);
            summary.sum += point.value;
        }
        Some(summary)
    }

    /// The aggregates of both sets.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        Self {
            count: self.count + other.count,
            first: self.first.min(other.first),
            last: self.last.max(other.last),
            min: self.min.min(other.min),
            max: self.max.max(other.max),
            sum: self.sum + other.sum,
        }
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "an average of up to 2^53 points is exact enough"
    )]
    pub fn stats(self) -> TimeSeriesStats {
        TimeSeriesStats {
            min: self.min,
            max: self.max,
            avg: self.sum / self.count as f64,
            sum: self.sum,
            count: self.count,
        }
    }
}

/// The chunk of `points`, which are in timestamp order and not empty.
///
/// # Errors
///
/// [`crate::Error::Corruption`] when there are no points or too many for a chunk.
pub fn encode(points: &[TimeSeriesPoint]) -> Result<Bytes> {
    let summary = Summary::of(points).ok_or_else(|| corruption("an empty series chunk"))?;
    let count = u32::try_from(points.len())
        .ok()
        .filter(|&n| n as usize <= CHUNK_POINTS)
        .ok_or_else(|| corruption("a series chunk over its size"))?;
    let data = compress_timeseries(points).map_err(|e| corruption(e.to_string()))?;
    let mut out = Vec::with_capacity(HEADER + data.len());
    out.push(FORMAT);
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&summary.first.to_le_bytes());
    out.extend_from_slice(&summary.last.to_le_bytes());
    out.extend_from_slice(&summary.min.to_le_bytes());
    out.extend_from_slice(&summary.max.to_le_bytes());
    out.extend_from_slice(&summary.sum.to_le_bytes());
    out.extend_from_slice(&data);
    Ok(out.into())
}

fn word(raw: &[u8], at: usize) -> [u8; 8] {
    let mut out = [0; 8];
    out.copy_from_slice(&raw[at..at + 8]);
    out
}

/// The aggregates of a chunk, from its header.
///
/// # Errors
///
/// [`crate::Error::Corruption`] for a record that is not a chunk.
pub fn summary(raw: &[u8]) -> Result<Summary> {
    if raw.len() < HEADER || raw[0] != FORMAT {
        return Err(corruption("series chunk with an unknown format"));
    }
    let count = u32::from_le_bytes([raw[1], raw[2], raw[3], raw[4]]);
    Ok(Summary {
        count: u64::from(count),
        first: i64::from_le_bytes(word(raw, 5)),
        last: i64::from_le_bytes(word(raw, 13)),
        min: f64::from_le_bytes(word(raw, 21)),
        max: f64::from_le_bytes(word(raw, 29)),
        sum: f64::from_le_bytes(word(raw, 37)),
    })
}

/// The points of a chunk.
///
/// # Errors
///
/// [`crate::Error::Corruption`] for a record that does not decode or disagrees with its header.
pub fn decode(raw: &[u8]) -> Result<Vec<TimeSeriesPoint>> {
    let summary = summary(raw)?;
    let points = decompress_timeseries(&raw[HEADER..]).map_err(|e| corruption(e.to_string()))?;
    if points.len() as u64 != summary.count
        || points.first().map(|p| p.timestamp) != Some(summary.first)
        || points.last().map(|p| p.timestamp) != Some(summary.last)
        || points.windows(2).any(|w| w[0].timestamp >= w[1].timestamp)
    {
        return Err(corruption("series chunk that disagrees with its header"));
    }
    Ok(points)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_round_trip_with_their_aggregates() {
        let points: Vec<_> = (0..100)
            .map(|i| TimeSeriesPoint::new(1_000 * i, f64::from(i32::try_from(i % 7).unwrap())))
            .collect();
        let raw = encode(&points).unwrap();
        assert_eq!(decode(&raw).unwrap(), points);
        let summary = summary(&raw).unwrap();
        assert_eq!(
            (summary.count, summary.first, summary.last),
            (100, 0, 99_000)
        );
        assert_eq!((summary.min, summary.max), (0.0, 6.0));
        assert!(encode(&[]).is_err());
        assert!(decode(&raw[..HEADER - 1]).is_err());
        let mut forged = raw.to_vec();
        forged[1] = 99;
        assert!(
            decode(&forged).is_err(),
            "a count that disagrees is corruption"
        );
    }
}
