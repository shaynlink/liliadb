use std::path::Path;

use lilia_core::energy::{DeviceEnergyProfile, EnergyDiagnosis};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::Value;

fn setup_db(dir: &Path, profile: DeviceEnergyProfile) -> Database {
    let path = dir.join("battery_aware.lilia");
    let opts = DatabaseOptions::mobile(path).with_energy_profile(profile);
    Database::open(opts).unwrap()
}

fn seed_logs(db: &Database, count: usize) {
    db.sql_query(
        "CREATE TABLE logs(id INTEGER PRIMARY KEY, level TEXT, msg TEXT);",
        &[],
    )
    .unwrap();

    let mut writer = db.lock_writer().unwrap();
    let tx = writer.transaction().unwrap();
    {
        let mut stmt = tx
            .prepare("INSERT INTO logs(id, level, msg) VALUES(?1, ?2, ?3);")
            .unwrap();

        for i in 1..=count {
            let level = if i % 25 == 0 {
                "ERROR"
            } else if i % 5 == 0 {
                "WARN"
            } else {
                "INFO"
            };
            stmt.bind_int64(1, i as i64).unwrap();
            stmt.bind_text_transient(2, level).unwrap();
            stmt.bind_text_transient(3, &format!("Event log message number {i}"))
                .unwrap();
            stmt.step().unwrap();
            stmt.reset().unwrap();
        }
    }
    tx.commit().unwrap();
}

#[test]
fn test_battery_aware_profiling_and_pedagogical_recommendation() {
    let dir = tempfile::tempdir().unwrap();
    let db = setup_db(dir.path(), DeviceEnergyProfile::Smartphone);
    seed_logs(&db, 1_000);

    // 1. Diagnose unindexed query: SELECT * FROM logs WHERE level = 'ERROR'
    let query = "SELECT * FROM logs WHERE level = 'ERROR'";
    let diag = db.explain_diagnose(query, &[]).unwrap();

    assert_eq!(diag.query, query);
    assert!(diag.execution_time_us > 0);
    assert!(diag.estimated_energy_uj > 0.0);
    assert_eq!(diag.diagnosis, EnergyDiagnosis::FullTableScanDetected);

    let rec = diag
        .recommendation
        .as_ref()
        .expect("recommendation should exist");
    assert!(
        rec.contains("Créer l'index partiel idx_logs_error"),
        "Recommendation was: {rec}"
    );
    assert!(
        rec.contains("réduirait la consommation énergétique de"),
        "Recommendation was: {rec}"
    );

    // Validate predicted savings is high (typically ~96%)
    let savings = diag.energy_savings_percent.unwrap_or(0.0);
    assert!(
        savings >= 80.0 && savings <= 99.0,
        "Expected high predicted savings, got {savings}%"
    );

    // 2. Test SQL query syntax: EXPLAIN DIAGNOSE <query>
    let sql_res = db
        .sql_query(
            "EXPLAIN DIAGNOSE SELECT * FROM logs WHERE level = 'ERROR'",
            &[],
        )
        .unwrap();
    assert_eq!(
        sql_res.columns,
        vec![
            "query",
            "execution_time_us",
            "estimated_energy_uj",
            "diagnosis",
            "recommendation"
        ]
    );
    assert_eq!(sql_res.rows.len(), 1);
    let row = &sql_res.rows[0];
    assert_eq!(row[0], Value::String(query.to_string()));
    assert!(row[1].as_u64().unwrap() > 0);
    assert!(row[2].as_f64().unwrap() > 0.0);
    assert_eq!(
        row[3],
        Value::String("FULL_TABLE_SCAN_DETECTED".to_string())
    );
    let sql_rec = row[4].as_str().unwrap();
    assert!(sql_rec.contains("Créer l'index partiel idx_logs_error"));

    // 3. Apply the recommended partial index
    db.sql_query(
        "CREATE INDEX idx_logs_error ON logs(level) WHERE level = 'ERROR';",
        &[],
    )
    .unwrap();

    // 4. Re-diagnose the query: should now use the optimal partial index!
    let diag_optimized = db.explain_diagnose(query, &[]).unwrap();
    assert_eq!(diag_optimized.diagnosis, EnergyDiagnosis::OptimalIndexUsed);
    let opt_rec = diag_optimized.recommendation.as_ref().unwrap();
    assert!(
        opt_rec.contains("Requête optimisée"),
        "Optimized recommendation was: {opt_rec}"
    );

    // CPU VM steps should be massively reduced (from ~10,000 steps down to ~300 steps)
    let vm_unindexed = diag.metrics.unwrap().cpu_vm_steps;
    let vm_optimized = diag_optimized.metrics.unwrap().cpu_vm_steps;
    assert!(
        vm_optimized < vm_unindexed / 5,
        "Optimized VM steps ({vm_optimized}) should be much lower than unindexed ({vm_unindexed})"
    );

    let cpu_e_unindexed = diag.metrics.unwrap().cpu_energy_uj;
    let cpu_e_optimized = diag_optimized.metrics.unwrap().cpu_energy_uj;
    assert!(
        cpu_e_optimized < cpu_e_unindexed,
        "Optimized CPU energy ({cpu_e_optimized} µJ) should be < unindexed ({cpu_e_unindexed} µJ)"
    );

    // Predicted energy should be dramatically lower than unindexed scan
    let pred_energy = diag.predicted_energy_uj.unwrap();
    assert!(
        pred_energy < diag.estimated_energy_uj,
        "Predicted energy ({pred_energy} µJ) should be < unindexed scan energy ({} µJ)",
        diag.estimated_energy_uj
    );
}

