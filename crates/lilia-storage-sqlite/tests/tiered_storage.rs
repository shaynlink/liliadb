#![allow(clippy::too_many_lines, clippy::cast_precision_loss)]

use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use lilia_core::{
    BatchOperation, CollectionKind, CollectionSpec, Direction, ErrorCode, HnswConfig,
    OverflowPolicy, RingBufferConfig, StorageTier, TimeSeriesPoint, VectorElementType,
    VectorMetric,
};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::{json, Map, Value};

fn put_kv(db: &Database, namespace: &str, key: &[u8], value: &[u8]) -> lilia_core::Result<()> {
    db.batch(&[BatchOperation::KvSet {
        namespace: namespace.into(),
        key: key.to_vec(),
        value: value.to_vec(),
        if_version: None,
        expires_at_ms: None,
    }])?;
    Ok(())
}

fn del_kv(db: &Database, namespace: &str, key: &[u8]) -> lilia_core::Result<()> {
    db.batch(&[BatchOperation::KvDelete {
        namespace: namespace.into(),
        key: key.to_vec(),
        if_version: None,
    }])?;
    Ok(())
}

fn put_json(db: &Database, space: &str, id: &str, value: Value) -> lilia_core::Result<()> {
    db.batch(&[BatchOperation::JsonPut {
        space: space.into(),
        id: id.into(),
        value,
        if_version: None,
    }])?;
    Ok(())
}

fn f32_to_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for &x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

fn scalar_i64(db: &Database, sql: &str) -> i64 {
    let res = db.sql_query(sql, &[]).expect("sql_query should succeed");
    res.rows[0][0].as_i64().expect("expected i64 scalar")
}

