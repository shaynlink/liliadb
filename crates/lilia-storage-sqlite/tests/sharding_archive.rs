use lilia_storage_sqlite::{Database, DatabaseOptions, ShardStatus, ShardingConfig};
use serde_json::json;

fn open(directory: &tempfile::TempDir) -> Database {
    Database::open(
        DatabaseOptions::durable(directory.path().join("app.lilia"))
            .with_sharding(ShardingConfig::new()),
    )
    .expect("open sharded database")
}

/// Registers an empty historical shard (a zero-byte file is a valid SQLite database).
fn add_historical_shard(directory: &tempfile::TempDir, database: &Database, shard_id: &str) {
    let seed = directory.path().join(format!("seed-{shard_id}.lilia"));
    std::fs::write(&seed, []).expect("create empty shard file");
    database
        .attach_shard(shard_id, &seed)
        .expect("attach historical shard");
}

fn insert_event(database: &Database, alias: &str, id: &str, padding: usize) {
    database
        .sql_query(
            &format!(
                "INSERT INTO {alias}._lilia_json(space, id, value, version) VALUES('events', ?1, ?2, 1);"
            ),
            &[json!(id), json!(json!({"id": id, "pad": "x".repeat(padding)}).to_string())],
        )
        .expect("insert event into shard");
}

fn event_ids(database: &Database) -> Vec<String> {
    let result = database
        .sql_query(
            "SELECT id FROM lilia_json WHERE space = 'events' ORDER BY id;",
            &[],
        )
        .expect("query unified view");
    result
        .rows
        .iter()
        .map(|row| row[0].as_str().expect("id text").to_string())
        .collect()
}

#[test]
fn opening_creates_active_shard_and_layout() {
    let directory = tempfile::tempdir().unwrap();
    let database = open(&directory);

    let shards = database.list_shards().unwrap();
    assert_eq!(shards.len(), 1);
    let active = database.active_shard().unwrap();
    assert_eq!(active.status, ShardStatus::Active);
    assert_eq!(shards[0].shard_id, active.shard_id);
    assert!(directory.path().join("app.lilia.shards").is_dir());
    assert!(directory
        .path()
        .join("app.lilia.shards")
        .join(format!("{}.lilia", active.shard_id))
        .exists());
}

#[test]
fn sharding_calls_fail_when_disabled() {
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(DatabaseOptions::durable(
        directory.path().join("plain.lilia"),
    ))
    .unwrap();
    assert!(database.list_shards().is_err());
    assert!(database.rollover_shard().is_err());
}

#[test]
fn unified_view_spans_attached_shards() {
    let directory = tempfile::tempdir().unwrap();
    let database = open(&directory);
    let active = database.active_shard().unwrap();
    add_historical_shard(&directory, &database, "2020-01");
    add_historical_shard(&directory, &database, "2020-02");

    insert_event(&database, "shard_2020_01", "a", 0);
    insert_event(&database, "shard_2020_02", "b", 0);
    insert_event(
        &database,
        &format!("shard_{}", active.shard_id.replace('-', "_")),
        "c",
        0,
    );

    assert_eq!(event_ids(&database), vec!["a", "b", "c"]);
}

#[test]
fn archive_and_restore_cold_shard() {
    let directory = tempfile::tempdir().unwrap();
    let database = open(&directory);
    add_historical_shard(&directory, &database, "2020-01");
    add_historical_shard(&directory, &database, "2020-02");
    for i in 0..200 {
        insert_event(&database, "shard_2020_01", &format!("old-{i:03}"), 512);
    }
    insert_event(&database, "shard_2020_02", "kept", 0);

    let shards_dir = directory.path().join("app.lilia.shards");
    let archive = database
        .detach_and_compress("2020-01")
        .expect("archive shard");
    assert!(
        archive.compression_ratio < 0.3,
        "ratio {}",
        archive.compression_ratio
    );
    assert!(archive.original_size_bytes > archive.compressed_size_bytes);
    assert!(shards_dir.join("2020-01.lilia.zst").exists());
    assert!(!shards_dir.join("2020-01.lilia").exists());
    assert!(!shards_dir.join("2020-01.lilia-wal").exists());

    let cold = database
        .list_shards()
        .unwrap()
        .into_iter()
        .find(|s| s.shard_id == "2020-01")
        .unwrap();
    assert_eq!(cold.status, ShardStatus::Cold);
    assert!(cold.is_compressed);
    assert_eq!(cold.checksum.as_deref(), Some(archive.checksum.as_str()));

    // The detached shard disappears from the unified view; others keep working.
    assert_eq!(event_ids(&database), vec!["kept"]);
    insert_event(&database, "shard_2020_02", "after-archive", 0);
    assert_eq!(event_ids(&database), vec!["after-archive", "kept"]);

    let restored = database
        .decompress_and_attach("2020-01")
        .expect("restore shard");
    assert_eq!(restored.status, ShardStatus::Warm);
    assert!(shards_dir.join("2020-01.lilia").exists());
    let ids = event_ids(&database);
    assert_eq!(ids.len(), 202);
    assert!(ids.contains(&"old-199".to_string()));
}

