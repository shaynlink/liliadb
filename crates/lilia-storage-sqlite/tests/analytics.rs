#![cfg(all(feature = "analytics", not(target_arch = "wasm32")))]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lilia_core::fle::{FieldCipherAlgorithm, FlePolicy};
use lilia_core::{
    BatchOperation, CollectionKind, CollectionSpec, ErrorCode, Result, StorageTier, ValueCodec,
};
use lilia_storage_sqlite::{
    Analytics, AnalyticsOptions, ColumnType, ColumnarSourceKind, ColumnarTableSpec, Database,
    DatabaseOptions, ShardingConfig,
};
use serde_json::{json, Value};
use tempfile::TempDir;

const CITIES: [&str; 4] = ["lyon", "marseille", "paris", "toulouse"];

/// Options with background compaction disabled, so tests decide when the delta is folded.
fn manual_compaction() -> AnalyticsOptions {
    AnalyticsOptions {
        compaction_interval_ms: 0,
        ..AnalyticsOptions::default()
    }
}

fn open(directory: &TempDir, name: &str) -> Database {
    Database::open(
        DatabaseOptions::durable(directory.path().join(name))
            .with_analytics_options(manual_compaction()),
    )
    .unwrap()
}

/// In-memory model of the `users` collection: id -> (age, city).
type Users = BTreeMap<String, (i64, &'static str)>;

fn user_id(index: usize) -> String {
    format!("u{index:06}")
}

fn user(index: usize) -> (i64, &'static str) {
    (age_from(index * 7), CITIES[index % CITIES.len()])
}

/// An adult age derived from an arbitrary number.
fn age_from(number: usize) -> i64 {
    18 + i64::try_from(number % 60).unwrap()
}

/// The `profile.score` stored with every user: half their age.
fn score(age: i64) -> f64 {
    f64::from(i32::try_from(age).unwrap()) / 2.0
}

fn put_users(db: &Database, users: &Users) {
    let operations: Vec<BatchOperation> = users
        .iter()
        .map(|(id, (age, city))| BatchOperation::JsonPut {
            space: "users".into(),
            id: id.clone(),
            value: json!({ "age": age, "city": city, "profile": { "score": score(*age) } }),
            if_version: None,
        })
        .collect();
    for chunk in operations.chunks(500) {
        db.batch(chunk).unwrap();
    }
}

fn delete_users(db: &Database, ids: &[String]) {
    let operations: Vec<BatchOperation> = ids
        .iter()
        .map(|id| BatchOperation::JsonDelete {
            space: "users".into(),
            id: id.clone(),
            if_version: None,
        })
        .collect();
    db.batch(&operations).unwrap();
}

fn users_table(segment_rows: usize) -> ColumnarTableSpec {
    ColumnarTableSpec::json("users_col", "users")
        .column("age", "$.age", ColumnType::Int64)
        .column("city", "$.city", ColumnType::Utf8)
        .column("score", "$.profile.score", ColumnType::Float64)
        .segment_rows(segment_rows)
}

fn rows(analytics: &Analytics<'_>, sql: &str) -> Vec<Vec<Value>> {
    analytics.query_json(sql).unwrap().rows
}

/// Checks the columnar table against the model, row by row and through aggregates.
fn assert_matches(analytics: &Analytics<'_>, users: &Users) {
    let expected: Vec<Vec<Value>> = users
        .iter()
        .map(|(id, (age, city))| vec![json!(id), json!(age), json!(city), json!(score(*age))])
        .collect();
    assert_eq!(
        rows(
            analytics,
            "SELECT id, age, city, score FROM users_col ORDER BY id"
        ),
        expected
    );

    let mut by_city: BTreeMap<&str, (i64, i64)> = BTreeMap::new();
    for (age, city) in users.values().filter(|(age, _)| *age > 40) {
        let entry = by_city.entry(city).or_default();
        entry.0 += 1;
        entry.1 += age;
    }
    let expected: Vec<Vec<Value>> = by_city
        .into_iter()
        .map(|(city, (count, total))| vec![json!(city), json!(count), json!(total)])
        .collect();
    assert_eq!(
        rows(
            analytics,
            "SELECT city, count(*), sum(age) FROM users_col WHERE age > 40 \
             GROUP BY city ORDER BY city"
        ),
        expected
    );
}

#[test]
fn aggregates_a_json_collection_through_vectorized_segments() {
    let directory = tempfile::tempdir().unwrap();
    let db = open(&directory, "aggregates.lilia");
    let users: Users = (0..5000).map(|i| (user_id(i), user(i))).collect();
    put_users(&db, &users);

    let analytics = db.analytics().unwrap();
    analytics.create_table(users_table(1024)).unwrap();

    let stats = analytics.table_stats("users_col").unwrap();
    assert!(stats.ready);
    assert_eq!(stats.segment_rows, 5000);
    assert_eq!(stats.segments, 5);
    assert_eq!(stats.delta_keys, 0);
    assert!(stats.stored_bytes > 0);
    assert_matches(&analytics, &users);

    // Arrow output: the schema is typed and batches respect the configured size.
    let output = analytics
        .query("SELECT age, score FROM users_col WHERE city = 'paris'")
        .unwrap();
    assert_eq!(output.column_names(), ["age", "score"]);
    assert_eq!(output.row_count(), 1250);
    assert!(output.batches.iter().all(|batch| batch.num_rows() <= 1024));

    let tables = analytics.list_tables().unwrap();
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].name, "users_col");
    assert_eq!(tables[0].source_kind, ColumnarSourceKind::Json);
    assert_eq!(tables[0].source, "users");
    assert!(tables[0].ready);
    assert_eq!(tables[0].columns.len(), 3);
}

