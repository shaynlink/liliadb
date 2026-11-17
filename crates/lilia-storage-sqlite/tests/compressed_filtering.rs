use lilia_core::aggregation::doc;
use lilia_core::BatchOperation;
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

#[test]
#[allow(clippy::too_many_lines)]
fn test_compressed_domain_filtering_full_scenario() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("compressed_filter.lilia");

    let mut options = DatabaseOptions::durable(&db_path);
    options.json_options.dict_sample_threshold = 40;
    options.json_options.dict_target_size_bytes = 4 * 1024;

    let db = Database::open(options).unwrap();

    // 1. Insert 50 training documents to trigger automatic dictionary training
    let mut train_ops = Vec::new();
    for i in 0..50 {
        let status = match i % 4 {
            0 => "archived",
            1 => "active",
            2 => "pending",
            _ => "draft",
        };
        let doc = json!({
            "order_id": format!("ord_{i:04}"),
            "status": status,
            "customer": format!("cust_{}", i % 5),
            "region": "europe-west",
            "metadata": {
                "priority": "normal",
                "warehouse": "wh-paris-01"
            }
        });
        train_ops.push(BatchOperation::JsonPut {
            space: "orders".into(),
            id: format!("ord_{i:04}"),
            value: doc,
            if_version: None,
        });
    }
    db.batch(&train_ops).unwrap();

    // Verify dictionary was trained
    let dict = db.dict_registry().get("orders");
    assert!(dict.is_some(), "Dictionary for 'orders' must be trained");
    let dict_info = dict.unwrap();
    assert!(dict_info.dict_id >= 1);
    assert_ne!(dict_info.raw_bytes, [] as [u8; 0]);
    assert!(dict_info.symbols.vocabulary_len() > 0);

    // 2. Insert 100 new documents using the trained dictionary:
    // Exactly 10 documents with status = 'archived'
    // 90 documents with other statuses ('active', 'pending', 'draft')
    let mut test_ops = Vec::new();
    for i in 100..200 {
        let status = if i % 10 == 0 {
            "archived"
        } else if i % 3 == 0 {
            "active"
        } else if i % 2 == 0 {
            "pending"
        } else {
            "draft"
        };
        let doc = json!({
            "order_id": format!("ord_{i:04}"),
            "status": status,
            "customer": format!("cust_{}", i % 5),
            "region": "europe-west",
            "metadata": {
                "priority": "normal",
                "warehouse": "wh-paris-01"
            }
        });
        test_ops.push(BatchOperation::JsonPut {
            space: "orders".into(),
            id: format!("ord_{i:04}"),
            value: doc,
            if_version: None,
        });
    }
    db.batch(&test_ops).unwrap();

    // Verify wire format is format 2 with LBZ\x02 magic
    let inspect_res = db
        .sql_query(
            "SELECT format, value FROM _lilia_json WHERE space = 'orders' AND id = 'ord_0100';",
            &[],
        )
        .unwrap();
    assert_eq!(inspect_res.rows.len(), 1);
    assert_eq!(inspect_res.rows[0][0].as_i64(), Some(2));

    // 3. Query with json_find: filter for status == 'archived'
    let serde_json::Value::Object(query_map) = json!({ "status": "archived" }) else {
        unreachable!()
    };

    let start_time = std::time::Instant::now();
    let results = db
        .json_find("orders", &query_map, Some("ord_0099"), 100)
        .unwrap();
    let elapsed = start_time.elapsed();

    // Verify exactly 10 archived documents from ord_0100..ord_0199
    assert_eq!(
        results.len(),
        10,
        "Exactly 10 archived orders must be returned (zero false negatives)"
    );
    for entry in &results {
        assert_eq!(
            entry.extract_str("status"),
            Some("archived"),
            "Every returned document must match the predicate"
        );
    }
    println!(
        "Compressed-domain query returned {} docs in {:?}",
        results.len(),
        elapsed
    );

    // 4. Test AggregationPipeline streaming with filter
    let coll = db.collection("orders").json().unwrap();
    let agg_results = coll
        .aggregate()
        .filter(doc! { "status": "archived" })
        .execute()
        .unwrap();

    // In total across 0..50 (train) and 100..200 (test):
    // In 0..50: 0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48 => 13 docs
    // In 100..200: 10 docs => total 23 docs
    assert_eq!(
        agg_results.len(),
        23,
        "Aggregation pipeline filter must find all archived docs without false negatives"
    );

    // 5. Test SQLite SQL function lilia_compressed_match
    let sql_res = db
        .sql_query(
            "SELECT count(*) FROM _lilia_json WHERE space = 'orders' AND lilia_compressed_match(value, 'archived') = 1;",
            &[],
        )
        .unwrap();
    let match_count = sql_res.rows[0][0].as_i64().unwrap();
    assert!(
        match_count >= 23,
        "SQL lilia_compressed_match must match all archived documents"
    );

    let sql_none = db
        .sql_query(
            "SELECT count(*) FROM _lilia_json WHERE space = 'orders' AND lilia_compressed_match(value, 'nonexistent_token_xyz') = 1;",
            &[],
        )
        .unwrap();
    let none_count = sql_none.rows[0][0].as_i64().unwrap();
    assert_eq!(
        none_count, 0,
        "SQL lilia_compressed_match must eliminate all non-matching documents in compressed domain"
    );
}

#[test]
fn test_compressed_skipping_ratio_and_trace() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("skipping_ratio.lilia");

    let mut options = DatabaseOptions::durable(&db_path);
    options.json_options.dict_sample_threshold = 30;
    options.json_options.dict_target_size_bytes = 4 * 1024;

    let db = Database::open(options).unwrap();

    // Insert 40 docs to train dictionary
    let mut train_ops = Vec::new();
    for i in 0..40 {
        train_ops.push(BatchOperation::JsonPut {
            space: "events".into(),
            id: format!("e_{i:03}"),
            value: json!({
                "type": if i % 2 == 0 { "login" } else { "logout" },
                "service": "auth",
                "code": i
            }),
            if_version: None,
        });
    }
    db.batch(&train_ops).unwrap();
    assert!(db.dict_registry().get("events").is_some());

    // Insert 100 new events where only 5 are 'exception'
    let mut test_ops = Vec::new();
    for i in 100..200 {
        test_ops.push(BatchOperation::JsonPut {
            space: "events".into(),
            id: format!("e_{i:03}"),
            value: json!({
                "type": if i % 20 == 0 { "exception" } else { "heartbeat" },
                "service": "auth",
                "code": i
            }),
            if_version: None,
        });
    }
    db.batch(&test_ops).unwrap();

    let serde_json::Value::Object(query_map) = json!({ "type": "exception" }) else {
        unreachable!()
    };

    let (results, trace) = db
        .json_find_with_trace("events", &query_map, Some("e_099"), 100)
        .unwrap();
    assert_eq!(results.len(), 5);
    for entry in &results {
        assert_eq!(entry.extract_str("type"), Some("exception"));
    }

    println!(
        "Trace: examined={}, compressed_skipped={}",
        trace.rows_examined, trace.compressed_skipped
    );
    assert!(
        trace.rows_examined >= 5 && trace.rows_examined <= 10,
        "Only candidate documents (approx 5) should be decompressed, got {}",
        trace.rows_examined
    );
    assert!(
        trace.compressed_skipped >= 90,
        "At least 90 non-matching compressed documents must be eliminated without decompression, got {}",
        trace.compressed_skipped
    );
}
