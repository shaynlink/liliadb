use lilia_core::rules::{Action, Rule, SlidingWindow, TriggerMode, WindowCondition};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;
use tempfile::tempdir;

#[test]
fn test_in_transaction_reactive_heartrate_alert() {
    let dir = tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("reactive.db"))).unwrap();

    // Register declarative reactive rule
    db.rules()
        .register(
            "alert_high_heartrate",
            Rule {
                when: "events.heart_rate > 140 AND events.motion == 'still'".to_string(),
                window: None,
                then: Action::Insert {
                    collection: "emergency_alerts".to_string(),
                    document: json!({
                        "level": "critical",
                        "created_at": "$now",
                        "patient": "$id",
                        "rate": "$event.heart_rate"
                    }),
                },
                target: None,
            },
        )
        .unwrap();

    let events = db.collection("events").json().unwrap();
    let alerts = db.collection("emergency_alerts").json().unwrap();

    // 1. Non-matching event: heart rate normal
    events
        .put(
            "patient_normal",
            json!({
                "heart_rate": 75,
                "motion": "still"
            }),
        )
        .unwrap();

    let normal_alerts = alerts.aggregate().execute().unwrap();
    assert_eq!(normal_alerts.len(), 0);

    // 2. Non-matching event: heart rate high, but motion is running
    events
        .put(
            "patient_running",
            json!({
                "heart_rate": 160,
                "motion": "running"
            }),
        )
        .unwrap();

    let running_alerts = alerts.aggregate().execute().unwrap();
    assert_eq!(running_alerts.len(), 0);

    // 3. Matching event: high heart rate AND still motion -> in-transaction trigger!
    events
        .put(
            "patient_critical",
            json!({
                "heart_rate": 152,
                "motion": "still"
            }),
        )
        .unwrap();

    // The alert is already inserted and committed in the exact same transaction!
    let critical_alerts = alerts.aggregate().execute().unwrap();
    assert_eq!(critical_alerts.len(), 1);
    let alert = &critical_alerts[0];
    assert_eq!(alert["level"], "critical");
    assert_eq!(alert["patient"], "patient_critical");
    assert_eq!(alert["rate"], 152);
    assert!(alert["created_at"].as_str().unwrap().contains("T"));
    let alert = &critical_alerts[0];
    assert_eq!(alert["level"], "critical");
    assert_eq!(alert["patient"], "patient_critical");
    assert_eq!(alert["rate"], 152);
    assert!(alert["created_at"].as_str().unwrap().contains("T"));
}

#[test]
fn test_in_transaction_acid_rollback() {
    let dir = tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("rollback.db"))).unwrap();

    db.rules()
        .register(
            "alert_rule",
            Rule::when("orders.total > 500")
                .then_insert("vip_orders", json!({ "status": "flagged", "id": "$id" }))
                .build(),
        )
        .unwrap();

    // Perform an aborted transaction
    let outcome = db.transaction(|tx| {
        tx.json_put(
            "orders",
            "order_999",
            json!({ "total": 999 }),
            None,
        );
        // Abort the transaction explicitly
        tx.set_rollback_only();
        Ok(())
    });
    assert!(outcome.is_err());

    // Both initial mutation and triggered action must not exist
    let orders = db.collection("orders").json().unwrap();
    assert!(orders.get("order_999").unwrap().is_none());

    let vip = db.collection("vip_orders").json().unwrap();
    assert!(vip.get("order_999").unwrap().is_none());
}

#[test]
fn test_sliding_window_cep_kv_lockout() {
    let dir = tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("cep_kv.db"))).unwrap();

    // « Si plus de 5 tentatives de connexion échouent dans une fenêtre de 60 secondes pour la même clé KV, basculer le statut en verrouillé »
    db.rules()
        .register(
            "lockout_brute_force",
            Rule {
                when: "login_attempts.status == 'failed'".to_string(),
                window: Some(SlidingWindow {
                    duration_ms: 60_000,
                    partition_by: Some("$key".to_string()),
                    condition: WindowCondition::CountGreaterThan(5),
                    trigger_mode: TriggerMode::OnThresholdCrossed,
                }),
                then: Action::KvSet {
                    namespace: "account_status".to_string(),
                    key: "$key".to_string(),
                    value: b"locked".to_vec(),
                },
                target: None,
            },
        )
        .unwrap();

    let logins = db.collection("login_attempts").kv().unwrap();
    let status = db.collection("account_status").kv().unwrap();

    // 5 failed login attempts for alice
    for _ in 1..=5 {
        logins.set(b"alice", b"failed").unwrap();
    }

    // Account for alice must NOT be locked yet (5 is not > 5)
    assert!(status.get(b"alice").unwrap().is_none());

    // 2 failed attempts for bob: partition key isolation
    logins.set(b"bob", b"failed").unwrap();
    logins.set(b"bob", b"failed").unwrap();
    assert!(status.get(b"bob").unwrap().is_none());

    // 6th failed attempt for alice -> threshold is crossed!
    logins.set(b"alice", b"failed").unwrap();

    // Alice account status is locked immediately and atomically!
    let alice_status = status.get(b"alice").unwrap().expect("alice status should be set");
    assert_eq!(alice_status.value, b"locked");

    // Bob still not locked
    assert!(status.get(b"bob").unwrap().is_none());
}

