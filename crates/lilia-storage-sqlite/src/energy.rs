//! Battery-Aware Query Profiling and Pedagogical Predictive Diagnostics Engine.
//!
//! Evaluates query energy consumption in micro-joules (µJ) across CPU VM steps,
//! physical flash I/O, and memory bus wakeups. Inspects SQLite physical execution
//! plans to synthesize pedagogical diagnostics and predictive index recommendations.

use std::sync::Arc;
use std::time::Instant;

use lilia_core::energy::{
    DeviceEnergyProfile, EnergyDiagnosis, QueryEnergyDiagnostic, QueryEnergyMetrics,
};
use lilia_core::wear::FlashWearTracker;
use lilia_core::Result;
use lilia_sqlite_native::{ExplainPlanRow, NativeConnection};
use serde_json::Value;

/// Profiler and diagnostic generator for database queries.
#[derive(Debug, Clone)]
pub struct BatteryAwareProfiler {
    profile: DeviceEnergyProfile,
    wear_tracker: Arc<FlashWearTracker>,
}

impl BatteryAwareProfiler {
    /// Creates a new battery-aware query profiler.
    #[must_use]
    pub fn new(profile: DeviceEnergyProfile, wear_tracker: Arc<FlashWearTracker>) -> Self {
        Self {
            profile,
            wear_tracker,
        }
    }

    /// Returns the active device energy profile.
    #[must_use]
    pub const fn profile(&self) -> DeviceEnergyProfile {
        self.profile
    }

    /// Sets the active device energy profile.
    pub fn set_profile(&mut self, profile: DeviceEnergyProfile) {
        self.profile = profile;
    }

    /// Diagnoses and profiles a SQL statement, executing it on the given connection
    /// and analyzing the SQLite query plan to produce pedagogical recommendations.
    pub fn diagnose_sql(
        &self,
        conn: &NativeConnection,
        sql: &str,
        params: &[Value],
    ) -> Result<QueryEnergyDiagnostic> {
        let clean_sql = strip_explain_prefix(sql);

        // 1. Measure physical execution (also ensures connection schema is fresh)
        let (mut metrics, _) = self.profile_execution(conn, clean_sql, params)?;

        // 2. Collect query execution plan
        let plan_steps = conn.explain_query_plan(clean_sql)?;

        // 3. Compute energy in micro-joules (µJ)
        let total_energy_uj = self.profile.calculate_energy(&mut metrics);

        // 4. Analyze execution plan & anti-patterns
        let (diagnosis, plan_details) = analyze_plan(&plan_steps, &metrics);

        // 5. Synthesize pedagogical recommendation and predictive savings
        let (recommendation, recommended_sql, pred_energy, savings_pct) = synthesize_recommendation(
            conn,
            clean_sql,
            diagnosis,
            &plan_steps,
            &metrics,
            total_energy_uj,
        );

        Ok(QueryEnergyDiagnostic {
            query: clean_sql.to_string(),
            execution_time_us: metrics.execution_time_us,
            estimated_energy_uj: total_energy_uj,
            diagnosis,
            recommendation,
            recommended_index_sql: recommended_sql,
            predicted_energy_uj: pred_energy,
            energy_savings_percent: savings_pct,
            metrics: Some(metrics),
            plan_details,
        })
    }

    /// Profiles the physical execution of a query and returns its hardware metrics
    /// along with the retrieved rows.
    pub fn profile_execution(
        &self,
        conn: &NativeConnection,
        sql: &str,
        params: &[Value],
    ) -> Result<(QueryEnergyMetrics, Vec<Vec<Value>>)> {
        let mut stmt = conn.prepare(sql)?;
        let col_count = stmt.column_count();

        // Bind parameters
        let mut string_cache = Vec::new();
        crate::sql::bind_json_params(&mut stmt, params, &mut string_cache)?;

        // Capture hardware state before execution
        let pager_before = conn.pager_stats(false);
        let now_ms = crate::storage::now_ms();
        let wear_policy = lilia_core::wear::WearLevelingPolicy::default();
        let wear_before = self.wear_tracker.snapshot(now_ms, &wear_policy);
        let started = Instant::now();

        // Execute query
        let mut rows = Vec::new();
        while stmt.step()? {
            let mut row = Vec::with_capacity(col_count);
            for i in 0..col_count {
                row.push(crate::sql::read_column_as_json(&stmt, i as i32));
            }
            rows.push(row);
        }

        let elapsed = started.elapsed();
        let elapsed_us = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);

        // Capture hardware state after execution
        let stmt_counters = stmt.energy_counters(false);
        let pager_after = conn.pager_stats(false);
        let wear_after = self.wear_tracker.snapshot(now_ms, &wear_policy);

