use lilia_core::{BatchOperation, IndexOptions, StorageTier};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

#[test]
fn test_partial_secondary_index_alerts() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("partial_alerts.lilia");
    let db = Database::open(DatabaseOptions::durable(db_path)).unwrap();

    // 1. Insert 100 alerts: 95 resolved (true), 5 unresolved (false)
    let mut batch = Vec::new();
    for i in 0..100 {
        let is_resolved = i >= 5; // 0..5 false, 5..100 true
        batch.push(BatchOperation::JsonPut {
            space: "alerts".into(),
            id: format!("alert_{i:03}"),
            value: json!({
                "id": format!("alert_{i:03}"),
                "priority": if i % 2 == 0 { "high" } else { "low" },
                "resolved": is_resolved,
            }),
            if_version: None,
        });
    }
    db.batch(&batch).unwrap();

    // 2. Create partial secondary index matching user prompt syntax:
    // db.json().create_index("idx_unresolved_alerts", "alerts", "$.priority", IndexOptions::new().filter("$.resolved == false"))
    db.json()
        .create_index(
            "idx_unresolved_alerts",
            "alerts",
            "$.priority",
            IndexOptions::new().filter("$.resolved == false"),
        )
        .unwrap();

    // Verify index metadata
    let indexes = db.list_json_indexes("alerts").unwrap();
    assert_eq!(indexes.len(), 1);
    assert_eq!(indexes[0].index_name, "idx_unresolved_alerts");
    assert_eq!(indexes[0].json_path, "$.priority");
    assert_eq!(indexes[0].filter.as_deref(), Some("$.resolved == false"));
    assert!(indexes[0].is_partial());

    // 3. Scan unresolved high priority alerts:
    // Among 0..5, alerts 000, 002, 004 have priority "high" and resolved == false
    let high_unresolved = db
        .json_scan_by_index("alerts", "idx_unresolved_alerts", "high", None, 50)
        .unwrap();
    assert_eq!(high_unresolved.len(), 3);
    let ids: Vec<&str> = high_unresolved.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["alert_000", "alert_002", "alert_004"]);

    // 4. Low priority unresolved:
    // Alerts 001, 003 have priority "low" and resolved == false
    let low_unresolved = db
        .json_scan_by_index("alerts", "idx_unresolved_alerts", "low", None, 50)
        .unwrap();
    assert_eq!(low_unresolved.len(), 2);
    let ids_low: Vec<&str> = low_unresolved.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids_low, vec!["alert_001", "alert_003"]);

    // 5. Total indexed entries in _lilia_json_index_entries is exactly 5 (20x smaller than 100!)
    let count_query = db
        .sql_query(
            "SELECT COUNT(*) FROM _lilia_json_index_entries WHERE index_name = 'idx_unresolved_alerts';",
            &[],
        )
        .unwrap();
    assert_eq!(count_query.rows[0][0], json!(5));

    // 6. Update mutation: resolve alert_000 (false -> true)
    db.batch(&[BatchOperation::JsonPut {
        space: "alerts".into(),
        id: "alert_000".into(),
        value: json!({
            "id": "alert_000",
            "priority": "high",
            "resolved": true,
        }),
        if_version: None,
    }])
    .unwrap();

    // Now alert_000 is resolved, it should be removed from partial index!
    let high_after = db
        .json_scan_by_index("alerts", "idx_unresolved_alerts", "high", None, 50)
        .unwrap();
    assert_eq!(high_after.len(), 2);
    let ids_after: Vec<&str> = high_after.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids_after, vec!["alert_002", "alert_004"]);

    // 7. Update mutation: unresolve alert_010 (true -> false)
    db.batch(&[BatchOperation::JsonPut {
        space: "alerts".into(),
        id: "alert_010".into(),
        value: json!({
            "id": "alert_010",
            "priority": "high",
            "resolved": false,
        }),
        if_version: None,
    }])
    .unwrap();

    let high_with_010 = db
        .json_scan_by_index("alerts", "idx_unresolved_alerts", "high", None, 50)
        .unwrap();
    assert_eq!(high_with_010.len(), 3);
    let ids_with_010: Vec<&str> = high_with_010.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids_with_010, vec!["alert_002", "alert_004", "alert_010"]);
}

