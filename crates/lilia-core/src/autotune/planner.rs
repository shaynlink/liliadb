use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::fingerprint::{
    composite_expression, index_fields, index_name, index_name_hashed, is_indexable_field,
    QueryFingerprint, MAX_COMPOSITE_FIELDS,
};
use super::options::{AutotuneOptions, PruneScope};
use super::stats::QueryStats;

/// Who created an index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IndexOrigin {
    /// Synthesized by the auto-tuner.
    Auto,
    /// Created by the application.
    Manual,
}

impl IndexOrigin {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

/// Lifecycle phase of an index known to the auto-tuner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IndexPhase {
    /// Being backfilled; not usable by queries yet.
    Building,
    Ready,
    /// Removed; kept as a tombstone to enforce the re-creation cool-down.
    Dropped,
}

impl IndexPhase {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Building => "building",
            Self::Ready => "ready",
            Self::Dropped => "dropped",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "building" => Some(Self::Building),
            "ready" => Some(Self::Ready),
            "dropped" => Some(Self::Dropped),
            _ => None,
        }
    }
}

/// An index of the database as seen by the planner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedIndex {
    pub space: String,
    pub index_name: String,
    pub expression: String,
    /// Partial indexes only serve queries satisfying their filter, so they never count as covering.
    pub partial: bool,
    pub origin: IndexOrigin,
    pub phase: IndexPhase,
    /// When the tuner first observed the index (start of its idle window).
    pub first_seen_ms: i64,
    pub last_used_ms: Option<i64>,
    pub lookups: u64,
    /// When `phase` last changed (the drop time for tombstones).
    pub changed_ms: i64,
}

/// An action the auto-tuner suggests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Recommendation {
    CreateIndex {
        space: String,
        index_name: String,
        expression: String,
        fields: Vec<String>,
        /// Un-indexed executions that motivated the index.
        scans: u64,
        avg_rows_examined: u64,
    },
    DropIndex {
        space: String,
        index_name: String,
        idle_ms: i64,
        lookups: u64,
    },
}

fn millis(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Decides which indexes to create and which to drop. Pure: all inputs, including the clock, are
/// parameters.
///
/// Creations come first, ordered by the scan work they would save; drops follow, ordered by idle
/// time. Each list is capped by `max_actions_per_cycle`.
#[must_use]
pub fn plan(
    options: &AutotuneOptions,
    queries: &[(QueryFingerprint, QueryStats)],
    indexes: &[TrackedIndex],
    now_ms: i64,
) -> Vec<Recommendation> {
    let mut recommendations = plan_creations(options, queries, indexes, now_ms);
    recommendations.extend(plan_drops(options, indexes, now_ms));
    recommendations
}

fn plan_creations(
    options: &AutotuneOptions,
    queries: &[(QueryFingerprint, QueryStats)],
    indexes: &[TrackedIndex],
    now_ms: i64,
) -> Vec<Recommendation> {
    let live: Vec<(&TrackedIndex, Option<Vec<String>>)> = indexes
        .iter()
        .filter(|index| index.phase != IndexPhase::Dropped)
        .map(|index| (index, index_fields(&index.expression)))
        .collect();

    let mut candidates: Vec<(u128, &QueryFingerprint, &QueryStats)> = queries
        .iter()
        .filter(|(fingerprint, stats)| {
            let fields = &fingerprint.fields;
            let avg_rows = stats.avg_rows_per_scan();
            !fields.is_empty()
                && fields.len() <= MAX_COMPOSITE_FIELDS
                && fields.iter().all(|field| is_indexable_field(field))
                && stats.scans >= options.min_executions
                && (avg_rows >= options.min_rows_examined
                    || stats.slow_scans >= options.min_executions)
                && !live.iter().any(|(index, covered)| {
                    !index.partial
                        && index.space == fingerprint.space
                        && covered.as_ref() == Some(fields)
                })
        })
        .map(|(fingerprint, stats)| {
            let cost = u128::from(stats.scans) * u128::from(stats.avg_rows_per_scan().max(1));
            (cost, fingerprint, stats)
        })
        .collect();
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));

    let mut taken: HashSet<(&str, String)> = live
        .iter()
        .map(|(index, _)| (index.space.as_str(), index.index_name.clone()))
        .collect();
    let mut planned_per_space: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    let mut recommendations = Vec::new();

    for (_, fingerprint, stats) in candidates {
        if recommendations.len() >= options.max_actions_per_cycle {
            break;
        }
        let space = fingerprint.space.as_str();
        let auto_in_space = live
            .iter()
            .filter(|(index, _)| index.space == space && index.origin == IndexOrigin::Auto)
            .count()
            + planned_per_space.get(space).copied().unwrap_or(0);
        if auto_in_space >= options.max_auto_indexes_per_space {
            continue;
        }

        let mut name = index_name(space, &fingerprint.fields);
        if taken.contains(&(space, name.clone())) {
            name = index_name_hashed(space, &fingerprint.fields);
            if taken.contains(&(space, name.clone())) {
                continue;
            }
        }
        let cooling_down = indexes.iter().any(|index| {
            index.phase == IndexPhase::Dropped
                && index.space == space
                && index.index_name == name
                && now_ms.saturating_sub(index.changed_ms) < millis(options.recreate_cooldown_ms)
        });
        if cooling_down {
            continue;
        }

        taken.insert((space, name.clone()));
        *planned_per_space.entry(space).or_default() += 1;
        recommendations.push(Recommendation::CreateIndex {
            space: space.to_owned(),
            index_name: name,
            expression: composite_expression(&fingerprint.fields),
            fields: fingerprint.fields.clone(),
            scans: stats.scans,
            avg_rows_examined: stats.avg_rows_per_scan(),
        });
    }
    recommendations
}

