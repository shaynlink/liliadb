use lilia_core::{BatchOperation, DatabaseEvent, Direction};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

#[test]
fn test_secondary_indexing_and_prefix_scan() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("idx_test.lilia"))).unwrap();

    // Register index on space "items" for $.status
    db.create_json_index("items", "by_status", "$.status")
        .unwrap();

    // Insert documents
    db.batch(&[
        BatchOperation::JsonPut {
            space: "items".into(),
            id: "repoA:1".into(),
            value: json!({ "title": "First", "status": "todo" }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "items".into(),
            id: "repoA:2".into(),
            value: json!({ "title": "Second", "status": "in_progress" }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "items".into(),
            id: "repoB:1".into(),
            value: json!({ "title": "Third", "status": "todo" }),
            if_version: None,
        },
    ])
    .unwrap();

    // Query by index "by_status" with value "todo"
    let todos = db
        .json_scan_by_index("items", "by_status", "todo", None, 10)
        .unwrap();
    assert_eq!(todos.len(), 2);
    assert_eq!(todos[0].id, "repoA:1");
    assert_eq!(todos[1].id, "repoB:1");

    // Prefix scan on repoA
    let repo_a_items = db.json_scan_prefix("items", "repoA:", None, 10).unwrap();
    assert_eq!(repo_a_items.len(), 2);
    assert_eq!(repo_a_items[0].id, "repoA:1");
    assert_eq!(repo_a_items[1].id, "repoA:2");

    // Updating a document updates the index
    db.batch(&[BatchOperation::JsonPut {
        space: "items".into(),
        id: "repoA:1".into(),
        value: json!({ "title": "First updated", "status": "done" }),
        if_version: None,
    }])
    .unwrap();

    let todos_after = db
        .json_scan_by_index("items", "by_status", "todo", None, 10)
        .unwrap();
    assert_eq!(todos_after.len(), 1);
    assert_eq!(todos_after[0].id, "repoB:1");

    let dones = db
        .json_scan_by_index("items", "by_status", "done", None, 10)
        .unwrap();
    assert_eq!(dones.len(), 1);
    assert_eq!(dones[0].id, "repoA:1");

    // Deleting a document removes it from index
    db.batch(&[BatchOperation::JsonDelete {
        space: "items".into(),
        id: "repoA:1".into(),
        if_version: None,
    }])
    .unwrap();

    let dones_after = db
        .json_scan_by_index("items", "by_status", "done", None, 10)
        .unwrap();
    assert_eq!(dones_after.len(), 0);
}

#[test]
fn test_json_find_and_index_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("idx_find.lilia"))).unwrap();

    // 1. Insert documents
    db.batch(&[
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "u1".into(),
            value: json!({ "name": "Alice", "role": "admin", "age": 30, "tags": ["rust", "db"] }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "u2".into(),
            value: json!({ "name": "Bob", "role": "viewer", "age": 25, "tags": ["js"] }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "u3".into(),
            value: json!({ "name": "Charlie", "role": "admin", "age": 35, "tags": ["db"] }),
            if_version: None,
        },
    ])
    .unwrap();

    // 2. Query without index (fallback scan)
    let mut q1 = serde_json::Map::new();
    q1.insert("role".into(), json!("admin"));
    let res1 = db.json_find("users", &q1, None, 10).unwrap();
    assert_eq!(res1.len(), 2);
    assert_eq!(res1[0].id, "u1");
    assert_eq!(res1[1].id, "u3");

    // 3. Create index on "role" and list indexes
    db.create_json_index("users", "role", "$.role").unwrap();
    let idxs = db.list_json_indexes("users").unwrap();
    assert_eq!(idxs.len(), 1);
    assert_eq!(idxs[0].index_name, "role");
    assert_eq!(idxs[0].json_path, "$.role");

    // 4. Query with index acceleration
    let res2 = db.json_find("users", &q1, None, 10).unwrap();
    assert_eq!(res2.len(), 2);
    assert_eq!(res2[0].id, "u1");
    assert_eq!(res2[1].id, "u3");

    // 5. Multi-field query (index on role, filter on age)
    let mut q2 = serde_json::Map::new();
    q2.insert("role".into(), json!("admin"));
    q2.insert("age".into(), json!(35));
    let res3 = db.json_find("users", &q2, None, 10).unwrap();
    assert_eq!(res3.len(), 1);
    assert_eq!(res3[0].id, "u3");

    // 6. Multikey array index test
    db.create_json_index("users", "tags", "$.tags").unwrap();
    let mut q3 = serde_json::Map::new();
    q3.insert("tags".into(), json!("db"));
    let res4 = db.json_find("users", &q3, None, 10).unwrap();
    assert_eq!(res4.len(), 2);
    assert_eq!(res4[0].id, "u1");
    assert_eq!(res4[1].id, "u3");

    // 7. Drop index
    let dropped = db.drop_json_index("users", "role").unwrap();
    assert!(dropped);
    let idxs_after = db.list_json_indexes("users").unwrap();
    assert_eq!(idxs_after.len(), 1);
    assert_eq!(idxs_after[0].index_name, "tags");
}

