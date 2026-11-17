use std::path::Path;
use std::time::Duration;

use lilia_core::{
    AutotuneOptions, BatchOperation, ErrorCode, IndexOrigin, PruneScope, Recommendation,
};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

const STATUSES: [&str; 4] = ["paid", "pending", "shipped", "refunded"];

fn tuned() -> AutotuneOptions {
    AutotuneOptions {
        min_executions: 5,
        min_rows_examined: 10,
        // The background worker never fires during a test: cycles are driven explicitly.
        interval_ms: 3_600_000,
        backfill_chunk_docs: 25,
        ..AutotuneOptions::enabled()
    }
}

fn open(dir: &Path, options: AutotuneOptions) -> Database {
    Database::open(DatabaseOptions::durable(dir.join("db.lilia")).with_autotune(options)).unwrap()
}

fn seed_orders(db: &Database, count: usize) {
    let batch: Vec<BatchOperation> = (0..count)
        .map(|i| BatchOperation::JsonPut {
            space: "orders".into(),
            id: format!("order_{i:03}"),
            value: json!({
                "status": STATUSES[i % STATUSES.len()],
                "store_id": i % 10,
                "total": i,
            }),
            if_version: None,
        })
        .collect();
    db.batch(&batch).unwrap();
}

fn find_ids(db: &Database, query: &serde_json::Value) -> Vec<String> {
    let query = query.as_object().unwrap();
    let mut ids: Vec<String> = db
        .json_find("orders", query, None, 1_000)
        .unwrap()
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    ids.sort();
    ids
}

fn expected_ids(count: usize, status: &str, store_id: usize) -> Vec<String> {
    (0..count)
        .filter(|i| STATUSES[i % STATUSES.len()] == status && i % 10 == store_id)
        .map(|i| format!("order_{i:03}"))
        .collect()
}

fn count_rows(db: &Database, sql: &str) -> i64 {
    db.sql_query(sql, &[]).unwrap().rows[0][0].as_i64().unwrap()
}

#[test]
fn composite_index_is_synthesized_from_repeated_full_scans() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), tuned());
    seed_orders(&db, 200);
    let query = json!({ "status": "paid", "store_id": 4 });
    let expected = expected_ids(200, "paid", 4);
    assert_ne!(expected, [] as [std::string::String; 0]);

    for _ in 0..6 {
        assert_eq!(find_ids(&db, &query), expected);
    }
    let before = db.autotune_report().unwrap();
    assert_eq!(before.json_queries.len(), 1);
    assert_eq!(before.json_queries[0].fields, vec!["status", "store_id"]);
    assert_eq!(before.json_queries[0].stats.scans, 6);
    assert!(before.json_queries[0].stats.avg_rows_per_scan() >= 200);
    assert!(matches!(
        &before.recommendations[..],
        [Recommendation::CreateIndex { index_name, .. }] if index_name == "idx_orders_status_store_id"
    ));
    assert_eq!(
        db.list_json_indexes("orders").unwrap(),
        [] as [lilia_core::JsonIndexInfo; 0]
    );

    let cycle = db.autotune_run_now().unwrap();
    assert_eq!(cycle.created.len(), 1);
    assert_eq!(cycle.created[0].index_name, "idx_orders_status_store_id");
    assert!(cycle.failures.is_empty(), "{:?}", cycle.failures);

    let indexes = db.list_json_indexes("orders").unwrap();
    assert_eq!(indexes.len(), 1);
    assert_eq!(indexes[0].json_path, "composite($.status, $.store_id)");
    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM _lilia_json_index_entries WHERE index_name = 'idx_orders_status_store_id'"
        ),
        200
    );

    // Same answers, now served by the index, whatever the order of the query keys.
    assert_eq!(find_ids(&db, &query), expected);
    assert_eq!(
        find_ids(&db, &json!({ "store_id": 4, "status": "paid" })),
        expected
    );
    let after = db.autotune_report().unwrap();
    let index = &after.indexes[0];
    assert_eq!(index.origin, IndexOrigin::Auto);
    assert_eq!(index.entries, 200);
    assert_eq!(index.lookups, 2);
    assert!(index.approx_bytes > 0);
    assert!(!index.zombie);
    assert_eq!(
        after.json_queries[0].stats.scans, 0,
        "scan evidence is cleared once indexed"
    );
    assert_eq!(after.recommendations, [] as [lilia_core::Recommendation; 0]);

    // Nothing left to do: a second cycle changes nothing.
    let again = db.autotune_run_now().unwrap();
    assert!(again.created.is_empty() && again.dropped.is_empty());
}

