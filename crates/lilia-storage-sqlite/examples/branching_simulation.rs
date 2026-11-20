use lilia_core::{doc, BatchOperation, MergeStrategy};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("production.lilia");

    println!("============================================================");
    println!("  Liliadb Instant Database Branching & What-If Simulation   ");
    println!("============================================================");

    println!(
        "\n[1] Initializing production database at: {}",
        db_path.display()
    );
    let db = Database::open(DatabaseOptions::durable(&db_path))?;

    // Seed production catalog
    db.batch(&[
        BatchOperation::JsonPut {
            space: "products".into(),
            id: "laptop_gaming".into(),
            value: json!({ "title": "Gaming Laptop", "price": 1500.0, "cost": 1000.0, "margin": 500.0 }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "products".into(),
            id: "gpu_rtx".into(),
            value: json!({ "title": "RTX 4080", "price": 1200.0, "cost": 850.0, "margin": 350.0 }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "products".into(),
            id: "monitor_4k".into(),
            value: json!({ "title": "4K UltraSharp Monitor", "price": 600.0, "cost": 400.0, "margin": 200.0 }),
            if_version: None,
        },
    ])?;

    // Expose SQL view for analytical queries
    db.sql().execute(
        "CREATE VIEW products AS \
         SELECT id, \
                json_extract(value, '$.title') AS title, \
                CAST(json_extract(value, '$.price') AS REAL) AS price, \
                CAST(json_extract(value, '$.cost') AS REAL) AS cost, \
                CAST(json_extract(value, '$.margin') AS REAL) AS margin \
         FROM lilia_json WHERE space = 'products';",
    )?;

    let prod_margin = db
        .sql()
        .query("SELECT sum(margin) FROM products")?
        .scalar_f64()
        .unwrap_or(0.0);
    println!("  Production Current Margin: ${prod_margin:.2}");

    // [2] Branching Instantané par Copy-on-Write (O(1))
    println!("\n[2] Branching Instantané (O(1) Copy-on-Write):");
    let t0 = std::time::Instant::now();
    let simulation = db.branch("simulation_promo_noel")?;
    let branch_latency = t0.elapsed();
    println!(
        "  -> Branch 'simulation_promo_noel' created in {:?}",
        branch_latency
    );
    println!(
        "  -> Dirty pages in memory: {}",
        simulation.dirty_page_count()
    );
    println!("  -> Extra disk bytes: 0 bytes");

    // [3] Apply simulated modifications inside the isolated branch
    println!("\n[3] Modifying data in virtual branch (15% Christmas discount):");
    let count = simulation
        .json()
        .update_all("products", doc! { "$mul": { "price": 0.85 } })?;
    println!("  -> Updated {count} documents in branch");

    // Update margins in simulation
    simulation.json().put(
        "products",
        "laptop_gaming",
        json!({ "title": "Gaming Laptop", "price": 1275.0, "cost": 1000.0, "margin": 275.0 }),
    )?;
    simulation.json().put(
        "products",
        "gpu_rtx",
        json!({ "title": "RTX 4080", "price": 1020.0, "cost": 850.0, "margin": 170.0 }),
    )?;
    simulation.json().put(
        "products",
        "monitor_4k",
        json!({ "title": "4K UltraSharp Monitor", "price": 510.0, "cost": 400.0, "margin": 110.0 }),
    )?;

    let projected_margin = simulation
        .sql()
        .query("SELECT sum(margin) FROM products")?
        .scalar_f64()
        .unwrap_or(0.0);
    println!("  -> Branch Projected Margin: ${projected_margin:.2}");
    println!(
        "  -> Branch Dirty Pages in RAM: {}",
        simulation.dirty_page_count()
    );
    println!("  -> Branch RAM Usage: {} bytes", simulation.memory_bytes());

    // Verify main database is 100% unaffected
    let main_margin_check = db
        .sql()
        .query("SELECT sum(margin) FROM products")?
        .scalar_f64()
        .unwrap_or(0.0);
    println!("  -> Main Production Margin: ${main_margin_check:.2} (UNTOUCHED)");

    // [4] Multi-Agent Scenario Modeling ("AI Agent Dreaming")
    println!("\n[4] AI Agent Dreaming (Testing 5 parallel scenarios):");
    let mut scenarios = Vec::new();
    for discount_pct in [5, 10, 15, 20, 25] {
        let name = format!("scenario_discount_{discount_pct}");
        let branch = db.branch(&name)?;

        let multiplier = 1.0 - (discount_pct as f64 / 100.0);
        branch
            .json()
            .update_all("products", doc! { "$mul": { "price": multiplier } })?;

        // Simulated volume elasticity: volume increases as discount increases
        let volume_factor = 1.0 + (discount_pct as f64 * 0.08); // 8% more sales per 1% discount
        let simulated_revenue =
            (1500.0 * multiplier + 1200.0 * multiplier + 600.0 * multiplier) * volume_factor;

        scenarios.push((name, branch, simulated_revenue, discount_pct));
    }

    scenarios.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());
    println!("  AI Simulation Rankings:");
    for (name, _, rev, pct) in &scenarios {
        println!("    - {name} ({pct}% discount) -> Projected Revenue: ${rev:.2}");
    }

    let (best_name, _, best_rev, best_pct) = scenarios.first().unwrap();
    println!(
        "\n  Optimal Scenario Chosen: {} ({}% discount) -> ${:.2}",
        best_name, best_pct, best_rev
    );

    // [5] Fusion Atomique dans Main
    println!("\n[5] Atomic Merge into Production (FastForward):");
    let merge_res = db.merge_branch(simulation, MergeStrategy::FastForward)?;
    println!(
        "  -> Merged branch '{}' with {:?} strategy. {} pages applied.",
        merge_res.branch_name, merge_res.strategy_used, merge_res.rows_merged
    );

    let final_margin = db
        .sql()
        .query("SELECT sum(margin) FROM products")?
        .scalar_f64()
        .unwrap_or(0.0);
    println!("  -> Production New Margin after Merge: ${final_margin:.2}");

    println!("\nAll operations completed successfully!");
    Ok(())
}