#[test]
fn test_graph_and_reachability() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        dir.path().join("graph_test.lilia"),
    ))
    .unwrap();

    // Build dependency graph:
    // app -> lib_core -> lib_utils
    // app -> lib_ui -> lib_utils
    // worker -> lib_core
    db.graph_set_node("repo_graph", "app", "project", &json!({ "lang": "rust" }))
        .unwrap();
    db.graph_set_node(
        "repo_graph",
        "worker",
        "project",
        &json!({ "lang": "rust" }),
    )
    .unwrap();
    db.graph_set_node(
        "repo_graph",
        "lib_core",
        "library",
        &json!({ "lang": "rust" }),
    )
    .unwrap();
    db.graph_set_node(
        "repo_graph",
        "lib_ui",
        "library",
        &json!({ "lang": "typescript" }),
    )
    .unwrap();
    db.graph_set_node(
        "repo_graph",
        "lib_utils",
        "library",
        &json!({ "lang": "rust" }),
    )
    .unwrap();

    db.graph_add_edge("repo_graph", "app", "lib_core", "depends_on")
        .unwrap();
    db.graph_add_edge("repo_graph", "app", "lib_ui", "depends_on")
        .unwrap();
    db.graph_add_edge("repo_graph", "worker", "lib_core", "depends_on")
        .unwrap();
    db.graph_add_edge("repo_graph", "lib_core", "lib_utils", "depends_on")
        .unwrap();
    db.graph_add_edge("repo_graph", "lib_ui", "lib_utils", "depends_on")
        .unwrap();

    // Verify node retrieval
    let node = db.graph_get_node("repo_graph", "app").unwrap().unwrap();
    assert_eq!(node.kind, "project");
    assert_eq!(node.metadata["lang"], "rust");

    // Test Outgoing reachability from "app" (what does app depend on?)
    let deps = db
        .graph_reachability("repo_graph", &["app".into()], Direction::Outgoing)
        .unwrap();
    assert_eq!(deps, vec!["app", "lib_core", "lib_ui", "lib_utils"]);

    // Test Incoming reachability (impact of changing "lib_utils": who depends on it?)
    let impact = db
        .graph_reachability("repo_graph", &["lib_utils".into()], Direction::Incoming)
        .unwrap();
    assert_eq!(
        impact,
        vec!["app", "lib_core", "lib_ui", "lib_utils", "worker"]
    );

    // Impact of changing "lib_ui" only
    let ui_impact = db
        .graph_reachability("repo_graph", &["lib_ui".into()], Direction::Incoming)
        .unwrap();
    assert_eq!(ui_impact, vec!["app", "lib_ui"]);
}