#[test]
fn test_memory_tier_zero_disk_and_all_models() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("mem_zero_disk.lilia");

    {
        let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

        let kv = db.collection("ephem_kv").memory().kv().unwrap();
        let json_col = db.collection("ephem_json").memory().json().unwrap();
        let ts_col = db.collection("ephem_ts").memory().timeseries().unwrap();
        let graph_col = db.collection("ephem_graph").memory().graph().unwrap();
        let vec_col = db.collection("ephem_vec").memory().vector().unwrap();
        let fts_col = db.collection("ephem_fts").memory().fts().unwrap();
        let crdt_col = db.collection("ephem_crdt").memory().crdt().unwrap();

        assert_eq!(kv.tier(), StorageTier::Memory);
        assert_eq!(json_col.tier(), StorageTier::Memory);

        // KV
        let res = kv.set(b"session_1", b"alice").unwrap();
        assert_eq!(res.tier, StorageTier::Memory);
        assert!(!res.buffered);
        assert_eq!(kv.get(b"session_1").unwrap().unwrap().value, b"alice");
        assert_eq!(kv.scan(None, 10).unwrap().len(), 1);

        // JSON + in-memory index
        json_col
            .put(
                "doc1",
                json!({ "user": "alice", "role": "admin", "score": 99 }),
            )
            .unwrap();
        json_col
            .put(
                "doc2",
                json!({ "user": "bob", "role": "viewer", "score": 40 }),
            )
            .unwrap();
        json_col.create_index("by_role", "$.role").unwrap();
        assert_eq!(
            db.json_extract_str("ephem_json", "doc1", "$.user")
                .unwrap()
                .as_deref(),
            Some("alice")
        );
        assert_eq!(
            db.json_scan_by_index("ephem_json", "by_role", "admin", None, 10)
                .unwrap()
                .len(),
            1
        );
        let mut q = Map::new();
        q.insert("role".into(), json!("viewer"));
        assert_eq!(json_col.find(&q, None, 10).unwrap().len(), 1);

        // TimeSeries
        ts_col
            .append_points(&[
                TimeSeriesPoint::new(1_000, 10.0),
                TimeSeriesPoint::new(2_000, 20.0),
                TimeSeriesPoint::new(3_000, 30.0),
            ])
            .unwrap();
        let pts = ts_col.query_range(0, 5_000).unwrap();
        assert_eq!(pts.len(), 3);
        let stats = db
            .ts_query_stats("ephem_ts", 0, 5_000)
            .unwrap()
            .expect("stats");
        assert_eq!(stats.count, 3);
        assert!((stats.avg - 20.0).abs() < 1e-9);
        assert_eq!(db.ts_query_auto("ephem_ts", 0, 5_000, 10).unwrap().len(), 3);

        // Graph
        graph_col
            .set_node("a", "service", &json!({"region": "eu"}))
            .unwrap();
        graph_col
            .set_node("b", "service", &json!({"region": "us"}))
            .unwrap();
        graph_col.add_edge("a", "b", "calls").unwrap();
        assert_eq!(
            graph_col
                .reachability(&["a".to_string()], Direction::Outbound)
                .unwrap(),
            vec!["a", "b"]
        );
        assert_eq!(
            graph_col
                .neighbors("a", Direction::Outbound, Some("calls"))
                .unwrap(),
            vec!["b"]
        );

        // Vector + HNSW
        vec_col
            .create_index(HnswConfig {
                dim: 2,
                element_type: VectorElementType::Float32,
                metric: VectorMetric::L2,
                ..HnswConfig::default()
            })
            .unwrap();
        vec_col
            .set(
                "v1",
                2,
                VectorElementType::Float32,
                &f32_to_bytes(&[1.0, 0.0]),
            )
            .unwrap();
        vec_col
            .set(
                "v2",
                2,
                VectorElementType::Float32,
                &f32_to_bytes(&[10.0, 10.0]),
            )
            .unwrap();
        let hits = vec_col
            .search(
                &f32_to_bytes(&[1.1, 0.0]),
                2,
                VectorElementType::Float32,
                VectorMetric::L2,
                1,
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "v1");

        // FTS5 in _lilia_mem
        fts_col
            .index("f1", "ephemeral in memory full text search engine")
            .unwrap();
        let fhits = fts_col.search("ephemeral", 10).unwrap();
        assert_eq!(fhits.len(), 1);
        assert_eq!(fhits[0].doc_id, "f1");

        // CRDT
        let counter = crdt_col.counter("online");
        counter.increment("node1", 5).unwrap();
        assert_eq!(counter.get().unwrap(), 5);
        let orset = crdt_col.set("users");
        orset.add("node1", "alice").unwrap();
        assert!(orset.contains("alice").unwrap());
        let doc = crdt_col.doc("state");
        doc.set_field("node1", "status", json!("ready"), Some(100))
            .unwrap();
        assert_eq!(doc.get().unwrap().unwrap()["status"], "ready");

        // Ephemeral SQL table
        db.sql_query(
            "CREATE EPHEMERAL TABLE live_sessions (id TEXT PRIMARY KEY, user_id TEXT NOT NULL);",
            &[],
        )
        .unwrap();
        db.sql_query(
            "INSERT INTO live_sessions(id, user_id) VALUES('s1', 'alice');",
            &[],
        )
        .unwrap();
        assert_eq!(scalar_i64(&db, "SELECT COUNT(*) FROM live_sessions;"), 1);

        // Unified SQL views expose tier='memory'
        assert_eq!(
            scalar_i64(&db, "SELECT COUNT(*) FROM lilia_kv WHERE tier = 'memory';"),
            1
        );
        assert_eq!(
            scalar_i64(
                &db,
                "SELECT COUNT(*) FROM lilia_json WHERE tier = 'memory';"
            ),
            2
        );
        assert_eq!(
            scalar_i64(
                &db,
                "SELECT COUNT(*) FROM lilia_timeseries_chunks WHERE tier = 'memory';"
            ),
            1
        );
        assert_eq!(
            scalar_i64(
                &db,
                "SELECT COUNT(*) FROM lilia_graph_nodes WHERE tier = 'memory';"
            ),
            2
        );
        assert_eq!(
            scalar_i64(
                &db,
                "SELECT COUNT(*) FROM lilia_vectors WHERE tier = 'memory';"
            ),
            2
        );
        assert_eq!(
            scalar_i64(&db, "SELECT COUNT(*) FROM lilia_fts WHERE tier = 'memory';"),
            1
        );
        assert_eq!(
            scalar_i64(
                &db,
                "SELECT COUNT(*) FROM lilia_crdt_counters WHERE tier = 'memory';"
            ),
            1
        );

        // STRICT ZERO-DISK PROOF: main SQLite tables & catalog must contain 0 rows!
        for raw_table in [
            "_lilia_kv",
            "_lilia_json",
            "_lilia_timeseries_chunks",
            "_lilia_graph_nodes",
            "_lilia_graph_edges",
            "_lilia_vectors",
            "_lilia_hnsw_meta",
            "_lilia_fts",
            "_lilia_crdt_counters",
            "_lilia_crdt_orsets",
            "_lilia_crdt_json",
            "_lilia_catalog",
        ] {
            let count = scalar_i64(&db, &format!("SELECT COUNT(*) FROM main.{raw_table};"));
            assert_eq!(count, 0, "expected 0 disk rows in main.{raw_table}");
        }

        db.close(Duration::from_secs(5)).unwrap();
    }
    eprintln!("STAGE 1 OK");

    // Reopen DB: 100% of StorageTier::Memory data & ephemeral tables vanished
    let reopened = Database::open(DatabaseOptions::durable(&db_path)).unwrap();
    assert!(reopened.kv_get("ephem_kv", b"session_1").unwrap().is_none());
    assert!(reopened.json_get("ephem_json", "doc1").unwrap().is_none());
    assert_eq!(
        reopened.ts_query_range("ephem_ts", 0, 5_000).unwrap(),
        [] as [lilia_core::TimeSeriesPoint; 0]
    );
    assert!(reopened
        .graph_get_node("ephem_graph", "a")
        .unwrap()
        .is_none());
    assert!(reopened.vector_get("ephem_vec", "v1").unwrap().is_none());
    assert!(reopened
        .fts_search("ephem_fts", "ephemeral", 10)
        .unwrap()
        .is_empty());
    assert_eq!(
        reopened
            .crdt()
            .counter("ephem_crdt", "online")
            .get()
            .unwrap(),
        0
    );
    assert!(reopened
        .sql_query("SELECT COUNT(*) FROM live_sessions;", &[])
        .is_err());
}

