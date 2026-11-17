use serde::{Deserialize, Serialize};

/// One observed execution of a JSON `find`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryObservation {
    /// Whether a secondary index served the lookup (otherwise the collection was scanned).
    pub used_index: bool,
    /// Documents read and tested against the filter.
    pub rows_examined: u64,
    /// Documents returned to the caller.
    pub rows_returned: u64,
    pub duration_us: u64,
    pub at_ms: i64,
}

/// Cumulative statistics of one query shape.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryStats {
    pub executions: u64,
    /// Executions that scanned the collection because no index matched.
    pub scans: u64,
    /// Scans slower than the configured threshold.
    pub slow_scans: u64,
    /// Documents examined by the scans only.
    pub scan_rows_examined: u64,
    /// Documents examined by all executions.
    pub rows_examined: u64,
    pub rows_returned: u64,
    pub total_duration_us: u64,
    pub max_duration_us: u64,
    pub first_seen_ms: i64,
    pub last_seen_ms: i64,
}

impl QueryStats {
    pub(crate) fn record(&mut self, observation: &QueryObservation, slow_threshold_us: u64) {
        if self.executions == 0 {
            self.first_seen_ms = observation.at_ms;
        }
        self.executions = self.executions.saturating_add(1);
        self.rows_examined = self.rows_examined.saturating_add(observation.rows_examined);
        self.rows_returned = self.rows_returned.saturating_add(observation.rows_returned);
        self.total_duration_us = self
            .total_duration_us
            .saturating_add(observation.duration_us);
        self.max_duration_us = self.max_duration_us.max(observation.duration_us);
        self.last_seen_ms = observation.at_ms;
        if !observation.used_index {
            self.scans = self.scans.saturating_add(1);
            self.scan_rows_examined = self
                .scan_rows_examined
                .saturating_add(observation.rows_examined);
            if observation.duration_us >= slow_threshold_us {
                self.slow_scans = self.slow_scans.saturating_add(1);
            }
        }
    }

    /// Average number of documents examined by one scan.
    #[must_use]
    pub fn avg_rows_per_scan(&self) -> u64 {
        self.scan_rows_examined.checked_div(self.scans).unwrap_or(0)
    }

    /// Forgets the scan evidence, e.g. once an index now serves this shape.
    pub(crate) fn clear_scan_evidence(&mut self) {
        self.scans = 0;
        self.slow_scans = 0;
        self.scan_rows_examined = 0;
    }
}

/// One observed execution of a SQL statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqlObservation {
    pub duration_us: u64,
    /// Rows stepped over by full table scans (`SQLITE_STMTSTATUS_FULLSCAN_STEP`).
    pub full_scan_steps: u64,
    pub at_ms: i64,
}

/// Cumulative statistics of one normalized SQL statement.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqlQueryStats {
    pub executions: u64,
    /// Executions that performed at least one full table scan step.
    pub full_scan_executions: u64,
    pub full_scan_steps: u64,
    pub slow_executions: u64,
    pub total_duration_us: u64,
    pub max_duration_us: u64,
    pub first_seen_ms: i64,
    pub last_seen_ms: i64,
}

impl SqlQueryStats {
    pub(crate) fn record(&mut self, observation: &SqlObservation, slow_threshold_us: u64) {
        if self.executions == 0 {
            self.first_seen_ms = observation.at_ms;
        }
        self.executions = self.executions.saturating_add(1);
        self.total_duration_us = self
            .total_duration_us
            .saturating_add(observation.duration_us);
        self.max_duration_us = self.max_duration_us.max(observation.duration_us);
        self.last_seen_ms = observation.at_ms;
        if observation.full_scan_steps > 0 {
            self.full_scan_executions = self.full_scan_executions.saturating_add(1);
            self.full_scan_steps = self
                .full_scan_steps
                .saturating_add(observation.full_scan_steps);
        }
        if observation.duration_us >= slow_threshold_us {
            self.slow_executions = self.slow_executions.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(used_index: bool, rows: u64, duration_us: u64, at_ms: i64) -> QueryObservation {
        QueryObservation {
            used_index,
            rows_examined: rows,
            rows_returned: 1,
            duration_us,
            at_ms,
        }
    }

    #[test]
    fn scans_and_indexed_runs_are_counted_separately() {
        let mut stats = QueryStats::default();
        stats.record(&observation(false, 1_000, 9_000, 10), 5_000);
        stats.record(&observation(false, 3_000, 100, 20), 5_000);
        stats.record(&observation(true, 5, 10, 30), 5_000);
        assert_eq!(stats.executions, 3);
        assert_eq!(stats.scans, 2);
        assert_eq!(stats.slow_scans, 1);
        assert_eq!(stats.avg_rows_per_scan(), 2_000);
        assert_eq!(stats.rows_examined, 4_005);
        assert_eq!(stats.max_duration_us, 9_000);
        assert_eq!((stats.first_seen_ms, stats.last_seen_ms), (10, 30));
        stats.clear_scan_evidence();
        assert_eq!(
            (stats.scans, stats.slow_scans, stats.avg_rows_per_scan()),
            (0, 0, 0)
        );
        assert_eq!(stats.executions, 3);
    }

    #[test]
    fn sql_full_scans_are_detected() {
        let mut stats = SqlQueryStats::default();
        stats.record(
            &SqlObservation {
                duration_us: 10,
                full_scan_steps: 0,
                at_ms: 1,
            },
            5_000,
        );
        stats.record(
            &SqlObservation {
                duration_us: 8_000,
                full_scan_steps: 250,
                at_ms: 2,
            },
            5_000,
        );
        assert_eq!(stats.executions, 2);
        assert_eq!(stats.full_scan_executions, 1);
        assert_eq!(stats.full_scan_steps, 250);
        assert_eq!(stats.slow_executions, 1);
    }
}
