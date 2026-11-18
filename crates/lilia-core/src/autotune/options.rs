use serde::{Deserialize, Serialize};

use crate::{ErrorCode, LiliaError, Result};

/// Which indexes the pruner is allowed to remove.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PruneScope {
    /// Only indexes created by the auto-tuner itself.
    #[default]
    AutoCreatedOnly,
    /// Every index, including those created manually by the application.
    All,
}

/// Configuration of the auto-tuning engine.
///
/// The engine is disabled by default: a database opened without explicit options behaves exactly
/// as before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutotuneOptions {
    /// Master switch. When `false` nothing is recorded and no worker is started.
    pub enabled: bool,
    /// Delay between two tuning cycles, in milliseconds.
    pub interval_ms: u64,
    /// Minimum number of un-indexed executions of a query shape before an index is considered.
    pub min_executions: u64,
    /// Minimum average number of documents examined per un-indexed execution.
    pub min_rows_examined: u64,
    /// Executions slower than this (microseconds) are counted as slow scans.
    pub slow_query_threshold_us: u64,
    /// Build recommended indexes automatically. When `false` they are only reported.
    pub auto_create: bool,
    /// Drop unused indexes automatically. When `false` they are only reported.
    pub auto_prune: bool,
    /// Which indexes may be pruned.
    pub prune_scope: PruneScope,
    /// An index unused for this long (milliseconds) is considered a zombie.
    pub prune_idle_ms: u64,
    /// Minimum delay (milliseconds) before an index that was dropped may be built again.
    pub recreate_cooldown_ms: u64,
    /// Maximum number of auto-created indexes per collection.
    pub max_auto_indexes_per_space: usize,
    /// Maximum number of indexes created, and of indexes dropped, per cycle.
    pub max_actions_per_cycle: usize,
    /// Maximum number of distinct query shapes tracked in memory.
    pub max_tracked_queries: usize,
    /// Documents indexed per writer transaction while backfilling a new index.
    pub backfill_chunk_docs: usize,
}

impl Default for AutotuneOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_ms: 30_000,
            min_executions: 20,
            min_rows_examined: 200,
            slow_query_threshold_us: 5_000,
            auto_create: true,
            auto_prune: false,
            prune_scope: PruneScope::AutoCreatedOnly,
            prune_idle_ms: 7 * 24 * 60 * 60 * 1_000,
            recreate_cooldown_ms: 24 * 60 * 60 * 1_000,
            max_auto_indexes_per_space: 8,
            max_actions_per_cycle: 2,
            max_tracked_queries: 1_024,
            backfill_chunk_docs: 500,
        }
    }
}

impl AutotuneOptions {
    /// Default options with the engine switched on.
    #[must_use]
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }

    /// Checks that the options can drive the engine.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when an interval, threshold or capacity that must be positive is zero.
    pub fn validate(&self) -> Result<()> {
        let checks = [
            (self.interval_ms > 0, "interval_ms"),
            (self.min_executions > 0, "min_executions"),
            (self.max_actions_per_cycle > 0, "max_actions_per_cycle"),
            (self.max_tracked_queries > 0, "max_tracked_queries"),
            (self.backfill_chunk_docs > 0, "backfill_chunk_docs"),
            (
                self.max_auto_indexes_per_space > 0,
                "max_auto_indexes_per_space",
            ),
        ];
        for (valid, name) in checks {
            if !valid {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("autotune option `{name}` must be greater than zero"),
                    false,
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled_and_valid() {
        let options = AutotuneOptions::default();
        assert!(!options.enabled);
        assert!(!options.auto_prune);
        assert!(options.validate().is_ok());
        assert!(AutotuneOptions::enabled().enabled);
    }

    #[test]
    fn zero_capacities_are_rejected() {
        let options = AutotuneOptions {
            backfill_chunk_docs: 0,
            ..AutotuneOptions::enabled()
        };
        let error = options.validate().unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(error.message.contains("backfill_chunk_docs"));
    }
}
