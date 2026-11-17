use std::sync::{Arc, Mutex};

use lilia_core::LiveQueryDiff;
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

#[test]
fn test_incremental_materialized_view_grouped() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("ivm_grouped_test.lilia"),
    ))
    .unwrap();

    // 1. Create source table
    db.sql_query(
        "CREATE TABLE tasks (id TEXT PRIMARY KEY, status TEXT, priority INT);",
        &[],
    )
    .unwrap();

    // 2. Define incremental materialized view
    db.create_materialized_view(
        "task_stats",
        "SELECT status, count(*) AS total_tasks, sum(priority) AS total_points FROM tasks GROUP BY status;",
    )
    .unwrap();

    // Verify it is listed in catalog
    let list = db.list_materialized_views().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "task_stats");
    assert_eq!(list[0].source_table, "tasks");

    // 3. Insert rows into tasks
    db.sql_query("INSERT INTO tasks VALUES ('T1', 'pending', 10);", &[])
        .unwrap();
    db.sql_query("INSERT INTO tasks VALUES ('T2', 'pending', 20);", &[])
        .unwrap();
    db.sql_query("INSERT INTO tasks VALUES ('T3', 'completed', 50);", &[])
        .unwrap();

    // 4. Query the materialized view directly in O(1)
    let res = db
        .sql_query(
            "SELECT status, total_tasks, total_points FROM task_stats ORDER BY status ASC;",
            &[],
        )
        .unwrap();

    assert_eq!(res.rows.len(), 2);
    // completed
    assert_eq!(res.rows[0], vec![json!("completed"), json!(1), json!(50.0)]);
    // pending
    assert_eq!(res.rows[1], vec![json!("pending"), json!(2), json!(30.0)]);

    // 5. UPDATE T1: move from 'pending' to 'completed', priority increases from 10 to 15
    db.sql_query(
        "UPDATE tasks SET status = 'completed', priority = 15 WHERE id = 'T1';",
        &[],
    )
    .unwrap();

    let res2 = db
        .sql_query(
            "SELECT status, total_tasks, total_points FROM task_stats ORDER BY status ASC;",
            &[],
        )
        .unwrap();

    assert_eq!(res2.rows.len(), 2);
    // completed: was 1 -> now 2, points: 50 + 15 = 65
    assert_eq!(
        res2.rows[0],
        vec![json!("completed"), json!(2), json!(65.0)]
    );
    // pending: was 2 -> now 1, points: 30 - 10 = 20
    assert_eq!(res2.rows[1], vec![json!("pending"), json!(1), json!(20.0)]);

    // 6. DELETE T2 (the only remaining 'pending' task)
    db.sql_query("DELETE FROM tasks WHERE id = 'T2';", &[])
        .unwrap();

    let res3 = db
        .sql_query(
            "SELECT status, total_tasks, total_points FROM task_stats ORDER BY status ASC;",
            &[],
        )
        .unwrap();

    // 'pending' row should be cleaned up automatically since total_tasks dropped to 0
    assert_eq!(res3.rows.len(), 1);
    assert_eq!(
        res3.rows[0],
        vec![json!("completed"), json!(2), json!(65.0)]
    );
}