#[test]
fn test_expression_index_lower_email_and_transparent_matching() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("expr_email.lilia");
    let db = Database::open(DatabaseOptions::durable(db_path)).unwrap();

    db.batch(&[
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "user_1".into(),
            value: json!({ "name": "Alice", "email": "Alice.Wonderland@EXAMPLE.com" }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "user_2".into(),
            value: json!({ "name": "Bob", "email": "BOB@example.com" }),
            if_version: None,
        },
    ])
    .unwrap();

    // Create functional expression index: lower($.email)
    db.json()
        .create_index(
            "idx_lower_email",
            "users",
            "lower($.email)",
            IndexOptions::default(),
        )
        .unwrap();

    // 1. Direct scan by transformed lower-case email
    let found = db
        .json_scan_by_index("users", "idx_lower_email", "alice.wonderland@example.com", None, 10)
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "user_1");

    // 2. Query via json_find with explicit expression key:
    let mut q1 = serde_json::Map::new();
    q1.insert(
        "lower($.email)".into(),
        json!("alice.wonderland@example.com"),
    );
    let res1 = db.json_find("users", &q1, None, 10).unwrap();
    assert_eq!(res1.len(), 1);
    assert_eq!(res1[0].id, "user_1");

    // 3. Double-solution: query via json_find with plain email key and mixed case
    // Query optimizer transparently detects lower($.email) index and transforms lookup value!
    let mut q2 = serde_json::Map::new();
    q2.insert("email".into(), json!("ALICE.WONDERLAND@example.COM"));
    let res2 = db.json_find("users", &q2, None, 10).unwrap();
    assert_eq!(res2.len(), 1);
    assert_eq!(res2[0].id, "user_1");
}

#[test]
fn test_expression_index_strftime_and_json_array_length() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("expr_strftime_array.lilia");
    let db = Database::open(DatabaseOptions::durable(db_path)).unwrap();

    db.batch(&[
        BatchOperation::JsonPut {
            space: "orders".into(),
            id: "ord_2024".into(),
            value: json!({
                "timestamp": "2024-05-10T12:00:00Z",
                "items": ["laptop", "mouse"],
            }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "orders".into(),
            id: "ord_2026_1".into(),
            value: json!({
                "timestamp": "2026-11-13T01:31:34Z",
                "items": ["gpu", "cpu", "ram", "ssd"],
            }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "orders".into(),
            id: "ord_2026_2".into(),
            value: json!({
                "timestamp": "2026-12-25T18:00:00Z",
                "items": ["keyboard"],
            }),
            if_version: None,
        },
    ])
    .unwrap();

    // 1. Index on year extraction: strftime('%Y', $.timestamp)
    db.json()
        .create_index(
            "idx_order_year",
            "orders",
            "strftime('%Y', $.timestamp)",
            IndexOptions::default(),
        )
        .unwrap();

    // 2. Index on array length: json_array_length($.items)
    db.json()
        .create_index(
            "idx_item_count",
            "orders",
            "json_array_length($.items)",
            IndexOptions::default(),
        )
        .unwrap();

    // Search orders by year "2026"
    let orders_2026 = db
        .json_scan_by_index("orders", "idx_order_year", "2026", None, 10)
        .unwrap();
    assert_eq!(orders_2026.len(), 2);
    let ids: Vec<&str> = orders_2026.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["ord_2026_1", "ord_2026_2"]);

    // Search orders with item count 4
    let large_orders = db
        .json_scan_by_index("orders", "idx_item_count", "4", None, 10)
        .unwrap();
    assert_eq!(large_orders.len(), 1);
    assert_eq!(large_orders[0].id, "ord_2026_1");
}

#[test]
fn test_memory_tier_partial_and_expression_index() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("mem_tier_index.lilia");
    let db = Database::open(DatabaseOptions::durable(db_path)).unwrap();

    // Configure collection as StorageTier::Memory
    let alerts_col = db
        .collection("mem_alerts")
        .tier(StorageTier::Memory)
        .json()
        .unwrap();

    alerts_col
        .put(
            "a1",
            json!({ "priority": "high", "resolved": false, "email": "ADMIN@lilia.io" }),
        )
        .unwrap();
    alerts_col
        .put(
            "a2",
            json!({ "priority": "high", "resolved": true, "email": "USER@lilia.io" }),
        )
        .unwrap();

    // Create partial index on memory collection
    alerts_col
        .create_index_with_options(
            "mem_unresolved_priority",
            "$.priority",
            IndexOptions::new().filter("$.resolved == false"),
        )
        .unwrap();

    // Create expression index on memory collection
    alerts_col
        .create_index_with_options("mem_lower_email", "lower($.email)", IndexOptions::default())
        .unwrap();

    // Verify partial index on memory collection
    let unresolved = db
        .json_scan_by_index("mem_alerts", "mem_unresolved_priority", "high", None, 10)
        .unwrap();
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].id, "a1");

    // Verify expression index on memory collection
    let email_found = db
        .json_scan_by_index("mem_alerts", "mem_lower_email", "admin@lilia.io", None, 10)
        .unwrap();
    assert_eq!(email_found.len(), 1);
    assert_eq!(email_found[0].id, "a1");
}