#[test]
fn values_of_the_wrong_type_become_null() {
    let directory = tempfile::tempdir().unwrap();
    let db = open(&directory, "types.lilia");
    let put = |id: &str, value: Value| BatchOperation::JsonPut {
        space: "mixed".into(),
        id: id.into(),
        value,
        if_version: None,
    };
    db.batch(&[
        put(
            "a",
            json!({ "n": 3, "f": 2, "b": true, "s": "x", "t": 1_700_000_000_000_i64 }),
        ),
        put(
            "b",
            json!({ "n": "three", "f": 2.5, "b": 1, "s": 7, "t": "yesterday" }),
        ),
        put("c", json!({ "n": 1.5 })),
    ])
    .unwrap();

    let analytics = db.analytics().unwrap();
    analytics
        .create_table(
            ColumnarTableSpec::json("mixed_col", "mixed")
                .column("n", "$.n", ColumnType::Int64)
                .column("f", "$.f", ColumnType::Float64)
                .column("b", "$.b", ColumnType::Boolean)
                .column("s", "$.s", ColumnType::Utf8)
                .column("t", "$.t", ColumnType::TimestampMillis),
        )
        .unwrap();

    assert_eq!(
        rows(
            &analytics,
            "SELECT id, n, f, b, s FROM mixed_col ORDER BY id"
        ),
        vec![
            vec![json!("a"), json!(3), json!(2.0), json!(true), json!("x")],
            vec![
                json!("b"),
                Value::Null,
                json!(2.5),
                Value::Null,
                Value::Null
            ],
            vec![
                json!("c"),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null
            ],
        ]
    );
    assert_eq!(
        rows(
            &analytics,
            "SELECT count(t), date_part('year', max(t)) FROM mixed_col"
        ),
        vec![vec![json!(1), json!(2023)]]
    );
}

