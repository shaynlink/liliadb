use std::thread::sleep;
use std::time::Duration;

use lilia_core::{BatchOperation, MarkovConfig};
use lilia_storage_sqlite::{CacheOptions, Database, DatabaseOptions};
use serde_json::json;

fn create_test_db(persist_markov: bool) -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("predictive.lilia");

    let cache_options = CacheOptions {
        enabled: true,
        l1_capacity: 512,
        l2_capacity: 2048,
        prefetch_enabled: true,
        prefetch_max_depth: 2,
        markov_config: MarkovConfig {
            max_states: 512,
            max_transitions_per_state: 16,
            confidence_threshold: 0.50,
            min_observations: 2,
            decay_threshold: 500,
            session_window_ms: 2000,
        },
        persist_markov,
    };

    let opts = DatabaseOptions::mobile(db_path).with_cache_options(cache_options);
    let db = Database::open(opts).expect("open database");
    (dir, db)
}

#[test]
fn test_markov_learning_and_prefetch_hit() {
    let (_dir, db) = create_test_db(false);

    // 1. Seed the database with a conversation, its messages, and participant profiles
    let mut batch_ops = vec![
        BatchOperation::JsonPut {
            space: "conversations".into(),
            id: "conv_12".into(),
            value: json!({"title": "General Chat", "participants": ["alice", "bob"]}),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "profiles".into(),
            id: "alice".into(),
            value: json!({"name": "Alice Wonderland", "avatar": "alice.png"}),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "profiles".into(),
            id: "bob".into(),
            value: json!({"name": "Bob Builder", "avatar": "bob.png"}),
            if_version: None,
        },
    ];

    for i in 1..=5 {
        batch_ops.push(BatchOperation::JsonPut {
            space: "messages".into(),
            id: format!("msg_{i}"),
            value: json!({"conv": "conv_12", "text": format!("Message #{i}")}),
            if_version: None,
        });
    }

    db.batch(&batch_ops).expect("seed data");

    // Clear initial cache from seeds so we test true learning & prefetching
    db.clear_cache();
    assert_eq!(db.cache_stats().l1_entries, 0);

    // 2. Training phase: simulate user opening conv_12, then viewing messages and profiles 3 times
    for _ in 0..3 {
        let _ = db.json_get("conversations", "conv_12").expect("get conv");
        for i in 1..=5 {
            let _ = db.json_get("messages", &format!("msg_{i}")).expect("get msg");
        }
        let _ = db.json_get("profiles", "alice").expect("get alice");
        let _ = db.json_get("profiles", "bob").expect("get bob");
    }

    // Check markov stats: transitions should be registered
    let m_stats = db.markov_stats();
    assert!(m_stats.state_count > 0);
    assert!(m_stats.transition_count > 0);

    // 3. Clear cache completely (Markov model in RAM is preserved)
    db.clear_cache();
    assert_eq!(db.cache_stats().l1_entries, 0);
    let initial_hits = db.cache_stats().l1_hits;

    // 4. Trigger event: user accesses ONLY "conversations/conv_12"
    let conv = db.json_get("conversations", "conv_12").expect("get conv");
    assert!(conv.is_some());

    // Give the prefetch worker thread a brief window to asynchronously pre-load predicted documents
    sleep(Duration::from_millis(50));

    // 5. Subsequent reads: should hit L1 Cache immediately (0.00 ms)!
    let msg1 = db.json_get("messages", "msg_1").expect("get msg1");
    assert!(msg1.is_some());
    assert_eq!(msg1.unwrap().value()["text"], "Message #1");

    let stats_after = db.cache_stats();
    // Verify that at least one predicted document was a cache hit
    assert!(stats_after.l1_hits > initial_hits);
    assert!(stats_after.prefetches_completed > 0);
}

#[test]
fn test_cache_invalidation_consistency() {
    let (_dir, db) = create_test_db(false);

    // Insert doc
    db.batch(&[BatchOperation::JsonPut {
        space: "tasks".into(),
        id: "task_1".into(),
        value: json!({"status": "todo"}),
        if_version: None,
    }])
    .expect("insert");

    // First read -> populates cache
    let t1 = db.json_get("tasks", "task_1").unwrap().unwrap();
    assert_eq!(t1.value()["status"], "todo");

    // Update doc via batch
    db.batch(&[BatchOperation::JsonPut {
        space: "tasks".into(),
        id: "task_1".into(),
        value: json!({"status": "done"}),
        if_version: None,
    }])
    .expect("update");

    // Second read -> MUST return updated value "done", never stale "todo"
    let t2 = db.json_get("tasks", "task_1").unwrap().unwrap();
    assert_eq!(t2.value()["status"], "done");

    // Delete doc
    db.batch(&[BatchOperation::JsonDelete {
        space: "tasks".into(),
        id: "task_1".into(),
        if_version: None,
    }])
    .expect("delete");

    // Third read -> MUST return None
    assert!(db.json_get("tasks", "task_1").unwrap().is_none());
}