#[test]
fn active_shard_cannot_be_archived_or_detached() {
    let directory = tempfile::tempdir().unwrap();
    let database = open(&directory);
    let active = database.active_shard().unwrap();
    assert!(database.detach_and_compress(&active.shard_id).is_err());
    assert!(database.detach_shard(&active.shard_id).is_err());
    assert!(database.detach_and_compress("1999-01").is_err());
}

#[test]
fn detach_and_reattach_warm_shard() {
    let directory = tempfile::tempdir().unwrap();
    let database = open(&directory);
    add_historical_shard(&directory, &database, "2020-03");
    insert_event(&database, "shard_2020_03", "x", 0);
    assert_eq!(event_ids(&database), vec!["x"]);

    database.detach_shard("2020-03").expect("detach");
    assert_eq!(event_ids(&database), [] as [std::string::String; 0]);
    let detached = database
        .list_shards()
        .unwrap()
        .into_iter()
        .find(|s| s.shard_id == "2020-03")
        .unwrap();
    assert_eq!(detached.status, ShardStatus::Detached);

    database
        .attach_shard("2020-03", &detached.file_path)
        .expect("reattach");
    assert_eq!(event_ids(&database), vec!["x"]);
}

#[test]
fn catalogue_survives_reopen() {
    let directory = tempfile::tempdir().unwrap();
    {
        let database = open(&directory);
        add_historical_shard(&directory, &database, "2020-01");
        add_historical_shard(&directory, &database, "2020-02");
        insert_event(&database, "shard_2020_02", "persisted", 0);
        database.detach_and_compress("2020-01").expect("archive");
    }
    let database = open(&directory);
    let shards = database.list_shards().unwrap();
    let status_of = |id: &str| shards.iter().find(|s| s.shard_id == id).map(|s| s.status);
    assert_eq!(status_of("2020-01"), Some(ShardStatus::Cold));
    assert_eq!(status_of("2020-02"), Some(ShardStatus::Warm));
    assert_eq!(event_ids(&database), vec!["persisted"]);
}

#[test]
fn rollover_is_idempotent_within_a_period() {
    let directory = tempfile::tempdir().unwrap();
    let database = open(&directory);
    let before = database.active_shard().unwrap();
    let after = database.rollover_shard().unwrap();
    assert_eq!(before.shard_id, after.shard_id);
    assert_eq!(database.list_shards().unwrap().len(), 1);
}

#[test]
fn auto_archive_oldest_enforces_attached_limit() {
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(
        DatabaseOptions::durable(directory.path().join("app.lilia")).with_sharding(
            ShardingConfig::new()
                .with_max_attached_shards(2)
                .with_auto_archive_oldest(true),
        ),
    )
    .unwrap();
    add_historical_shard(&directory, &database, "2020-01");
    add_historical_shard(&directory, &database, "2020-02");
    // Active + 2 warm = 3 attached; the next rollover check must archive the oldest warm shard.
    database.rollover_shard().unwrap();
    let shards = database.list_shards().unwrap();
    let status_of = |id: &str| shards.iter().find(|s| s.shard_id == id).map(|s| s.status);
    assert_eq!(status_of("2020-01"), Some(ShardStatus::Cold));
    assert_eq!(status_of("2020-02"), Some(ShardStatus::Warm));
}