#[test]
fn changed_rows_are_served_before_and_after_compaction() {
    let directory = tempfile::tempdir().unwrap();
    let db = open(&directory, "merge_on_read.lilia");
    // Even indexes only, so new keys can be inserted between existing ones.
    let mut users: Users = (0..3000).map(|i| (user_id(i * 2), user(i))).collect();
    put_users(&db, &users);

    let analytics = db.analytics().unwrap();
    analytics.create_table(users_table(500)).unwrap();
    assert_eq!(analytics.table_stats("users_col").unwrap().segments, 6);

    // A deterministic stream of updates, deletes and inserts spread over the key space,
    // including keys before the first and after the last segment.
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        usize::try_from(seed % bound as u64).unwrap()
    };
    // Ids are never reused once deleted.
    let mut retired = std::collections::BTreeSet::new();
    for round in 0..6 {
        let mut written = Users::new();
        let mut deleted = Vec::new();
        for _ in 0..150 {
            let index = next(6200);
            let id = if index == 0 {
                format!("a-before-{round}")
            } else if index == 1 {
                format!("z-after-{round}")
            } else {
                user_id(index)
            };
            if retired.contains(&id) {
                continue;
            }
            if next(4) == 0 {
                // Only rows that already reached the row store are deleted.
                if !written.contains_key(&id) && users.remove(&id).is_some() {
                    retired.insert(id.clone());
                    deleted.push(id);
                }
            } else {
                let value = (age_from(next(60)), CITIES[next(CITIES.len())]);
                users.insert(id.clone(), value);
                written.insert(id, value);
            }
        }
        put_users(&db, &written);
        delete_users(&db, &deleted);

        // Merge-on-read: correct while the changes still sit in the delta...
        let pending = analytics.table_stats("users_col").unwrap().delta_keys;
        assert!(pending > 0, "round {round} tracked no change");
        assert_matches(&analytics, &users);

        // ...and after they are folded into segments.
        let stats = analytics.compact("users_col").unwrap();
        assert_eq!(stats.delta_keys, pending);
        assert!(stats.segments_rewritten > 0);
        let after = analytics.table_stats("users_col").unwrap();
        assert_eq!(after.delta_keys, 0);
        assert_eq!(after.segment_rows, users.len());
        assert_matches(&analytics, &users);
    }

    // Segments stay close to their target size instead of fragmenting.
    let stats = analytics.table_stats("users_col").unwrap();
    assert!(
        stats.segments <= users.len() / 250 + 1,
        "{} segments for {} rows",
        stats.segments,
        users.len()
    );
    // Nothing left to do: compaction is a no-op.
    assert_eq!(
        analytics.compact("users_col").unwrap().segments_rewritten,
        0
    );
}

#[test]
fn deleting_everything_and_refilling_keeps_the_table_exact() {
    let directory = tempfile::tempdir().unwrap();
    let db = open(&directory, "empty.lilia");
    let analytics = db.analytics().unwrap();

    // Built over an empty collection: no segment at all.
    analytics.create_table(users_table(100)).unwrap();
    assert_eq!(analytics.table_stats("users_col").unwrap().segments, 0);
    assert_matches(&analytics, &Users::new());
    assert_eq!(
        rows(&analytics, "SELECT count(*) FROM users_col"),
        vec![vec![json!(0)]]
    );

    let users: Users = (0..350).map(|i| (user_id(i), user(i))).collect();
    put_users(&db, &users);
    assert_matches(&analytics, &users);
    let stats = analytics.compact("users_col").unwrap();
    assert_eq!(stats.rows_written, 350);
    // Three full segments, and a remainder of exactly half a segment kept on its own.
    assert_eq!(stats.segments_written, 4);
    assert_eq!(analytics.table_stats("users_col").unwrap().segments, 4);
    assert_matches(&analytics, &users);

    delete_users(&db, &users.keys().cloned().collect::<Vec<_>>());
    assert_matches(&analytics, &Users::new());
    analytics.compact("users_col").unwrap();
    let stats = analytics.table_stats("users_col").unwrap();
    assert_eq!(
        (stats.segments, stats.segment_rows, stats.delta_keys),
        (0, 0, 0)
    );
    assert_matches(&analytics, &Users::new());
}

/// Runs the same query on SQLite and on the columnar engine and compares the rows,
/// allowing the last bits of floating-point sums to differ.
fn assert_same_as_sqlite(db: &Database, analytics: &Analytics<'_>, query: &str) {
    let row_store = db.sql_query(&query.replace("{t}", "orders"), &[]).unwrap();
    let columnar = analytics
        .query_json(&query.replace("{t}", "orders_col"))
        .unwrap();
    assert_eq!(columnar.rows.len(), row_store.rows.len(), "{query}");
    for (actual, expected) in columnar.rows.iter().zip(&row_store.rows) {
        assert_eq!(actual.len(), expected.len(), "{query}");
        for (a, e) in actual.iter().zip(expected) {
            match (a.as_f64(), e.as_f64()) {
                (Some(a), Some(e)) => {
                    assert!(
                        (a - e).abs() <= 1e-6 * e.abs().max(1.0),
                        "{query}: {a} vs {e}"
                    );
                }
                _ => assert_eq!(a, e, "{query}"),
            }
        }
    }
}