#[test]
fn test_markov_model_sqlite_persistence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("persistent_markov.lilia");

    let cache_options = CacheOptions {
        enabled: true,
        l1_capacity: 512,
        l2_capacity: 2048,
        prefetch_enabled: true,
        prefetch_max_depth: 2,
        markov_config: MarkovConfig {
            max_states: 512,
            max_transitions_per_state: 8,
            confidence_threshold: 0.50,
            min_observations: 2,
            decay_threshold: 500,
            session_window_ms: 2000,
        },
        persist_markov: true,
    };

    // 1. Open first session and train Markov model
    {
        let opts = DatabaseOptions::mobile(&db_path).with_cache_options(cache_options.clone());
        let db = Database::open(opts).expect("open database");

        db.batch(&[
            BatchOperation::JsonPut {
                space: "docs".into(),
                id: "A".into(),
                value: json!({"name": "A"}),
                if_version: None,
            },
            BatchOperation::JsonPut {
                space: "docs".into(),
                id: "B".into(),
                value: json!({"name": "B"}),
                if_version: None,
            },
        ])
        .expect("insert");

        // Record transition A -> B multiple times
        for _ in 0..4 {
            let _ = db.json_get("docs", "A").unwrap();
            let _ = db.json_get("docs", "B").unwrap();
        }

        let m_stats = db.markov_stats();
        assert!(m_stats.state_count > 0);
        assert!(m_stats.transition_count > 0);

        // Close cleanly, persisting the Markov model to _lilia_markov_state
        db.close(Duration::from_secs(2)).expect("clean close");
    }

    // 2. Open second session and verify Markov model is restored from SQLite!
    {
        let opts = DatabaseOptions::mobile(&db_path).with_cache_options(cache_options);
        let db = Database::open(opts).expect("reopen database");

        let restored_stats = db.markov_stats();
        // The restored Markov model must retain the learned states and transitions!
        assert!(restored_stats.state_count > 0);
        assert!(restored_stats.transition_count > 0);

        // Accessing A should trigger prefetching for B immediately!
        let _ = db.json_get("docs", "A").unwrap();
        sleep(Duration::from_millis(50));

        let hit_b = db.json_get("docs", "B").unwrap();
        assert!(hit_b.is_some());
        assert_eq!(hit_b.unwrap().value()["name"], "B");
    }
}

#[test]
fn test_all_data_models_caching_and_prefetching() {
    let (_dir, db) = create_test_db(false);

    // KV
    db.batch(&[BatchOperation::KvSet {
        namespace: "settings".into(),
        key: b"theme".to_vec(),
        value: b"dark".to_vec(),
        if_version: None,
        expires_at_ms: None,
    }])
    .expect("kv set");

    let kv1 = db.kv_get("settings", b"theme").unwrap().unwrap();
    assert_eq!(kv1.value, b"dark");
    // Second read -> hit cache
    let kv2 = db.kv_get("settings", b"theme").unwrap().unwrap();
    assert_eq!(kv2.value, b"dark");

    // Graph
    db.graph_set_node("social", "user_1", "person", &json!({"role": "admin"}))
        .expect("set graph node");
    let node1 = db.graph_get_node("social", "user_1").unwrap().unwrap();
    assert_eq!(node1.kind, "person");
    // Second read -> hit cache
    let node2 = db.graph_get_node("social", "user_1").unwrap().unwrap();
    assert_eq!(node2.kind, "person");

    // Bitemporal
    let bt_store = db.bitemporal();
    let period = lilia_core::BitemporalInterval::inclusive_days("2026-11-01", "2026-11-30").unwrap();
    let point = lilia_core::BitemporalPoint::at("2026-11-15").unwrap();

    let res = db
        .batch(&[BatchOperation::BitemporalPut {
            space: "deals".into(),
            id: "deal_1".into(),
            value: json!({"val": 100}),
            vt_from: period.from,
            vt_to: period.to,
            system_time: Some(point.as_ms()),
        }])
        .unwrap();
    assert_eq!(res[0].version, Some(1));

    let bt1 = bt_store.get("deals", "deal_1", point, point).unwrap();
    assert!(bt1.is_some());
    assert_eq!(bt1.unwrap().value["val"], 100);

    // Second read
    let bt2 = bt_store.get("deals", "deal_1", point, point).unwrap();
    assert!(bt2.is_some());

    let stats = db.cache_stats();
    assert!(stats.l1_hits > 0);
}
