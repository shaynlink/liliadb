use lilia_core::{BatchOperation, Version};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

#[test]
fn test_json_time_travel_and_delta_encoding() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("time_travel.lilia"),
    ))
    .unwrap();

    // Version 1: initial creation
    let res1 = db
        .batch(&[BatchOperation::JsonPut {
            space: "docs".into(),
            id: "contract_42".into(),
            value: json!({
                "title": "Service Agreement",
                "amount": 1000,
                "status": "draft",
                "author": "Alice"
            }),
            if_version: None,
        }])
        .unwrap();
    assert_eq!(res1[0].version, Some(1));

    // Version 2: update amount and status (delta patch should only store diff)
    let res2 = db
        .batch(&[BatchOperation::JsonPut {
            space: "docs".into(),
            id: "contract_42".into(),
            value: json!({
                "title": "Service Agreement",
                "amount": 1250,
                "status": "in_review",
                "author": "Alice"
            }),
            if_version: Some(1),
        }])
        .unwrap();
    assert_eq!(res2[0].version, Some(2));

    // Version 3: add approved flag
    let res3 = db
        .batch(&[BatchOperation::JsonPut {
            space: "docs".into(),
            id: "contract_42".into(),
            value: json!({
                "title": "Service Agreement",
                "amount": 1250,
                "status": "approved",
                "author": "Alice",
                "approved_by": "Bob"
            }),
            if_version: Some(2),
        }])
        .unwrap();
    assert_eq!(res3[0].version, Some(3));

    // 1. Current version from standard json_get (zero overhead)
    let current = db.json().get("docs", "contract_42").unwrap().unwrap();
    assert_eq!(current.version, 3);
    assert_eq!(current.value["status"], "approved");
    assert_eq!(current.value["amount"], 1250);

    // 2. Query at Version 1
    let v1 = db
        .json()
        .get_at_version("docs", "contract_42", Version(1))
        .unwrap()
        .expect("version 1 must exist");
    assert_eq!(v1.version, 1);
    assert_eq!(v1.value["status"], "draft");
    assert_eq!(v1.value["amount"], 1000);
    assert!(v1.value.get("approved_by").is_none());

    // 3. Query at Version 2
    let v2 = db
        .json()
        .get_at_version("docs", "contract_42", Version(2))
        .unwrap()
        .expect("version 2 must exist");
    assert_eq!(v2.version, 2);
    assert_eq!(v2.value["status"], "in_review");
    assert_eq!(v2.value["amount"], 1250);
    assert!(v2.value.get("approved_by").is_none());

    // 4. Query at Version 3
    let v3 = db
        .json()
        .get_at_version("docs", "contract_42", Version(3))
        .unwrap()
        .expect("version 3 must exist");
    assert_eq!(v3.version, 3);
    assert_eq!(v3.value["status"], "approved");
    assert_eq!(v3.value["approved_by"], "Bob");

    // 5. Query non-existent future version
    let v99 = db
        .json()
        .get_at_version("docs", "contract_42", Version(99))
        .unwrap();
    // At version 99, latest revision is version 3
    assert_eq!(v99.unwrap().version, 3);

    // Version 0 does not exist
    assert!(db
        .json()
        .get_at_version("docs", "contract_42", Version(0))
        .unwrap()
        .is_none());

    // 6. Test History inspection
    let history = db.json().history("docs", "contract_42").unwrap();
    assert_eq!(history.len(), 3);
    assert_eq!(history[0].version, Version(1));
    assert!(history[0].is_keyframe);
    assert_eq!(history[1].version, Version(2));
    assert!(!history[1].is_keyframe);
    assert!(history[1].patch.is_some());

    // 7. Test Diff computation between Version 1 and Version 3
    let diff = db
        .json()
        .diff("docs", "contract_42", Version(1), Version(3))
        .unwrap();
    assert!(diff.is_array());
    let diff_ops = diff.as_array().unwrap();
    assert!(!diff_ops.is_empty());

    // 8. Delete document (creates revision 4 tombstone)
    db.batch(&[BatchOperation::JsonDelete {
        space: "docs".into(),
        id: "contract_42".into(),
        if_version: Some(3),
    }])
    .unwrap();

    assert!(db.json().get("docs", "contract_42").unwrap().is_none());
    // At version 4 (or later), document was deleted
    assert!(db
        .json()
        .get_at_version("docs", "contract_42", Version(4))
        .unwrap()
        .is_none());
    // But prior versions 1, 2, 3 remain intact and queryable!
    assert_eq!(
        db.json()
            .get_at_version("docs", "contract_42", Version(2))
            .unwrap()
            .unwrap()
            .value["status"],
        "in_review"
    );
}

#[test]
fn test_sql_declarative_time_travel() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("sql_time_travel.lilia"),
    ))
    .unwrap();

    // Seed v1
    db.batch(&[BatchOperation::JsonPut {
        space: "docs".into(),
        id: "contract_42".into(),
        value: json!({ "title": "Draft NDA", "cost": 500 }),
        if_version: None,
    }])
    .unwrap();

    // Seed v2
    db.batch(&[BatchOperation::JsonPut {
        space: "docs".into(),
        id: "contract_42".into(),
        value: json!({ "title": "Final NDA", "cost": 750 }),
        if_version: Some(1),
    }])
    .unwrap();

    // 1. Declarative SQL with `FROM <space> AS OF VERSION <v>`
    let res_v1 = db
        .sql_query(
            "SELECT id, json_extract(value, '$.title') AS title, json_extract(value, '$.cost') AS cost \
             FROM docs AS OF VERSION 1 WHERE id = 'contract_42';",
            &[],
        )
        .unwrap();

    assert_eq!(res_v1.rows.len(), 1);
    assert_eq!(res_v1.rows[0][0], json!("contract_42"));
    assert_eq!(res_v1.rows[0][1], json!("Draft NDA"));
    assert_eq!(res_v1.rows[0][2], json!(500));

    // 2. Query at version 2
    let res_v2 = db
        .sql_query(
            "SELECT id, json_extract(value, '$.title') AS title, json_extract(value, '$.cost') AS cost \
             FROM docs AS OF VERSION 2 WHERE id = 'contract_42';",
            &[],
        )
        .unwrap();

    assert_eq!(res_v2.rows.len(), 1);
    assert_eq!(res_v2.rows[0][1], json!("Final NDA"));
    assert_eq!(res_v2.rows[0][2], json!(750));

    // 3. Declarative SQL on global view: `FROM lilia_json AS OF VERSION 1 WHERE space = 'docs'`
    let res_global = db
        .sql_query(
            "SELECT space, id, json_extract(value, '$.cost') AS cost \
             FROM lilia_json AS OF VERSION 1 WHERE space = 'docs' AND id = 'contract_42';",
            &[],
        )
        .unwrap();

    assert_eq!(res_global.rows.len(), 1);
    assert_eq!(res_global.rows[0][0], json!("docs"));
    assert_eq!(res_global.rows[0][1], json!("contract_42"));
    assert_eq!(res_global.rows[0][2], json!(500));
}