        let cache_hits = (pager_after.cache_hit - pager_before.cache_hit).max(0) as u64;
        let cache_misses = (pager_after.cache_miss - pager_before.cache_miss).max(0) as u64;
        let cache_writes = (pager_after.cache_write - pager_before.cache_write).max(0) as u64;
        let fsync_count = wear_after
            .total_fsync_count
            .saturating_sub(wear_before.total_fsync_count);

        let lookaside_misses = ((pager_after.lookaside_miss_size
            - pager_before.lookaside_miss_size)
            + (pager_after.lookaside_miss_full - pager_before.lookaside_miss_full))
            .max(0) as u64;

        let metrics = QueryEnergyMetrics {
            execution_time_us: elapsed_us,
            cpu_vm_steps: stmt_counters.vm_steps,
            full_scan_steps: stmt_counters.fullscan_steps,
            sort_operations: stmt_counters.sort_operations,
            auto_index_inserts: stmt_counters.auto_index_inserts,
            page_cache_hits: cache_hits,
            page_cache_misses: cache_misses,
            page_cache_writes: cache_writes,
            fsync_count,
            lookaside_misses,
            cpu_energy_uj: 0.0,
            flash_energy_uj: 0.0,
            memory_energy_uj: 0.0,
            total_energy_uj: 0.0,
        };

        Ok((metrics, rows))
    }
}

/// Strips any `EXPLAIN DIAGNOSE` or `EXPLAIN (ENERGY)` prefix from a SQL query.
#[must_use]
pub fn strip_explain_prefix(sql: &str) -> &str {
    let trimmed = sql.trim();
    let lower = trimmed.to_ascii_lowercase();

    if let Some(rest) = lower.strip_prefix("explain diagnose ") {
        trimmed[trimmed.len() - rest.len()..].trim()
    } else if let Some(rest) = lower.strip_prefix("explain battery ") {
        trimmed[trimmed.len() - rest.len()..].trim()
    } else if let Some(rest) = lower.strip_prefix("explain energy ") {
        trimmed[trimmed.len() - rest.len()..].trim()
    } else if let Some(rest) = lower.strip_prefix("explain (energy) ") {
        trimmed[trimmed.len() - rest.len()..].trim()
    } else if let Some(rest) = lower.strip_prefix("explain (diagnose) ") {
        trimmed[trimmed.len() - rest.len()..].trim()
    } else if let Some(rest) = lower.strip_prefix("explain predictive ") {
        trimmed[trimmed.len() - rest.len()..].trim()
    } else {
        trimmed
    }
}

/// Checks if a SQL query string requests energy diagnosis.
#[must_use]
pub fn is_explain_diagnose_query(sql: &str) -> bool {
    let lower = sql.trim_start().to_ascii_lowercase();
    lower.starts_with("explain diagnose ")
        || lower.starts_with("explain battery ")
        || lower.starts_with("explain energy ")
        || lower.starts_with("explain (energy) ")
        || lower.starts_with("explain (diagnose) ")
        || lower.starts_with("explain predictive ")
}

/// Analyzes plan step strings and runtime metrics to identify anti-patterns.
fn analyze_plan(
    plan_steps: &[ExplainPlanRow],
    metrics: &QueryEnergyMetrics,
) -> (EnergyDiagnosis, Vec<String>) {
    let mut details = Vec::new();
    let mut has_scan = false;
    let mut has_temp_sort = false;
    let mut has_indexed_seek = false;
    let mut has_auto_index = false;

    for step in plan_steps {
        details.push(step.detail.clone());
        let lower = step.detail.to_ascii_lowercase();

        if (lower.contains("scan ") || lower.contains("scan table "))
            && !lower.contains("using index")
            && !lower.contains("using covering index")
            && !lower.contains("using automatic")
        {
            has_scan = true;
        }

        if lower.contains("use temp b-tree") {
            has_temp_sort = true;
        }

        if lower.contains("using automatic") {
            has_auto_index = true;
        }

        if lower.contains("using index")
            || lower.contains("using covering index")
            || lower.contains("search ")
        {
            has_indexed_seek = true;
        }
    }

    if metrics.full_scan_steps > 0 {
        has_scan = true;
    }

    if metrics.fsync_count >= 2 {
        (EnergyDiagnosis::ExcessiveFsyncDetected, details)
    } else if has_scan {
        (EnergyDiagnosis::FullTableScanDetected, details)
    } else if has_temp_sort {
        (EnergyDiagnosis::TemporaryBTreeSort, details)
    } else if has_auto_index {
        (EnergyDiagnosis::UnindexedJoin, details)
    } else if has_indexed_seek {
        (EnergyDiagnosis::OptimalIndexUsed, details)
    } else if metrics.page_cache_misses > 20 || metrics.lookaside_misses > 50 {
        (EnergyDiagnosis::HighMemoryBusPressure, details)
    } else {
        (EnergyDiagnosis::Normal, details)
    }
}