#[test]
fn sql_tables_give_the_same_answers_as_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    let db = open(&directory, "orders.lilia");
    db.sql_query(
        "CREATE TABLE orders(id INTEGER PRIMARY KEY, customer TEXT, amount REAL, qty INTEGER, \
         shipped INTEGER, note TEXT)",
        &[],
    )
    .unwrap();
    db.sql_query(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 4000) \
         INSERT INTO orders SELECT i, 'c' || (i % 37), (i % 500) * 1.25, i % 9, i % 2, \
         CASE WHEN i % 5 = 0 THEN NULL ELSE 'n' || i END FROM n",
        &[],
    )
    .unwrap();

    let analytics = db.analytics().unwrap();
    analytics
        .create_table(
            ColumnarTableSpec::sql("orders_col", "orders")
                .column("customer", "customer", ColumnType::Utf8)
                .column("amount", "amount", ColumnType::Float64)
                .column("qty", "qty", ColumnType::Int64)
                .column("shipped", "shipped", ColumnType::Boolean)
                .column("note", "note", ColumnType::Utf8)
                .segment_rows(700),
        )
        .unwrap();
    analytics
        .create_table(
            ColumnarTableSpec::sql("orders_small", "orders")
                .column("qty", "qty", ColumnType::Int64)
                .segment_rows(4096),
        )
        .unwrap();

    let queries = [
        "SELECT count(*), sum(qty), min(amount), max(amount), avg(amount) FROM {t}",
        "SELECT customer, count(*), sum(qty), sum(amount), avg(amount) FROM {t} \
         WHERE qty >= 5 GROUP BY customer ORDER BY customer",
        "SELECT qty, count(note), count(*) FROM {t} WHERE amount < 200.5 AND customer <> 'c3' \
         GROUP BY qty ORDER BY qty",
        "SELECT customer, max(amount) FROM {t} GROUP BY customer \
         HAVING max(amount) > 600 ORDER BY customer LIMIT 5",
    ];
    let check_all = || {
        for query in queries {
            assert_same_as_sqlite(&db, &analytics, query);
        }
        // The key column is the rowid, and joins between columnar tables work.
        assert_eq!(
            rows(
                &analytics,
                "SELECT min(rowid), max(rowid), count(*) FROM orders_col"
            )[0][2],
            db.sql_query("SELECT count(*) FROM orders", &[])
                .unwrap()
                .rows[0][0]
        );
        assert_eq!(
            rows(
                &analytics,
                "SELECT count(*) FROM orders_col a JOIN orders_small b ON a.rowid = b.rowid \
                 WHERE a.qty <> b.qty"
            ),
            vec![vec![json!(0)]]
        );
    };
    check_all();

    // Raw SQL writes are tracked like any other write path.
    db.sql_query(
        "UPDATE orders SET qty = qty + 100, amount = amount * 2 WHERE id % 11 = 0",
        &[],
    )
    .unwrap();
    db.sql_query("DELETE FROM orders WHERE id % 13 = 0", &[])
        .unwrap();
    db.sql_query(
        "INSERT INTO orders(id, customer, amount, qty, shipped) VALUES \
         (-5, 'early', 1.0, 1, 1), (100000, 'late', 2.0, 2, 0)",
        &[],
    )
    .unwrap();
    // A rowid change moves the row: both the old and the new key are tracked.
    db.sql_query("UPDATE orders SET id = 50001 WHERE id = 7", &[])
        .unwrap();
    assert!(analytics.table_stats("orders_col").unwrap().delta_keys > 600);
    check_all();

    analytics.compact("orders_col").unwrap();
    analytics.compact("orders_small").unwrap();
    assert_eq!(analytics.table_stats("orders_col").unwrap().delta_keys, 0);
    check_all();

    // A user statement with its own conflict policy must not trip over tracking rows
    // that are already present.
    db.sql_query("UPDATE OR FAIL orders SET qty = 1 WHERE id = 8", &[])
        .unwrap();
    db.sql_query("UPDATE OR FAIL orders SET qty = 2 WHERE id = 8", &[])
        .unwrap();
    db.sql_query(
        "INSERT OR REPLACE INTO orders(id, customer, qty) VALUES (8, 'r', 3)",
        &[],
    )
    .unwrap();
    assert_eq!(analytics.table_stats("orders_col").unwrap().delta_keys, 1);
    check_all();
}

