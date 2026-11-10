#![allow(
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use lilia_core::timeseries::{
    compress_timeseries, decompress_timeseries, downsample_points, pattern_matches,
};
use lilia_core::{
    Result, TimeSeriesLifecyclePolicy, TimeSeriesPoint, TimeSeriesRetentionStats, TimeSeriesStats,
};

use crate::engine::Resources;
use crate::lifecycle::Lifecycle;
use crate::storage::validate_name;
use crate::Database;

/// Default point count per compressed timeseries chunk (approx 1.5 - 2.5 KB on disk).
pub const DEFAULT_CHUNK_SIZE: usize = 1_000;

impl Database {
    /// Appends observations to a raw time-series stream (`resolution_ms = 0`).
    ///
    /// The points are partitioned into chunks of up to 1,000 observations,
    /// compressed with the Gorilla Delta-of-Delta & XOR algorithm, and stored
    /// atomically into `_lilia_timeseries_chunks`.
    pub fn ts_append_points(&self, series_id: &str, points: &[TimeSeriesPoint]) -> Result<()> {
        self.ts_append_chunk(series_id, 0, points)
    }

    /// Appends observations to a time-series stream at a specific resolution tier.
    pub fn ts_append_chunk(
        &self,
        series_id: &str,
        resolution_ms: i64,
        points: &[TimeSeriesPoint],
    ) -> Result<()> {
        append_chunk_internal(
            &self.resources,
            &self.lifecycle,
            series_id,
            resolution_ms,
            points,
        )
    }

    /// Queries raw observations (`resolution_ms = 0`) within `[start_time, end_time]`.
    pub fn ts_query_range(
        &self,
        series_id: &str,
        start_time: i64,
        end_time: i64,
    ) -> Result<Vec<TimeSeriesPoint>> {
        self.ts_query_range_with_resolution(series_id, 0, start_time, end_time)
    }

    /// Queries observations for a series at a specific resolution tier within `[start_time, end_time]`.
    pub fn ts_query_range_with_resolution(
        &self,
        series_id: &str,
        resolution_ms: i64,
        start_time: i64,
        end_time: i64,
    ) -> Result<Vec<TimeSeriesPoint>> {
        query_range_internal(
            &self.resources,
            &self.lifecycle,
            series_id,
            resolution_ms,
            start_time,
            end_time,
        )
    }

    /// Adaptively selects the optimal resolution tier (raw, 1m, 1h, etc.) for `series_id`
    /// such that the returned point count does not exceed `max_points`.
    ///
    /// Ideal for mobile charting, telemetry graphs, and constrained network/RAM environments.
    pub fn ts_query_auto(
        &self,
        series_id: &str,
        start_time: i64,
        end_time: i64,
        max_points: usize,
    ) -> Result<Vec<TimeSeriesPoint>> {
        validate_name("series_id", series_id)?;
        let conn = self.resources.read_lock(&self.lifecycle)?;

        // Discover available resolutions overlapping [start_time, end_time]
        let mut stmt = conn.prepare(
            "SELECT resolution_ms, SUM(point_count)
             FROM _lilia_timeseries_chunks
             WHERE series_id = ?1 AND start_time <= ?3 AND end_time >= ?2
             GROUP BY resolution_ms
             ORDER BY resolution_ms ASC;",
        )?;

        stmt.bind_text_static(1, series_id)?;
        stmt.bind_int64(2, start_time)?;
        stmt.bind_int64(3, end_time)?;

        let mut available: Vec<(i64, u64)> = Vec::new();
        while stmt.step()? {
            let res = stmt.column_int64(0);
            let count = stmt.column_int64(1).max(0) as u64;
            available.push((res, count));
        }
        stmt.reset()?;
        drop(conn);

        if available.is_empty() {
            return Ok(Vec::new());
        }

        // Check if raw resolution (or finest available) fits within max_points
        let target_res = if max_points == 0 {
            available[0].0
        } else {
            // Find the finest resolution whose estimated count <= max_points
            let mut chosen = available.last().unwrap().0;
            for (res, count) in &available {
                if (*count as usize) <= max_points {
                    chosen = *res;
                    break;
                }
            }
            chosen
        };

        self.ts_query_range_with_resolution(series_id, target_res, start_time, end_time)
    }

