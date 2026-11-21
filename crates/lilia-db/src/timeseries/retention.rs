//! Lifecycle policies: which resolutions a series keeps and for how long, each coarser
//! resolution downsampled from the next finer one.
//!
//! A policy names the series it governs by a pattern (`"heart_rate"`, `"sensor:*"`, `"*"`);
//! a series follows the policy of its own name, else the one with the longest matching
//! pattern. Its rules, in resolution order, describe tiers: the finest describes the raw points
//! (stored at resolution 0, whatever rate its `resolution_ms` names), each other one a tier of
//! buckets that wide. A retention pass downsamples every closed bucket of a tier from the tier
//! before it, then deletes the points older than each tier's retention.
//!
//! Each tier records how far it has been downsampled, in the same commit as its new points: a
//! point that arrives later for a time already downsampled stays out of the coarser tiers.

use std::ops::Bound;

use bytes::Bytes;
use lilia_core::timeseries::pattern_matches;
use lilia_core::{
    DownsampleAggregation, TimeSeriesLifecyclePolicy, TimeSeriesPoint, TimeSeriesRetentionStats,
};

use super::chunk::{self, CHUNK_POINTS};
use super::handle::{oldest, visit_chunks};
use super::{chunk_key, decode_state, state_key, tier, TimeSeriesWrite};
use crate::db::{Database, MAINTENANCE_CHUNK};
use crate::error::{corruption, invalid, Result};
use crate::guard::Access;
use crate::keys::{self, validate_name};
use crate::read::Latest;
use crate::record::now_ms;
use crate::write::Planner;

fn validate(policy: &TimeSeriesLifecyclePolicy) -> Result<()> {
    validate_name("series pattern", &policy.series_pattern)?;
    if policy.rules.is_empty() {
        return Err(invalid("a lifecycle policy needs at least one rule"));
    }
    let mut resolutions: Vec<i64> = policy.rules.iter().map(|r| r.resolution_ms).collect();
    resolutions.sort_unstable();
    if resolutions.first().is_some_and(|&r| r < 0) || resolutions.windows(2).any(|w| w[0] == w[1]) {
        return Err(invalid(
            "the rules of a lifecycle policy need distinct resolutions of at least 0",
        ));
    }
    if policy
        .rules
        .iter()
        .any(|r| r.retention_ms.is_some_and(|ms| ms <= 0))
    {
        return Err(invalid("a retention must be positive"));
    }
    Ok(())
}

/// The policy `series` follows among `policies`.
fn governing<'p>(
    policies: &'p [TimeSeriesLifecyclePolicy],
    series: &str,
) -> Option<&'p TimeSeriesLifecyclePolicy> {
    policies
        .iter()
        .find(|p| p.series_pattern == series)
        .or_else(|| {
            policies
                .iter()
                .filter(|p| pattern_matches(&p.series_pattern, series))
                .max_by_key(|p| p.series_pattern.len())
        })
}

/// One bucket being aggregated.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    start: i64,
    count: u64,
    min: f64,
    max: f64,
    sum: f64,
    first: f64,
    last: f64,
}

impl Bucket {
    fn new(start: i64, value: f64) -> Self {
        Self {
            start,
            count: 1,
            min: value,
            max: value,
            sum: value,
            first: value,
            last: value,
        }
    }

    fn add(&mut self, value: f64) {
        self.count += 1;
        self.min = self.min.min(value);
        self.max = self.max.max(value);
        self.sum += value;
        self.last = value;
    }

    #[allow(clippy::cast_precision_loss)] // bucket counts stay far below 2^53
    fn point(&self, aggregation: DownsampleAggregation) -> TimeSeriesPoint {
        let value = match aggregation {
            DownsampleAggregation::Avg => self.sum / self.count as f64,
            DownsampleAggregation::Min => self.min,
            DownsampleAggregation::Max => self.max,
            DownsampleAggregation::Sum => self.sum,
            DownsampleAggregation::First => self.first,
            DownsampleAggregation::Last => self.last,
            DownsampleAggregation::Count => self.count as f64,
        };
        TimeSeriesPoint::new(self.start, value)
    }
}