#[test]
fn writes_racing_with_the_initial_build_are_not_lost() {
    let directory = tempfile::tempdir().unwrap();
    let db = Arc::new(open(&directory, "racing_build.lilia"));
    let mut users: Users = (0..20_000).map(|i| (user_id(i), user(i))).collect();
    put_users(&db, &users);

    let stop = Arc::new(AtomicBool::new(false));
    let background = {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut written = Users::new();
            let mut deleted = Vec::new();
            let mut turn = 0_usize;
            // Keep going until the build is over, with a floor so the race is real.
            while !stop.load(Ordering::Relaxed) || turn < 40 {
                // 7919 is coprime with 21 000: every id is visited at most once per cycle,
                // and an id is never written again after it was deleted.
                let index = (turn * 7919) % 21_000;
                let id = user_id(index);
                if deleted.contains(&id) {
                    turn += 1;
                    continue;
                }
                if turn.is_multiple_of(5) && index < 20_000 && !written.contains_key(&id) {
                    delete_users(&db, std::slice::from_ref(&id));
                    deleted.push(id);
                } else {
                    let value = (99, "paris");
                    put_users(&db, &Users::from([(id.clone(), value)]));
                    written.insert(id, value);
                }
                turn += 1;
            }
            (written, deleted)
        })
    };

    let analytics = db.analytics().unwrap();
    analytics.create_table(users_table(512)).unwrap();
    stop.store(true, Ordering::Relaxed);
    let (written, deleted) = background.join().unwrap();

    for id in &deleted {
        users.remove(id);
    }
    users.extend(written);
    assert_matches(&analytics, &users);
    analytics.compact("users_col").unwrap();
    assert_matches(&analytics, &users);
}

#[test]
fn tables_persist_and_interrupted_builds_resume_on_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let mut users: Users = (0..1200).map(|i| (user_id(i), user(i))).collect();
    {
        let db = open(&directory, "reopen.lilia");
        put_users(&db, &users);
        db.analytics()
            .unwrap()
            .create_table(users_table(300))
            .unwrap();
    }
    {
        // Writes made before the engine is started in this process are still tracked.
        let db = open(&directory, "reopen.lilia");
        let late: Users = Users::from([(user_id(5), (77, "lyon")), (user_id(5000), (33, "paris"))]);
        put_users(&db, &late);
        users.extend(late);
        let analytics = db.analytics().unwrap();
        assert_matches(&analytics, &users);
        assert_eq!(analytics.table_stats("users_col").unwrap().delta_keys, 2);

        // Simulate a crash in the middle of a build: state `building`, partial segments.
        db.sql_query("UPDATE _lilia_col_tables SET state = 'building'", &[])
            .unwrap();
        db.sql_query("DELETE FROM _lilia_col_chunks WHERE segment_id > 0; ", &[])
            .unwrap();
        db.sql_query("DELETE FROM _lilia_col_segments WHERE segment_id > 0", &[])
            .unwrap();
    }
    let db = open(&directory, "reopen.lilia");
    let analytics = db.analytics().unwrap();
    let stats = analytics.table_stats("users_col").unwrap();
    assert!(stats.ready);
    assert_eq!(stats.segment_rows, users.len());
    assert_matches(&analytics, &users);

    assert!(analytics.drop_table("users_col").unwrap());
    assert!(!analytics.drop_table("users_col").unwrap());
    assert_eq!(analytics.list_tables().unwrap(), []);
    assert_eq!(
        analytics.query("SELECT 1 FROM users_col").unwrap_err().code,
        ErrorCode::InvalidInput
    );
    // Dropping removed the change tracking: writes no longer touch the delta.
    put_users(&db, &Users::from([(user_id(1), (20, "lyon"))]));
    let leftovers = db
        .sql_query(
            "SELECT (SELECT count(*) FROM _lilia_col_delta) + (SELECT count(*) FROM \
             _lilia_col_segments) + (SELECT count(*) FROM _lilia_col_chunks) + (SELECT count(*) \
             FROM sqlite_master WHERE name LIKE '\\_lilia\\_col\\_trg\\_%' ESCAPE '\\')",
            &[],
        )
        .unwrap();
    assert_eq!(leftovers.rows[0][0], json!(0));

    // The name can be reused, and the new table starts from the current rows.
    users.insert(user_id(1), (20, "lyon"));
    analytics.create_table(users_table(300)).unwrap();
    assert_matches(&analytics, &users);
}

