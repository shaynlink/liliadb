//! Time series: timestamped values per series, at the raw resolution and at the coarser ones
//! lifecycle policies downsample them to.
//!
//! Points live in compressed chunks of up to 1,024 points (`series\0`
//! resolution, upper bound of the chunk's timestamps), each a record the ledger attests; the
//! `effect` module describes how writes keep the chunks of a tier partitioning its timeline. A series holds
//! one value per timestamp and resolution: a later write of a timestamp replaces its value.
//! Lifecycle policies, downsampling and retention are in the `retention` module.

mod chunk;
mod effect;
mod handle;
mod retention;

use std::ops::Bound;

use bytes::Bytes;
use lilia_core::TimeSeriesPoint;

pub use handle::TimeSeries;

use self::effect::{ChunkSource, Effect};
use crate::error::{corruption, invalid, Result};
use crate::keys::{self, validate_name};
use crate::read::Source;
use crate::txn::View;
use crate::write::overlay::Changes;
use crate::write::{Model, Planner};

/// A time-series write, for a [`crate::Batch`]; see [`TimeSeries`] for each operation.
#[derive(Debug, Clone, PartialEq)]
pub enum TimeSeriesWrite {
    Append {
        series: String,
        resolution_ms: i64,
        points: Vec<TimeSeriesPoint>,
    },
    /// Deletes the points with timestamps in `[start, end]`.
    DeleteRange {
        series: String,
        resolution_ms: i64,
        start: i64,
        end: i64,
    },
}

impl TimeSeriesWrite {
    fn series(&self) -> &str {
        match self {
            Self::Append { series, .. } | Self::DeleteRange { series, .. } => series,
        }
    }

    fn validate(&self) -> Result<()> {
        validate_name("series", self.series())?;
        match self {
            Self::Append {
                resolution_ms,
                points,
                ..
            } => {
                validate_resolution(*resolution_ms)?;
                if points.iter().any(|p| !p.value.is_finite()) {
                    return Err(invalid("time-series values must be finite"));
                }
                Ok(())
            }
            Self::DeleteRange {
                resolution_ms,
                start,
                end,
                ..
            } => {
                validate_resolution(*resolution_ms)?;
                if start > end {
                    return Err(invalid(
                        "a deleted time range must not end before it starts",
                    ));
                }
                Ok(())
            }
        }
    }

    fn effect(&self, source: &mut impl ChunkSource) -> Result<Effect> {
        match self {
            Self::Append {
                series,
                resolution_ms,
                points,
            } => effect::append(source, series, *resolution_ms, &effect::normalized(points)),
            Self::DeleteRange {
                series,
                resolution_ms,
                start,
                end,
            } => effect::delete_range(source, series, *resolution_ms, *start, *end),
        }
    }
}

pub(crate) fn validate_resolution(resolution_ms: i64) -> Result<()> {
    if resolution_ms < 0 {
        return Err(invalid(
            "a resolution is 0 (raw points) or a bucket width in milliseconds",
        ));
    }
    Ok(())
}

/// Prefix of the chunks of tier `resolution` of `series`.
pub(crate) fn tier(series: &str, resolution: i64) -> Vec<u8> {
    let mut key = keys::collection(keys::TS_CHUNK, series);
    key.extend_from_slice(&keys::ordered(resolution));
    key
}

pub(crate) fn tier_end(tier: &[u8]) -> Vec<u8> {
    // A tier prefix ends with a resolution, which is at most 0xFF..: its end is past it, inside
    // the series' prefix.
    keys::prefix_end(tier).unwrap_or_default()
}

pub(crate) fn chunk_key(tier: &[u8], bound: i64) -> Bytes {
    let mut key = tier.to_vec();
    key.extend_from_slice(&keys::ordered(bound));
    key.into()
}

/// The upper bound of the chunk at `key` of `tier`.
pub(crate) fn bound_of(key: &[u8], tier: &[u8]) -> Result<i64> {
    keys::from_ordered(keys::suffix(key, tier))
        .ok_or_else(|| corruption("unreadable series chunk key"))
}

/// The state record of tier `resolution` of `series`: its downsampling watermark.
pub(crate) fn state_key(series: &str, resolution: i64) -> Bytes {
    let mut key = keys::collection(keys::TS_STATE, series);
    key.extend_from_slice(&keys::ordered(resolution));
    key.into()
}

const STATE_FORMAT: u8 = 1;

/// A tier state: the time before which the tier has been downsampled from the finer one.
pub(crate) fn encode_state(watermark: Option<i64>) -> Bytes {
    let mut out = vec![STATE_FORMAT];
    if let Some(watermark) = watermark {
        out.extend_from_slice(&watermark.to_le_bytes());
    }
    out.into()
}

