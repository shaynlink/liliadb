use std::fs::File;
use std::sync::Arc;

use arrow::array::{Float64Array, Int64Array, RecordBatch, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use lilia_core::BatchOperation;
use lilia_storage_sqlite::{Database, DatabaseOptions};
use parquet::arrow::arrow_writer::ArrowWriter;
use serde_json::json;

fn create_sample_parquet(path: &std::path::Path) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("category", DataType::Utf8, false),
        Field::new("price", DataType::Float64, false),
        Field::new("region", DataType::Utf8, false),
        Field::new("quantity", DataType::Int64, false),
    ]));

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(StringArray::from(vec![
                "books",
                "electronics",
                "books",
                "clothing",
                "electronics",
            ])),
            Arc::new(Float64Array::from(vec![15.5, 299.99, 42.0, 59.9, 120.0])),
            Arc::new(StringArray::from(vec!["EU", "US", "EU", "EU", "EU"])),
            Arc::new(Int64Array::from(vec![1, 2, 3, 1, 4])),
        ],
    )
    .unwrap();

    let file = File::create(path).unwrap();
    let mut writer = ArrowWriter::try_new(file, Arc::clone(&schema), None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

#[test]
fn test_read_parquet_direct_aggregation() {
    let temp_dir = tempfile::tempdir().unwrap();
    let parquet_path = temp_dir.path().join("sales_2025.parquet");
    create_sample_parquet(&parquet_path);

    let db_path = temp_dir.path().join("polyglot_test.lilia");
    let db = Database::open(DatabaseOptions::durable(db_path)).unwrap();

    let path_str = parquet_path.to_str().unwrap();
    let query = format!(
        "SELECT category, count(*) AS count_items, sum(price) AS total_price \
         FROM read_parquet('{path_str}') \
         WHERE region = 'EU' \
         GROUP BY category \
         ORDER BY category ASC;"
    );

    let result = db.sql_query(&query, &[]).unwrap();
    assert_eq!(
        result.columns,
        vec!["category", "count_items", "total_price"]
    );
    assert_eq!(result.rows.len(), 3);

    // 1. books: 15.5 + 42.0 = 57.5, count = 2
    assert_eq!(result.rows[0][0], json!("books"));
    assert_eq!(result.rows[0][1], json!(2));
    assert_eq!(result.rows[0][2], json!(57.5));

    // 2. clothing: 59.9, count = 1
    assert_eq!(result.rows[1][0], json!("clothing"));
    assert_eq!(result.rows[1][1], json!(1));
    assert_eq!(result.rows[1][2], json!(59.9));

    // 3. electronics: 120.0 (US row 299.99 is filtered out!), count = 1
    assert_eq!(result.rows[2][0], json!("electronics"));
    assert_eq!(result.rows[2][1], json!(1));
    assert_eq!(result.rows[2][2], json!(120.0));
}

#[test]
fn test_read_csv_direct_query() {
    let temp_dir = tempfile::tempdir().unwrap();
    let csv_path = temp_dir.path().join("customers.csv");
    std::fs::write(
        &csv_path,
        "name;age;city\nAlice;34;Paris\nBob;28;Berlin\nCharlie;45;Rome\nDavid;30;Madrid\n",
    )
    .unwrap();

    let db_path = temp_dir.path().join("csv_test.lilia");
    let db = Database::open(DatabaseOptions::durable(db_path)).unwrap();

    let path_str = csv_path.to_str().unwrap();
    let query = format!(
        "SELECT name, age, city \
         FROM read_csv('{path_str}', delimiter => ';', header => true) \
         WHERE age >= 30 \
         ORDER BY age DESC;"
    );

    let result = db.sql_query(&query, &[]).unwrap();
    assert_eq!(result.columns, vec!["name", "age", "city"]);
    assert_eq!(result.rows.len(), 3);

    assert_eq!(
        result.rows[0],
        vec![json!("Charlie"), json!(45), json!("Rome")]
    );
    assert_eq!(
        result.rows[1],
        vec![json!("Alice"), json!(34), json!("Paris")]
    );
    assert_eq!(
        result.rows[2],
        vec![json!("David"), json!(30), json!("Madrid")]
    );
}

#[test]
fn test_polyglot_join_with_lilia_json_and_kv() {
    let temp_dir = tempfile::tempdir().unwrap();
    let parquet_path = temp_dir.path().join("sales_2025.parquet");
    create_sample_parquet(&parquet_path);

    let db_path = temp_dir.path().join("join_test.lilia");
    let db = Database::open(DatabaseOptions::durable(db_path)).unwrap();

    // Populate local JSON collection 'categories'
    db.batch(&[
        BatchOperation::JsonPut {
            space: "categories".into(),
            id: "books".into(),
            value: json!({ "department": "culture", "tax_rate": 0.05 }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "categories".into(),
            id: "electronics".into(),
            value: json!({ "department": "tech", "tax_rate": 0.20 }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "categories".into(),
            id: "clothing".into(),
            value: json!({ "department": "fashion", "tax_rate": 0.10 }),
            if_version: None,
        },
    ])
    .unwrap();

    let path_str = parquet_path.to_str().unwrap();
    let query = format!(
        "SELECT \
            p.category, \
            count(*) AS cnt, \
            sum(p.price) AS total_price, \
            json_extract(j.value, '$.department') AS dept, \
            json_extract(j.value, '$.tax_rate') AS tax \
         FROM read_parquet('{path_str}') AS p \
         JOIN lilia_json AS j ON j.space = 'categories' AND j.id = p.category \
         WHERE p.region = 'EU' \
         GROUP BY p.category \
         ORDER BY p.category ASC;"
    );

    let result = db.sql_query(&query, &[]).unwrap();
    assert_eq!(
        result.columns,
        vec!["category", "cnt", "total_price", "dept", "tax"]
    );
    assert_eq!(result.rows.len(), 3);

    // books
    assert_eq!(
        result.rows[0],
        vec![
            json!("books"),
            json!(2),
            json!(57.5),
            json!("culture"),
            json!(0.05)
        ]
    );

    // clothing
    assert_eq!(
        result.rows[1],
        vec![
            json!("clothing"),
            json!(1),
            json!(59.9),
            json!("fashion"),
            json!(0.10)
        ]
    );

    // electronics
    assert_eq!(
        result.rows[2],
        vec![
            json!("electronics"),
            json!(1),
            json!(120.0),
            json!("tech"),
            json!(0.20)
        ]
    );
}

#[test]
fn test_create_virtual_table_ddl() {
    let temp_dir = tempfile::tempdir().unwrap();
    let parquet_path = temp_dir.path().join("sales.parquet");
    create_sample_parquet(&parquet_path);

    let db_path = temp_dir.path().join("vtab_ddl.lilia");
    let db = Database::open(DatabaseOptions::durable(db_path)).unwrap();

    let path_str = parquet_path.to_str().unwrap();

    // 1. CREATE VIRTUAL TABLE
    db.sql_query(
        &format!("CREATE VIRTUAL TABLE external_sales USING read_parquet('{path_str}');"),
        &[],
    )
    .unwrap();

    // 2. Query virtual table
    let res = db
        .sql_query(
            "SELECT category, sum(quantity) as total_qty \
             FROM external_sales \
             WHERE region = 'EU' \
             GROUP BY category \
             ORDER BY category ASC;",
            &[],
        )
        .unwrap();

    assert_eq!(res.columns, vec!["category", "total_qty"]);
    assert_eq!(res.rows.len(), 3);
    assert_eq!(res.rows[0], vec![json!("books"), json!(4)]); // 1 + 3 = 4
    assert_eq!(res.rows[1], vec![json!("clothing"), json!(1)]);
    assert_eq!(res.rows[2], vec![json!("electronics"), json!(4)]);
}

#[test]
fn test_zero_wal_writes_and_no_disk_duplication() {
    let temp_dir = tempfile::tempdir().unwrap();
    let parquet_path = temp_dir.path().join("sales_large.parquet");
    create_sample_parquet(&parquet_path);

    let db_path = temp_dir.path().join("zero_wal.lilia");
    let wal_path = temp_dir.path().join("zero_wal.lilia-wal");
    let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

    let path_str = parquet_path.to_str().unwrap();
    let query = format!(
        "SELECT category, count(*), sum(price) \
         FROM read_parquet('{path_str}') \
         WHERE region = 'EU' \
         GROUP BY category;"
    );

    // Initial WAL size (or non-existent)
    let initial_wal_size = if wal_path.exists() {
        std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };

    // Run query 10 times
    for _ in 0..10 {
        let res = db.sql_query(&query, &[]).unwrap();
        assert_eq!(res.rows.len(), 3);
        assert_eq!(res.rows_affected, 0);
    }

    // After read queries, WAL size must not grow
    let post_wal_size = if wal_path.exists() {
        std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };

    assert_eq!(
        initial_wal_size, post_wal_size,
        "WAL size must not increase when querying polyglot virtual tables"
    );
}

#[test]
fn test_error_handling_for_missing_or_invalid_files() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("err_test.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

    let err = db
        .sql_query("SELECT * FROM read_parquet('non_existent.parquet');", &[])
        .unwrap_err();
    assert_eq!(err.code, lilia_core::ErrorCode::NotFound);

    let err_csv = db
        .sql_query("SELECT * FROM read_csv('non_existent.csv');", &[])
        .unwrap_err();
    assert_eq!(err_csv.code, lilia_core::ErrorCode::NotFound);
}