#[derive(Debug)]
struct ShiftCodec;

impl ValueCodec for ShiftCodec {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "shift"
    }

    fn encode<'a>(&self, plaintext: &'a [u8]) -> Result<std::borrow::Cow<'a, [u8]>> {
        Ok(std::borrow::Cow::Owned(
            plaintext.iter().map(|b| b.wrapping_add(13)).collect(),
        ))
    }

    fn decode(&self, encoded: &[u8]) -> Result<Vec<u8>> {
        Ok(encoded.iter().map(|b| b.wrapping_sub(13)).collect())
    }
}

/// Number of stored chunk or metadata blobs that contain `needle` in clear.
fn blobs_containing(db: &Database, needle: &str) -> i64 {
    let result = db
        .sql_query(
            "SELECT (SELECT count(*) FROM _lilia_col_chunks WHERE instr(data, CAST(?1 AS BLOB))) \
             + (SELECT count(*) FROM _lilia_col_segments WHERE instr(meta, CAST(?1 AS BLOB)))",
            &[json!(needle)],
        )
        .unwrap();
    result.rows[0][0].as_i64().unwrap()
}

#[test]
fn segments_go_through_the_value_codec() {
    let directory = tempfile::tempdir().unwrap();
    let secret = Users::from([("u1".to_string(), (41, "montpellier-sur-secret"))]);

    let plain = open(&directory, "plain.lilia");
    put_users(&plain, &secret);
    plain
        .analytics()
        .unwrap()
        .create_table(users_table(100))
        .unwrap();
    // Sanity: without a codec the value is visible, in the chunk and in its zone map.
    assert_eq!(blobs_containing(&plain, "montpellier-sur-secret"), 2);

    let encoded = Database::open(
        DatabaseOptions::durable(directory.path().join("encoded.lilia"))
            .with_codec(Arc::new(ShiftCodec))
            .with_analytics_options(manual_compaction()),
    )
    .unwrap();
    put_users(&encoded, &secret);
    let analytics = encoded.analytics().unwrap();
    analytics.create_table(users_table(100)).unwrap();
    assert_eq!(blobs_containing(&encoded, "montpellier-sur-secret"), 0);
    assert_matches(&analytics, &secret);

    // Compaction rewrites segments through the codec too.
    let more = Users::from([("u2".to_string(), (52, "montpellier-sur-secret"))]);
    put_users(&encoded, &more);
    analytics.compact("users_col").unwrap();
    assert_eq!(blobs_containing(&encoded, "montpellier-sur-secret"), 0);
    assert_eq!(
        rows(
            &analytics,
            "SELECT count(*) FROM users_col WHERE city LIKE 'montpellier%'"
        ),
        vec![vec![json!(2)]]
    );
}

#[test]
fn field_level_encrypted_values_stay_encrypted_in_segments() {
    let directory = tempfile::tempdir().unwrap();
    let db = open(&directory, "fle.lilia");
    db.create_compartment("vault:health", "passphrase").unwrap();
    db.collection("patients")
        .compartment("vault:health")
        .fle_policy(FlePolicy::new().field(
            "diagnosis",
            FieldCipherAlgorithm::RandomizedGcm,
            "vault:health",
        ))
        .json()
        .unwrap();
    db.batch(&[BatchOperation::JsonPut {
        space: "patients".into(),
        id: "p1".into(),
        value: json!({ "department": "cardiology", "diagnosis": "hypertension-stage-2" }),
        if_version: None,
    }])
    .unwrap();

    let analytics = db.analytics().unwrap();
    analytics
        .create_table(
            ColumnarTableSpec::json("patients_col", "patients")
                .column("department", "$.department", ColumnType::Utf8)
                .column("diagnosis", "$.diagnosis", ColumnType::Utf8),
        )
        .unwrap();

    // The compartment is unlocked, yet the projection only ever sees the ciphertext.
    let result = rows(&analytics, "SELECT department, diagnosis FROM patients_col");
    assert_eq!(result[0][0], json!("cardiology"));
    assert_ne!(result[0][1], json!("hypertension-stage-2"));
    assert_eq!(blobs_containing(&db, "hypertension-stage-2"), 0);
    assert_eq!(blobs_containing(&db, "cardiology"), 2);
}