#[test]
fn test_fts5_indexing_and_search() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("fts_test.lilia"))).unwrap();

    db.fts_index(
        "docs",
        "arch.md",
        "Worklens connects repository architecture, Git worktrees and GitHub delivery",
    )
    .unwrap();
    db.fts_index(
        "docs",
        "lilia.md",
        "LiliaDB is a local-first multimodel database in Rust with SQLite WAL storage",
    )
    .unwrap();
    db.fts_index(
        "logs",
        "proc_1",
        "INFO Starting compilation of lilia-storage-sqlite crate in release mode",
    )
    .unwrap();
    db.fts_index(
        "logs",
        "proc_2",
        "ERROR Failed to bind socket address already in use",
    )
    .unwrap();

    // Search in docs
    let hits_arch = db.fts_search("docs", "worktrees", 10).unwrap();
    assert_eq!(hits_arch.len(), 1);
    assert_eq!(hits_arch[0].doc_id, "arch.md");
    assert!(hits_arch[0].snippet.contains("<b>worktrees</b>"));

    // Search in logs
    let hits_err = db.fts_search("logs", "Failed socket", 10).unwrap();
    assert_eq!(hits_err.len(), 1);
    assert_eq!(hits_err[0].doc_id, "proc_2");

    // Deletion
    db.fts_delete("docs", "arch.md").unwrap();
    let hits_deleted = db.fts_search("docs", "worktrees", 10).unwrap();
    assert_eq!(hits_deleted.len(), 0);
}

#[test]
fn test_cdc_broadcast_events() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("cdc_test.lilia"))).unwrap();

    let mut subscriber = db.subscribe();

    db.batch(&[
        BatchOperation::KvSet {
            namespace: "settings".into(),
            key: b"active_theme".to_vec(),
            value: b"dark".to_vec(),
            if_version: None,
            expires_at_ms: None,
        },
        BatchOperation::JsonPut {
            space: "work_items".into(),
            id: "item_1".into(),
            value: json!({ "title": "Refactor" }),
            if_version: None,
        },
    ])
    .unwrap();

    // Check received events
    let event1 = subscriber.try_recv().unwrap();
    match event1 {
        DatabaseEvent::KvMutated {
            namespace,
            key,
            version,
            deleted,
        } => {
            assert_eq!(namespace, "settings");
            assert_eq!(key, b"active_theme");
            assert_eq!(version, Some(1));
            assert!(!deleted);
        }
        _ => panic!("Expected KvMutated"),
    }

    let event2 = subscriber.try_recv().unwrap();
    match event2 {
        DatabaseEvent::JsonMutated {
            space,
            id,
            version,
            deleted,
        } => {
            assert_eq!(space, "work_items");
            assert_eq!(id, "item_1");
            assert_eq!(version, Some(1));
            assert!(!deleted);
        }
        _ => panic!("Expected JsonMutated"),
    }
}