#[test]
fn composite_index_stays_correct_across_updates_and_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), AutotuneOptions::default());
    seed_orders(&db, 60);
    db.create_json_index("orders", "idx_pair", "composite($.status, $.store_id)")
        .unwrap();

    db.batch(&[
        BatchOperation::JsonPut {
            space: "orders".into(),
            id: "order_000".into(),
            value: json!({ "status": "refunded", "store_id": 9, "total": 0 }),
            if_version: None,
        },
        BatchOperation::JsonDelete {
            space: "orders".into(),
            id: "order_004".into(),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "orders".into(),
            id: "order_new".into(),
            value: json!({ "status": "paid", "store_id": 4 }),
            if_version: None,
        },
    ])
    .unwrap();

    let mut paid_store_4 = expected_ids(60, "paid", 4);
    paid_store_4.retain(|id| id != "order_004");
    paid_store_4.push("order_new".to_owned());
    paid_store_4.sort();
    assert_eq!(
        find_ids(&db, &json!({ "status": "paid", "store_id": 4 })),
        paid_store_4
    );

    let mut refunded_store_9 = expected_ids(60, "refunded", 9);
    refunded_store_9.push("order_000".to_owned());
    refunded_store_9.sort();
    assert_eq!(
        find_ids(&db, &json!({ "status": "refunded", "store_id": 9 })),
        refunded_store_9
    );
    // order_000 left the (paid, 0) group when it was rewritten.
    let mut paid_store_0 = expected_ids(60, "paid", 0);
    paid_store_0.retain(|id| id != "order_000");
    assert_eq!(
        find_ids(&db, &json!({ "status": "paid", "store_id": 0 })),
        paid_store_0
    );
    assert_eq!(
        find_ids(&db, &json!({ "status": "paid", "store_id": 99 })),
        [] as [std::string::String; 0]
    );
}

#[test]
fn disabled_engine_records_nothing_and_refuses_cycles() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), AutotuneOptions::default());
    seed_orders(&db, 100);
    for _ in 0..30 {
        find_ids(&db, &json!({ "status": "paid", "store_id": 4 }));
    }
    let report = db.autotune_report().unwrap();
    assert!(!report.enabled);
    assert!(report.json_queries.is_empty() && report.indexes.is_empty());
    assert_eq!(
        db.autotune_run_now().unwrap_err().code,
        ErrorCode::Unsupported
    );
    assert_eq!(
        count_rows(&db, "SELECT COUNT(*) FROM _lilia_autotune_query_stats"),
        0
    );
}

#[test]
fn indexes_are_only_recommended_once_the_thresholds_are_met() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), tuned());
    seed_orders(&db, 200);
    for _ in 0..4 {
        find_ids(&db, &json!({ "status": "paid" }));
    }
    assert_eq!(
        db.autotune_report().unwrap().recommendations,
        [] as [lilia_core::Recommendation; 0]
    );
    assert_eq!(
        db.autotune_run_now().unwrap().created,
        [] as [lilia_core::IndexRef; 0]
    );

    find_ids(&db, &json!({ "status": "paid" }));
    assert_eq!(db.autotune_run_now().unwrap().created.len(), 1);
    assert_eq!(
        db.list_json_indexes("orders").unwrap()[0].json_path,
        "$.status"
    );
}

#[test]
fn recommendations_are_only_reported_when_auto_create_is_off() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(
        dir.path(),
        AutotuneOptions {
            auto_create: false,
            ..tuned()
        },
    );
    seed_orders(&db, 200);
    for _ in 0..6 {
        find_ids(&db, &json!({ "status": "paid", "store_id": 4 }));
    }
    let cycle = db.autotune_run_now().unwrap();
    assert_eq!(cycle.created, [] as [lilia_core::IndexRef; 0]);
    assert_eq!(cycle.pending.len(), 1);
    assert_eq!(
        db.list_json_indexes("orders").unwrap(),
        [] as [lilia_core::JsonIndexInfo; 0]
    );
}

#[test]
fn backfill_in_small_chunks_indexes_every_document() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(
        dir.path(),
        AutotuneOptions {
            backfill_chunk_docs: 7,
            ..tuned()
        },
    );
    seed_orders(&db, 100);
    for _ in 0..5 {
        find_ids(&db, &json!({ "store_id": 3 }));
    }
    assert_eq!(db.autotune_run_now().unwrap().created.len(), 1);
    assert_eq!(
        count_rows(&db, "SELECT COUNT(*) FROM _lilia_json_index_entries WHERE index_name = 'idx_orders_store_id'"),
        100
    );
    let ids = find_ids(&db, &json!({ "store_id": 3 }));
    assert_eq!(ids.len(), 10);
    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM _lilia_autotune_indexes WHERE phase = 'ready'"
        ),
        1
    );
}