/// Synthesizes index recommendations and calculates predicted energy savings.
fn synthesize_recommendation(
    conn: &NativeConnection,
    sql: &str,
    diagnosis: EnergyDiagnosis,
    plan_steps: &[ExplainPlanRow],
    metrics: &QueryEnergyMetrics,
    current_energy_uj: f64,
) -> (Option<String>, Option<String>, Option<f64>, Option<f64>) {
    match diagnosis {
        EnergyDiagnosis::FullTableScanDetected => {
            // Extract table name from plan or query
            let table = extract_table_name(sql, plan_steps).unwrap_or_else(|| "logs".to_string());
            let (col, filter_val) = extract_filter_predicate(sql);

            // Fetch table row count to estimate savings
            let row_count = fetch_table_row_count(conn, &table).unwrap_or(1_000);
            let n = metrics.full_scan_steps.max(row_count).max(100);

            // Index seek work: O(log2 N) B-tree page lookups + matching fraction
            let index_seek_steps = ((n as f64).log2().ceil() as u64).max(3);
            let matching_steps = (n / 25).max(1); // typical filtered selectivity
            let indexed_total_work = index_seek_steps + matching_steps;

            let savings_ratio = (n.saturating_sub(indexed_total_work) as f64) / (n as f64).max(1.0);
            let savings_pct = (savings_ratio * 100.0).clamp(50.0, 99.0);
            let savings_int = savings_pct.round() as u64;

            let pred_energy =
                ((current_energy_uj * (1.0 - savings_pct / 100.0)) * 10.0).round() / 10.0;

            if let (Some(c), Some(v)) = (col.as_deref(), filter_val.as_deref()) {
                // Partial index recommendation
                let slug = sanitize_slug(v);
                let index_name = if slug.is_empty() {
                    format!("idx_{table}_{c}")
                } else {
                    format!("idx_{table}_{slug}")
                };
                let recommended_sql =
                    format!("CREATE INDEX {index_name} ON {table}({c}) WHERE {c} = '{v}';");
                let recommendation = format!(
                    "Créer l'index partiel {index_name} réduirait la consommation énergétique de {savings_int} %."
                );

                (
                    Some(recommendation),
                    Some(recommended_sql),
                    Some(pred_energy),
                    Some(savings_pct),
                )
            } else if let Some(c) = col.as_deref() {
                // Standard index recommendation
                let index_name = format!("idx_{table}_{c}");
                let recommended_sql = format!("CREATE INDEX {index_name} ON {table}({c});");
                let recommendation = format!(
                    "Créer l'index {index_name} réduirait la consommation énergétique de {savings_int} %."
                );

                (
                    Some(recommendation),
                    Some(recommended_sql),
                    Some(pred_energy),
                    Some(savings_pct),
                )
            } else {
                let recommendation = format!(
                    "Créer un index sur la table '{table}' pour couvrir le prédicat réduirait la consommation énergétique de {savings_int} %."
                );
                (
                    Some(recommendation),
                    None,
                    Some(pred_energy),
                    Some(savings_pct),
                )
            }
        }
        EnergyDiagnosis::TemporaryBTreeSort => {
            let table =
                extract_table_name(sql, plan_steps).unwrap_or_else(|| "records".to_string());
            let order_col = extract_order_by_col(sql).unwrap_or_else(|| "id".to_string());
            let index_name = format!("idx_{table}_{order_col}");
            let recommended_sql = format!("CREATE INDEX {index_name} ON {table}({order_col});");
            let savings_pct = 75.0;
            let pred_energy = ((current_energy_uj * 0.25) * 10.0).round() / 10.0;
            let recommendation = format!(
                "Créer l'index {index_name} sur {table}({order_col}) éliminerait le tri temporaire B-Tree et réduirait la consommation énergétique de 75 %."
            );

            (
                Some(recommendation),
                Some(recommended_sql),
                Some(pred_energy),
                Some(savings_pct),
            )
        }
        EnergyDiagnosis::ExcessiveFsyncDetected => {
            let recommendation =
                "Regrouper les opérations dans une transaction unique pour coalescer les fsyncs préserverait significativement la batterie."
                    .to_string();
            (Some(recommendation), None, None, None)
        }
        EnergyDiagnosis::OptimalIndexUsed => {
            let recommendation =
                "Requête optimisée : l'utilisation d'index couvre efficacement l'accès mémoire et préserve la batterie."
                    .to_string();
            (Some(recommendation), None, None, None)
        }
        _ => (None, None, None, None),
    }
}