    /// Computes aggregated statistics (min, max, avg, sum, count) across raw observations.
    pub fn ts_query_stats(
        &self,
        series_id: &str,
        start_time: i64,
        end_time: i64,
    ) -> Result<Option<TimeSeriesStats>> {
        self.ts_query_stats_with_resolution(series_id, 0, start_time, end_time)
    }

    /// Computes aggregated statistics across observations at a specific resolution tier.
    pub fn ts_query_stats_with_resolution(
        &self,
        series_id: &str,
        resolution_ms: i64,
        start_time: i64,
        end_time: i64,
    ) -> Result<Option<TimeSeriesStats>> {
        validate_name("series_id", series_id)?;
        let conn = self.resources.read_lock(&self.lifecycle)?;
        let mut stmt = conn.prepare(
            "SELECT MIN(min_val), MAX(max_val), SUM(sum_val), SUM(point_count)
             FROM _lilia_timeseries_chunks
             WHERE series_id = ?1 AND resolution_ms = ?2 AND start_time <= ?4 AND end_time >= ?3;",
        )?;

        stmt.bind_text_static(1, series_id)?;
        stmt.bind_int64(2, resolution_ms)?;
        stmt.bind_int64(3, start_time)?;
        stmt.bind_int64(4, end_time)?;

        let stats = if stmt.step()? {
            if stmt.column_is_null(0) || stmt.column_is_null(3) {
                None
            } else {
                let min = stmt.column_double(0);
                let max = stmt.column_double(1);
                let sum = stmt.column_double(2);
                let count = stmt.column_int64(3) as u64;
                if count == 0 {
                    None
                } else {
                    #[allow(clippy::cast_precision_loss)]
                    let avg = sum / (count as f64);
                    Some(TimeSeriesStats {
                        min,
                        max,
                        avg,
                        sum,
                        count,
                    })
                }
            }
        } else {
            None
        };

        stmt.reset()?;
        Ok(stats)
    }

    // ==========================================
    // DECLARATIVE LIFECYCLE POLICIES
    // ==========================================

    /// Registers or updates a declarative time-series lifecycle policy.
    pub fn ts_set_policy(&self, policy: &TimeSeriesLifecyclePolicy) -> Result<()> {
        let json = serde_json::to_string(policy).map_err(|e| {
            lilia_core::LiliaError::new(
                lilia_core::ErrorCode::InvalidInput,
                format!("failed to serialize lifecycle policy: {e}"),
                false,
            )
        })?;

        let writer = self.resources.lock_writer(&self.lifecycle, None)?;
        writer.exec("BEGIN IMMEDIATE;")?;
        let mut stmt = writer.prepare(
            "INSERT OR REPLACE INTO _lilia_timeseries_policies(series_pattern, policy_json)
             VALUES (?1, ?2);",
        )?;
        stmt.bind_text_static(1, &policy.series_pattern)?;
        stmt.bind_text_static(2, &json)?;
        stmt.step()?;
        stmt.reset()?;
        writer.exec("COMMIT;")?;
        Ok(())
    }

    /// Retrieves a declarative lifecycle policy by series pattern.
    pub fn ts_get_policy(&self, series_pattern: &str) -> Result<Option<TimeSeriesLifecyclePolicy>> {
        let conn = self.resources.read_lock(&self.lifecycle)?;
        let mut stmt = conn.prepare(
            "SELECT policy_json FROM _lilia_timeseries_policies WHERE series_pattern = ?1;",
        )?;
        stmt.bind_text_static(1, series_pattern)?;

        let policy = if stmt.step()? {
            let json = stmt.column_text(0);
            serde_json::from_str::<TimeSeriesLifecyclePolicy>(json).ok()
        } else {
            None
        };
        stmt.reset()?;
        Ok(policy)
    }

    /// Lists all registered declarative lifecycle policies.
    pub fn ts_list_policies(&self) -> Result<Vec<TimeSeriesLifecyclePolicy>> {
        let conn = self.resources.read_lock(&self.lifecycle)?;
        let mut stmt = conn.prepare(
            "SELECT policy_json FROM _lilia_timeseries_policies ORDER BY series_pattern;",
        )?;

        let mut policies = Vec::new();
        while stmt.step()? {
            let json = stmt.column_text(0);
            if let Ok(policy) = serde_json::from_str::<TimeSeriesLifecyclePolicy>(json) {
                policies.push(policy);
            }
        }
        stmt.reset()?;
        Ok(policies)
    }