impl Database {
    /// Stores `policy`, replacing the one of the same pattern.
    ///
    /// # Errors
    ///
    /// [`crate::Error::InvalidArgument`] for an invalid pattern, no rules, repeated or negative
    /// resolutions, or a retention that is not positive; engine errors.
    pub fn set_series_policy(&self, policy: &TimeSeriesLifecyclePolicy) -> Result<()> {
        validate(policy)?;
        let encoded =
            serde_json::to_vec(policy).map_err(|e| corruption(format!("series policy: {e}")))?;
        self.engine()
            .put(keys::series_policy(&policy.series_pattern), encoded)?;
        Ok(())
    }

    /// The policy of pattern `pattern`.
    ///
    /// # Errors
    ///
    /// Invalid pattern, engine errors, a stored policy that does not decode.
    pub fn series_policy(&self, pattern: &str) -> Result<Option<TimeSeriesLifecyclePolicy>> {
        validate_name("series pattern", pattern)?;
        self.engine()
            .get(&keys::series_policy(pattern))?
            .map(|raw| decode_policy(&raw))
            .transpose()
    }

    /// Every policy, by pattern.
    ///
    /// # Errors
    ///
    /// Engine errors, a stored policy that does not decode.
    pub fn series_policies(&self) -> Result<Vec<TimeSeriesLifecyclePolicy>> {
        let end = keys::prefix_end(keys::SERIES_POLICY_PREFIX).unwrap_or_default();
        self.engine()
            .range(
                Bound::Included(keys::SERIES_POLICY_PREFIX),
                Bound::Excluded(&end),
            )?
            .map(|entry| decode_policy(&entry?.value))
            .collect()
    }

    /// Deletes the policy of pattern `pattern`; `false` if there was none. The points of the
    /// series it governed stay.
    ///
    /// # Errors
    ///
    /// Invalid pattern, engine errors.
    pub fn delete_series_policy(&self, pattern: &str) -> Result<bool> {
        validate_name("series pattern", pattern)?;
        let key = keys::series_policy(pattern);
        if self.engine().get(&key)?.is_none() {
            return Ok(false);
        }
        self.engine().delete(key)?;
        Ok(true)
    }

    /// Applies every series' lifecycle policy as of now; see [`Database::apply_series_retention_at`].
    ///
    /// # Errors
    ///
    /// As [`Database::apply_series_retention_at`].
    pub fn apply_series_retention(&self) -> Result<TimeSeriesRetentionStats> {
        self.apply_series_retention_at(now_ms())
    }

    /// Applies every series' lifecycle policy as of `now_ms`: downsamples the buckets closed
    /// by then, and deletes the points older than their tier's retention. Each step commits on
    /// its own (with a ledger, as ledger commits), so an interrupted pass resumes where it was.
    ///
    /// # Errors
    ///
    /// Engine errors, records that do not decode.
    pub fn apply_series_retention_at(&self, now_ms: i64) -> Result<TimeSeriesRetentionStats> {
        let policies = self.series_policies()?;
        let mut total = TimeSeriesRetentionStats::default();
        if policies.is_empty() {
            return Ok(total);
        }
        for series in self.series_names()? {
            let Some(policy) = governing(&policies, &series) else {
                continue;
            };
            let stats = self.retain(&series, policy, now_ms)?;
            total.series_processed += stats.series_processed;
            total.downsampled_points_created += stats.downsampled_points_created;
            total.chunks_pruned += stats.chunks_pruned;
            total.bytes_reclaimed_approx += stats.bytes_reclaimed_approx;
        }
        Ok(total)
    }