#[test]
fn unsupported_sources_and_invalid_declarations_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    let db = open(&directory, "refusals.lilia");
    db.set_collection_spec(CollectionSpec::new(
        CollectionKind::Json,
        "hot",
        StorageTier::Memory,
    ))
    .unwrap();
    db.sql_query("CREATE TABLE plain(a INTEGER, b TEXT)", &[])
        .unwrap();
    db.sql_query(
        "CREATE TABLE keyed(k TEXT PRIMARY KEY, v INTEGER) WITHOUT ROWID",
        &[],
    )
    .unwrap();
    db.sql_query("CREATE VIEW plain_view AS SELECT a FROM plain", &[])
        .unwrap();

    let analytics = db.analytics().unwrap();
    let refused = |spec: ColumnarTableSpec| analytics.create_table(spec).unwrap_err().code;
    let int = ColumnType::Int64;

    assert_eq!(
        refused(ColumnarTableSpec::json("t", "hot").column("a", "$.a", int)),
        ErrorCode::InvalidInput
    );
    assert_eq!(
        refused(ColumnarTableSpec::sql("t", "missing").column("a", "a", int)),
        ErrorCode::InvalidInput
    );
    assert_eq!(
        refused(ColumnarTableSpec::sql("t", "keyed").column("v", "v", int)),
        ErrorCode::InvalidInput
    );
    assert_eq!(
        refused(ColumnarTableSpec::sql("t", "plain_view").column("a", "a", int)),
        ErrorCode::InvalidInput
    );
    assert_eq!(
        refused(ColumnarTableSpec::sql("t", "_lilia_json").column("v", "version", int)),
        ErrorCode::InvalidInput
    );
    assert!(analytics
        .create_table(ColumnarTableSpec::sql("t", "plain").column("c", "no_such_column", int))
        .is_err());
    assert_eq!(
        refused(ColumnarTableSpec::sql("Bad Name", "plain").column("a", "a", int)),
        ErrorCode::InvalidInput
    );
    assert_eq!(
        refused(ColumnarTableSpec::sql("t", "plain")),
        ErrorCode::InvalidInput
    );
    assert_eq!(
        refused(ColumnarTableSpec::sql("t", "plain").column("rowid", "a", int)),
        ErrorCode::InvalidInput
    );
    assert_eq!(
        refused(
            ColumnarTableSpec::sql("t", "plain")
                .column("a", "a", int)
                .segment_rows(0)
        ),
        ErrorCode::InvalidInput
    );
    // Nothing was left behind by the refused declarations.
    assert_eq!(analytics.list_tables().unwrap(), []);

    analytics
        .create_table(ColumnarTableSpec::sql("t", "plain").column("a", "a", int))
        .unwrap();
    assert_eq!(
        refused(ColumnarTableSpec::sql("t", "plain").column("a", "a", int)),
        ErrorCode::Conflict
    );
}

#[test]
fn the_engine_is_read_only_and_reports_unknown_objects() {
    let directory = tempfile::tempdir().unwrap();
    let db = open(&directory, "read_only.lilia");
    db.sql_query("CREATE TABLE plain(a INTEGER, b TEXT)", &[])
        .unwrap();
    let analytics = db.analytics().unwrap();
    analytics
        .create_table(ColumnarTableSpec::sql("t", "plain").column("a", "a", ColumnType::Int64))
        .unwrap();

    for sql in [
        "INSERT INTO t VALUES (1, 2)",
        "DROP TABLE t",
        "CREATE TABLE u AS SELECT * FROM t",
        "SELECT * FROM plain",
        "SELECT nope FROM t",
    ] {
        assert_eq!(
            analytics.query(sql).unwrap_err().code,
            ErrorCode::InvalidInput,
            "{sql}"
        );
    }
    assert_eq!(
        analytics.table_stats("nope").unwrap_err().code,
        ErrorCode::NotFound
    );
    assert_eq!(
        analytics.compact("nope").unwrap_err().code,
        ErrorCode::NotFound
    );
}