#[test]
fn test_vector_lifecycle_and_similarity_search() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("vec_test.lilia"))).unwrap();

    let v1: Vec<f32> = vec![1.0, 0.0, 0.0];
    let v2: Vec<f32> = vec![0.0, 1.0, 0.0];
    let v3: Vec<f32> = vec![
        std::f32::consts::FRAC_1_SQRT_2,
        std::f32::consts::FRAC_1_SQRT_2,
        0.0,
    ];

    fn to_bytes(v: &[f32]) -> Vec<u8> {
        let mut b = Vec::with_capacity(v.len() * 4);
        for f in v {
            b.extend_from_slice(&f.to_le_bytes());
        }
        b
    }

    db.vector_set(
        "docs",
        "doc1",
        3,
        lilia_core::VectorElementType::Float32,
        &to_bytes(&v1),
    )
    .unwrap();
    db.vector_set(
        "docs",
        "doc2",
        3,
        lilia_core::VectorElementType::Float32,
        &to_bytes(&v2),
    )
    .unwrap();
    db.vector_set(
        "docs",
        "doc3",
        3,
        lilia_core::VectorElementType::Float32,
        &to_bytes(&v3),
    )
    .unwrap();

    let got = db.vector_get("docs", "doc1").unwrap().unwrap();
    assert_eq!(got.0, 3);
    assert_eq!(got.1, lilia_core::VectorElementType::Float32);
    assert_eq!(got.2, to_bytes(&v1));

    // Query close to v1
    let q: Vec<f32> = vec![0.9, 0.1, 0.0];
    let hits = db
        .vector_search(
            "docs",
            &to_bytes(&q),
            3,
            lilia_core::VectorElementType::Float32,
            lilia_core::VectorMetric::Cosine,
            2,
        )
        .unwrap();

    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, "doc1");
    assert_eq!(hits[1].id, "doc3");

    // Test Int8 Quantized search
    let (q1, _) = lilia_core::quantization::quantize_f32_to_i8(&v1);
    let (q2, _) = lilia_core::quantization::quantize_f32_to_i8(&v2);
    let q1_bytes: Vec<u8> = q1.iter().map(|&x| x as u8).collect();
    let q2_bytes: Vec<u8> = q2.iter().map(|&x| x as u8).collect();

    db.vector_set(
        "int8_docs",
        "i1",
        3,
        lilia_core::VectorElementType::Int8,
        &q1_bytes,
    )
    .unwrap();
    db.vector_set(
        "int8_docs",
        "i2",
        3,
        lilia_core::VectorElementType::Int8,
        &q2_bytes,
    )
    .unwrap();

    let int8_hits = db
        .vector_search(
            "int8_docs",
            &q1_bytes,
            3,
            lilia_core::VectorElementType::Int8,
            lilia_core::VectorMetric::Cosine,
            1,
        )
        .unwrap();
    assert_eq!(int8_hits.len(), 1);
    assert_eq!(int8_hits[0].id, "i1");

    // Test Delete
    assert!(db.vector_delete("docs", "doc1").unwrap());
    assert!(db.vector_get("docs", "doc1").unwrap().is_none());
}

#[test]
fn test_rrf_hybrid_search() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("rrf_test.lilia"))).unwrap();

    // Index documents in FTS
    db.fts_index(
        "articles",
        "art1",
        "Rust embedded database with SQLite storage",
    )
    .unwrap();
    db.fts_index(
        "articles",
        "art2",
        "High performance distributed system in Go",
    )
    .unwrap();
    db.fts_index(
        "articles",
        "art3",
        "Vector similarity search and embeddings in Rust",
    )
    .unwrap();

    fn to_bytes(v: &[f32]) -> Vec<u8> {
        let mut b = Vec::with_capacity(v.len() * 4);
        for f in v {
            b.extend_from_slice(&f.to_le_bytes());
        }
        b
    }

    // Index embeddings
    let v_rust_db: Vec<f32> = vec![0.9, 0.1, 0.0];
    let v_go: Vec<f32> = vec![0.0, 0.9, 0.1];
    let v_vector_search: Vec<f32> = vec![0.85, 0.15, 0.2];

    db.vector_set(
        "articles",
        "art1",
        3,
        lilia_core::VectorElementType::Float32,
        &to_bytes(&v_rust_db),
    )
    .unwrap();
    db.vector_set(
        "articles",
        "art2",
        3,
        lilia_core::VectorElementType::Float32,
        &to_bytes(&v_go),
    )
    .unwrap();
    db.vector_set(
        "articles",
        "art3",
        3,
        lilia_core::VectorElementType::Float32,
        &to_bytes(&v_vector_search),
    )
    .unwrap();

    // Query combining lexical ("database") and semantic vector close to v_rust_db
    let query_vec = to_bytes(&[0.92, 0.08, 0.0]);
    let results = db
        .hybrid_search(
            "articles",
            Some("database"),
            Some((
                &query_vec,
                3,
                lilia_core::VectorElementType::Float32,
                lilia_core::VectorMetric::Cosine,
            )),
            60,
            1.0,
            1.0,
            5,
        )
        .unwrap();

    assert!(!results.is_empty());
    // art1 is top ranked by both FTS and Vector, so it must be top RRF score
    assert_eq!(results[0].id, "art1");
    assert!(results[0].fts_rank.is_some());
    assert!(results[0].vector_rank.is_some());
    assert!(results[0].rrf_score > 0.0);
}