#[test]
fn test_sliding_window_cep_sum_aggregate() {
    let dir = tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("cep_sum.db"))).unwrap();

    // High velocity spending: trigger if sum of transactions for a user > 1000.0 in 30 seconds
    db.rules()
        .register(
            "fraud_high_spend",
            Rule {
                when: "tx_events.amount > 0".to_string(),
                window: Some(SlidingWindow {
                    duration_ms: 30_000,
                    partition_by: Some("user".to_string()),
                    condition: WindowCondition::SumGreaterThan {
                        field: "amount".to_string(),
                        threshold: 1000.0,
                    },
                    trigger_mode: TriggerMode::OnThresholdCrossed,
                }),
                then: Action::Insert {
                    collection: "fraud_alerts".to_string(),
                    document: json!({
                        "user": "$key",
                        "reason": "spend_threshold_exceeded",
                        "triggered_at": "$now"
                    }),
                },
                target: None,
            },
        )
        .unwrap();

    let tx_coll = db.collection("tx_events").json().unwrap();
    let alerts = db.collection("fraud_alerts").json().unwrap();

    // Transaction 1: 400.0
    tx_coll
        .put("t1", json!({ "user": "charlie", "amount": 400.0 }))
        .unwrap();
    assert_eq!(alerts.aggregate().execute().unwrap().len(), 0);

    // Transaction 2: 500.0 (sum = 900.0 <= 1000.0)
    tx_coll
        .put("t2", json!({ "user": "charlie", "amount": 500.0 }))
        .unwrap();
    assert_eq!(alerts.aggregate().execute().unwrap().len(), 0);

    // Transaction 3: 300.0 (sum = 1200.0 > 1000.0) -> Triggers fraud alert!
    tx_coll
        .put("t3", json!({ "user": "charlie", "amount": 300.0 }))
        .unwrap();

    let fraud = alerts.aggregate().execute().unwrap();
    assert_eq!(fraud.len(), 1);
    assert_eq!(fraud[0]["user"], "charlie");
    assert_eq!(fraud[0]["reason"], "spend_threshold_exceeded");
}

#[test]
fn test_rules_persistence_across_reopen() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("persist_rules.db");

    // 1. Open DB, register rule, close DB
    {
        let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();
        db.rules()
            .register(
                "persist_alert",
                Rule::when("sensors.temp > 100")
                    .then_insert("temp_alerts", json!({ "warn": "overheat" }))
                    .build(),
            )
            .unwrap();
        assert_eq!(db.rules().list().len(), 1);
    }

    // 2. Reopen DB and verify rule is loaded and fires
    {
        let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();
        assert_eq!(db.rules().list().len(), 1);
        let rule = db.rules().get("persist_alert").unwrap();
        assert_eq!(rule.when, "sensors.temp > 100");

        let sensors = db.collection("sensors").json().unwrap();
        sensors.put("sensor_a", json!({ "temp": 115 })).unwrap();

        let alerts = db.collection("temp_alerts").json().unwrap();
        let list = alerts.aggregate().execute().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["warn"], "overheat");
    }
}

#[test]
fn test_rules_infinite_recursion_prevention() {
    let dir = tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("cycle.db"))).unwrap();

    // Cyclic rules: A inserts into B, B inserts into A
    db.rules()
        .register(
            "rule_ping",
            Rule::when("coll_a.val > 0")
                .then_insert("coll_b", json!({ "val": 1 }))
                .build(),
        )
        .unwrap();

    db.rules()
        .register(
            "rule_pong",
            Rule::when("coll_b.val > 0")
                .then_insert("coll_a", json!({ "val": 1 }))
                .build(),
        )
        .unwrap();

    let coll_a = db.collection("coll_a").json().unwrap();
    // Writing into coll_a must trigger recursion guard and return ErrorCode::Conflict
    let res = coll_a.put("start", json!({ "val": 1 }));
    assert!(res.is_err());
    let err = res.err().unwrap();
    assert_eq!(err.code, lilia_core::ErrorCode::Conflict);
}