    /// Names of the series with points.
    fn series_names(&self) -> Result<Vec<String>> {
        let snapshot = self.engine().snapshot();
        let end = [keys::TS_CHUNK + 1];
        let mut from = vec![keys::TS_CHUNK];
        let mut names = Vec::new();
        loop {
            let first = self
                .engine()
                .range_at(&snapshot, Bound::Included(&from), Bound::Excluded(&end))?
                .next()
                .transpose()?;
            let Some(entry) = first else {
                return Ok(names);
            };
            let rest = &entry.key[1..];
            let split = rest
                .iter()
                .position(|&b| b == 0)
                .ok_or_else(|| corruption("series chunk key without a series"))?;
            let name = String::from_utf8(rest[..split].to_vec())
                .map_err(|_| corruption("series name is not UTF-8"))?;
            from = keys::prefix_end(&keys::collection(keys::TS_CHUNK, &name)).unwrap_or_default();
            names.push(name);
        }
    }

    fn retain(
        &self,
        series: &str,
        policy: &TimeSeriesLifecyclePolicy,
        now_ms: i64,
    ) -> Result<TimeSeriesRetentionStats> {
        let mut rules = policy.rules.clone();
        rules.sort_by_key(|r| r.resolution_ms);
        // The finest rule describes the raw points.
        let tiers: Vec<i64> = rules
            .iter()
            .enumerate()
            .map(|(i, r)| if i == 0 { 0 } else { r.resolution_ms })
            .collect();
        let mut stats = TimeSeriesRetentionStats {
            series_processed: 1,
            ..TimeSeriesRetentionStats::default()
        };
        for (pair, rule) in tiers.windows(2).zip(&rules[1..]) {
            stats.downsampled_points_created +=
                self.downsample(series, pair[0], pair[1], rule.aggregation, now_ms)?;
        }
        for (&resolution, rule) in tiers.iter().zip(&rules) {
            if let Some(retention) = rule.retention_ms {
                let (chunks, bytes) =
                    self.prune(series, resolution, now_ms.saturating_sub(retention))?;
                stats.chunks_pruned += chunks;
                stats.bytes_reclaimed_approx += bytes;
            }
        }
        Ok(stats)
    }

    /// Commits downsampled `points` of tier `target` of `series`, and its new watermark.
    fn commit_downsampled(
        &self,
        series: &str,
        target: i64,
        points: &[TimeSeriesPoint],
        watermark: i64,
    ) -> Result<()> {
        let write = TimeSeriesWrite::Append {
            series: series.to_owned(),
            resolution_ms: target,
            points: points.to_vec(),
        };
        self.run(|catalog| {
            let mut planner = Planner::new(self.engine(), catalog, &Access::System, now_ms());
            planner.series(&write)?;
            planner.series_watermark(series, target, watermark);
            Ok(planner.finish())
        })?;
        Ok(())
    }

    /// Downsamples the buckets of width `target` closed by `now_ms` from tier `source` of
    /// `series`, from the tier's watermark on; returns how many points it wrote.
    fn downsample(
        &self,
        series: &str,
        source: i64,
        target: i64,
        aggregation: DownsampleAggregation,
        now_ms: i64,
    ) -> Result<u64> {
        let boundary = now_ms.div_euclid(target).saturating_mul(target);
        let reader = Latest::pinned(self.engine());
        let watermark = self
            .engine()
            .get(&state_key(series, target))?
            .map(|raw| decode_state(&raw))
            .transpose()?
            .flatten();
        let start = match watermark {
            Some(watermark) => watermark,
            None => match oldest(&reader, series, source)? {
                Some(first) => first.first.div_euclid(target).saturating_mul(target),
                None => return Ok(0),
            },
        };
        if start >= boundary {
            return Ok(0);
        }
        let mut created = 0u64;
        let mut out: Vec<TimeSeriesPoint> = Vec::with_capacity(CHUNK_POINTS);
        let mut bucket: Option<Bucket> = None;
        visit_chunks(&reader, series, source, start, boundary - 1, |raw, _| {
            for point in chunk::decode(raw)? {
                if point.timestamp < start || point.timestamp >= boundary {
                    continue;
                }
                let at = point.timestamp.div_euclid(target).saturating_mul(target);
                match &mut bucket {
                    Some(open) if open.start == at => open.add(point.value),
                    _ => {
                        if let Some(done) = bucket.replace(Bucket::new(at, point.value)) {
                            out.push(done.point(aggregation));
                            if out.len() == CHUNK_POINTS {
                                self.commit_downsampled(series, target, &out, at)?;
                                created += out.len() as u64;
                                out.clear();
                            }
                        }
                    }
                }
            }
            Ok(())
        })?;
        if let Some(done) = bucket {
            out.push(done.point(aggregation));
        }
        self.commit_downsampled(series, target, &out, boundary)?;
        Ok(created + out.len() as u64)
    }