#[test]
fn test_mem_engine_ttl_and_lru_drop_oldest_eviction() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(temp.path().join("evict.lilia"))).unwrap();

    // 1. LRU eviction: access k1 before inserting k4 => k2 evicted, k1/k3/k4 kept
    let lru_ns = db.collection("lru_cache").memory().lru(3).kv().unwrap();
    lru_ns.set(b"k1", b"v1").unwrap();
    lru_ns.set(b"k2", b"v2").unwrap();
    lru_ns.set(b"k3", b"v3").unwrap();
    assert_eq!(lru_ns.get(b"k1").unwrap().unwrap().value, b"v1");
    lru_ns.set(b"k4", b"v4").unwrap();

    assert!(
        lru_ns.get(b"k2").unwrap().is_none(),
        "k2 should be LRU-evicted"
    );
    assert!(lru_ns.get(b"k1").unwrap().is_some());
    assert!(lru_ns.get(b"k3").unwrap().is_some());
    assert!(lru_ns.get(b"k4").unwrap().is_some());
    assert!(db.memory_tier_stats().lru_evictions >= 1);

    // 2. DropOldest eviction: reading f1 must NOT save f1 from FIFO eviction
    let fifo_ns = db
        .collection("fifo_cache")
        .memory()
        .drop_oldest(3)
        .kv()
        .unwrap();
    fifo_ns.set(b"f1", b"1").unwrap();
    fifo_ns.set(b"f2", b"2").unwrap();
    fifo_ns.set(b"f3", b"3").unwrap();
    assert_eq!(fifo_ns.get(b"f1").unwrap().unwrap().value, b"1");
    fifo_ns.set(b"f4", b"4").unwrap();

    assert!(
        fifo_ns.get(b"f1").unwrap().is_none(),
        "f1 should be evicted oldest first"
    );
    assert!(fifo_ns.get(b"f2").unwrap().is_some());
    assert!(fifo_ns.get(b"f3").unwrap().is_some());
    assert!(fifo_ns.get(b"f4").unwrap().is_some());

    // 3. Native collection TTL
    let ttl_ns = db.collection("short_ttl").memory().ttl_ms(25).kv().unwrap();
    ttl_ns.set(b"tok", b"abc").unwrap();
    assert!(ttl_ns.get(b"tok").unwrap().is_some());
    thread::sleep(Duration::from_millis(45));
    assert!(ttl_ns.get(b"tok").unwrap().is_none());
    assert!(db.memory_tier_stats().ttl_evictions >= 1);
}