pub(crate) fn decode_state(raw: &[u8]) -> Result<Option<i64>> {
    match raw {
        [STATE_FORMAT] => Ok(None),
        [STATE_FORMAT, rest @ ..] => rest
            .try_into()
            .map(|bytes| Some(i64::from_le_bytes(bytes)))
            .map_err(|_| corruption("unreadable series state")),
        _ => Err(corruption("series state with an unknown format")),
    }
}

/// Ledger identity of the chunk at `rest` (a key after `series\0`): `resolution:bound`.
pub(crate) fn identity(rest: &[u8]) -> Option<String> {
    let (resolution, bound) = rest.split_at_checked(8)?;
    Some(format!(
        "{}:{}",
        keys::from_ordered(resolution)?,
        keys::from_ordered(bound)?
    ))
}

/// Key of the chunk of tier `resolution` of `series` holding a point at `timestamp`.
///
/// # Errors
///
/// Engine errors, chunks that do not decode.
pub(crate) fn chunk_holding(
    source: &impl Source,
    series: &str,
    resolution: i64,
    timestamp: i64,
) -> Result<Option<Bytes>> {
    let tier = tier(series, resolution);
    let end = tier_end(&tier);
    let first = source
        .range(
            Bound::Included(&chunk_key(&tier, timestamp)),
            Bound::Excluded(&end),
        )?
        .next()
        .transpose()?;
    let Some(item) = first else {
        return Ok(None);
    };
    let holds = chunk::decode(&item.value)?
        .binary_search_by_key(&timestamp, |p| p.timestamp)
        .is_ok();
    Ok(holds.then_some(item.key))
}

impl ChunkSource for Planner<'_> {
    fn seek(&mut self, start: &[u8], end: &[u8]) -> Result<Option<Bytes>> {
        self.first_live(start, end)
    }

    fn read(&mut self, key: &Bytes) -> Result<Option<Bytes>> {
        self.current(key)
    }
}

impl Planner<'_> {
    /// Plans one time-series write.
    ///
    /// # Errors
    ///
    /// Invalid arguments, chunks that do not decode, engine read errors.
    pub(crate) fn series(&mut self, write: &TimeSeriesWrite) -> Result<()> {
        write.validate()?;
        let effect = write.effect(self)?;
        let series = write.series();
        let base = keys::collection(keys::TS_CHUNK, series);
        let mut record_op = None;
        for (key, value) in effect.changes {
            if key.starts_with(&base) {
                let id = identity(keys::suffix(&key, &base))
                    .ok_or_else(|| corruption("unreadable series chunk key"))?;
                record_op = Some(self.write_record(&key, value, (Model::TimeSeries, series, id)));
            } else {
                self.op(key, value);
            }
        }
        self.result(record_op, effect.removed);
        Ok(())
    }

    /// Records that tier `resolution` of `series` has been downsampled up to `watermark`.
    pub(crate) fn series_watermark(&mut self, series: &str, resolution: i64, watermark: i64) {
        self.op(
            state_key(series, resolution),
            Some(encode_state(Some(watermark))),
        );
    }
}

/// A transaction's view as a chunk source.
struct ViewSource<'v, 't>(&'v View<'t>);

impl ChunkSource for ViewSource<'_, '_> {
    fn seek(&mut self, start: &[u8], end: &[u8]) -> Result<Option<Bytes>> {
        let first = self
            .0
            .range(Bound::Included(start), Bound::Excluded(end))?
            .next()
            .transpose()?;
        Ok(first.map(|item| item.key))
    }

    fn read(&mut self, key: &Bytes) -> Result<Option<Bytes>> {
        Ok(self.0.peek(key)?.map(|item| item.value))
    }
}

/// The records a time-series write changes in a transaction's view; see
/// [`crate::write::overlay`].
///
/// # Errors
///
/// As [`Planner::series`].
pub(crate) fn overlay(write: &TimeSeriesWrite, view: &View<'_>) -> Result<(Changes, bool)> {
    write.validate()?;
    let effect = write.effect(&mut ViewSource(view))?;
    Ok((effect.changes, effect.removed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_order_like_their_times() {
        let tier = tier("s", 60_000);
        let times = [i64::MIN, -1, 0, 1, i64::MAX];
        let keys: Vec<Bytes> = times.iter().map(|&t| chunk_key(&tier, t)).collect();
        assert!(keys.windows(2).all(|w| w[0] < w[1]));
        for (key, &t) in keys.iter().zip(&times) {
            assert_eq!(bound_of(key, &tier).unwrap(), t);
        }
        assert!(chunk_key(&tier, i64::MAX).as_ref() < tier_end(&tier).as_slice());
        let base = keys::collection(keys::TS_CHUNK, "s");
        assert_eq!(
            identity(keys::suffix(&chunk_key(&tier, 7), &base)).as_deref(),
            Some("60000:7")
        );
        assert_eq!(decode_state(&encode_state(Some(-5))).unwrap(), Some(-5));
        assert_eq!(decode_state(&encode_state(None)).unwrap(), None);
        assert!(decode_state(&[2]).is_err());
    }
}
