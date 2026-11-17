use lilia_core::{BatchOperation, TimeSeriesPoint};
use lilia_storage_sqlite::{Database, DatabaseOptions, ShardStatus, ShardingConfig};
use serde_json::json;

/// 2020-01-15T00:00:00Z in milliseconds.
const JAN_2020_MS: i64 = 1_579_046_400_000;

fn open(directory: &tempfile::TempDir) -> Database {
    Database::open(
        DatabaseOptions::durable(directory.path().join("app.lilia"))
            .with_sharding(ShardingConfig::new()),
    )
    .expect("open sharded database")
}

fn add_historical_shard(directory: &tempfile::TempDir, database: &Database, shard_id: &str) {
    let seed = directory.path().join(format!("seed-{shard_id}.lilia"));
    std::fs::write(&seed, []).expect("create empty shard file");
    database.attach_shard(shard_id, &seed).expect("attach historical shard");
}

fn active_alias(database: &Database) -> String {
    format!("shard_{}", database.active_shard().unwrap().shard_id.replace('-', "_"))
}

fn count(database: &Database, table: &str) -> i64 {
    database
        .sql_query(&format!("SELECT count(*) FROM {table};"), &[])
        .expect("count rows")
        .rows[0][0]
        .as_i64()
        .expect("integer count")
}

fn kv_set(namespace: &str, key: &str, value: &str) -> BatchOperation {
    BatchOperation::KvSet {
        namespace: namespace.into(),
        key: key.as_bytes().to_vec(),
        value: value.as_bytes().to_vec(),
        if_version: None,
        expires_at_ms: None,
    }
}

fn json_put(space: &str, id: &str, value: serde_json::Value) -> BatchOperation {
    BatchOperation::JsonPut { space: space.into(), id: id.into(), value, if_version: None }
}

#[test]
fn new_records_are_written_to_the_active_shard() {
    let directory = tempfile::tempdir().unwrap();
    let database = open(&directory);
    let active = active_alias(&database);

    database
        .batch(&[kv_set("cache", "k1", "v1"), json_put("people", "ada", json!({"name": "Ada"}))])
        .unwrap();

    assert_eq!(count(&database, &format!("{active}._lilia_kv")), 1);
    assert_eq!(count(&database, &format!("{active}._lilia_json")), 1);
    assert_eq!(count(&database, "main._lilia_kv"), 0);
    assert_eq!(count(&database, "main._lilia_json"), 0);
    assert_eq!(database.kv_get("cache", b"k1").unwrap().unwrap().value, b"v1");
    assert_eq!(database.json_get("people", "ada").unwrap().unwrap().value(), json!({"name": "Ada"}));
}

#[test]
fn updates_and_deletes_follow_the_record_into_its_shard() {
    let directory = tempfile::tempdir().unwrap();
    let database = open(&directory);
    let active = active_alias(&database);
    add_historical_shard(&directory, &database, "2020-01");
    database
        .sql_query(
            "INSERT INTO shard_2020_01._lilia_kv(namespace, key, value, version) VALUES('cache', x'6b31', x'6f6c64', 1);",
            &[],
        )
        .unwrap();
    database
        .sql_query(
            "INSERT INTO shard_2020_01._lilia_json(space, id, value, version) VALUES('people', 'ada', '{\"n\":1}', 1);",
            &[],
        )
        .unwrap();

    database
        .batch(&[kv_set("cache", "k1", "new"), json_put("people", "ada", json!({"n": 2}))])
        .unwrap();

    let kv = database.kv_get("cache", b"k1").unwrap().unwrap();
    assert_eq!((kv.value.as_slice(), kv.version), (b"new".as_slice(), 2));
    assert_eq!(database.json_get("people", "ada").unwrap().unwrap().value(), json!({"n": 2}));
    assert_eq!(count(&database, "shard_2020_01._lilia_kv"), 1);
    assert_eq!(count(&database, "shard_2020_01._lilia_json"), 1);
    assert_eq!(count(&database, &format!("{active}._lilia_kv")), 0);
    assert_eq!(count(&database, &format!("{active}._lilia_json")), 0);

    database
        .batch(&[
            BatchOperation::KvDelete { namespace: "cache".into(), key: b"k1".to_vec(), if_version: Some(2) },
            BatchOperation::JsonDelete { space: "people".into(), id: "ada".into(), if_version: None },
        ])
        .unwrap();
    assert!(database.kv_get("cache", b"k1").unwrap().is_none());
    assert!(database.json_get("people", "ada").unwrap().is_none());
    assert_eq!(count(&database, "shard_2020_01._lilia_kv"), 0);
    assert_eq!(count(&database, "shard_2020_01._lilia_json"), 0);
}