#[test]
fn sharded_json_collections_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    let int = ColumnType::Int64;
    let sharded = Database::open(
        DatabaseOptions::durable(directory.path().join("sharded.lilia"))
            .with_sharding(ShardingConfig::new()),
    )
    .unwrap();
    let error = sharded
        .analytics()
        .unwrap()
        .create_table(ColumnarTableSpec::json("t", "events").column("a", "$.a", int))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidInput);
}

#[test]
fn buffered_writes_are_visible_to_queries() {
    let directory = tempfile::tempdir().unwrap();
    let db = open(&directory, "buffered.lilia");
    db.set_collection_spec(CollectionSpec::new(
        CollectionKind::Json,
        "users",
        StorageTier::Buffered,
    ))
    .unwrap();
    let analytics = db.analytics().unwrap();
    analytics.create_table(users_table(100)).unwrap();

    // Acknowledged writes may still sit in the ring buffer: a scan flushes them first.
    let users: Users = (0..250).map(|i| (user_id(i), user(i))).collect();
    put_users(&db, &users);
    assert_matches(&analytics, &users);
    analytics.compact("users_col").unwrap();
    assert_matches(&analytics, &users);
}

#[test]
fn undo_keeps_the_table_in_step_with_the_row_store() {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::open(
        DatabaseOptions::durable(directory.path().join("undo.lilia"))
            .with_undo_log_capacity(10)
            .with_analytics_options(manual_compaction()),
    )
    .unwrap();
    let before = Users::from([(user_id(1), (30, "lyon")), (user_id(2), (40, "paris"))]);
    put_users(&db, &before);
    let analytics = db.analytics().unwrap();
    analytics.create_table(users_table(100)).unwrap();

    // The update is compacted into segments, then undone in the row store.
    let after = Users::from([(user_id(1), (31, "toulouse")), (user_id(2), (40, "paris"))]);
    put_users(&db, &Users::from([(user_id(1), (31, "toulouse"))]));
    analytics.compact("users_col").unwrap();
    assert_matches(&analytics, &after);

    assert!(db.undo().unwrap().is_some());
    assert_matches(&analytics, &before);
    analytics.compact("users_col").unwrap();
    assert_matches(&analytics, &before);

    assert!(db.redo().unwrap().is_some());
    assert_matches(&analytics, &after);
}

#[test]
fn background_worker_compacts_large_deltas() {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::open(
        DatabaseOptions::durable(directory.path().join("worker.lilia")).with_analytics_options(
            AnalyticsOptions {
                compaction_delta_rows: 50,
                compaction_interval_ms: 20,
                ..AnalyticsOptions::default()
            },
        ),
    )
    .unwrap();
    let analytics = db.analytics().unwrap();
    analytics.create_table(users_table(100)).unwrap();

    // Below the threshold the delta is left alone and simply overlaid on read.
    let mut users: Users = (0..10).map(|i| (user_id(i), user(i))).collect();
    put_users(&db, &users);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(analytics.table_stats("users_col").unwrap().delta_keys, 10);

    users.extend((10..400).map(|i| (user_id(i), user(i))));
    put_users(&db, &users);
    let deadline = Instant::now() + Duration::from_secs(10);
    while analytics.table_stats("users_col").unwrap().delta_keys != 0 {
        assert!(
            Instant::now() < deadline,
            "the worker never compacted the table"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        analytics.table_stats("users_col").unwrap().segment_rows,
        400
    );
    assert_matches(&analytics, &users);
}

#[test]
fn works_from_async_code_and_after_close() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .unwrap();
    runtime.block_on(async {
        let db = open(&directory, "async.lilia");
        let users: Users = (0..300).map(|i| (user_id(i), user(i))).collect();
        put_users(&db, &users);
        let analytics = db.analytics().unwrap();
        analytics.create_table(users_table(100)).unwrap();
        assert_matches(&analytics, &users);

        // A closed database reports it instead of hanging or panicking.
        db.close(Duration::from_secs(5)).unwrap();
        assert_eq!(
            analytics
                .query("SELECT count(*) FROM users_col")
                .unwrap_err()
                .code,
            ErrorCode::Closed
        );
        // Dropping the engine and the database inside the runtime must not panic.
        drop(analytics);
        drop(db);
    });
}