#[test]
fn zombie_indexes_are_proposed_then_dropped_without_touching_manual_ones() {
    const T0: i64 = 1_000_000;
    let dir = tempfile::tempdir().unwrap();
    let options = AutotuneOptions {
        prune_idle_ms: 10_000,
        recreate_cooldown_ms: 60_000,
        ..tuned()
    };
    let db = open(dir.path(), options);
    seed_orders(&db, 200);
    db.create_json_index("orders", "manual_total", "$.total")
        .unwrap();
    for _ in 0..5 {
        find_ids(&db, &json!({ "status": "paid" }));
    }

    // First cycle: builds the auto index and starts the idle window of the manual one.
    let first = db.autotune_run_at(T0).unwrap();
    assert_eq!(first.created.len(), 1);

    // Propose only (auto_prune is off by default).
    let proposal = db.autotune_run_at(T0 + 10_001).unwrap();
    assert_eq!(proposal.dropped, [] as [lilia_core::IndexRef; 0]);
    assert!(matches!(
        &proposal.pending[..],
        [Recommendation::DropIndex { index_name, .. }] if index_name == "idx_orders_status"
    ));
    let report = db.autotune_report_at(T0 + 10_001).unwrap();
    assert!(report.indexes.iter().all(|index| index.zombie));
    assert_eq!(db.list_json_indexes("orders").unwrap().len(), 2);
    drop(db);

    // Applying: only the auto-created index goes away.
    let db = open(
        dir.path(),
        AutotuneOptions {
            auto_prune: true,
            ..options
        },
    );
    let applied = db.autotune_run_at(T0 + 20_000).unwrap();
    assert_eq!(applied.dropped.len(), 1);
    assert_eq!(applied.dropped[0].index_name, "idx_orders_status");
    let names: Vec<String> = db
        .list_json_indexes("orders")
        .unwrap()
        .into_iter()
        .map(|index| index.index_name)
        .collect();
    assert_eq!(names, vec!["manual_total"]);
    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM _lilia_json_index_entries WHERE index_name = 'idx_orders_status'"
        ),
        0
    );

    // The dropped index is not rebuilt during the cool-down, even if the workload returns.
    for _ in 0..5 {
        find_ids(&db, &json!({ "status": "paid" }));
    }
    assert_eq!(
        db.autotune_run_at(T0 + 30_000).unwrap().created,
        [] as [lilia_core::IndexRef; 0]
    );
    assert_eq!(db.autotune_run_at(T0 + 100_000).unwrap().created.len(), 1);
}

#[test]
fn all_scope_also_prunes_manual_indexes() {
    const T0: i64 = 5_000_000;
    let dir = tempfile::tempdir().unwrap();
    let db = open(
        dir.path(),
        AutotuneOptions {
            auto_prune: true,
            prune_scope: PruneScope::All,
            prune_idle_ms: 1_000,
            ..tuned()
        },
    );
    seed_orders(&db, 20);
    db.create_json_index("orders", "manual_total", "$.total")
        .unwrap();
    assert_eq!(
        db.autotune_run_at(T0).unwrap().dropped,
        [] as [lilia_core::IndexRef; 0]
    );
    let cycle = db.autotune_run_at(T0 + 1_000).unwrap();
    assert_eq!(cycle.dropped.len(), 1);
    assert_eq!(
        db.list_json_indexes("orders").unwrap(),
        [] as [lilia_core::JsonIndexInfo; 0]
    );
}

#[test]
fn used_indexes_are_not_zombies() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(
        dir.path(),
        AutotuneOptions {
            auto_prune: true,
            prune_idle_ms: 60_000,
            ..tuned()
        },
    );
    seed_orders(&db, 40);
    db.create_json_index("orders", "idx_status", "$.status")
        .unwrap();
    let now = lilia_now_ms();
    db.autotune_run_at(now).unwrap();
    find_ids(&db, &json!({ "status": "paid" }));
    // Far later than the idle window relative to adoption, but the index was used "just now".
    let cycle = db.autotune_run_at(now + 30_000).unwrap();
    assert_eq!(cycle.dropped, [] as [lilia_core::IndexRef; 0]);
    assert_eq!(db.list_json_indexes("orders").unwrap().len(), 1);
}

