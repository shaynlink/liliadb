//! What a time-series write does to the chunks of its tier (one series at one resolution).
//!
//! The chunks of a tier partition the timeline. Each is keyed by an upper bound of its
//! timestamps and holds the points after the previous chunk's bound up to its own; the newest
//! chunk, still filling, is keyed by `i64::MAX`. The chunk of a timestamp is therefore the first
//! chunk at or after the timestamp's key: a forward seek. A chunk that grows past
//! [`CHUNK_POINTS`] is split, every full piece but the last keyed by its newest timestamp and the
//! last keeping the chunk's key, so the partition only ever changes inside a chunk being
//! rewritten.
//!
//! A write's plan expects the version at which its seek found each chunk, which makes it
//! conflict with any concurrent write that changes the partition around its points. The one case
//! without a chunk is a point after every chunk of the tier: the write that creates the newest
//! chunk then also rewrites the tier's state record, which every append reads before its first
//! seek, so a seek that found nothing cannot miss a chunk created after it.

use std::collections::BTreeMap;
use std::ops::Bound;

use bytes::Bytes;
use lilia_core::TimeSeriesPoint;

use super::chunk::{self, CHUNK_POINTS};
use super::{bound_of, chunk_key, encode_state, state_key, tier, tier_end};
use crate::error::{corruption, Result};

/// Where a write reads the chunks of a tier.
pub trait ChunkSource {
    /// The first key in `[start, end)`.
    ///
    /// # Errors
    ///
    /// Engine errors.
    fn seek(&mut self, start: &[u8], end: &[u8]) -> Result<Option<Bytes>>;

    /// The record at `key`.
    ///
    /// # Errors
    ///
    /// Engine errors.
    fn read(&mut self, key: &Bytes) -> Result<Option<Bytes>>;
}

/// The records a write changes (`None` deletes), and whether it removed a point.
#[derive(Debug, Default)]
pub struct Effect {
    pub changes: Vec<(Bytes, Option<Bytes>)>,
    pub removed: bool,
}

/// A source under the write's own changes.
struct Work<'s, S: ChunkSource> {
    source: &'s mut S,
    written: BTreeMap<Bytes, Option<Bytes>>,
}

impl<S: ChunkSource> Work<'_, S> {
    fn seek(&mut self, start: &[u8], end: &[u8]) -> Result<Option<Bytes>> {
        let mut from = start.to_vec();
        let found = loop {
            match self.source.seek(&from, end)? {
                Some(key) if matches!(self.written.get(&key), Some(None)) => {
                    from = key.to_vec();
                    from.push(0);
                }
                other => break other,
            }
        };
        let own = self
            .written
            .range::<[u8], _>((Bound::Included(start), Bound::Excluded(end)))
            .find(|(_, value)| value.is_some())
            .map(|(key, _)| key.clone());
        Ok(match (found, own) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        })
    }

    fn read(&mut self, key: &Bytes) -> Result<Option<Bytes>> {
        match self.written.get(key) {
            Some(value) => Ok(value.clone()),
            None => self.source.read(key),
        }
    }

    /// Stores `points` (sorted, distinct timestamps) as the chunk at `key` of `tier`, split as
    /// needed; deletes the chunk when there are none.
    fn store(&mut self, tier: &[u8], key: Bytes, points: &[TimeSeriesPoint]) -> Result<()> {
        let pieces = points.len().div_ceil(CHUNK_POINTS);
        if pieces == 0 {
            self.written.insert(key, None);
            return Ok(());
        }
        for (i, piece) in points.chunks(CHUNK_POINTS).enumerate() {
            let piece_key = match piece.last() {
                Some(last) if i + 1 < pieces => chunk_key(tier, last.timestamp),
                _ => key.clone(),
            };
            self.written.insert(piece_key, Some(chunk::encode(piece)?));
        }
        Ok(())
    }

    fn finish(self, removed: bool) -> Effect {
        Effect {
            changes: self.written.into_iter().collect(),
            removed,
        }
    }
}

/// `points` sorted by timestamp, the last of several with one timestamp winning.
pub fn normalized(points: &[TimeSeriesPoint]) -> Vec<TimeSeriesPoint> {
    let mut sorted = points.to_vec();
    sorted.sort_by_key(|p| p.timestamp);
    let mut out: Vec<TimeSeriesPoint> = Vec::with_capacity(sorted.len());
    for point in sorted {
        match out.last_mut() {
            Some(last) if last.timestamp == point.timestamp => *last = point,
            _ => out.push(point),
        }
    }
    out
}