/// Extracts table name from SQLite plan detail or raw query.
fn extract_table_name(sql: &str, plan_steps: &[ExplainPlanRow]) -> Option<String> {
    // 1. Check plan steps: "SCAN logs", "SCAN TABLE logs"
    for step in plan_steps {
        let text = &step.detail;
        if let Some(idx) = text.find("SCAN TABLE ") {
            let after = text[idx + "SCAN TABLE ".len()..].trim();
            let table = after.split_whitespace().next().unwrap_or("");
            if !table.is_empty() {
                return Some(clean_ident(table));
            }
        } else if let Some(idx) = text.find("SCAN ") {
            let after = text[idx + "SCAN ".len()..].trim();
            let table = after.split_whitespace().next().unwrap_or("");
            if !table.is_empty() && !table.eq_ignore_ascii_case("table") {
                return Some(clean_ident(table));
            }
        }
    }

    // 2. Fallback to SQL parsing: FROM <table>
    let lower = sql.to_ascii_lowercase();
    if let Some(from_idx) = lower.find(" from ") {
        let after = sql[from_idx + " from ".len()..].trim();
        let table = after
            .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
            .next()
            .unwrap_or("");
        if !table.is_empty() {
            return Some(clean_ident(table));
        }
    }

    None
}

/// Extracts column name and literal filter value from WHERE clause.
/// e.g. `WHERE level = 'ERROR'` -> (`Some("level")`, `Some("ERROR")`).
fn extract_filter_predicate(sql: &str) -> (Option<String>, Option<String>) {
    let lower = sql.to_ascii_lowercase();
    let Some(where_idx) = lower.find(" where ") else {
        return (None, None);
    };

    let after_where = sql[where_idx + " where ".len()..].trim();
    // Look for equality: col = 'val' or col = "val"
    let Some(eq_idx) = after_where.find('=') else {
        // Look for simple identifier
        let col = after_where
            .split_whitespace()
            .next()
            .map(clean_ident)
            .filter(|s| !s.is_empty());
        return (col, None);
    };

    let left = after_where[..eq_idx].trim();
    let right = after_where[eq_idx + 1..].trim();

    // Extract column from left
    let col = left.split_whitespace().next_back().map(clean_ident);

    // Extract value from right (quoted string or identifier)
    let val = if let Some(stripped) = right.strip_prefix('\'') {
        stripped.split('\'').next().map(ToString::to_string)
    } else if let Some(stripped) = right.strip_prefix('"') {
        stripped.split('"').next().map(ToString::to_string)
    } else {
        right
            .split(|c: char| c.is_whitespace() || c == ';' || c == ')')
            .next()
            .map(clean_ident)
    };

    (col, val)
}

/// Extracts ORDER BY column name from SQL.
fn extract_order_by_col(sql: &str) -> Option<String> {
    let lower = sql.to_ascii_lowercase();
    let idx = lower.find(" order by ")?;
    let after = sql[idx + " order by ".len()..].trim();
    let col = after
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .next()?;
    Some(clean_ident(col))
}

fn fetch_table_row_count(conn: &NativeConnection, table: &str) -> Option<u64> {
    let clean = clean_ident(table);
    let count_sql = format!("SELECT count(*) FROM {clean};");
    conn.query_int(&count_sql).ok().map(|c| c.max(0) as u64)
}

fn clean_ident(s: &str) -> String {
    s.trim_matches(|c| c == '"' || c == '`' || c == '\'' || c == '[' || c == ']')
        .to_string()
}

fn sanitize_slug(val: &str) -> String {
    val.chars()
        .filter_map(|c| {
            if c.is_ascii_alphanumeric() {
                Some(c.to_ascii_lowercase())
            } else if c == '_' || c == '-' {
                Some('_')
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_predicate() {
        let sql = "SELECT * FROM logs WHERE level = 'ERROR'";
        let (col, val) = extract_filter_predicate(sql);
        assert_eq!(col.as_deref(), Some("level"));
        assert_eq!(val.as_deref(), Some("ERROR"));
    }

    #[test]
    fn test_strip_explain_prefix() {
        assert_eq!(
            strip_explain_prefix("EXPLAIN DIAGNOSE SELECT * FROM logs WHERE level = 'ERROR'"),
            "SELECT * FROM logs WHERE level = 'ERROR'"
        );
        assert_eq!(
            strip_explain_prefix("explain battery SELECT 1;"),
            "SELECT 1;"
        );
    }
}