    /// Deletes a registered declarative lifecycle policy by pattern.
    pub fn ts_delete_policy(&self, series_pattern: &str) -> Result<bool> {
        let writer = self.resources.lock_writer(&self.lifecycle, None)?;
        writer.exec("BEGIN IMMEDIATE;")?;
        let mut stmt =
            writer.prepare("DELETE FROM _lilia_timeseries_policies WHERE series_pattern = ?1;")?;
        stmt.bind_text_static(1, series_pattern)?;
        stmt.step()?;
        stmt.reset()?;
        let changes = writer.pragma_query_int("SELECT changes();").unwrap_or(0);
        writer.exec("COMMIT;")?;
        Ok(changes > 0)
    }

    // ==========================================
    // DOWNSAMPLING & RETENTION EXECUTION
    // ==========================================

    /// Executes downsampling and retention pruning across all active series matching
    /// registered lifecycle policies, using current wall-clock time.
    pub fn ts_apply_retention(&self) -> Result<TimeSeriesRetentionStats> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        self.ts_apply_retention_at(now_ms)
    }

    /// Executes downsampling and retention pruning given an explicit reference timestamp.
    /// Enables deterministic testing and time-travel simulations.
    pub fn ts_apply_retention_at(&self, now_ms: i64) -> Result<TimeSeriesRetentionStats> {
        run_retention_pass(&self.resources, &self.lifecycle, now_ms)
    }

    /// Executes downsampling and retention pruning for a specific series and policy.
    pub fn ts_apply_retention_for_series(
        &self,
        series_id: &str,
        policy: &TimeSeriesLifecyclePolicy,
        now_ms: i64,
    ) -> Result<TimeSeriesRetentionStats> {
        run_retention_for_series(&self.resources, &self.lifecycle, series_id, policy, now_ms)
    }
}

pub(crate) fn append_chunk_internal(
    resources: &Resources,
    lifecycle: &Lifecycle,
    series_id: &str,
    resolution_ms: i64,
    points: &[TimeSeriesPoint],
) -> Result<()> {
    validate_name("series_id", series_id)?;
    if points.is_empty() {
        return Ok(());
    }

    let writer = resources.lock_writer(lifecycle, None)?;
    writer.exec("BEGIN IMMEDIATE;")?;

    let mut stmt = writer.prepare(
        "INSERT OR REPLACE INTO _lilia_timeseries_chunks(
            series_id, resolution_ms, start_time, end_time, point_count, min_val, max_val, sum_val, data
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9);",
    )?;

    for chunk in points.chunks(DEFAULT_CHUNK_SIZE) {
        let start_time = chunk[0].timestamp;
        let end_time = chunk.last().map_or(start_time, |p| p.timestamp);
        let point_count = chunk.len() as i64;

        let mut min_val = chunk[0].value;
        let mut max_val = chunk[0].value;
        let mut sum_val = 0.0;

        for p in chunk {
            if p.value < min_val {
                min_val = p.value;
            }
            if p.value > max_val {
                max_val = p.value;
            }
            sum_val += p.value;
        }

        let compressed_data = compress_timeseries(chunk)?;

        stmt.reset()?;
        stmt.bind_text_static(1, series_id)?;
        stmt.bind_int64(2, resolution_ms)?;
        stmt.bind_int64(3, start_time)?;
        stmt.bind_int64(4, end_time)?;
        stmt.bind_int64(5, point_count)?;
        stmt.bind_double(6, min_val)?;
        stmt.bind_double(7, max_val)?;
        stmt.bind_double(8, sum_val)?;
        stmt.bind_blob_static(9, &compressed_data)?;

        stmt.step()?;
    }

    stmt.reset()?;
    writer.exec("COMMIT;")?;
    Ok(())
}