fn plan_drops(options: &AutotuneOptions, indexes: &[TrackedIndex], now_ms: i64) -> Vec<Recommendation> {
    let mut zombies: Vec<(i64, &TrackedIndex)> = indexes
        .iter()
        .filter(|index| index.phase == IndexPhase::Ready)
        .filter(|index| options.prune_scope == PruneScope::All || index.origin == IndexOrigin::Auto)
        .filter_map(|index| {
            let reference = index.last_used_ms.unwrap_or(i64::MIN).max(index.first_seen_ms);
            let idle = now_ms.saturating_sub(reference);
            (idle >= millis(options.prune_idle_ms)).then_some((idle, index))
        })
        .collect();
    zombies.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| (&a.1.space, &a.1.index_name).cmp(&(&b.1.space, &b.1.index_name)))
    });
    zombies
        .into_iter()
        .take(options.max_actions_per_cycle)
        .map(|(idle_ms, index)| Recommendation::DropIndex {
            space: index.space.clone(),
            index_name: index.index_name.clone(),
            idle_ms,
            lookups: index.lookups,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

    fn options() -> AutotuneOptions {
        AutotuneOptions {
            min_executions: 10,
            min_rows_examined: 100,
            max_actions_per_cycle: 4,
            ..AutotuneOptions::enabled()
        }
    }

    fn scanning(fields: &[&str], scans: u64, rows: u64) -> (QueryFingerprint, QueryStats) {
        (
            QueryFingerprint::new("orders", fields.iter().copied()),
            QueryStats {
                executions: scans,
                scans,
                scan_rows_examined: scans * rows,
                ..QueryStats::default()
            },
        )
    }

    fn index(name: &str, expression: &str, origin: IndexOrigin) -> TrackedIndex {
        TrackedIndex {
            space: "orders".to_owned(),
            index_name: name.to_owned(),
            expression: expression.to_owned(),
            partial: false,
            origin,
            phase: IndexPhase::Ready,
            first_seen_ms: 0,
            last_used_ms: None,
            lookups: 0,
            changed_ms: 0,
        }
    }

    #[test]
    fn synthesizes_the_composite_index_of_the_motivating_query() {
        let plan = plan(&options(), &[scanning(&["store_id", "status"], 25, 5_000)], &[], 0);
        assert_eq!(
            plan,
            vec![Recommendation::CreateIndex {
                space: "orders".to_owned(),
                index_name: "idx_orders_status_store_id".to_owned(),
                expression: "composite($.status, $.store_id)".to_owned(),
                fields: vec!["status".to_owned(), "store_id".to_owned()],
                scans: 25,
                avg_rows_examined: 5_000,
            }]
        );
    }

    #[test]
    fn single_field_shapes_get_a_plain_path_index() {
        let plan = plan(&options(), &[scanning(&["status"], 25, 5_000)], &[], 0);
        assert!(matches!(
            &plan[0],
            Recommendation::CreateIndex { expression, .. } if expression == "$.status"
        ));
    }

    #[test]
    fn thresholds_must_be_reached() {
        let few = scanning(&["status"], 9, 5_000);
        let small = scanning(&["store_id"], 50, 10);
        assert!(plan(&options(), &[few, small], &[], 0).is_empty());
    }

    #[test]
    fn slow_scans_qualify_even_over_small_collections() {
        let mut slow = scanning(&["status"], 12, 10);
        slow.1.slow_scans = 12;
        assert_eq!(plan(&options(), &[slow], &[], 0).len(), 1);
    }

    #[test]
    fn already_covered_shapes_are_skipped_regardless_of_column_order() {
        let existing = index("my_idx", "composite($.store_id, $.status)", IndexOrigin::Manual);
        let queries = [scanning(&["status", "store_id"], 50, 5_000)];
        assert!(plan(&options(), &queries, std::slice::from_ref(&existing), 0).is_empty());

        let partial = TrackedIndex { partial: true, ..existing };
        assert_eq!(plan(&options(), &queries, &[partial], 0).len(), 1);
    }

    #[test]
    fn unindexable_shapes_are_ignored() {
        let queries = [
            scanning(&["lower($.email)"], 50, 5_000),
            scanning(&["a", "b", "c", "d", "e"], 50, 5_000),
        ];
        assert!(plan(&options(), &queries, &[], 0).is_empty());
    }

    #[test]
    fn most_expensive_shapes_win_the_per_cycle_budget() {
        let queries = [
            scanning(&["cheap"], 20, 200),
            scanning(&["costly"], 20, 9_000),
            scanning(&["middle"], 20, 2_000),
        ];
        let limited = AutotuneOptions { max_actions_per_cycle: 2, ..options() };
        let names: Vec<_> = plan(&limited, &queries, &[], 0)
            .into_iter()
            .map(|r| match r {
                Recommendation::CreateIndex { index_name, .. }
                | Recommendation::DropIndex { index_name, .. } => index_name,
            })
            .collect();
        assert_eq!(names, vec!["idx_orders_costly", "idx_orders_middle"]);
    }

    #[test]
    fn per_space_quota_counts_existing_auto_indexes() {
        let limited = AutotuneOptions { max_auto_indexes_per_space: 1, ..options() };
        let existing = index("idx_orders_other", "$.other", IndexOrigin::Auto);
        assert!(plan(&limited, &[scanning(&["status"], 50, 5_000)], &[existing], 0).is_empty());
        let manual = index("idx_orders_other", "$.other", IndexOrigin::Manual);
        assert_eq!(plan(&limited, &[scanning(&["status"], 50, 5_000)], &[manual], 0).len(), 1);
    }

    #[test]
    fn name_collisions_never_overwrite_an_unrelated_index() {
        let squatter = index("idx_orders_status", "lower($.status)", IndexOrigin::Manual);
        let plan = plan(&options(), &[scanning(&["status"], 50, 5_000)], &[squatter], 0);
        let Recommendation::CreateIndex { index_name, .. } = &plan[0] else {
            panic!("expected a creation");
        };
        assert_ne!(index_name, "idx_orders_status");
        assert!(index_name.starts_with("idx_orders_status_"));
    }

    #[test]
    fn dropped_indexes_cool_down_before_being_rebuilt() {
        let queries = [scanning(&["status"], 50, 5_000)];
        let tombstone = TrackedIndex {
            phase: IndexPhase::Dropped,
            changed_ms: 10 * DAY_MS,
            ..index("idx_orders_status", "$.status", IndexOrigin::Auto)
        };
        let options = options();
        let during = 10 * DAY_MS + millis(options.recreate_cooldown_ms) - 1;
        assert!(plan(&options, &queries, std::slice::from_ref(&tombstone), during).is_empty());
        assert_eq!(plan(&options, &queries, &[tombstone], during + 1).len(), 1);
    }

    #[test]
    fn zombie_auto_indexes_are_proposed_after_the_idle_window() {
        let options = options();
        let idle = millis(options.prune_idle_ms);
        let unused = index("idx_orders_a", "$.a", IndexOrigin::Auto);
        assert!(plan(&options, &[], std::slice::from_ref(&unused), idle - 1).is_empty());
        assert_eq!(
            plan(&options, &[], std::slice::from_ref(&unused), idle),
            vec![Recommendation::DropIndex {
                space: "orders".to_owned(),
                index_name: "idx_orders_a".to_owned(),
                idle_ms: idle,
                lookups: 0,
            }]
        );

        let used = TrackedIndex { last_used_ms: Some(idle), lookups: 7, ..unused };
        assert!(plan(&options, &[], &[used], idle + 1).is_empty());
    }

    #[test]
    fn manual_indexes_are_only_pruned_with_the_all_scope() {
        let manual = index("mine", "$.a", IndexOrigin::Manual);
        let now = millis(options().prune_idle_ms) * 2;
        assert!(plan(&options(), &[], std::slice::from_ref(&manual), now).is_empty());
        let all = AutotuneOptions { prune_scope: PruneScope::All, ..options() };
        assert_eq!(plan(&all, &[], &[manual], now).len(), 1);
    }

    #[test]
    fn building_and_dropped_indexes_are_never_pruned() {
        let now = millis(options().prune_idle_ms) * 2;
        let building = TrackedIndex { phase: IndexPhase::Building, ..index("b", "$.a", IndexOrigin::Auto) };
        let dropped = TrackedIndex { phase: IndexPhase::Dropped, ..index("d", "$.a", IndexOrigin::Auto) };
        assert!(plan(&options(), &[], &[building, dropped], now).is_empty());
    }

    #[test]
    fn enum_names_round_trip() {
        for origin in [IndexOrigin::Auto, IndexOrigin::Manual] {
            assert_eq!(IndexOrigin::parse(origin.as_str()), Some(origin));
        }
        for phase in [IndexPhase::Building, IndexPhase::Ready, IndexPhase::Dropped] {
            assert_eq!(IndexPhase::parse(phase.as_str()), Some(phase));
        }
        assert_eq!(IndexOrigin::parse("?"), None);
    }
}
