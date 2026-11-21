//! The time-series handle of a session or transaction.

use std::ops::Bound;

use lilia_core::{TimeSeriesPoint, TimeSeriesStats};
use serde_json::json;

use super::chunk::{self, Summary};
use super::TimeSeriesWrite;
use super::{bound_of, chunk_key, from_ordered, tier, tier_end, validate_resolution};
use crate::batch::{Outcome, Write};
use crate::context::{guard, Context};
use crate::error::{corruption, Result};
use crate::keys::{self, validate_name};
use crate::read::Source;

/// Series `name` of a session or transaction.
///
/// A series holds points at resolution 0 (the raw points) and at the bucket widths, in
/// milliseconds, its lifecycle policy downsamples them to ([`crate::Database::set_series_policy`]).
/// Each resolution holds one value per timestamp: writing a timestamp again replaces its value.
/// Time ranges are inclusive. The series is readable when the read policies of the collection
/// of its name admit the document `{"series": name}`.
#[derive(Debug)]
pub struct TimeSeries<'c, C: Context> {
    context: &'c C,
    name: String,
}

impl<'c, C: Context> TimeSeries<'c, C> {
    pub(crate) fn new(context: &'c C, name: &str) -> Self {
        Self {
            context,
            name: name.to_owned(),
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    fn submit(&self, write: TimeSeriesWrite) -> Result<Outcome> {
        let mut outcomes = self.context.submit(vec![Write::TimeSeries(write)])?;
        outcomes
            .pop()
            .ok_or_else(|| corruption("a time-series write produced no outcome"))
    }

    /// Writes raw points, replacing the values of timestamps already present (and, among
    /// `points`, the last value of a repeated timestamp wins). The outcome's version is that of
    /// the last chunk written; `None` for no points.
    ///
    /// # Errors
    ///
    /// Invalid names, values that are not finite; as [`crate::Session::commit`].
    pub fn append(&self, points: &[TimeSeriesPoint]) -> Result<Outcome> {
        self.append_at(0, points)
    }

    /// As [`TimeSeries::append`], at resolution `resolution_ms`.
    ///
    /// # Errors
    ///
    /// As [`TimeSeries::append`], and a negative resolution.
    pub fn append_at(&self, resolution_ms: i64, points: &[TimeSeriesPoint]) -> Result<Outcome> {
        self.submit(TimeSeriesWrite::Append {
            series: self.name.clone(),
            resolution_ms,
            points: points.to_vec(),
        })
    }

    /// Deletes the raw points in `[start, end]`; `true` if there were some.
    ///
    /// # Errors
    ///
    /// As [`TimeSeries::append`], and a range that ends before it starts.
    pub fn delete_range(&self, start: i64, end: i64) -> Result<bool> {
        self.delete_range_at(0, start, end)
    }

    /// As [`TimeSeries::delete_range`], at resolution `resolution_ms`.
    ///
    /// # Errors
    ///
    /// As [`TimeSeries::delete_range`].
    pub fn delete_range_at(&self, resolution_ms: i64, start: i64, end: i64) -> Result<bool> {
        Ok(self
            .submit(TimeSeriesWrite::DeleteRange {
                series: self.name.clone(),
                resolution_ms,
                start,
                end,
            })?
            .deleted)
    }

    /// The raw points in `[start, end]`, oldest first.
    ///
    /// # Errors
    ///
    /// Invalid name, chunks that do not decode, engine errors.
    pub fn range(&self, start: i64, end: i64) -> Result<Vec<TimeSeriesPoint>> {
        self.range_at(0, start, end)
    }

    /// The points of resolution `resolution_ms` in `[start, end]`, oldest first.
    ///
    /// # Errors
    ///
    /// As [`TimeSeries::range`], and a negative resolution.
    pub fn range_at(
        &self,
        resolution_ms: i64,
        start: i64,
        end: i64,
    ) -> Result<Vec<TimeSeriesPoint>> {
        validate_resolution(resolution_ms)?;
        if !self.readable()? {
            return Ok(Vec::new());
        }
        let reader = self.context.reader(true);
        points(&reader, &self.name, resolution_ms, start, end)
    }

    /// Minimum, maximum, average, sum and count of the raw points in `[start, end]`; `None`
    /// when there are none. Chunks the range covers whole are summed from their headers.
    ///
    /// # Errors
    ///
    /// As [`TimeSeries::range`].
    pub fn stats(&self, start: i64, end: i64) -> Result<Option<TimeSeriesStats>> {
        self.stats_at(0, start, end)
    }

    /// As [`TimeSeries::stats`], at resolution `resolution_ms`.
    ///
    /// # Errors
    ///
    /// As [`TimeSeries::range_at`].
    pub fn stats_at(
        &self,
        resolution_ms: i64,
        start: i64,
        end: i64,
    ) -> Result<Option<TimeSeriesStats>> {
        validate_resolution(resolution_ms)?;
        if !self.readable()? {
            return Ok(None);
        }
        let reader = self.context.reader(true);
        Ok(summary(&reader, &self.name, resolution_ms, start, end)?.map(Summary::stats))
    }

    /// The resolutions the series holds points at, finest first.
    ///
    /// # Errors
    ///
    /// As [`TimeSeries::range`].
    pub fn resolutions(&self) -> Result<Vec<i64>> {
        if !self.readable()? {
            return Ok(Vec::new());
        }
        resolutions(&self.context.reader(true), &self.name)
    }

    /// The points in `[start, end]` at the finest resolution that has at most `max_points` of
    /// them there (with `max_points` 0, the finest that has any), else at the coarsest one.
    ///
    /// # Errors
    ///
    /// As [`TimeSeries::range`].
    pub fn auto(&self, start: i64, end: i64, max_points: usize) -> Result<Vec<TimeSeriesPoint>> {
        if !self.readable()? {
            return Ok(Vec::new());
        }
        let reader = self.context.reader(true);
        let mut available = Vec::new();
        for resolution in resolutions(&reader, &self.name)? {
            if let Some(summary) = summary(&reader, &self.name, resolution, start, end)? {
                available.push((resolution, summary.count));
            }
        }
        let fits = |count: u64| usize::try_from(count).is_ok_and(|n| n <= max_points);
        let chosen = if max_points == 0 {
            available.first()
        } else {
            available
                .iter()
                .find(|(_, count)| fits(*count))
                .or(available.last())
        };
        match chosen {
            Some(&(resolution, _)) => points(&reader, &self.name, resolution, start, end),
            None => Ok(Vec::new()),
        }
    }

    fn readable(&self) -> Result<bool> {
        validate_name("series", &self.name)?;
        Ok(guard(self.context, &self.name).related_hit(None, || json!({ "series": self.name })))
    }
}

/// Calls `visit` with each chunk of tier `resolution` of `series` that may hold points in
/// `[start, end]`, oldest first.
pub(super) fn visit_chunks(
    reader: &impl Source,
    series: &str,
    resolution: i64,
    start: i64,
    end: i64,
    mut visit: impl FnMut(&[u8], Summary) -> Result<()>,
) -> Result<()> {
    if start > end {
        return Ok(());
    }
    let tier = tier(series, resolution);
    let from = chunk_key(&tier, start);
    let to = tier_end(&tier);
    for item in reader.range(Bound::Included(&from), Bound::Excluded(&to))? {
        let item = item?;
        reader.observe(&item);
        let summary = chunk::summary(&item.value)?;
        if summary.first > end {
            break;
        }
        visit(&item.value, summary)?;
        if bound_of(&item.key, &tier)? >= end {
            break;
        }
    }
    Ok(())
}

fn points(
    reader: &impl Source,
    series: &str,
    resolution: i64,
    start: i64,
    end: i64,
) -> Result<Vec<TimeSeriesPoint>> {
    let mut out = Vec::new();
    visit_chunks(reader, series, resolution, start, end, |raw, _| {
        out.extend(
            chunk::decode(raw)?
                .into_iter()
                .filter(|p| p.timestamp >= start && p.timestamp <= end),
        );
        Ok(())
    })?;
    Ok(out)
}

fn summary(
    reader: &impl Source,
    series: &str,
    resolution: i64,
    start: i64,
    end: i64,
) -> Result<Option<Summary>> {
    let mut total: Option<Summary> = None;
    visit_chunks(reader, series, resolution, start, end, |raw, summary| {
        let part = if summary.first >= start && summary.last <= end {
            Some(summary)
        } else {
            let inside: Vec<TimeSeriesPoint> = chunk::decode(raw)?
                .into_iter()
                .filter(|p| p.timestamp >= start && p.timestamp <= end)
                .collect();
            Summary::of(&inside)
        };
        if let Some(part) = part {
            total = Some(total.map_or(part, |t| t.merge(part)));
        }
        Ok(())
    })?;
    Ok(total)
}

/// The aggregates of the oldest chunk of tier `resolution` of `series`.
pub(super) fn oldest(
    reader: &impl Source,
    series: &str,
    resolution: i64,
) -> Result<Option<Summary>> {
    let tier = tier(series, resolution);
    let to = tier_end(&tier);
    let first = reader
        .range(Bound::Included(&tier), Bound::Excluded(&to))?
        .next()
        .transpose()?;
    first.map(|item| chunk::summary(&item.value)).transpose()
}

/// The resolutions `series` holds points at, finest first.
fn resolutions(reader: &impl Source, series: &str) -> Result<Vec<i64>> {
    let base = keys::collection(keys::TS_CHUNK, series);
    let end = keys::prefix_end(&base).unwrap_or_default();
    let mut from = base.clone();
    let mut out = Vec::new();
    loop {
        let first = reader
            .range(Bound::Included(&from), Bound::Excluded(&end))?
            .next()
            .transpose()?;
        let Some(item) = first else {
            return Ok(out);
        };
        reader.observe(&item);
        let resolution = keys::suffix(&item.key, &base)
            .get(..8)
            .and_then(from_ordered)
            .ok_or_else(|| corruption("unreadable series chunk key"))?;
        out.push(resolution);
        from = tier_end(&tier(series, resolution));
    }
}