#[test]
fn test_hybrid_json_index_and_graph_vector() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        dir.path().join("hybrid_filtering.lilia"),
    ))
    .unwrap();

    fn to_bytes(v: &[f32]) -> Vec<u8> {
        let mut b = Vec::with_capacity(v.len() * 4);
        for f in v {
            b.extend_from_slice(&f.to_le_bytes());
        }
        b
    }

    // 1. Setup JSON index
    db.create_json_index("products", "by_category", "$.category")
        .unwrap();
    db.batch(&[
        BatchOperation::JsonPut {
            space: "products".into(),
            id: "p1".into(),
            value: json!({ "name": "Laptop", "category": "electronics" }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "products".into(),
            id: "p2".into(),
            value: json!({ "name": "Phone", "category": "electronics" }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "products".into(),
            id: "p3".into(),
            value: json!({ "name": "Apple", "category": "grocery" }),
            if_version: None,
        },
    ])
    .unwrap();

    // Vectors for products
    db.vector_set(
        "products",
        "p1",
        2,
        lilia_core::VectorElementType::Float32,
        &to_bytes(&[1.0, 0.0]),
    )
    .unwrap();
    db.vector_set(
        "products",
        "p2",
        2,
        lilia_core::VectorElementType::Float32,
        &to_bytes(&[0.9, 0.1]),
    )
    .unwrap();
    db.vector_set(
        "products",
        "p3",
        2,
        lilia_core::VectorElementType::Float32,
        &to_bytes(&[1.0, 0.0]),
    )
    .unwrap();

    // Query electronics category only
    let q_vec = to_bytes(&[1.0, 0.0]);
    let hits = db
        .vector_search_by_json_index(
            "products",
            "products",
            "by_category",
            "electronics",
            &q_vec,
            2,
            lilia_core::VectorElementType::Float32,
            lilia_core::VectorMetric::Cosine,
            10,
        )
        .unwrap();

    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, "p1");
    assert_eq!(hits[1].id, "p2");

    // 2. Setup Graph + Vector
    db.batch(&[
        BatchOperation::GraphSetNode {
            graph: "network".into(),
            id: "user_a".into(),
            kind: "person".into(),
            metadata: json!({}),
            doc_space: None,
            doc_id: None,
        },
        BatchOperation::GraphSetNode {
            graph: "network".into(),
            id: "user_b".into(),
            kind: "person".into(),
            metadata: json!({}),
            doc_space: None,
            doc_id: None,
        },
        BatchOperation::GraphSetNode {
            graph: "network".into(),
            id: "user_c".into(),
            kind: "person".into(),
            metadata: json!({}),
            doc_space: None,
            doc_id: None,
        },
        BatchOperation::GraphAddEdge {
            graph: "network".into(),
            source: "user_a".into(),
            target: "user_b".into(),
            kind: "knows".into(),
            weight: 1.0,
        },
    ])
    .unwrap();

    db.vector_set(
        "users",
        "user_a",
        2,
        lilia_core::VectorElementType::Float32,
        &to_bytes(&[1.0, 0.0]),
    )
    .unwrap();
    db.vector_set(
        "users",
        "user_b",
        2,
        lilia_core::VectorElementType::Float32,
        &to_bytes(&[0.8, 0.2]),
    )
    .unwrap();
    db.vector_set(
        "users",
        "user_c",
        2,
        lilia_core::VectorElementType::Float32,
        &to_bytes(&[1.0, 0.0]),
    )
    .unwrap();

    // Traverse from user_a (outgoing) -> user_a and user_b are reachable (user_c is disconnected)
    let graph_hits = db
        .vector_search_in_graph_neighbors(
            "users",
            "network",
            "user_a",
            Direction::Outgoing,
            &q_vec,
            2,
            lilia_core::VectorElementType::Float32,
            lilia_core::VectorMetric::Cosine,
            10,
        )
        .unwrap();

    assert_eq!(graph_hits.len(), 2);
    assert_eq!(graph_hits[0].id, "user_a");
    assert_eq!(graph_hits[1].id, "user_b");
}