    /// Deletes the points of tier `resolution` of `series` older than `cutoff`; returns the
    /// chunks deleted whole and their bytes.
    fn prune(&self, series: &str, resolution: i64, cutoff: i64) -> Result<(u64, u64)> {
        if cutoff == i64::MIN {
            return Ok((0, 0));
        }
        let tier = tier(series, resolution);
        // Chunks keyed below the cutoff hold only older points.
        let below = chunk_key(&tier, cutoff);
        let (mut chunks, mut bytes) = (0u64, 0u64);
        loop {
            let mut count = 0usize;
            let mut last: Option<Bytes> = None;
            for entry in self
                .engine()
                .range(Bound::Included(&tier), Bound::Excluded(&below))?
                .take(MAINTENANCE_CHUNK)
            {
                let entry = entry?;
                count += 1;
                bytes += entry.value.len() as u64;
                last = Some(entry.key);
            }
            let Some(last) = last else {
                break;
            };
            self.delete_series_range(series, resolution, super::bound_of(&last, &tier)?)?;
            chunks += count as u64;
            if count < MAINTENANCE_CHUNK {
                break;
            }
        }
        // The chunk holding the cutoff, which may hold older points too.
        self.delete_series_range(series, resolution, cutoff - 1)?;
        Ok((chunks, bytes))
    }

    fn delete_series_range(&self, series: &str, resolution: i64, end: i64) -> Result<()> {
        let write = TimeSeriesWrite::DeleteRange {
            series: series.to_owned(),
            resolution_ms: resolution,
            start: i64::MIN,
            end,
        };
        self.run(|catalog| {
            let mut planner = Planner::new(self.engine(), catalog, &Access::System, now_ms());
            planner.series(&write)?;
            Ok(planner.finish())
        })?;
        Ok(())
    }
}

fn decode_policy(raw: &[u8]) -> Result<TimeSeriesLifecyclePolicy> {
    serde_json::from_slice(raw).map_err(|e| corruption(format!("series policy: {e}")))
}

#[cfg(test)]
mod tests {
    use lilia_core::TimeSeriesRetentionRule;

    use super::*;

    fn policy(pattern: &str, resolutions: &[i64]) -> TimeSeriesLifecyclePolicy {
        TimeSeriesLifecyclePolicy::new(
            pattern,
            resolutions
                .iter()
                .map(|&r| TimeSeriesRetentionRule::new(r, None, DownsampleAggregation::Avg))
                .collect(),
        )
    }

    #[test]
    fn policies_are_validated_and_the_most_specific_one_governs() {
        assert!(validate(&policy("s", &[0, 60_000])).is_ok());
        assert!(validate(&policy("s", &[])).is_err());
        assert!(validate(&policy("s", &[60_000, 60_000])).is_err());
        assert!(validate(&policy("s", &[-1])).is_err());
        let policies = [
            policy("*", &[0]),
            policy("sensor:*", &[0]),
            policy("sensor:a", &[0]),
        ];
        let pattern = |s| governing(&policies, s).map(|p| p.series_pattern.as_str());
        assert_eq!(pattern("sensor:a"), Some("sensor:a"));
        assert_eq!(pattern("sensor:b"), Some("sensor:*"));
        assert_eq!(pattern("other"), Some("*"));
    }
}