#[test]
fn test_mixed_disk_memory_transactions_and_promote_demote() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("mixed.lilia");

    {
        let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();
        let disk_kv = db.collection("orders").disk().kv().unwrap();
        let mem_kv = db.collection("hot_state").memory().kv().unwrap();
        let mem_json = db.collection("hot_docs").memory().json().unwrap();
        let mem_ts = db.collection("hot_ts").memory().timeseries().unwrap();

        disk_kv.set(b"ord_1", b"pending").unwrap();
        mem_kv.set(b"lock_1", b"held").unwrap();

        // Atomic cross-tier commit
        db.cross_tier_transaction(|tx| {
            tx.kv_set_if_version("orders", b"ord_1", b"paid", Some(1));
            tx.kv_set_if_version("hot_state", b"lock_1", b"released", Some(1));
            tx.json_put("hot_docs", "receipt_1", json!({"total": 42}), None);
            Ok(())
        })
        .unwrap();

        assert_eq!(disk_kv.get(b"ord_1").unwrap().unwrap().value, b"paid");
        assert_eq!(mem_kv.get(b"lock_1").unwrap().unwrap().value, b"released");
        assert_eq!(
            mem_json.get("receipt_1").unwrap().unwrap().value()["total"],
            42
        );

        // Rollback case 1: Memory CAS mismatch must abort Disk write as well!
        let err = db
            .cross_tier_transaction(|tx| {
                tx.kv_set("orders", b"ord_rollback", b"should_not_exist");
                tx.kv_set_if_version("hot_state", b"lock_1", b"bad", Some(999));
                Ok(())
            })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
        assert!(disk_kv.get(b"ord_rollback").unwrap().is_none());
        assert_eq!(mem_kv.get(b"lock_1").unwrap().unwrap().value, b"released");

        // Rollback case 2: Disk CAS mismatch must abort valid Memory write too!
        let err = db
            .cross_tier_transaction(|tx| {
                tx.kv_set_if_version("hot_state", b"lock_1", b"bad2", Some(2));
                tx.kv_set_if_version("orders", b"ord_1", b"broken", Some(999));
                Ok(())
            })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
        assert_eq!(mem_kv.get(b"lock_1").unwrap().unwrap().value, b"released");
        assert_eq!(mem_kv.get(b"lock_1").unwrap().unwrap().version, 2);

        // Populate TimeSeries in memory then promote KV, JSON, and TimeSeries to Disk
        mem_ts
            .append_points(&[
                TimeSeriesPoint::new(100, 1.5),
                TimeSeriesPoint::new(200, 2.5),
            ])
            .unwrap();
        assert_eq!(mem_kv.promote().unwrap(), 1);
        assert_eq!(mem_json.promote().unwrap(), 1);
        assert_eq!(mem_ts.promote().unwrap(), 2);

        assert_eq!(mem_kv.tier(), StorageTier::Disk);
        assert_eq!(mem_json.tier(), StorageTier::Disk);
        assert_eq!(mem_ts.tier(), StorageTier::Disk);
        assert_eq!(db.memory_tier_stats().kv_entries, 0);

        // Demote a collection back to Memory and verify removal from SQLite main tables
        db.collection("orders")
            .kv()
            .unwrap()
            .demote(StorageTier::Memory)
            .unwrap();
        assert_eq!(
            scalar_i64(
                &db,
                "SELECT COUNT(*) FROM main._lilia_kv WHERE namespace = 'orders';"
            ),
            0
        );
        assert_eq!(
            db.kv_get("orders", b"ord_1").unwrap().unwrap().value,
            b"paid"
        );
    }

    // Promoted hot_state / hot_docs / hot_ts survive DB reopen on disk; demoted `orders` does not
    let reopened = Database::open(DatabaseOptions::durable(&db_path)).unwrap();
    assert_eq!(
        reopened
            .kv_get("hot_state", b"lock_1")
            .unwrap()
            .unwrap()
            .value,
        b"released"
    );
    assert_eq!(
        reopened
            .json_get("hot_docs", "receipt_1")
            .unwrap()
            .unwrap()
            .value()["total"],
        42
    );
    assert_eq!(reopened.ts_query_range("hot_ts", 0, 500).unwrap().len(), 2);
    assert!(reopened.kv_get("orders", b"ord_1").unwrap().is_none());
}