#[test]
fn test_sql_vector_functions_and_view() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("sql_vec.lilia"))).unwrap();

    fn to_bytes(v: &[f32]) -> Vec<u8> {
        let mut b = Vec::with_capacity(v.len() * 4);
        for f in v {
            b.extend_from_slice(&f.to_le_bytes());
        }
        b
    }

    db.vector_set(
        "items",
        "i1",
        2,
        lilia_core::VectorElementType::Float32,
        &to_bytes(&[1.0, 0.0]),
    )
    .unwrap();
    db.vector_set(
        "items",
        "i2",
        2,
        lilia_core::VectorElementType::Float32,
        &to_bytes(&[0.0, 1.0]),
    )
    .unwrap();

    let res = db.sql_query(
        "SELECT id, lilia_vec_distance_cosine(vector, lilia_vec_from_json('[1.0, 0.0]')) AS dist FROM lilia_vectors WHERE collection = 'items' ORDER BY dist ASC;",
        &[],
    ).unwrap();

    assert_eq!(res.rows.len(), 2);
    assert_eq!(res.rows[0][0], json!("i1"));
    assert_eq!(res.rows[1][0], json!("i2"));
    let dist_i1 = res.rows[0][1].as_f64().unwrap();
    assert!(dist_i1 < 1e-5);

    // Test new scalar functions: lilia_vec_dimension, lilia_vec_quantize_sq8, lilia_vec_quantize_bit
    let res_fn = db.sql_query(
        "SELECT lilia_vec_dimension(vector), length(lilia_vec_quantize_sq8(vector)), length(lilia_vec_quantize_bit(vector)) \
         FROM lilia_vectors WHERE id = 'i1';",
        &[],
    ).unwrap();
    assert_eq!(res_fn.rows[0][0], json!(2));
    assert_eq!(res_fn.rows[0][1], json!(2)); // 2 bytes for 2 dims in i8
    assert_eq!(res_fn.rows[0][2], json!(1)); // 1 byte for 2 dims in bit (div_ceil 8)
}

#[test]
fn test_hnsw_lifecycle_persistence_and_search() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("hnsw_test.lilia");

    fn to_bytes(v: &[f32]) -> Vec<u8> {
        let mut b = Vec::with_capacity(v.len() * 4);
        for f in v {
            b.extend_from_slice(&f.to_le_bytes());
        }
        b
    }

    {
        let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

        // 1. Create HNSW Index
        let config = lilia_core::HnswConfig {
            m: 16,
            ef_construction: 64,
            ef_search: 32,
            metric: lilia_core::VectorMetric::Cosine,
            element_type: lilia_core::VectorElementType::Float32,
            dim: 3,
        };
        db.vector_create_index("embeddings", config).unwrap();

        let info = db.vector_get_index_info("embeddings").unwrap().unwrap();
        assert_eq!(info.collection, "embeddings");
        assert_eq!(info.m, 16);
        assert_eq!(info.dim, 3);

        // 2. Insert vectors incrementally into HNSW
        let v1 = vec![1.0f32, 0.0, 0.0];
        let v2 = vec![0.0f32, 1.0, 0.0];
        let v3 = vec![0.85f32, 0.15, 0.0];
        let v4 = vec![0.0f32, 0.0, 1.0];

        db.vector_set(
            "embeddings",
            "doc1",
            3,
            lilia_core::VectorElementType::Float32,
            &to_bytes(&v1),
        )
        .unwrap();
        db.vector_set(
            "embeddings",
            "doc2",
            3,
            lilia_core::VectorElementType::Float32,
            &to_bytes(&v2),
        )
        .unwrap();
        db.vector_set(
            "embeddings",
            "doc3",
            3,
            lilia_core::VectorElementType::Float32,
            &to_bytes(&v3),
        )
        .unwrap();
        db.vector_set(
            "embeddings",
            "doc4",
            3,
            lilia_core::VectorElementType::Float32,
            &to_bytes(&v4),
        )
        .unwrap();

        // 3. Query via HNSW
        let q = to_bytes(&[0.92, 0.08, 0.0]);
        let hits = db
            .vector_search(
                "embeddings",
                &q,
                3,
                lilia_core::VectorElementType::Float32,
                lilia_core::VectorMetric::Cosine,
                2,
            )
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, "doc1");
        assert_eq!(hits[1].id, "doc3");

        // Verify view lilia_vector_indexes
        let idx_view = db.sql_query("SELECT collection, m, dim FROM lilia_vector_indexes WHERE collection = 'embeddings';", &[]).unwrap();
        assert_eq!(idx_view.rows.len(), 1);
        assert_eq!(idx_view.rows[0][0], json!("embeddings"));
        assert_eq!(idx_view.rows[0][1], json!(16));
    }

    // 4. Reopen database from disk and verify HNSW index reloads and queries without re-creating
    {
        let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();
        let info = db.vector_get_index_info("embeddings").unwrap().unwrap();
        assert_eq!(info.collection, "embeddings");
        assert_eq!(info.dim, 3);

        let q = to_bytes(&[0.92, 0.08, 0.0]);
        let hits = db
            .vector_search(
                "embeddings",
                &q,
                3,
                lilia_core::VectorElementType::Float32,
                lilia_core::VectorMetric::Cosine,
                2,
            )
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, "doc1");
        assert_eq!(hits[1].id, "doc3");

        // 5. Test vector_drop_index
        assert!(db.vector_drop_index("embeddings").unwrap());
        assert!(db.vector_get_index_info("embeddings").unwrap().is_none());
    }
}

