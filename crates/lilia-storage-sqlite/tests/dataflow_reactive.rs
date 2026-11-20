use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use lilia_core::BatchOperation;
use lilia_storage_sqlite::{Database, DatabaseOptions};

fn setup_db() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        dir.path().join("dataflow_test.lilia"),
    ))
    .unwrap();
    (dir, db)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Order {
    customer_id: String,
    status: String,
    amount: f64,
}

#[test]
fn test_dataflow_exact_user_pipeline() {
    let (_dir, db) = setup_db();

    // 1. Setup Graph: regions and customer BELONGS_TO edges
    db.graph_set_node(
        "regions",
        "reg_eu",
        "Region",
        &json!({ "name": "Europe", "currency": "EUR" }),
    )
    .unwrap();
    db.graph_set_node(
        "regions",
        "reg_us",
        "Region",
        &json!({ "name": "US", "currency": "USD" }),
    )
    .unwrap();

    db.graph_add_edge("regions", "cust_alice", "reg_eu", "BELONGS_TO")
        .unwrap();
    db.graph_add_edge("regions", "cust_bob", "reg_us", "BELONGS_TO")
        .unwrap();

    // 2. Insert initial orders
    db.batch(&[
        BatchOperation::JsonPut {
            space: "orders".into(),
            id: "ord_1".into(),
            value: json!({
                "customer_id": "cust_alice",
                "status": "delivered",
                "amount": 120.0
            }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "orders".into(),
            id: "ord_2".into(),
            value: json!({
                "customer_id": "cust_alice",
                "status": "pending",
                "amount": 80.0
            }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "orders".into(),
            id: "ord_3".into(),
            value: json!({
                "customer_id": "cust_bob",
                "status": "delivered",
                "amount": 250.0
            }),
            if_version: None,
        },
    ])
    .unwrap();

    // 3. Define declarative reactive dataflow pipeline
    let active_user_stats = db
        .dataflow()
        .from_collection("orders")
        .filter(|doc| doc["status"] == "delivered")
        .join_graph("customer_id", "BELONGS_TO", "regions")
        .aggregate_by("region.name", |orders| orders.sum("amount"))
        .into_view("regional_revenue")
        .unwrap();

    // Initial bootstrapped snapshot
    assert_eq!(active_user_stats.get("Europe"), Some(120.0));
    assert_eq!(active_user_stats.get("US"), Some(250.0));
    assert_eq!(active_user_stats.len(), 2);

    // 4. Pure differential update: Insert a new delivered order for Alice (+Δ = 50.0)
    db.batch(&[BatchOperation::JsonPut {
        space: "orders".into(),
        id: "ord_4".into(),
        value: json!({
            "customer_id": "cust_alice",
            "status": "delivered",
            "amount": 50.0
        }),
        if_version: None,
    }])
    .unwrap();

    // View updated in O(1) constant time
    assert_eq!(active_user_stats.get("Europe"), Some(170.0));
    assert_eq!(active_user_stats.get("US"), Some(250.0));

    // 5. Update existing order: ord_2 changes status from "pending" to "delivered" (+Δ = 80.0)
    db.batch(&[BatchOperation::JsonPut {
        space: "orders".into(),
        id: "ord_2".into(),
        value: json!({
            "customer_id": "cust_alice",
            "status": "delivered",
            "amount": 80.0
        }),
        if_version: None,
    }])
    .unwrap();

    assert_eq!(active_user_stats.get("Europe"), Some(250.0));

    // 6. Delete order: delete ord_1 (-Δ = 120.0)
    db.batch(&[BatchOperation::JsonDelete {
        space: "orders".into(),
        id: "ord_1".into(),
        if_version: None,
    }])
    .unwrap();

    assert_eq!(active_user_stats.get("Europe"), Some(130.0));
}

#[test]
fn test_dataflow_functional_cascade() {
    let (_dir, db) = setup_db();

    db.graph_set_node("regions", "reg_apac", "Region", &json!({ "name": "APAC" }))
        .unwrap();
    db.graph_add_edge("regions", "cust_charlie", "reg_apac", "BELONGS_TO")
        .unwrap();

    // Pipeline 1: Raw orders -> Regional Revenue view
    let _regional_rev = db
        .dataflow()
        .from_collection("orders")
        .filter(|doc| doc["status"] == "delivered")
        .join_graph("customer_id", "BELONGS_TO", "regions")
        .aggregate_by("region.name", |orders| orders.sum("amount"))
        .into_view("regional_revenue")
        .unwrap();

    // Pipeline 2 (Cascade): Regional Revenue view -> High Revenue Alerts view
    let high_revenue = db
        .dataflow()
        .from_view("regional_revenue")
        .filter(|entry| entry.get_f64("value").unwrap_or(0.0) >= 500.0)
        .into_view("high_revenue_regions")
        .unwrap();

    assert_eq!(high_revenue.len(), 0);

    // Insert order below threshold
    db.batch(&[BatchOperation::JsonPut {
        space: "orders".into(),
        id: "o1".into(),
        value: json!({
            "customer_id": "cust_charlie",
            "status": "delivered",
            "amount": 300.0
        }),
        if_version: None,
    }])
    .unwrap();

    assert_eq!(
        db.dataflow_view("regional_revenue").unwrap().get("APAC"),
        Some(300.0)
    );
    assert_eq!(high_revenue.get("APAC"), None);

    // Insert second order crossing threshold to 600.0 -> cascades to View 2
    db.batch(&[BatchOperation::JsonPut {
        space: "orders".into(),
        id: "o2".into(),
        value: json!({
            "customer_id": "cust_charlie",
            "status": "delivered",
            "amount": 300.0
        }),
        if_version: None,
    }])
    .unwrap();

    assert_eq!(
        db.dataflow_view("regional_revenue").unwrap().get("APAC"),
        Some(600.0)
    );
    assert_eq!(high_revenue.get("APAC"), Some(1.0));
}

#[test]
fn test_dataflow_typed_pipeline() {
    let (_dir, db) = setup_db();

    db.graph_set_node("regions", "reg_eu", "Region", &json!({ "name": "Europe" }))
        .unwrap();
    db.graph_add_edge("regions", "cust_1", "reg_eu", "BELONGS_TO")
        .unwrap();

    // Typed dataflow pipeline using concrete struct `Order`
    let view = db
        .dataflow()
        .from_collection_typed::<Order>("orders")
        .filter(|doc| doc.status == "delivered")
        .join_graph("customer_id", "BELONGS_TO", "regions")
        .aggregate_by("region.name", |orders| orders.sum("amount"))
        .into_view("typed_revenue")
        .unwrap();

    db.batch(&[BatchOperation::JsonPut {
        space: "orders".into(),
        id: "ord_10".into(),
        value: json!({
            "customer_id": "cust_1",
            "status": "delivered",
            "amount": 42.5
        }),
        if_version: None,
    }])
    .unwrap();

    assert_eq!(view.get("Europe"), Some(42.5));
}

#[test]
fn test_dataflow_view_subscription() {
    let (_dir, db) = setup_db();

    let view = db
        .dataflow()
        .from_collection("items")
        .aggregate_by("category", |items| items.count())
        .into_view("category_counts")
        .unwrap();

    let notification_count = Arc::new(AtomicUsize::new(0));
    let nc = Arc::clone(&notification_count);

    view.subscribe(move |diff| {
        if diff.key == "electronics" {
            nc.fetch_add(1, Ordering::SeqCst);
        }
    });

    db.batch(&[BatchOperation::JsonPut {
        space: "items".into(),
        id: "i1".into(),
        value: json!({ "category": "electronics", "price": 100 }),
        if_version: None,
    }])
    .unwrap();

    assert_eq!(notification_count.load(Ordering::SeqCst), 1);
    assert_eq!(view.get("electronics"), Some(1.0));
}

#[test]
fn test_dataflow_persistence_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("dataflow_persistence.lilia");

    {
        let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

        let view = db
            .dataflow()
            .from_collection("metrics")
            .aggregate_by("service", |m| m.sum("latency"))
            .into_view("service_latency")
            .unwrap();

        db.batch(&[
            BatchOperation::JsonPut {
                space: "metrics".into(),
                id: "m1".into(),
                value: json!({ "service": "auth", "latency": 15.2 }),
                if_version: None,
            },
            BatchOperation::JsonPut {
                space: "metrics".into(),
                id: "m2".into(),
                value: json!({ "service": "api", "latency": 45.8 }),
                if_version: None,
            },
        ])
        .unwrap();

        assert_eq!(view.get("auth"), Some(15.2));
        assert_eq!(view.get("api"), Some(45.8));
    }

    // Reopen database from disk
    {
        let db2 = Database::open(DatabaseOptions::durable(&db_path)).unwrap();
        let restored_view = db2
            .dataflow_view("service_latency")
            .expect("view must be restored");
        assert_eq!(restored_view.get("auth"), Some(15.2));
        assert_eq!(restored_view.get("api"), Some(45.8));
    }
}

#[test]
fn test_dataflow_all_aggregations() {
    let (_dir, db) = setup_db();

    let count_view = db
        .dataflow()
        .from_collection("scores")
        .aggregate_by("player", |s| s.count())
        .into_view("player_count")
        .unwrap();

    let avg_view = db
        .dataflow()
        .from_collection("scores")
        .aggregate_by("player", |s| s.avg("points"))
        .into_view("player_avg")
        .unwrap();

    let min_view = db
        .dataflow()
        .from_collection("scores")
        .aggregate_by("player", |s| s.min("points"))
        .into_view("player_min")
        .unwrap();

    let max_view = db
        .dataflow()
        .from_collection("scores")
        .aggregate_by("player", |s| s.max("points"))
        .into_view("player_max")
        .unwrap();

    db.batch(&[
        BatchOperation::JsonPut {
            space: "scores".into(),
            id: "s1".into(),
            value: json!({ "player": "p1", "points": 10.0 }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "scores".into(),
            id: "s2".into(),
            value: json!({ "player": "p1", "points": 20.0 }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "scores".into(),
            id: "s3".into(),
            value: json!({ "player": "p1", "points": 30.0 }),
            if_version: None,
        },
    ])
    .unwrap();

    assert_eq!(count_view.get("p1"), Some(3.0));
    assert_eq!(avg_view.get("p1"), Some(20.0));
    assert_eq!(min_view.get("p1"), Some(10.0));
    assert_eq!(max_view.get("p1"), Some(30.0));

    // Delete minimum (10.0) -> min should now become 20.0, avg becomes 25.0
    db.batch(&[BatchOperation::JsonDelete {
        space: "scores".into(),
        id: "s1".into(),
        if_version: None,
    }])
    .unwrap();

    assert_eq!(count_view.get("p1"), Some(2.0));
    assert_eq!(avg_view.get("p1"), Some(25.0));
    assert_eq!(min_view.get("p1"), Some(20.0));
    assert_eq!(max_view.get("p1"), Some(30.0));
}
