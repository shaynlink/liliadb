use lilia_core::{BatchOperation, Direction, HnswConfig, VectorElementType, VectorMetric};
use lilia_storage_sqlite::{doc, projection, Database, DatabaseOptions};
use serde::Deserialize;

fn setup_test_db() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("lilia_ql.lilia"))).unwrap();
    (dir, db)
}

#[derive(Debug, Deserialize, PartialEq)]
struct FriendRecommendation {
    user_name: String,
    friend_name: String,
    relevance_score: f64,
}

#[test]
fn test_lilia_ql_full_hybrid_pipeline() {
    let (_dir, db) = setup_test_db();

    // 1. JSON Document Model: Users
    db.batch(&[
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "u_alice".into(),
            value: serde_json::json!({
                "name": "Alice",
                "plan": "pro",
                "active": true,
                "city": "Paris"
            }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "u_bob".into(),
            value: serde_json::json!({
                "name": "Bob",
                "plan": "free",
                "active": true
            }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "u_charlie".into(),
            value: serde_json::json!({
                "name": "Charlie",
                "plan": "pro",
                "active": false
            }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "u_david".into(),
            value: serde_json::json!({
                "name": "David",
                "plan": "pro",
                "active": true
            }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "u_eve".into(),
            value: serde_json::json!({
                "name": "Eve",
                "plan": "pro",
                "active": true,
                "title": "Staff Rust Engineer"
            }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "u_frank".into(),
            value: serde_json::json!({
                "name": "Frank",
                "plan": "free",
                "active": true,
                "title": "Frontend Designer"
            }),
            if_version: None,
        },
    ])
    .unwrap();

    // 2. Graph Model: Social network with FOLLOWS relations
    db.graph_set_node(
        "social",
        "u_alice",
        "User",
        &serde_json::json!({ "name": "Alice" }),
    )
    .unwrap();
    db.graph_set_node(
        "social",
        "u_eve",
        "User",
        &serde_json::json!({ "name": "Eve" }),
    )
    .unwrap();
    db.graph_set_node(
        "social",
        "u_frank",
        "User",
        &serde_json::json!({ "name": "Frank" }),
    )
    .unwrap();

    db.graph_add_edge("social", "u_alice", "u_eve", "FOLLOWS")
        .unwrap();
    db.graph_add_edge("social", "u_alice", "u_frank", "FOLLOWS")
        .unwrap();

    // 3. FTS5 Lexical Model: "technologies" collection
    db.fts_index(
        "technologies",
        "u_eve",
        "High performance rust database architecture, embedded storage and query engine.",
    )
    .unwrap();
    db.fts_index(
        "technologies",
        "u_frank",
        "Modern CSS animations, UI web design, and typography.",
    )
    .unwrap();

    // 4. Vector Model: "bio_embedding" HNSW dense embeddings
    db.vector_create_index(
        "bio_embedding",
        HnswConfig {
            m: 16,
            ef_construction: 64,
            ef_search: 32,
            metric: VectorMetric::Cosine,
            element_type: VectorElementType::Float32,
            dim: 4,
        },
    )
    .unwrap();

    let eve_vec = [0.90_f32, 0.10, 0.0, 0.0];
    let frank_vec = [0.0_f32, 0.0, 0.85, 0.15];
    let eve_bytes: Vec<u8> = eve_vec.iter().flat_map(|f| f.to_le_bytes()).collect();
    let frank_bytes: Vec<u8> = frank_vec.iter().flat_map(|f| f.to_le_bytes()).collect();

    db.vector_set(
        "bio_embedding",
        "u_eve",
        4,
        VectorElementType::Float32,
        &eve_bytes,
    )
    .unwrap();
    db.vector_set(
        "bio_embedding",
        "u_frank",
        4,
        VectorElementType::Float32,
        &frank_bytes,
    )
    .unwrap();

    // 5. Query execution using the exact LiliaQL fluid API:
    let user_vec = [0.95_f32, 0.05, 0.0, 0.0];

    let results = db
        .query()
        .from_json("users")
        .filter(doc! { "plan": "pro", "active": true })
        .traverse_graph("FOLLOWS", Direction::Outbound)
        .search_fts("technologies", "rust database")
        .with_vector_near("bio_embedding", &user_vec, 0.85)
        .project(projection! {
            "user_name": "$.name",
            "friend_name": "$graph.target.name",
            "relevance_score": "$rrf_score",
        })
        .execute()
        .unwrap();

    assert_eq!(results.len(), 1);
    let row = &results.into_rows()[0];
    assert_eq!(row.get_str("user_name"), Some("Alice"));
    assert_eq!(row.get_str("friend_name"), Some("Eve"));
    let score = row.get_f64("relevance_score").unwrap();
    assert!(score > 0.0, "RRF score must be positive, got {score}");

    // 6. Test strongly-typed deserialization via execute_as::<T>()
    let typed: Vec<FriendRecommendation> = db
        .query()
        .from_json("users")
        .filter(doc! { "plan": "pro", "active": true })
        .traverse_graph("FOLLOWS", Direction::Outbound)
        .search_fts("technologies", "rust database")
        .with_vector_near("bio_embedding", &user_vec, 0.85)
        .project(projection! {
            "user_name": "$.name",
            "friend_name": "$graph.target.name",
            "relevance_score": "$rrf_score",
        })
        .execute_as()
        .unwrap();

    assert_eq!(typed.len(), 1);
    assert_eq!(typed[0].user_name, "Alice");
    assert_eq!(typed[0].friend_name, "Eve");
    assert!((typed[0].relevance_score - score).abs() < 1e-6);
}

#[test]
fn test_lilia_ql_json_filter_and_projection() {
    let (_dir, db) = setup_test_db();

    db.batch(&[
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "1".into(),
            value: serde_json::json!({ "name": "Alice", "role": "admin", "score": 95 }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "2".into(),
            value: serde_json::json!({ "name": "Bob", "role": "member", "score": 80 }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "3".into(),
            value: serde_json::json!({ "name": "Charlie", "role": "admin", "score": 90 }),
            if_version: None,
        },
    ])
    .unwrap();

    let results = db
        .query()
        .from_json("users")
        .filter(doc! { "role": "admin" })
        .project(projection! {
            "name": "$.name",
            "score": "$.score",
        })
        .order_by_desc("score")
        .execute()
        .unwrap();

    assert_eq!(results.len(), 2);
    let rows = results.into_rows();
    assert_eq!(rows[0].get_str("name"), Some("Alice"));
    assert_eq!(rows[0].get_i64("score"), Some(95));
    assert_eq!(rows[1].get_str("name"), Some("Charlie"));
    assert_eq!(rows[1].get_i64("score"), Some(90));
}

#[test]
fn test_lilia_ql_fts_and_vector_direct() {
    let (_dir, db) = setup_test_db();

    db.fts_index("articles", "doc1", "Fast database storage engine in Rust")
        .unwrap();
    db.fts_index(
        "articles",
        "doc2",
        "Frontend design with React and Tailwind",
    )
    .unwrap();

    let vec1 = [1.0_f32, 0.0, 0.0, 0.0];
    let vec2 = [0.0_f32, 1.0, 0.0, 0.0];
    let b1: Vec<u8> = vec1.iter().flat_map(|f| f.to_le_bytes()).collect();
    let b2: Vec<u8> = vec2.iter().flat_map(|f| f.to_le_bytes()).collect();

    db.vector_set("articles_vec", "doc1", 4, VectorElementType::Float32, &b1)
        .unwrap();
    db.vector_set("articles_vec", "doc2", 4, VectorElementType::Float32, &b2)
        .unwrap();

    let query_vec = [0.99_f32, 0.01, 0.0, 0.0];

    let results = db
        .query()
        .from_fts("articles", "database")
        .with_vector_near("articles_vec", &query_vec, 0.8)
        .project(projection! {
            "doc_id": "$.id",
            "snippet": "$fts_snippet",
            "score": "$rrf_score",
        })
        .execute()
        .unwrap();

    assert_eq!(results.len(), 1);
    let row = &results.into_rows()[0];
    assert_eq!(row.get_str("doc_id"), Some("doc1"));
    assert!(row.get_str("snippet").unwrap().contains("database"));
    assert!(row.get_f64("score").unwrap() > 0.0);
}

#[test]
fn test_lilia_ql_explain_plan() {
    let (_dir, db) = setup_test_db();

    let plan = db
        .query()
        .from_json("users")
        .filter(doc! { "plan": "pro" })
        .traverse_graph("FOLLOWS", Direction::Outbound)
        .search_fts("technologies", "rust")
        .with_vector_near("bio_embedding", &[1.0_f32, 0.0], 0.85)
        .project(projection! {
            "name": "$.name",
            "rrf": "$rrf_score",
        })
        .explain()
        .unwrap();

    assert!(plan.vector_prefilter);
    assert!(plan.rrf_active);
    assert!(!plan.stages.is_empty());
}

#[test]
fn test_lilia_ql_pagination_and_distinct() {
    let (_dir, db) = setup_test_db();

    for i in 1..=10 {
        db.batch(&[BatchOperation::JsonPut {
            space: "items".into(),
            id: format!("item_{i}"),
            value: serde_json::json!({
                "num": i,
                "category": if i % 2 == 0 { "even" } else { "odd" }
            }),
            if_version: None,
        }])
        .unwrap();
    }

    // Test offset and limit
    let paged = db
        .query()
        .from_json("items")
        .project(projection! {
            "id": "$.num",
        })
        .order_by_asc("id")
        .offset(3)
        .limit(4)
        .execute()
        .unwrap();

    assert_eq!(paged.len(), 4);
    let rows = paged.into_rows();
    assert_eq!(rows[0].get_i64("id"), Some(4));
    assert_eq!(rows[3].get_i64("id"), Some(7));
}

#[test]
fn test_lilia_ql_kv_and_sql_sources() {
    let (_dir, db) = setup_test_db();

    // 1. KV source
    db.batch(&[
        BatchOperation::KvSet {
            namespace: "config".into(),
            key: b"theme".to_vec(),
            value: br#"{"color":"dark","fontSize":14}"#.to_vec(),
            expires_at_ms: None,
            if_version: None,
        },
        BatchOperation::KvSet {
            namespace: "config".into(),
            key: b"telemetry".to_vec(),
            value: br#"{"enabled":true}"#.to_vec(),
            expires_at_ms: None,
            if_version: None,
        },
    ])
    .unwrap();

    let kv_res = db
        .query()
        .from_kv("config")
        .project(projection! {
            "key": "$.id",
            "color": "$.color",
        })
        .execute()
        .unwrap();

    assert_eq!(kv_res.len(), 2);
    let found_theme = kv_res
        .iter()
        .find(|r| r.get_str("key") == Some("theme"))
        .unwrap();
    assert_eq!(found_theme.get_str("color"), Some("dark"));

    // 2. SQL source
    let sql_res = db
        .query()
        .from_sql("SELECT 'system' AS role, 42 AS code")
        .project(projection! {
            "user_role": "$.role",
            "auth_code": "$.code",
        })
        .execute()
        .unwrap();

    assert_eq!(sql_res.len(), 1);
    let row = &sql_res.into_rows()[0];
    assert_eq!(row.get_str("user_role"), Some("system"));
    assert_eq!(row.get_i64("auth_code"), Some(42));
}

#[test]
fn test_lilia_ql_error_handling() {
    let (_dir, db) = setup_test_db();

    // Cannot execute without source
    let res = db.query().filter(doc! { "a": 1 }).execute();
    assert!(res.is_err());
}