#[test]
fn test_ring_buffer_ryow_overflow_policies_groupsync_and_close_drain() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("buffered.lilia");

    // 1. RYOW + IoT Gorilla TS coalescing + GroupSync + close() drain
    {
        let opts = DatabaseOptions::durable(&db_path)
            .with_ring_buffer(RingBufferConfig {
                capacity: 16_384,
                max_batch_size: 2_048,
                flush_interval_ms: 2_500, // Keep writes in PendingOverlay briefly for RYOW checks
                watermark_trigger: 16_384,
                overflow_policy: OverflowPolicy::Block {
                    timeout_ms: Some(2_000),
                },
                coalesce_timeseries: true,
                coalesce_kv_writes: true,
                enable_read_overlay: true,
            })
            .with_buffered_kv("events")
            .with_buffered_collection(CollectionKind::Json, "buf_docs")
            .with_buffered_series("iot_sensor_1");
        let db = Database::open(opts).unwrap();

        // Seed disk key in another buffered namespace after flush so we can verify overlay tombstones
        put_kv(&db, "events", b"stale_key", b"disk_val").unwrap();
        db.flush_writes().unwrap();

        // Immediate buffered mutations stay in PendingOverlay before interval/watermark flush
        put_kv(&db, "events", b"k_new", b"v_new").unwrap();
        del_kv(&db, "events", b"stale_key").unwrap();
        put_json(&db, "buf_docs", "d1", json!({"ok": true})).unwrap();

        // RYOW via Rust API
        assert_eq!(
            db.kv_get("events", b"k_new").unwrap().unwrap().value,
            b"v_new"
        );
        assert!(db.kv_get("events", b"stale_key").unwrap().is_none());
        let scanned = db.kv_scan("events", None, 10).unwrap();
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].key, b"k_new");
        assert_eq!(
            db.json_get("buf_docs", "d1").unwrap().unwrap().value()["ok"],
            true
        );
        assert_eq!(db.json_scan("buf_docs", None, 10).unwrap().len(), 1);

        // RYOW via unified SQL views (buffered rows visible, tombstoned disk row hidden!)
        assert_eq!(
            scalar_i64(
                &db,
                "SELECT COUNT(*) FROM lilia_kv WHERE namespace = 'events' AND tier = 'buffered';"
            ),
            1
        );
        assert_eq!(
            scalar_i64(
                &db,
                "SELECT COUNT(*) FROM lilia_kv WHERE namespace = 'events';"
            ),
            1
        );
        assert_eq!(
            scalar_i64(
                &db,
                "SELECT COUNT(*) FROM lilia_json WHERE space = 'buf_docs' AND tier = 'buffered';"
            ),
            1
        );

        // Stream 2,500 individual single-point IoT sensor events and flush => coalesced into Gorilla chunks!
        for ts in 1..=2_500_i64 {
            db.ts_append_buffered(
                "iot_sensor_1",
                &[TimeSeriesPoint::new(ts * 10, (ts as f64) * 0.5)],
            )
            .unwrap();
        }
        // RYOW TS stats & range even before flush
        let pre_stats = db
            .ts_query_stats("iot_sensor_1", 0, 100_000)
            .unwrap()
            .unwrap();
        assert_eq!(pre_stats.count, 2_500);

        let flush_stats = db.flush_writes().unwrap();
        assert!(flush_stats.coalesced_ts_points >= 2_500);
        let chunk_rows = scalar_i64(
            &db,
            "SELECT COUNT(*) FROM main._lilia_timeseries_chunks WHERE series_id = 'iot_sensor_1';",
        );
        assert!(
            chunk_rows <= 25,
            "expected >= 100x chunk coalescing reduction for 2,500 points, got {chunk_rows} rows"
        );
        assert_eq!(
            scalar_i64(
                &db,
                "SELECT COALESCE(SUM(point_count), 0) FROM main._lilia_timeseries_chunks WHERE series_id = 'iot_sensor_1';"
            ),
            2_500
        );

        // Concurrent GroupSync commit across 6 threads sharing group WAL commits
        let shared = Arc::new(db);
        let barrier = Arc::new(Barrier::new(6));
        let mut handles = Vec::new();
        for tid in 0..6_usize {
            let db_ref = Arc::clone(&shared);
            let bar = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                bar.wait();
                db_ref
                    .commit_group_batch(
                        vec![BatchOperation::KvSet {
                            namespace: "events".into(),
                            key: format!("sync_{tid}").into_bytes(),
                            value: b"ok".to_vec(),
                            if_version: None,
                            expires_at_ms: None,
                        }],
                        None,
                    )
                    .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        for tid in 0..6 {
            assert_eq!(
                shared
                    .kv_get("events", format!("sync_{tid}").as_bytes())
                    .unwrap()
                    .unwrap()
                    .value,
                b"ok"
            );
        }

        // Write without explicit flush, then close() must drain cleanly to disk
        put_kv(&shared, "events", b"on_close", b"persisted").unwrap();
        let db = Arc::try_unwrap(shared).expect("single owner remaining");
        db.close(Duration::from_secs(5)).unwrap();
    }

    let reopened = Database::open(DatabaseOptions::durable(&db_path)).unwrap();
    assert_eq!(
        reopened
            .kv_get("events", b"on_close")
            .unwrap()
            .unwrap()
            .value,
        b"persisted"
    );
    assert_eq!(
        reopened
            .ts_query_range("iot_sensor_1", 0, 100_000)
            .unwrap()
            .len(),
        2_500
    );

    // 2. OverflowPolicy::Reject deterministically returns Busy under writer contention
    {
        let dir2 = tempfile::tempdir().unwrap();
        let db2 = Arc::new(
            Database::open(
                DatabaseOptions::durable(dir2.path().join("reject.lilia"))
                    .with_ring_buffer(RingBufferConfig {
                        capacity: 16,
                        max_batch_size: 16,
                        flush_interval_ms: 5_000,
                        watermark_trigger: 16,
                        overflow_policy: OverflowPolicy::Reject,
                        ..RingBufferConfig::default()
                    })
                    .with_buffered_kv("flood"),
            )
            .unwrap(),
        );

        let barrier = Arc::new(Barrier::new(5));
        let mut threads = Vec::new();
        // Saturate SQLite writer in background so flusher cannot outpace flood
        let db_disk = Arc::clone(&db2);
        let b_disk = Arc::clone(&barrier);
        threads.push(thread::spawn(move || {
            b_disk.wait();
            for r in 0..40 {
                let batch: Vec<BatchOperation> = (0..250)
                    .map(|i| BatchOperation::KvSet {
                        namespace: "disk_slow".into(),
                        key: format!("s_{r}_{i}").into_bytes(),
                        value: vec![1u8; 512],
                        if_version: None,
                        expires_at_ms: None,
                    })
                    .collect();
                let _ = db_disk.batch(&batch);
            }
        }));
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for tid in 0..4 {
            let db_p = Arc::clone(&db2);
            let b_p = Arc::clone(&barrier);
            let hits_c = Arc::clone(&hits);
            threads.push(thread::spawn(move || {
                b_p.wait();
                for i in 0..2_000 {
                    if let Err(e) = put_kv(&db_p, "flood", format!("{tid}_{i}").as_bytes(), b"v") {
                        if e.code == ErrorCode::Busy {
                            hits_c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            break;
                        }
                    }
                }
            }));
        }
        for t in threads {
            t.join().unwrap();
        }
        assert!(
            hits.load(std::sync::atomic::Ordering::Relaxed) > 0
                || db2.ring_buffer_stats().rejected_total > 0,
            "expected Reject policy to return Busy when queue saturated"
        );
    }

    // 3. OverflowPolicy::DropOldest drops circularly without error and skips PendingOverlay
    {
        let dir3 = tempfile::tempdir().unwrap();
        let db3 = Arc::new(
            Database::open(
                DatabaseOptions::durable(dir3.path().join("drop_oldest.lilia"))
                    .with_ring_buffer(RingBufferConfig {
                        capacity: 16,
                        max_batch_size: 16,
                        flush_interval_ms: 5_000,
                        watermark_trigger: 16,
                        overflow_policy: OverflowPolicy::DropOldest,
                        ..RingBufferConfig::default()
                    })
                    .with_buffered_kv("telemetry"),
            )
            .unwrap(),
        );
        let big_disk: Vec<BatchOperation> = (0..1_500)
            .map(|i| BatchOperation::KvSet {
                namespace: "disk_hold".into(),
                key: format!("h_{i}").into_bytes(),
                value: vec![9u8; 512],
                if_version: None,
                expires_at_ms: None,
            })
            .collect();
        let db_disk = Arc::clone(&db3);
        let disk_handle = thread::spawn(move || {
            let _ = db_disk.batch(&big_disk);
        });
        for i in 0..2_000 {
            put_kv(&db3, "telemetry", format!("k_{i}").as_bytes(), b"v")
                .expect("DropOldest must never reject");
        }
        disk_handle.join().unwrap();
        let stats = db3.ring_buffer_stats();
        assert_eq!(stats.overlay_entries, 0, "DropOldest must bypass overlay");
        assert_eq!(stats.rejected_total, 0);
        assert!(stats.dropped_oldest_total > 0 || stats.committed_total > 0);
    }
}