#[test]
fn test_hnsw_two_stage_sq8_quantization_reranking() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(dir.path().join("sq8_hnsw.lilia"))).unwrap();

    fn to_bytes(v: &[f32]) -> Vec<u8> {
        let mut b = Vec::with_capacity(v.len() * 4);
        for f in v {
            b.extend_from_slice(&f.to_le_bytes());
        }
        b
    }

    // Configure collection with SQ8 Int8 quantization
    let config = lilia_core::HnswConfig {
        m: 16,
        ef_construction: 64,
        ef_search: 32,
        metric: lilia_core::VectorMetric::Cosine,
        element_type: lilia_core::VectorElementType::Int8,
        dim: 4,
    };
    db.vector_create_index("articles", config).unwrap();

    // Insert float32 vectors (automatically quantized in HNSW, f32 saved in _lilia_vectors)
    db.vector_set(
        "articles",
        "a1",
        4,
        lilia_core::VectorElementType::Float32,
        &to_bytes(&[1.0, 0.0, 0.0, 0.0]),
    )
    .unwrap();
    db.vector_set(
        "articles",
        "a2",
        4,
        lilia_core::VectorElementType::Float32,
        &to_bytes(&[0.0, 1.0, 0.0, 0.0]),
    )
    .unwrap();
    db.vector_set(
        "articles",
        "a3",
        4,
        lilia_core::VectorElementType::Float32,
        &to_bytes(&[0.9, 0.1, 0.0, 0.0]),
    )
    .unwrap();
    db.vector_set(
        "articles",
        "a4",
        4,
        lilia_core::VectorElementType::Float32,
        &to_bytes(&[0.0, 0.0, 1.0, 0.0]),
    )
    .unwrap();

    // Query with Float32 vector -> triggers two-stage re-ranking (quantized traversal + f32 re-ranking)
    let q = to_bytes(&[0.95, 0.05, 0.0, 0.0]);
    let hits = db
        .vector_search(
            "articles",
            &q,
            4,
            lilia_core::VectorElementType::Float32,
            lilia_core::VectorMetric::Cosine,
            2,
        )
        .unwrap();

    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, "a1");
    assert_eq!(hits[1].id, "a3");
    assert!(hits[0].distance < hits[1].distance);
}

