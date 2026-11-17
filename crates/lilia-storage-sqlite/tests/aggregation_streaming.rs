use lilia_core::BatchOperation;
use lilia_storage_sqlite::{doc, Aggregate, Database, DatabaseOptions, Sort};
use serde_json::json;

#[test]
fn test_aggregation_pipeline_full_user_scenario() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("aggregation_test.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path))?;

    // Seed 1,000 transactions
    // Half completed in 2026, half pending or 2025
    let mut batch = Vec::with_capacity(200);
    for i in 0..1000 {
        let is_target = i % 2 == 0;
        let status = if is_target { "completed" } else { "pending" };
        let year = if is_target { 2026 } else { 2025 };
        let doc_id = format!("tx_{i:04}");

        // Each completed transaction has 2 items:
        // Item 1: "electronics" with amount 100.0 + (i as f64)
        // Item 2: "books" with amount 25.0
        let items = if is_target {
            vec![
                json!({ "category": "electronics", "amount": 100.0 + f64::from(i) }),
                json!({ "category": "books", "amount": 25.0 }),
            ]
        } else {
            vec![json!({ "category": "furniture", "amount": 500.0 })]
        };

        batch.push(BatchOperation::JsonPut {
            space: "transactions".into(),
            id: doc_id,
            value: json!({
                "status": status,
                "year": year,
                "customer_id": format!("cust_{}", i % 50),
                "items": items,
            }),
            if_version: None,
        });

        if batch.len() >= 200 || i == 999 {
            db.batch(&batch)?;
            batch.clear();
        }
    }

    // Run the exact user pipeline query!
    let stats = db
        .json()
        .aggregate("transactions")
        .filter(doc! { "status": "completed", "year": 2026 })
        .unwind("items")
        .group_by(
            "category",
            vec![
                Aggregate::sum("amount", "total_revenue"),
                Aggregate::avg("amount", "average_basket"),
                Aggregate::count("count"),
                Aggregate::approx_count_distinct("customer_id", "unique_customers"),
            ],
        )
        .sort(Sort::desc("total_revenue"))
        .limit(10)
        .execute()?;

    assert_eq!(
        stats.len(),
        2,
        "Should return 2 categories: electronics and books"
    );

    // Top 1 must be electronics due to higher revenue
    let top_category = &stats[0];
    assert_eq!(top_category["category"], "electronics");
    assert_eq!(top_category["count"], 500);
    let total_rev = top_category["total_revenue"].as_f64().unwrap();
    assert!(total_rev > 50000.0);
    // Even indices mod 50 yields exactly 25 distinct customer IDs
    assert_eq!(top_category["unique_customers"], 25);

    let second_category = &stats[1];
    assert_eq!(second_category["category"], "books");
    assert_eq!(second_category["count"], 500);
    assert_eq!(second_category["total_revenue"], 12500.0); // 500 * 25.0
    assert_eq!(second_category["average_basket"], 25.0);

    Ok(())
}

#[test]
fn test_aggregation_streaming_pull_iterator() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("aggregation_stream.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path))?;

    let mut batch = Vec::new();
    for i in 0..100 {
        batch.push(BatchOperation::JsonPut {
            space: "events".into(),
            id: format!("ev_{i:03}"),
            value: json!({
                "severity": if i % 2 == 0 { "high" } else { "low" },
                "score": f64::from(i),
            }),
            if_version: None,
        });
    }
    db.batch(&batch)?;

    // Consume via .stream() instead of .execute()
    let mut stream = db
        .json()
        .aggregate("events")
        .filter_fn(|doc| doc.extract_str("severity").as_deref() == Some("high"))
        .group_by(
            "severity",
            vec![
                Aggregate::count("count"),
                Aggregate::avg("score", "mean_score"),
                Aggregate::variance("score", "score_variance"),
                Aggregate::quantile("score", 0.5, "median_score"),
                Aggregate::anomaly_count("score", 2.5, "anomalies"),
            ],
        )
        .stream()?;

    let first = stream.next().expect("should have at least 1 group")?;
    assert_eq!(first["severity"], "high");
    assert_eq!(first["count"], 50);
    assert!(first["mean_score"].as_f64().is_some());
    assert!(first["median_score"].as_f64().is_some());

    assert!(stream.next().is_none(), "no more groups");

    Ok(())
}

#[test]
fn test_aggregation_large_scale_streaming() -> Result<(), Box<dyn std::error::Error>> {
    const TOTAL: usize = 10_000;

    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("aggregation_large.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path))?;

    // Seed 10,000 documents to verify fast bounded streaming
    let mut batch = Vec::with_capacity(1000);
    for i in 0..TOTAL {
        batch.push(BatchOperation::JsonPut {
            space: "metrics".into(),
            id: format!("m_{i:05}"),
            value: json!({
                "device_id": format!("dev_{}", i % 10),
                "reading": f64::from(u8::try_from(i % 100)?),
                "valid": true,
            }),
            if_version: None,
        });

        if batch.len() >= 1000 || i == TOTAL - 1 {
            db.batch(&batch)?;
            batch.clear();
        }
    }

    let results = db
        .json()
        .aggregate("metrics")
        .filter(doc! { "valid": true })
        .group_by(
            "device_id",
            vec![
                Aggregate::count("total_readings"),
                Aggregate::avg("reading", "avg_reading"),
                Aggregate::min("reading", "min_reading"),
                Aggregate::max("reading", "max_reading"),
            ],
        )
        .sort(Sort::asc("device_id"))
        .execute()?;

    assert_eq!(results.len(), 10);
    for group in &results {
        let dev_id: u8 = group["device_id"]
            .as_str()
            .unwrap()
            .strip_prefix("dev_")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(group["total_readings"], 1000);
        assert_eq!(group["min_reading"], f64::from(dev_id));
        assert_eq!(group["max_reading"], f64::from(90 + dev_id));
    }

    Ok(())
}