#[test]
fn test_incremental_materialized_view_global_and_ddl_syntax() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("ivm_global_test.lilia"),
    ))
    .unwrap();

    // 1. Create table
    db.sql_query(
        "CREATE TABLE orders (id TEXT PRIMARY KEY, amount REAL);",
        &[],
    )
    .unwrap();

    // 2. Create materialized view via raw SQL DDL
    db.sql_query(
        "CREATE MATERIALIZED VIEW order_metrics AS \
         SELECT count(*) AS order_count, sum(amount) AS total_revenue, avg(amount) AS avg_ticket \
         FROM orders;",
        &[],
    )
    .unwrap();

    // Initial check: count = 0, revenue = 0
    let initial = db
        .sql_query(
            "SELECT order_count, total_revenue, avg_ticket FROM order_metrics;",
            &[],
        )
        .unwrap();
    assert_eq!(initial.rows.len(), 1);
    assert_eq!(initial.rows[0][0], json!(0));
    assert_eq!(initial.rows[0][1], json!(0.0));

    // 3. Insert orders
    db.sql_query("INSERT INTO orders VALUES ('O1', 100.0);", &[])
        .unwrap();
    db.sql_query("INSERT INTO orders VALUES ('O2', 200.0);", &[])
        .unwrap();

    let after_inserts = db
        .sql_query(
            "SELECT order_count, total_revenue, avg_ticket FROM order_metrics;",
            &[],
        )
        .unwrap();
    assert_eq!(after_inserts.rows[0][0], json!(2));
    assert_eq!(after_inserts.rows[0][1], json!(300.0));
    assert_eq!(after_inserts.rows[0][2], json!(150.0));

    // 4. Update order O1: change amount from 100.0 to 150.0
    db.sql_query("UPDATE orders SET amount = 150.0 WHERE id = 'O1';", &[])
        .unwrap();

    let after_update = db
        .sql_query(
            "SELECT order_count, total_revenue, avg_ticket FROM order_metrics;",
            &[],
        )
        .unwrap();
    assert_eq!(after_update.rows[0][0], json!(2));
    assert_eq!(after_update.rows[0][1], json!(350.0));
    assert_eq!(after_update.rows[0][2], json!(175.0));

    // 5. Delete order O2
    db.sql_query("DELETE FROM orders WHERE id = 'O2';", &[])
        .unwrap();

    let after_delete = db
        .sql_query(
            "SELECT order_count, total_revenue, avg_ticket FROM order_metrics;",
            &[],
        )
        .unwrap();
    assert_eq!(after_delete.rows[0][0], json!(1));
    assert_eq!(after_delete.rows[0][1], json!(150.0));
    assert_eq!(after_delete.rows[0][2], json!(150.0));

    // 6. DROP MATERIALIZED VIEW via SQL DDL
    db.sql_query("DROP MATERIALIZED VIEW order_metrics;", &[])
        .unwrap();

    let list = db.list_materialized_views().unwrap();
    assert_eq!(list, [] as [lilia_core::MaterializedViewInfo; 0]);
}

#[test]
fn test_live_query_on_materialized_view() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("live_query_ivm.lilia"),
    ))
    .unwrap();

    db.sql_query(
        "CREATE TABLE items (id TEXT PRIMARY KEY, category TEXT, price REAL);",
        &[],
    )
    .unwrap();

    db.create_materialized_view(
        "cat_stats",
        "SELECT category, count(*) AS total_items, sum(price) AS total_val FROM items GROUP BY category;",
    )
    .unwrap();

    // Subscribe to live query on the internal IVM table
    let received_diffs = Arc::new(Mutex::new(Vec::<LiveQueryDiff>::new()));
    let diffs_clone = Arc::clone(&received_diffs);

    let _sub = db
        .live_query(
            "SELECT * FROM _lilia_ivm_cat_stats WHERE category = 'books';",
            &[],
            move |diff| {
                diffs_clone.lock().unwrap().push(diff);
            },
        )
        .unwrap();

    // Initial snapshot
    {
        let diffs = received_diffs.lock().unwrap();
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].is_initial);
    }

    // Insert item in category 'books'
    db.sql_query("INSERT INTO items VALUES ('B1', 'books', 25.0);", &[])
        .unwrap();

    // The IVM trigger executed in O(1) on _lilia_ivm_cat_stats, and the live query picked it up!
    {
        let diffs = received_diffs.lock().unwrap();
        assert_eq!(diffs.len(), 2);
        assert_eq!(diffs[1].added.len(), 1);
        assert_eq!(diffs[1].added[0]["category"], "books");
        assert_eq!(diffs[1].added[0]["total_items"], 1);
        assert_eq!(diffs[1].added[0]["total_val"], 25.0);
    }

    // Insert second item in category 'books'
    db.sql_query("INSERT INTO items VALUES ('B2', 'books', 15.0);", &[])
        .unwrap();

    {
        let diffs = received_diffs.lock().unwrap();
        assert_eq!(diffs.len(), 3);
        assert_eq!(diffs[2].updated.len(), 1);
        assert_eq!(diffs[2].updated[0]["total_items"], 2);
        assert_eq!(diffs[2].updated[0]["total_val"], 40.0);
    }
}