pub(crate) fn query_range_internal(
    resources: &Resources,
    lifecycle: &Lifecycle,
    series_id: &str,
    resolution_ms: i64,
    start_time: i64,
    end_time: i64,
) -> Result<Vec<TimeSeriesPoint>> {
    validate_name("series_id", series_id)?;
    let conn = resources.read_lock(lifecycle)?;
    let mut stmt = conn.prepare(
        "SELECT data FROM _lilia_timeseries_chunks
         WHERE series_id = ?1 AND resolution_ms = ?2 AND start_time <= ?4 AND end_time >= ?3
         ORDER BY start_time ASC;",
    )?;

    stmt.bind_text_static(1, series_id)?;
    stmt.bind_int64(2, resolution_ms)?;
    stmt.bind_int64(3, start_time)?;
    stmt.bind_int64(4, end_time)?;

    let mut points = Vec::new();

    while stmt.step()? {
        let blob = stmt.column_blob(0);
        let chunk_points = decompress_timeseries(blob)?;
        for p in chunk_points {
            if p.timestamp >= start_time && p.timestamp <= end_time {
                points.push(p);
            }
        }
    }

    stmt.reset()?;
    Ok(points)
}

pub(crate) fn run_retention_pass(
    resources: &Resources,
    lifecycle: &Lifecycle,
    now_ms: i64,
) -> Result<TimeSeriesRetentionStats> {
    // 1. Fetch policies
    let conn = resources.read_lock(lifecycle)?;
    let mut stmt = conn
        .prepare("SELECT policy_json FROM _lilia_timeseries_policies ORDER BY series_pattern;")?;
    let mut policies = Vec::new();
    while stmt.step()? {
        let json = stmt.column_text(0);
        if let Ok(policy) = serde_json::from_str::<TimeSeriesLifecyclePolicy>(json) {
            policies.push(policy);
        }
    }
    stmt.reset()?;

    if policies.is_empty() {
        return Ok(TimeSeriesRetentionStats::default());
    }

    // 2. Fetch distinct series IDs
    let mut series_stmt = conn
        .prepare("SELECT DISTINCT series_id FROM _lilia_timeseries_chunks ORDER BY series_id;")?;
    let mut series_list = Vec::new();
    while series_stmt.step()? {
        series_list.push(series_stmt.column_text(0).to_string());
    }
    series_stmt.reset()?;
    drop(conn);

    let mut total_stats = TimeSeriesRetentionStats::default();

    for series_id in series_list {
        // Find best matching policy: exact match first, then pattern match
        let matched_policy = policies
            .iter()
            .find(|p| p.series_pattern == series_id)
            .or_else(|| {
                policies
                    .iter()
                    .find(|p| pattern_matches(&p.series_pattern, &series_id))
            });

        if let Some(policy) = matched_policy {
            let stats = run_retention_for_series(resources, lifecycle, &series_id, policy, now_ms)?;
            total_stats.series_processed += 1;
            total_stats.downsampled_points_created += stats.downsampled_points_created;
            total_stats.chunks_pruned += stats.chunks_pruned;
            total_stats.bytes_reclaimed_approx += stats.bytes_reclaimed_approx;
        }
    }

    Ok(total_stats)
}