/// `existing` and `incoming` (both sorted, distinct timestamps) merged; an incoming point
/// replaces the existing one of its timestamp.
fn merge(existing: &[TimeSeriesPoint], incoming: &[TimeSeriesPoint]) -> Vec<TimeSeriesPoint> {
    let mut out = Vec::with_capacity(existing.len() + incoming.len());
    let (mut a, mut b) = (existing.iter().peekable(), incoming.iter().peekable());
    loop {
        match (a.peek(), b.peek()) {
            (Some(x), Some(y)) if x.timestamp < y.timestamp => out.extend(a.next()),
            (Some(x), Some(y)) if x.timestamp == y.timestamp => {
                a.next();
                out.extend(b.next());
            }
            (_, Some(_)) => out.extend(b.next()),
            (Some(_), None) => out.extend(a.next()),
            (None, None) => return out,
        }
    }
}

/// Writes `points` (normalized) to tier `resolution` of `series`.
///
/// # Errors
///
/// Chunks that do not decode; engine errors.
pub fn append(
    source: &mut impl ChunkSource,
    series: &str,
    resolution: i64,
    points: &[TimeSeriesPoint],
) -> Result<Effect> {
    let tier = tier(series, resolution);
    let end = tier_end(&tier);
    let mut work = Work {
        source,
        written: BTreeMap::new(),
    };
    let state = state_key(series, resolution);
    let state_before = work.read(&state)?;
    let mut rest = points;
    while let Some(head) = rest.first() {
        let (key, bound) = if let Some(key) = work.seek(&chunk_key(&tier, head.timestamp), &end)? {
            let bound = bound_of(&key, &tier)?;
            (key, bound)
        } else {
            let unchanged = state_before.clone().unwrap_or_else(|| encode_state(None));
            work.written.insert(state.clone(), Some(unchanged));
            (chunk_key(&tier, i64::MAX), i64::MAX)
        };
        let taken = rest.partition_point(|p| p.timestamp <= bound);
        let existing = work
            .read(&key)?
            .map(|raw| chunk::decode(&raw))
            .transpose()?
            .unwrap_or_default();
        work.store(&tier, key, &merge(&existing, &rest[..taken]))?;
        rest = &rest[taken..];
    }
    Ok(work.finish(false))
}