#[test]
fn scans_and_indexes_span_shards() {
    let directory = tempfile::tempdir().unwrap();
    let database = open(&directory);
    add_historical_shard(&directory, &database, "2020-01");
    database
        .sql_query(
            "INSERT INTO shard_2020_01._lilia_json(space, id, value, version) VALUES('people', 'old', '{\"city\":\"Paris\"}', 1);",
            &[],
        )
        .unwrap();
    database
        .sql_query(
            "INSERT INTO shard_2020_01._lilia_kv(namespace, key, value, version) VALUES('cache', x'61', x'31', 1);",
            &[],
        )
        .unwrap();
    database
        .batch(&[
            json_put("people", "new", json!({"city": "Paris"})),
            json_put("people", "other", json!({"city": "Lyon"})),
            kv_set("cache", "b", "2"),
        ])
        .unwrap();

    let ids: Vec<String> = database.json_scan("people", None, 10).unwrap().into_iter().map(|e| e.id).collect();
    assert_eq!(ids, vec!["new", "old", "other"]);
    let keys: Vec<Vec<u8>> = database.kv_scan("cache", None, 10).unwrap().into_iter().map(|e| e.key).collect();
    assert_eq!(keys, vec![b"a".to_vec(), b"b".to_vec()]);

    // Index creation backfills each shard; later writes keep entries in the doc's shard.
    database.create_json_index("people", "by_city", "$.city").unwrap();
    let mut paris: Vec<String> = database
        .json_scan_by_index("people", "by_city", "Paris", None, 10)
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect();
    paris.sort();
    assert_eq!(paris, vec!["new", "old"]);
    assert_eq!(count(&database, "shard_2020_01._lilia_json_index_entries"), 1);

    database.batch(&[json_put("people", "old", json!({"city": "Lyon"}))]).unwrap();
    let paris = database.json_scan_by_index("people", "by_city", "Paris", None, 10).unwrap();
    assert_eq!(paris.len(), 1);
    assert_eq!(database.json_scan_by_index("people", "by_city", "Lyon", None, 10).unwrap().len(), 2);
    assert_eq!(count(&database, "shard_2020_01._lilia_json_index_entries"), 1);

    assert!(database.drop_json_index("people", "by_city").unwrap());
    assert_eq!(count(&database, "shard_2020_01._lilia_json_index_entries"), 0);
}

#[test]
fn time_series_points_route_by_timestamp() {
    let directory = tempfile::tempdir().unwrap();
    let database = open(&directory);
    let active = active_alias(&database);
    add_historical_shard(&directory, &database, "2020-01");
    let now = i64::try_from(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(),
    )
    .unwrap();

    let old: Vec<TimeSeriesPoint> =
        (0..5).map(|i| TimeSeriesPoint { timestamp: JAN_2020_MS + i * 1_000, value: i as f64 }).collect();
    database.ts_append_points("temp", &old).unwrap();
    database
        .ts_append_points("temp", &[TimeSeriesPoint { timestamp: now, value: 99.0 }])
        .unwrap();

    assert_eq!(count(&database, "shard_2020_01._lilia_timeseries_chunks"), 1);
    assert_eq!(count(&database, &format!("{active}._lilia_timeseries_chunks")), 1);
    assert_eq!(count(&database, "main._lilia_timeseries_chunks"), 0);

    let january = database.ts_query_range("temp", JAN_2020_MS, JAN_2020_MS + 60_000).unwrap();
    assert_eq!(january.len(), 5);
    let everything = database.ts_query_range("temp", JAN_2020_MS, now + 1).unwrap();
    assert_eq!(everything.len(), 6);
    assert!(everything.windows(2).all(|w| w[0].timestamp < w[1].timestamp));
    let recent = database.ts_query_range("temp", now - 1, now + 1).unwrap();
    assert_eq!(recent.len(), 1);
}

#[test]
fn archived_shard_data_returns_after_restore() {
    let directory = tempfile::tempdir().unwrap();
    let database = open(&directory);
    add_historical_shard(&directory, &database, "2020-01");
    let old: Vec<TimeSeriesPoint> =
        (0..50).map(|i| TimeSeriesPoint { timestamp: JAN_2020_MS + i * 1_000, value: (i % 7) as f64 }).collect();
    database.ts_append_points("temp", &old).unwrap();
    database
        .sql_query(
            "INSERT INTO shard_2020_01._lilia_json(space, id, value, version) VALUES('people', 'old', '{\"n\":1}', 1);",
            &[],
        )
        .unwrap();
    database.batch(&[json_put("people", "live", json!({"n": 2}))]).unwrap();

    database.detach_and_compress("2020-01").unwrap();
    assert!(database.json_get("people", "old").unwrap().is_none());
    assert!(database.json_get("people", "live").unwrap().is_some());
    assert!(database.ts_query_range("temp", JAN_2020_MS, JAN_2020_MS + 60_000).unwrap().is_empty());
    // Writes keep flowing to the active shard while the old one is cold.
    database.batch(&[json_put("people", "during", json!({"n": 3}))]).unwrap();

    let restored = database.decompress_and_attach("2020-01").unwrap();
    assert_eq!(restored.status, ShardStatus::Warm);
    assert!(database.json_get("people", "old").unwrap().is_some());
    assert_eq!(database.ts_query_range("temp", JAN_2020_MS, JAN_2020_MS + 60_000).unwrap().len(), 50);
    assert_eq!(database.json_scan("people", None, 10).unwrap().len(), 3);
}

#[test]
fn data_written_before_sharding_stays_reachable() {
    let directory = tempfile::tempdir().unwrap();
    {
        let database =
            Database::open(DatabaseOptions::durable(directory.path().join("app.lilia"))).unwrap();
        database.batch(&[kv_set("cache", "legacy", "1")]).unwrap();
    }
    let database = open(&directory);
    assert_eq!(database.kv_get("cache", b"legacy").unwrap().unwrap().value, b"1");
    database.batch(&[kv_set("cache", "legacy", "2")]).unwrap();
    assert_eq!(database.kv_get("cache", b"legacy").unwrap().unwrap().version, 2);
    assert_eq!(count(&database, "main._lilia_kv"), 1);
}
