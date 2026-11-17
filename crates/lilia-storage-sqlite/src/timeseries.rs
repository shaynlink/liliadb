#![allow(
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use std::collections::BTreeMap;

use lilia_core::timeseries::{
    compress_timeseries, decompress_timeseries, downsample_points, pattern_matches,
};
use lilia_core::{
    CollectionKind, Result, StorageTier, TimeSeriesLifecyclePolicy, TimeSeriesPoint,
    TimeSeriesRetentionStats, TimeSeriesStats,
};

use crate::engine::Resources;
use crate::lifecycle::Lifecycle;
use crate::storage::{now_ms, validate_name};
use crate::Database;

/// Default point count per compressed timeseries chunk (approx 1.5 - 2.5 KB on disk).
pub const DEFAULT_CHUNK_SIZE: usize = 1_000;

fn compute_ts_stats(
    base: Option<TimeSeriesStats>,
    extra: &[TimeSeriesPoint],
) -> Option<TimeSeriesStats> {
    if extra.is_empty() {
        return base;
    }
    let (mut min, mut max, mut sum, mut count) = match base {
        Some(b) => (b.min, b.max, b.sum, b.count),
        None => (f64::INFINITY, f64::NEG_INFINITY, 0.0, 0),
    };
    for p in extra {
        if p.value < min {
            min = p.value;
        }
        if p.value > max {
            max = p.value;
        }
        sum += p.value;
        count += 1;
    }
    if count == 0 {
        return None;
    }
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

impl Database {
    /// Appends observations to a raw time-series stream (`resolution_ms = 0`).
    ///
    /// The points are partitioned into chunks of up to 1,000 observations,
    /// compressed with the Gorilla Delta-of-Delta & XOR algorithm, and stored
    /// atomically into `_lilia_timeseries_chunks` (or routed by collection tier).
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
        validate_name("series_id", series_id)?;
        self.roll_shard_if_due()?;
        match self.catalog.tier_for(CollectionKind::TimeSeries, series_id) {
            StorageTier::Memory => self
                .mem_engine
                .ts_append_direct(series_id, resolution_ms, points),
            StorageTier::Buffered => self.ring_core.push_ts_points(series_id, resolution_ms, points.to_vec()),
            StorageTier::Disk => append_chunk_internal(
                &self.resources,
                &self.lifecycle,
                series_id,
                resolution_ms,
                points,
            ),
        }
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
        validate_name("series_id", series_id)?;
        match self.catalog.tier_for(CollectionKind::TimeSeries, series_id) {
            StorageTier::Memory => {
                let mut pts = self.mem_engine.ts_query_direct(
                    series_id,
                    resolution_ms,
                    start_time,
                    end_time,
                );
                pts.sort_by_key(|p| p.timestamp);
                Ok(pts)
            }
            StorageTier::Buffered => {
                let mut pts = query_range_internal(
                    &self.resources,
                    &self.lifecycle,
                    series_id,
                    resolution_ms,
                    start_time,
                    end_time,
                )?;
                if let Some(snap) = self.ring_core.overlay.ts.get(
                    series_id,
                    &resolution_ms.to_be_bytes(),
                    now_ms(),
                    false,
                ) {
                    for p in snap.value.points {
                        if p.timestamp >= start_time && p.timestamp <= end_time {
                            pts.push(p);
                        }
                    }
                    pts.sort_by_key(|p| p.timestamp);
                }
                Ok(pts)
            }
            StorageTier::Disk => query_range_internal(
                &self.resources,
                &self.lifecycle,
                series_id,
                resolution_ms,
                start_time,
                end_time,
            ),
        }
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
        let tier = self.catalog.tier_for(CollectionKind::TimeSeries, series_id);
        let mut counts: BTreeMap<i64, u64> = BTreeMap::new();

        if tier == StorageTier::Memory {
            let scanned = self.mem_engine.timeseries.scan(
                Some(series_id),
                None,
                usize::MAX,
                now_ms(),
            );
            for (_sid, key_bytes, snap) in scanned {
                if let Ok(arr) = <[u8; 8]>::try_from(key_bytes.as_slice()) {
                    let res = i64::from_be_bytes(arr);
                    let cnt = snap
                        .value
                        .iter()
                        .filter(|p| p.timestamp >= start_time && p.timestamp <= end_time)
                        .count() as u64;
                    if cnt > 0 {
                        *counts.entry(res).or_insert(0) += cnt;
                    }
                }
            }
        } else {
            let conn = self.resources.read_lock(&self.lifecycle)?;
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
            while stmt.step()? {
                let res = stmt.column_int64(0);
                let count = stmt.column_int64(1).max(0) as u64;
                counts.insert(res, count);
            }
            stmt.reset()?;
            drop(conn);

            if tier == StorageTier::Buffered {
                let scanned = self.ring_core.overlay.ts.scan(
                    Some(series_id),
                    None,
                    usize::MAX,
                    now_ms(),
                );
                for (_sid, key_bytes, snap) in scanned {
                    if let Ok(arr) = <[u8; 8]>::try_from(key_bytes.as_slice()) {
                        let res = i64::from_be_bytes(arr);
                        let cnt = snap
                            .value
                            .points
                            .iter()
                            .filter(|p| p.timestamp >= start_time && p.timestamp <= end_time)
                            .count() as u64;
                        if cnt > 0 {
                            *counts.entry(res).or_insert(0) += cnt;
                        }
                    }
                }
            }
        }

        let available: Vec<(i64, u64)> = counts.into_iter().collect();
        if available.is_empty() {
            return Ok(Vec::new());
        }

        let target_res = if max_points == 0 {
            available[0].0
        } else {
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
        let tier = self.catalog.tier_for(CollectionKind::TimeSeries, series_id);
        if tier == StorageTier::Memory {
            let pts = self.mem_engine.ts_query_direct(
                series_id,
                resolution_ms,
                start_time,
                end_time,
            );
            return Ok(compute_ts_stats(None, &pts));
        }

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
        drop(conn);

        if tier == StorageTier::Buffered {
            if let Some(snap) = self.ring_core.overlay.ts.get(
                series_id,
                &resolution_ms.to_be_bytes(),
                now_ms(),
                false,
            ) {
                let extra: Vec<TimeSeriesPoint> = snap
                    .value
                    .points
                    .into_iter()
                    .filter(|p| p.timestamp >= start_time && p.timestamp <= end_time)
                    .collect();
                return Ok(compute_ts_stats(stats, &extra));
            }
        }

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

/// Appends `points` as compressed chunks. With sharding enabled each point is
/// stored in the shard covering its timestamp (the active shard when no attached
/// shard covers it), so a chunk never spans two shards.
pub(crate) fn append_chunk_in_tx(
    tx: &lilia_sqlite_native::NativeTransaction<'_>,
    series_id: &str,
    resolution_ms: i64,
    points: &[TimeSeriesPoint],
    coalesce_open_tail: bool,
    shards: Option<&crate::sharding::ShardManager>,
) -> Result<()> {
    validate_name("series_id", series_id)?;
    if points.is_empty() {
        return Ok(());
    }
    let Some(shards) = shards else {
        return append_chunk_in_schema(tx, None, series_id, resolution_ms, points, coalesce_open_tail);
    };
    let mut by_schema: std::collections::BTreeMap<String, Vec<TimeSeriesPoint>> =
        std::collections::BTreeMap::new();
    for point in points {
        by_schema
            .entry(shards.resolve_target_shard_alias(Some(point.timestamp)))
            .or_default()
            .push(point.clone());
    }
    for (schema, group) in &by_schema {
        append_chunk_in_schema(
            tx,
            Some(schema),
            series_id,
            resolution_ms,
            group,
            coalesce_open_tail,
        )?;
    }
    Ok(())
}

fn append_chunk_in_schema(
    tx: &lilia_sqlite_native::NativeTransaction<'_>,
    schema: Option<&str>,
    series_id: &str,
    resolution_ms: i64,
    points: &[TimeSeriesPoint],
    coalesce_open_tail: bool,
) -> Result<()> {
    let chunks = crate::sharding::qualified(schema, "_lilia_timeseries_chunks");

    let mut owned_points: Option<Vec<TimeSeriesPoint>> = None;
    if points.windows(2).any(|w| w[0].timestamp > w[1].timestamp) {
        let mut sorted = points.to_vec();
        sorted.sort_by_key(|p| p.timestamp);
        owned_points = Some(sorted);
    }

    if coalesce_open_tail {
        let head_ts = owned_points.as_deref().unwrap_or(points)[0].timestamp;
        let mut tail_stmt = tx.prepare(&format!(
            "SELECT end_time, point_count, data
             FROM {chunks}
             WHERE series_id = ?1 AND resolution_ms = ?2
             ORDER BY start_time DESC
             LIMIT 1;"
        ))?;
        tail_stmt.bind_text_static(1, series_id)?;
        tail_stmt.bind_int64(2, resolution_ms)?;
        let tail_data = if tail_stmt.step()? {
            let end_time = tail_stmt.column_int64(0);
            let point_count = tail_stmt.column_int64(1);
            if point_count > 0 && point_count < (DEFAULT_CHUNK_SIZE as i64) && head_ts >= end_time {
                Some(tail_stmt.column_blob(2).to_vec())
            } else {
                None
            }
        } else {
            None
        };
        drop(tail_stmt);

        if let Some(blob) = tail_data {
            if let Ok(mut combined) = decompress_timeseries(&blob) {
                let incoming = owned_points.as_deref().unwrap_or(points);
                combined.reserve(incoming.len());
                combined.extend_from_slice(incoming);
                owned_points = Some(combined);
            }
        }
    }

    let final_points = owned_points.as_deref().unwrap_or(points);
    let mut stmt = tx.prepare(&format!(
        "INSERT OR REPLACE INTO {chunks}(
            series_id, resolution_ms, start_time, end_time, point_count, min_val, max_val, sum_val, data
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9);"
    ))?;

    for chunk in final_points.chunks(DEFAULT_CHUNK_SIZE) {
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

    Ok(())
}

/// Schemas holding time-series chunks: one unqualified entry without sharding,
/// otherwise every attached shard plus the coordinator.
pub(crate) fn data_schemas(resources: &Resources) -> Vec<Option<String>> {
    resources
        .shards()
        .map_or_else(|| vec![None], |m| m.data_schemas().into_iter().map(Some).collect())
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

    let mut writer = resources.lock_writer(lifecycle, None)?;
    let tx = writer.transaction()?;
    append_chunk_in_tx(&tx, series_id, resolution_ms, points, false, resources.shards())?;
    tx.commit()?;
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
    // Partition pruning: only shards whose period overlaps the range (plus the
    // active shard and coordinator, which also hold out-of-period chunks) are read.
    let chunks = match resources.shards() {
        Some(shards) => format!(
            "(SELECT series_id, resolution_ms, start_time, end_time, data FROM {})",
            shards
                .schemas_overlapping(start_time, end_time)
                .iter()
                .map(|schema| format!("{schema}._lilia_timeseries_chunks"))
                .collect::<Vec<_>>()
                .join(" UNION ALL SELECT series_id, resolution_ms, start_time, end_time, data FROM ")
        ),
        None => "_lilia_timeseries_chunks".to_string(),
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT data FROM {chunks}
         WHERE series_id = ?1 AND resolution_ms = ?2 AND start_time <= ?4 AND end_time >= ?3
         ORDER BY start_time ASC;"
    ))?;

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
                    for schema in data_schemas(resources) {
                        let chunks = crate::sharding::qualified(schema.as_deref(), "_lilia_timeseries_chunks");
                        let mut del_stmt = writer.prepare(&format!(
                            "DELETE FROM {chunks}
                             WHERE series_id = ?1 AND resolution_ms = ?2 AND end_time < ?3;"
                        ))?;
                        del_stmt.bind_text_static(1, series_id)?;
                        del_stmt.bind_int64(2, rule_res)?;
                        del_stmt.bind_int64(3, cutoff)?;
                        del_stmt.step()?;
                        del_stmt.reset()?;
                    }

                    stats.chunks_pruned += chunks_to_prune;
                    stats.bytes_reclaimed_approx += bytes_to_reclaim;
                }

                writer.exec("COMMIT;")?;
            }
        }
    }

    Ok(stats)
}