#[test]
fn test_hnsw_pre_filtered_search_and_hybrid_rrf() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        dir.path().join("prefiltered_hybrid.lilia"),
    ))
    .unwrap();

    fn to_bytes(v: &[f32]) -> Vec<u8> {
        let mut b = Vec::with_capacity(v.len() * 4);
        for f in v {
            b.extend_from_slice(&f.to_le_bytes());
        }
        b
    }

    // 1. Create HNSW index
    let config = lilia_core::HnswConfig {
        m: 16,
        ef_construction: 64,
        ef_search: 32,
        metric: lilia_core::VectorMetric::Cosine,
        element_type: lilia_core::VectorElementType::Float32,
        dim: 3,
    };
    db.vector_create_index("catalog", config).unwrap();

    // 2. Setup JSON index on category
    db.create_json_index("catalog", "by_category", "$.category")
        .unwrap();

    // 3. Batch atomic write across JSON, FTS, and Vectors
    db.batch(&[
        BatchOperation::JsonPut {
            space: "catalog".into(),
            id: "item1".into(),
            value: json!({ "name": "Rust Database Book", "category": "books" }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "catalog".into(),
            id: "item2".into(),
            value: json!({ "name": "Wireless Headphones", "category": "electronics" }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "catalog".into(),
            id: "item3".into(),
            value: json!({ "name": "Smart Watch", "category": "electronics" }),
            if_version: None,
        },
        BatchOperation::VectorSet {
            collection: "catalog".into(),
            id: "item1".into(),
            dim: 3,
            element_type: lilia_core::VectorElementType::Float32,
            vector: to_bytes(&[1.0, 0.0, 0.0]),
        },
        BatchOperation::VectorSet {
            collection: "catalog".into(),
            id: "item2".into(),
            dim: 3,
            element_type: lilia_core::VectorElementType::Float32,
            vector: to_bytes(&[0.85, 0.15, 0.0]),
        },
        BatchOperation::VectorSet {
            collection: "catalog".into(),
            id: "item3".into(),
            dim: 3,
            element_type: lilia_core::VectorElementType::Float32,
            vector: to_bytes(&[0.0, 1.0, 0.0]),
        },
    ])
    .unwrap();

    // Index text in FTS
    db.fts_index(
        "catalog",
        "item1",
        "Rust embedded database storage engine book",
    )
    .unwrap();
    db.fts_index(
        "catalog",
        "item2",
        "Wireless bluetooth noise cancelling headphones",
    )
    .unwrap();
    db.fts_index(
        "catalog",
        "item3",
        "Fitness tracker and smart watch with heart rate",
    )
    .unwrap();

    // 4. Test Pre-filtered Vector Search via JSON secondary index
    // item1 is closest to [1.0, 0.0, 0.0], but it is in 'books'.
    // Filtering on 'electronics' MUST return item2 and item3, skipping item1!
    let q_vec = to_bytes(&[1.0, 0.0, 0.0]);
    let filtered_hits = db
        .vector_search_by_json_index(
            "catalog",
            "catalog",
            "by_category",
            "electronics",
            &q_vec,
            3,
            lilia_core::VectorElementType::Float32,
            lilia_core::VectorMetric::Cosine,
            5,
        )
        .unwrap();

    assert_eq!(filtered_hits.len(), 2);
    assert_eq!(filtered_hits[0].id, "item2");
    assert_eq!(filtered_hits[1].id, "item3");

    // 5. Test ACID Hybrid Search (FTS5 BM25 + HNSW Vector via RRF)
    let hybrid_res = db
        .hybrid_search(
            "catalog",
            Some("database"),
            Some((
                &q_vec,
                3,
                lilia_core::VectorElementType::Float32,
                lilia_core::VectorMetric::Cosine,
            )),
            60,
            1.0,
            1.0,
            5,
        )
        .unwrap();

    assert!(!hybrid_res.is_empty());
    assert_eq!(hybrid_res[0].id, "item1");
    assert!(hybrid_res[0].fts_rank.is_some());
    assert!(hybrid_res[0].vector_rank.is_some());
    assert!(hybrid_res[0].rrf_score > 0.0);
}