#[test]
fn test_temporary_b_tree_sort_diagnostic() {
    let dir = tempfile::tempdir().unwrap();
    let db = setup_db(dir.path(), DeviceEnergyProfile::Smartphone);
    seed_logs(&db, 200);

    let query = "SELECT * FROM logs ORDER BY msg;";
    let diag = db.explain_diagnose(query, &[]).unwrap();

    // Sorting without index on msg requires temporary B-tree
    assert!(
        diag.diagnosis == EnergyDiagnosis::TemporaryBTreeSort
            || diag.diagnosis == EnergyDiagnosis::FullTableScanDetected
    );

    if let Some(ref rec) = diag.recommendation {
        assert!(rec.contains("idx_logs_") || rec.contains("index"));
    }
}

#[test]
fn test_smartwatch_profile_penalty_calibration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("watch_db.lilia");

    // Constrained profile defaults to Smartwatch energy profile
    let db = Database::open(DatabaseOptions::constrained(path)).unwrap();
    assert_eq!(db.energy_profile(), DeviceEnergyProfile::Smartwatch);

    seed_logs(&db, 500);

    let diag_watch = db
        .explain_diagnose("SELECT * FROM logs WHERE level = 'ERROR'", &[])
        .unwrap();

    // Switch profile to Smartphone
    db.set_energy_profile(DeviceEnergyProfile::Smartphone);
    assert_eq!(db.energy_profile(), DeviceEnergyProfile::Smartphone);

    let diag_phone = db
        .explain_diagnose("SELECT * FROM logs WHERE level = 'ERROR'", &[])
        .unwrap();

    assert!(diag_watch.estimated_energy_uj > 0.0);
    assert!(diag_phone.estimated_energy_uj > 0.0);
}

#[test]
fn test_profile_sql_method_returns_data_and_diagnostic() {
    let dir = tempfile::tempdir().unwrap();
    let db = setup_db(dir.path(), DeviceEnergyProfile::Smartphone);
    seed_logs(&db, 100);

    let (result, diag) = db
        .profile_sql("SELECT count(*) FROM logs WHERE level = 'ERROR'", &[])
        .unwrap();

    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0][0], Value::Number(4.into())); // 100 / 25 = 4
    assert!(diag.execution_time_us > 0);
    assert!(diag.estimated_energy_uj > 0.0);
}

#[test]
fn test_lilia_ql_explain_diagnose() {
    let dir = tempfile::tempdir().unwrap();
    let db = setup_db(dir.path(), DeviceEnergyProfile::Smartphone);

    let diag = db
        .query()
        .from_json("telemetry")
        .filter(serde_json::json!({ "battery_level": "critical" }))
        .explain_diagnose()
        .unwrap();

    assert!(diag.estimated_energy_uj > 0.0);
    assert!(diag.recommendation.is_some());
}