#[test]
fn statistics_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = open(dir.path(), tuned());
        seed_orders(&db, 100);
        for _ in 0..3 {
            find_ids(&db, &json!({ "status": "paid", "store_id": 4 }));
        }
        db.close(Duration::from_secs(5)).unwrap();
    }
    let db = open(dir.path(), tuned());
    let report = db.autotune_report().unwrap();
    assert_eq!(report.json_queries.len(), 1);
    assert_eq!(report.json_queries[0].stats.scans, 3);

    // The restored evidence adds up with new executions.
    for _ in 0..2 {
        find_ids(&db, &json!({ "status": "paid", "store_id": 4 }));
    }
    assert_eq!(db.autotune_run_now().unwrap().created.len(), 1);
}

#[test]
fn interrupted_index_builds_are_cleaned_up_on_open() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = open(dir.path(), AutotuneOptions::default());
        seed_orders(&db, 10);
        db.sql_query(
            "INSERT INTO _lilia_json_indexes(space, index_name, json_path, filter_predicate) VALUES('orders', 'idx_orders_status', '$.status', NULL)",
            &[],
        )
        .unwrap();
        db.sql_query(
            "INSERT INTO _lilia_json_index_entries(space, index_name, idx_value, doc_id) VALUES('orders', 'idx_orders_status', 'paid', 'order_000')",
            &[],
        )
        .unwrap();
        db.sql_query(
            "INSERT INTO _lilia_autotune_indexes(space, index_name, origin, phase, first_seen_ms, last_used_ms, lookups, changed_ms) VALUES('orders', 'idx_orders_status', 'auto', 'building', 1, NULL, 0, 1)",
            &[],
        )
        .unwrap();
        db.close(Duration::from_secs(5)).unwrap();
    }
    let db = open(dir.path(), AutotuneOptions::default());
    assert_eq!(
        db.list_json_indexes("orders").unwrap(),
        [] as [lilia_core::JsonIndexInfo; 0]
    );
    assert_eq!(
        count_rows(&db, "SELECT COUNT(*) FROM _lilia_json_index_entries"),
        0
    );
    assert_eq!(
        count_rows(&db, "SELECT COUNT(*) FROM _lilia_autotune_indexes"),
        0
    );
    // The collection is intact and queryable.
    assert_eq!(find_ids(&db, &json!({ "status": "paid" })).len(), 3);
}

#[test]
fn memory_tier_collections_are_not_tuned() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(
        DatabaseOptions::durable(dir.path().join("db.lilia"))
            .with_memory_json("orders")
            .with_autotune(tuned()),
    )
    .unwrap();
    seed_orders(&db, 100);
    for _ in 0..10 {
        find_ids(&db, &json!({ "status": "paid", "store_id": 4 }));
    }
    assert_eq!(
        db.autotune_report().unwrap().json_queries,
        [] as [lilia_core::JsonQueryReport; 0]
    );
    assert_eq!(
        db.autotune_run_now().unwrap().created,
        [] as [lilia_core::IndexRef; 0]
    );
}

#[test]
fn sql_full_scans_and_durations_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), tuned());
    seed_orders(&db, 200);
    for threshold in 1..=3 {
        db.sql_query(
            &format!("SELECT COUNT(*) FROM _lilia_json WHERE version = {threshold}"),
            &[],
        )
        .unwrap();
    }
    db.sql_query(
        "SELECT COUNT(*) FROM _lilia_json WHERE space = 'orders' AND id = 'order_001'",
        &[],
    )
    .unwrap();

    let report = db.autotune_report().unwrap();
    let scan = report
        .sql_queries
        .iter()
        .find(|entry| entry.sql == "SELECT COUNT(*) FROM _lilia_json WHERE version = ?")
        .expect("literals are normalized into one statement shape");
    assert_eq!(scan.stats.executions, 3);
    assert_eq!(scan.stats.full_scan_executions, 3);
    assert!(scan.stats.full_scan_steps >= 3 * 199);

    let lookup = report
        .sql_queries
        .iter()
        .find(|entry| entry.sql.contains("id = ?"))
        .unwrap();
    assert_eq!(lookup.stats.full_scan_executions, 0);
}

fn lilia_now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

#[test]
fn background_worker_builds_the_index_without_being_asked() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(
        dir.path(),
        AutotuneOptions {
            interval_ms: 25,
            ..tuned()
        },
    );
    seed_orders(&db, 200);
    let query = json!({ "status": "paid", "store_id": 4 });
    for _ in 0..6 {
        find_ids(&db, &query);
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while db.list_json_indexes("orders").unwrap().is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the worker never built the index"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(find_ids(&db, &query), expected_ids(200, "paid", 4));
    db.close(Duration::from_secs(5)).unwrap();
}
