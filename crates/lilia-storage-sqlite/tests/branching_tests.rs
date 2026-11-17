use lilia_core::{doc, BatchOperation, ErrorCode, MergeStrategy};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;
use tempfile::tempdir;

#[test]
fn test_user_scenario_promo_noel() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("store.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

    // Setup initial catalog
    db.batch(&[
        BatchOperation::JsonPut {
            space: "products".into(),
            id: "laptop".into(),
            value: json!({ "name": "Laptop Pro", "price": 1000.0, "cost": 700.0, "margin": 300.0 }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "products".into(),
            id: "mouse".into(),
            value: json!({ "name": "Wireless Mouse", "price": 50.0, "cost": 20.0, "margin": 30.0 }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "products".into(),
            id: "keyboard".into(),
            value: json!({ "name": "Mechanical Keyboard", "price": 100.0, "cost": 60.0, "margin": 40.0 }),
            if_version: None,
        },
    ])
    .unwrap();

    // Create a SQL view 'products' for fluent SQL querying
    db.sql()
        .execute(
            "CREATE VIEW products AS \
             SELECT id, \
                    json_extract(value, '$.name') AS name, \
                    CAST(json_extract(value, '$.price') AS REAL) AS price, \
                    CAST(json_extract(value, '$.cost') AS REAL) AS cost, \
                    CAST(json_extract(value, '$.margin') AS REAL) AS margin \
             FROM lilia_json WHERE space = 'products';",
        )
        .unwrap();

    // Initial margin check on main: 300 + 30 + 40 = 370
    let initial_margin = db
        .sql()
        .query("SELECT sum(margin) FROM products")
        .unwrap()
        .scalar_f64()
        .unwrap();
    assert_eq!(initial_margin, 370.0);

    // --- EXACT USER SNIPPET EXECUTION ---
    // Crée une branche isolée basée sur l'état actuel
    let simulation = db.branch("simulation_promo_noel").unwrap();

    // On peut tout modifier dans la branche sans impacter la base principale
    let updated = simulation
        .json()
        .update_all("products", doc! { "$mul": { "price": 0.8 } })
        .unwrap();
    assert_eq!(updated, 3);

    // Recalculate margins based on new prices: margin = price - cost
    // For laptop: price=800, cost=700 -> margin=100
    // For mouse: price=40, cost=20 -> margin=20
    // For keyboard: price=80, cost=60 -> margin=20
    // Expected sum(margin) = 100 + 20 + 20 = 140
    let laptop_sim = simulation.json().get("products", "laptop").unwrap().unwrap();
    assert_eq!(laptop_sim.value["price"], 800.0);

    // Update margin in the simulation
    simulation
        .json()
        .put(
            "products",
            "laptop",
            json!({ "name": "Laptop Pro", "price": 800.0, "cost": 700.0, "margin": 100.0 }),
        )
        .unwrap();
    simulation
        .json()
        .put(
            "products",
            "mouse",
            json!({ "name": "Wireless Mouse", "price": 40.0, "cost": 20.0, "margin": 20.0 }),
        )
        .unwrap();
    simulation
        .json()
        .put(
            "products",
            "keyboard",
            json!({ "name": "Mechanical Keyboard", "price": 80.0, "cost": 60.0, "margin": 20.0 }),
        )
        .unwrap();

    let projected_margin = simulation
        .sql()
        .query("SELECT sum(margin) FROM products")
        .unwrap();
    assert_eq!(projected_margin.scalar_f64(), Some(140.0));

    // MAIN IS UNTOUCHED
    let main_laptop = db.json().get("products", "laptop").unwrap().unwrap();
    assert_eq!(main_laptop.value["price"], 1000.0);
    assert_eq!(
        db.sql()
            .query("SELECT sum(margin) FROM products")
            .unwrap()
            .scalar_f64(),
        Some(370.0)
    );

    // Si le scénario est validé, fusion atomique dans main !
    let merge_res = db
        .merge_branch(simulation, MergeStrategy::FastForward)
        .unwrap();
    assert_eq!(merge_res.strategy_used, MergeStrategy::FastForward);

    // Main now reflects the simulation state!
    let merged_laptop = db.json().get("products", "laptop").unwrap().unwrap();
    assert_eq!(merged_laptop.value["price"], 800.0);
    assert_eq!(
        db.sql()
            .query("SELECT sum(margin) FROM products")
            .unwrap()
            .scalar_f64(),
        Some(140.0)
    );
}

#[test]
fn test_instant_branching_latency_and_zero_bytes() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("bench.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

    // Populate with 200 documents
    let mut batch = Vec::new();
    for i in 0..200 {
        batch.push(BatchOperation::JsonPut {
            space: "items".into(),
            id: format!("item_{i:04}"),
            value: json!({ "counter": i, "data": "abcdefghijklmnopqrstuvwxyz" }),
            if_version: None,
        });
    }
    db.batch(&batch).unwrap();

    let start = std::time::Instant::now();
    let branch = db.branch("sub_millisecond_test").unwrap();
    let elapsed = start.elapsed();

    println!("Branch creation latency: {elapsed:?}");
    assert!(
        elapsed < std::time::Duration::from_millis(50),
        "Branch creation should take < 50ms, took {elapsed:?}"
    );

    // Check disk: NO files other than bench.lilia and wal exist in directory
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();

    for file_name in &entries {
        assert!(
            !file_name.contains("sub_millisecond_test"),
            "Branch created a file on disk: {file_name}"
        );
    }

    assert_eq!(branch.dirty_page_count(), 0);
    assert_eq!(branch.memory_bytes(), 0);
}

#[test]
fn test_ai_agent_dreaming_multi_branch_simulation() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ai_agent.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

    // Setup market state
    db.batch(&[
        BatchOperation::JsonPut {
            space: "market".into(),
            id: "asset_alpha".into(),
            value: json!({ "price": 100.0, "risk": 0.2 }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "market".into(),
            id: "asset_beta".into(),
            value: json!({ "price": 200.0, "risk": 0.5 }),
            if_version: None,
        },
    ])
    .unwrap();

    // The AI Agent simulates 10 possible scenarios in parallel branches
    let mut branches = Vec::new();
    for i in 0..10 {
        let branch_name = format!("dream_scenario_{i}");
        let branch = db.branch(&branch_name).unwrap();

        // Each scenario simulates different market shocks
        let shock = 1.0 + (i as f64 - 5.0) * 0.05; // 0.75 to 1.20
        branch
            .json()
            .update_all("market", doc! { "$mul": { "price": shock } })
            .unwrap();

        branches.push((branch_name, branch, shock));
    }

    // Evaluate each scenario: calculate simulated portfolio return
    let mut scores = Vec::new();
    for (name, branch, shock) in &branches {
        let p_alpha = branch.json().get("market", "asset_alpha").unwrap().unwrap().value["price"].as_f64().unwrap();
        let p_beta = branch.json().get("market", "asset_beta").unwrap().unwrap().value["price"].as_f64().unwrap();

        // Utility score = expected return / risk
        let portfolio_value = p_alpha * 10.0 + p_beta * 5.0;
        scores.push((name.clone(), portfolio_value, *shock));
    }

    // Pick best scenario (highest portfolio value)
    scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    let (best_name, best_score, best_shock) = scores.first().unwrap();

    println!("Best AI scenario: {best_name} with portfolio value {best_score} (shock={best_shock})");

    // Drop non-optimal branches
    let mut winning_branch = None;
    for (name, branch, _) in branches {
        if &name == best_name {
            winning_branch = Some(branch);
        } else {
            drop(branch); // Instantly cleans up memory with zero traces
        }
    }

    // Merge the winning scenario into main
    let win = winning_branch.unwrap();
    db.merge_branch(win, MergeStrategy::FastForward).unwrap();

    // Main database now reflects the winning scenario!
    let alpha_final = db.json().get("market", "asset_alpha").unwrap().unwrap().value["price"].as_f64().unwrap();
    assert!((alpha_final - (100.0 * best_shock)).abs() < 1e-4);
}

#[test]
fn test_merge_strategy_fast_forward_conflict_detection() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("conflict.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

    db.batch(&[BatchOperation::JsonPut {
        space: "config".into(),
        id: "timeout".into(),
        value: json!({ "seconds": 30 }),
        if_version: None,
    }])
    .unwrap();

    let branch = db.branch("divergent_branch").unwrap();
    branch
        .json()
        .put("config", "timeout", json!({ "seconds": 60 }))
        .unwrap();

    // Main concurrently modifies the same document!
    db.json()
        .put("config", "timeout", json!({ "seconds": 45 }))
        .unwrap();

    // FastForward merge MUST detect the conflict and reject the merge
    let res = db.merge_branch(branch, MergeStrategy::FastForward);
    assert!(res.is_err());
    let err = res.err().unwrap();
    assert_eq!(err.code, ErrorCode::Conflict);

    // Main timeout is still 45
    assert_eq!(
        db.json().get("config", "timeout").unwrap().unwrap().value["seconds"],
        45
    );
}

#[test]
fn test_merge_strategy_replace_overwrites_conflict() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("replace.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

    db.batch(&[BatchOperation::JsonPut {
        space: "users".into(),
        id: "u1".into(),
        value: json!({ "status": "pending" }),
        if_version: None,
    }])
    .unwrap();

    let branch = db.branch("force_branch").unwrap();
    branch
        .json()
        .put("users", "u1", json!({ "status": "active" }))
        .unwrap();

    // Main also changed
    db.json()
        .put("users", "u1", json!({ "status": "suspended" }))
        .unwrap();

    // Replace strategy forces branch version
    let res = db.merge_branch(branch, MergeStrategy::Replace);
    assert!(res.is_ok());

    assert_eq!(
        db.json().get("users", "u1").unwrap().unwrap().value["status"],
        "active"
    );
}

#[test]
fn test_in_memory_database_branching() {
    let db = Database::open(DatabaseOptions::in_memory()).unwrap();

    db.batch(&[BatchOperation::KvSet {
        namespace: "secrets".into(),
        key: b"token".to_vec(),
        value: b"xyz123".to_vec(),
        if_version: None,
        expires_at_ms: None,
    }])
    .unwrap();

    let branch = db.branch("mem_branch").unwrap();

    // Branch modifies KV
    branch
        .batch(&[BatchOperation::KvSet {
            namespace: "secrets".into(),
            key: b"token".to_vec(),
            value: b"new_token_456".to_vec(),
            if_version: None,
            expires_at_ms: None,
        }])
        .unwrap();

    // Main still has original
    assert_eq!(
        db.kv_get("secrets", b"token").unwrap().unwrap().value,
        b"xyz123"
    );

    // Merge branch into in-memory main
    db.merge_branch(branch, MergeStrategy::FastForward).unwrap();
    assert_eq!(
        db.kv_get("secrets", b"token").unwrap().unwrap().value,
        b"new_token_456"
    );
}