#[test]
fn test_extreme_concurrency_vtab_cross_tier_join_and_ddl() {
    let temp = tempfile::tempdir().unwrap();
    let db =
        Arc::new(Database::open(DatabaseOptions::durable(temp.path().join("conc.lilia"))).unwrap());

    // Extended SQL DDL
    db.sql_query(
        "CREATE COLLECTION sessions (KIND = 'json', TIER = 'memory', TTL = '60s', EVICTION = lru(500));",
        &[],
    )
    .unwrap();
    db.sql_query(
        "CREATE COLLECTION IF NOT EXISTS sessions (KIND = 'json', TIER = 'disk');",
        &[],
    )
    .unwrap();
    assert_eq!(
        db.get_collection_spec(CollectionKind::Json, "sessions")
            .tier,
        StorageTier::Memory
    );

    db.sql_query(
        "CREATE EPHEMERAL TABLE active_peers (peer TEXT PRIMARY KEY, score INTEGER NOT NULL);",
        &[],
    )
    .unwrap();

    // Persistent disk profile records to join against in-memory sessions
    put_json(&db, "users", "u1", json!({"name": "Alice"})).unwrap();
    put_json(&db, "users", "u2", json!({"name": "Bob"})).unwrap();
    put_json(
        &db,
        "sessions",
        "s1",
        json!({"user_id": "u1", "online": true}),
    )
    .unwrap();
    put_json(
        &db,
        "sessions",
        "s2",
        json!({"user_id": "u2", "online": false}),
    )
    .unwrap();

    // Cross-tier SQL JOIN (`tier = 'disk'` joined with `tier = 'memory'` via lilia_vtab!)
    let join_res = db
        .sql_query(
            "SELECT s.id, u.value, s.tier AS session_tier, u.tier AS user_tier
             FROM lilia_json s
             JOIN lilia_json u ON json_extract(s.value, '$.user_id') = u.id
             WHERE s.space = 'sessions' AND s.tier = 'memory'
               AND u.space = 'users' AND u.tier = 'disk'
               AND json_extract(s.value, '$.online') = 1;",
            &[],
        )
        .unwrap();
    assert_eq!(join_res.rows.len(), 1);
    assert_eq!(join_res.rows[0][0], Value::String("s1".into()));
    assert_eq!(join_res.rows[0][2], Value::String("memory".into()));
    assert_eq!(join_res.rows[0][3], Value::String("disk".into()));

    // 8 writers + 8 readers hammering Memory KV/JSON + cross-tier SQL + _lilia_mem shared-cache UNLOCK_NOTIFY
    db.set_collection_spec(CollectionSpec::new(
        CollectionKind::Kv,
        "hot_kv",
        StorageTier::Memory,
    ))
    .unwrap();

    let barrier = Arc::new(Barrier::new(16));
    let mut workers = Vec::new();
    for wid in 0..8_usize {
        let d = Arc::clone(&db);
        let b = Arc::clone(&barrier);
        workers.push(thread::spawn(move || {
            b.wait();
            for step in 0..100 {
                let k = format!("w{wid}_{step}");
                put_kv(&d, "hot_kv", k.as_bytes(), b"val").unwrap();
                put_json(&d, "sessions", &k, json!({"user_id": "u1", "step": step})).unwrap();
                d.sql_query(
                    "INSERT OR REPLACE INTO active_peers(peer, score) VALUES(?1, ?2);",
                    &[json!(k), json!(step)],
                )
                .unwrap();
            }
        }));
    }
    for _rid in 0..8_usize {
        let d = Arc::clone(&db);
        let b = Arc::clone(&barrier);
        workers.push(thread::spawn(move || {
            b.wait();
            for step in 0..50 {
                let _ = d.kv_scan("hot_kv", None, 20).unwrap();
                let _ = d
                    .sql_query(
                        "SELECT COUNT(*) FROM lilia_kv WHERE namespace = 'hot_kv' AND tier = 'memory';",
                        &[],
                    )
                    .unwrap();
                let _ = d
                    .sql_query(
                        "SELECT COUNT(*) FROM active_peers WHERE score >= ?1;",
                        &[json!(step % 10)],
                    )
                    .unwrap();
            }
        }));
    }
    for w in workers {
        w.join().unwrap();
    }

    assert_eq!(
        scalar_i64(
            &db,
            "SELECT COUNT(*) FROM lilia_kv WHERE namespace = 'hot_kv' AND tier = 'memory';"
        ),
        800
    );
    assert_eq!(scalar_i64(&db, "SELECT COUNT(*) FROM active_peers;"), 800);
    assert_eq!(scalar_i64(&db, "SELECT COUNT(*) FROM main._lilia_kv;"), 0);
}