/// Deletes the points of tier `resolution` of `series` with timestamps in `[start, end]`.
///
/// # Errors
///
/// As [`append`].
pub fn delete_range(
    source: &mut impl ChunkSource,
    series: &str,
    resolution: i64,
    start: i64,
    end: i64,
) -> Result<Effect> {
    let tier = tier(series, resolution);
    let tier_end = tier_end(&tier);
    let mut work = Work {
        source,
        written: BTreeMap::new(),
    };
    let mut removed = false;
    let mut from = chunk_key(&tier, start).to_vec();
    while let Some(key) = work.seek(&from, &tier_end)? {
        let bound = bound_of(&key, &tier)?;
        let raw = work
            .read(&key)?
            .ok_or_else(|| corruption("a series chunk found by a seek is gone"))?;
        let points = chunk::decode(&raw)?;
        if points.first().is_some_and(|p| p.timestamp > end) {
            break;
        }
        let kept: Vec<TimeSeriesPoint> = points
            .iter()
            .filter(|p| p.timestamp < start || p.timestamp > end)
            .copied()
            .collect();
        if kept.len() != points.len() {
            removed = true;
            work.store(&tier, key.clone(), &kept)?;
        }
        if bound >= end {
            break;
        }
        from = key.to_vec();
        from.push(0);
    }
    Ok(work.finish(removed))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source over a map, as a committed state.
    #[derive(Default)]
    struct Map(BTreeMap<Bytes, Bytes>);

    impl ChunkSource for Map {
        fn seek(&mut self, start: &[u8], end: &[u8]) -> Result<Option<Bytes>> {
            Ok(self
                .0
                .range::<[u8], _>((Bound::Included(start), Bound::Excluded(end)))
                .next()
                .map(|(key, _)| key.clone()))
        }

        fn read(&mut self, key: &Bytes) -> Result<Option<Bytes>> {
            Ok(self.0.get(key).cloned())
        }
    }

    impl Map {
        fn apply(&mut self, effect: Effect) {
            for (key, value) in effect.changes {
                match value {
                    Some(value) => self.0.insert(key, value),
                    None => self.0.remove(&key),
                };
            }
        }

        /// Every chunk of the tier: its bound and points.
        fn chunks(&self) -> Vec<(i64, Vec<TimeSeriesPoint>)> {
            let tier = tier("s", 0);
            self.0
                .iter()
                .filter(|(key, _)| key.starts_with(&tier))
                .map(|(key, raw)| (bound_of(key, &tier).unwrap(), chunk::decode(raw).unwrap()))
                .collect()
        }

        /// Checks the partition invariant and returns every point.
        fn points(&self) -> Vec<TimeSeriesPoint> {
            let mut previous = i64::MIN;
            let mut all = Vec::new();
            for (bound, points) in self.chunks() {
                assert!(!points.is_empty() && points.len() <= CHUNK_POINTS);
                assert!(points.first().unwrap().timestamp >= previous);
                assert!(points.last().unwrap().timestamp <= bound);
                previous = bound;
                all.extend(points);
            }
            assert!(all.windows(2).all(|w| w[0].timestamp < w[1].timestamp));
            all
        }
    }

    fn points(range: impl Iterator<Item = i64>) -> Vec<TimeSeriesPoint> {
        range
            .map(|t| TimeSeriesPoint::new(t, f64::from(i32::try_from(t % 1000).unwrap())))
            .collect()
    }

    #[test]
    fn appends_fill_split_and_keep_the_partition() {
        let mut map = Map::default();
        let first = points(0..3000);
        let effect = append(&mut map, "s", 0, &first).unwrap();
        map.apply(effect);
        assert_eq!(map.points(), first);
        let chunks = map.chunks();
        assert_eq!(chunks.len(), 3);
        assert_eq!(
            chunks[0].0, 1023,
            "a full chunk is keyed by its newest point"
        );
        assert_eq!(chunks[2].0, i64::MAX, "the newest chunk stays open");

        // Points before and after the chunks, and a replacement inside one.
        let mut late = normalized(&points((-500..0).chain((1000..1050).map(|t| t * 3))));
        late.push(TimeSeriesPoint::new(10, -1.0));
        let late = normalized(&late);
        let effect = append(&mut map, "s", 0, &late).unwrap();
        map.apply(effect);
        let all = map.points();
        assert_eq!(all.len(), 3000 + 500 + 50, "the point at 10 is replaced");
        assert_eq!(all.iter().find(|p| p.timestamp == 10).unwrap().value, -1.0);
    }

    #[test]
    fn range_deletes_remove_exactly_their_points() {
        let mut map = Map::default();
        let effect = append(&mut map, "s", 0, &points(0..5000)).unwrap();
        map.apply(effect);
        let effect = delete_range(&mut map, "s", 0, 1000, 3999).unwrap();
        assert!(effect.removed);
        map.apply(effect);
        let left: Vec<i64> = map.points().iter().map(|p| p.timestamp).collect();
        assert_eq!(left, (0..1000).chain(4000..5000).collect::<Vec<_>>());
        assert!(!delete_range(&mut map, "s", 0, 1000, 3999).unwrap().removed);

        let effect = delete_range(&mut map, "s", 0, i64::MIN, i64::MAX).unwrap();

        map.apply(effect);
        assert_eq!(map.chunks(), []);
        let effect = append(&mut map, "s", 0, &points(0..10)).unwrap();
        map.apply(effect);
        assert_eq!(
            map.points().len(),
            10,
            "a tier starts over after it emptied"
        );
    }

    #[test]
    fn normalization_keeps_the_last_point_of_a_timestamp() {
        let raw = [
            TimeSeriesPoint::new(2, 1.0),
            TimeSeriesPoint::new(1, 1.0),
            TimeSeriesPoint::new(2, 2.0),
        ];
        assert_eq!(
            normalized(&raw),
            [TimeSeriesPoint::new(1, 1.0), TimeSeriesPoint::new(2, 2.0)]
        );
    }
}