pub(crate) fn run_retention_for_series(
    resources: &Resources,
    lifecycle: &Lifecycle,
    series_id: &str,
    policy: &TimeSeriesLifecyclePolicy,
    now_ms: i64,
) -> Result<TimeSeriesRetentionStats> {
    validate_name("series_id", series_id)?;
    if policy.rules.is_empty() {
        return Ok(TimeSeriesRetentionStats::default());
    }

    let mut stats = TimeSeriesRetentionStats {
        series_processed: 1,
        ..Default::default()
    };

    // Sort rules by resolution ascending
    let mut sorted_rules = policy.rules.clone();
    sorted_rules.sort_by_key(|r| r.resolution_ms);

    // 1. DOWNSAMPLING PASS
    for i in 0..(sorted_rules.len().saturating_sub(1)) {
        let source_rule = &sorted_rules[i];
        let target_rule = &sorted_rules[i + 1];

        let source_res = if source_rule.resolution_ms <= 1_000 {
            0
        } else {
            source_rule.resolution_ms
        };
        let target_res = target_rule.resolution_ms;
        if target_res <= 0 {
            continue;
        }

        // Only downsample complete closed buckets up to now_boundary
        let now_boundary = (now_ms / target_res) * target_res;

        let conn = resources.read_lock(lifecycle)?;
        let mut stmt = conn.prepare(
            "SELECT last_downsampled_time FROM _lilia_timeseries_downsample_state
             WHERE series_id = ?1 AND resolution_ms = ?2;",
        )?;
        stmt.bind_text_static(1, series_id)?;
        stmt.bind_int64(2, target_res)?;

        let last_downsampled = if stmt.step()? {
            Some(stmt.column_int64(0))
        } else {
            None
        };
        stmt.reset()?;

        let start_time = match last_downsampled {
            Some(t) => t,
            None => {
                let mut min_stmt = conn.prepare(
                    "SELECT MIN(start_time) FROM _lilia_timeseries_chunks
                     WHERE series_id = ?1 AND resolution_ms = ?2;",
                )?;
                min_stmt.bind_text_static(1, series_id)?;
                min_stmt.bind_int64(2, source_res)?;
                let min_t = if min_stmt.step()? && !min_stmt.column_is_null(0) {
                    min_stmt.column_int64(0)
                } else {
                    now_boundary
                };
                min_stmt.reset()?;
                (min_t / target_res) * target_res
            }
        };
        drop(conn);

        if start_time < now_boundary {
            let raw_points = query_range_internal(
                resources,
                lifecycle,
                series_id,
                source_res,
                start_time,
                now_boundary - 1,
            )?;

            if !raw_points.is_empty() {
                let downsampled =
                    downsample_points(&raw_points, target_res, target_rule.aggregation);
                if !downsampled.is_empty() {
                    stats.downsampled_points_created += downsampled.len() as u64;
                    append_chunk_internal(
                        resources,
                        lifecycle,
                        series_id,
                        target_res,
                        &downsampled,
                    )?;
                }
            }

            // Update watermark
            let writer = resources.lock_writer(lifecycle, None)?;
            writer.exec("BEGIN IMMEDIATE;")?;
            let mut state_stmt = writer.prepare(
                "INSERT OR REPLACE INTO _lilia_timeseries_downsample_state(
                    series_id, resolution_ms, last_downsampled_time
                 ) VALUES (?1, ?2, ?3);",
            )?;
            state_stmt.bind_text_static(1, series_id)?;
            state_stmt.bind_int64(2, target_res)?;
            state_stmt.bind_int64(3, now_boundary)?;
            state_stmt.step()?;
            state_stmt.reset()?;
            writer.exec("COMMIT;")?;
        }
    }

    // 2. RETENTION PRUNING PASS
    for rule in &sorted_rules {
        if let Some(retention) = rule.retention_ms {
            if retention > 0 {
                let cutoff = now_ms - retention;
                let rule_res = if rule.resolution_ms <= 1_000 {
                    0
                } else {
                    rule.resolution_ms
                };

                let writer = resources.lock_writer(lifecycle, None)?;
                writer.exec("BEGIN IMMEDIATE;")?;

                let mut count_stmt = writer.prepare(
                    "SELECT COUNT(*), COALESCE(SUM(LENGTH(data)), 0)
                     FROM _lilia_timeseries_chunks
                     WHERE series_id = ?1 AND resolution_ms = ?2 AND end_time < ?3;",
                )?;
                count_stmt.bind_text_static(1, series_id)?;
                count_stmt.bind_int64(2, rule_res)?;
                count_stmt.bind_int64(3, cutoff)?;

                let mut chunks_to_prune = 0u64;
                let mut bytes_to_reclaim = 0u64;
                if count_stmt.step()? {
                    chunks_to_prune = count_stmt.column_int64(0).max(0) as u64;
                    bytes_to_reclaim = count_stmt.column_int64(1).max(0) as u64;
                }
                count_stmt.reset()?;

                if chunks_to_prune > 0 {
                    let mut del_stmt = writer.prepare(
                        "DELETE FROM _lilia_timeseries_chunks
                         WHERE series_id = ?1 AND resolution_ms = ?2 AND end_time < ?3;",
                    )?;
                    del_stmt.bind_text_static(1, series_id)?;
                    del_stmt.bind_int64(2, rule_res)?;
                    del_stmt.bind_int64(3, cutoff)?;
                    del_stmt.step()?;
                    del_stmt.reset()?;

                    stats.chunks_pruned += chunks_to_prune;
                    stats.bytes_reclaimed_approx += bytes_to_reclaim;
                }

                writer.exec("COMMIT;")?;
            }
        }
    }

    Ok(stats)
}
