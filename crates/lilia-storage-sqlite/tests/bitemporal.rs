use lilia_core::{
    parse_iso_to_ms, BatchOperation, BitemporalInterval, BitemporalPoint,
};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

#[test]
fn test_bitemporal_exact_insurance_contract_scenario() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("insurance_bitemporal.lilia"),
    ))
    .unwrap();

    // 1. On 2026-11-10 (T1): Insurance policy signed, covering 2027-01-01 to 2027-12-31
    // Initial premium amount: 1000
    let t1_signing = parse_iso_to_ms("2026-11-10T10:00:00Z").unwrap();
    let coverage_period =
        BitemporalInterval::inclusive_days("2027-01-01", "2027-12-31").unwrap();

    let res1 = db
        .batch(&[BatchOperation::BitemporalPut {
            space: "contracts".into(),
            id: "pol_881".into(),
            value: json!({
                "type": "Auto Insurance",
                "amount": 1000,
                "status": "signed",
                "agent": "Alice"
            }),
            vt_from: coverage_period.from,
            vt_to: coverage_period.to,
            system_time: Some(t1_signing),
        }])
        .unwrap();
    assert_eq!(res1[0].version, Some(1));

    // 2. On 2026-12-15 (T2): Retroactive correction to rectify amount to 1050
    let t2_amendment = parse_iso_to_ms("2026-12-15T14:30:00Z").unwrap();
    let res2 = db
        .batch(&[BatchOperation::BitemporalPut {
            space: "contracts".into(),
            id: "pol_881".into(),
            value: json!({
                "type": "Auto Insurance",
                "amount": 1050,
                "status": "rectified",
                "agent": "Bob"
            }),
            vt_from: coverage_period.from,
            vt_to: coverage_period.to,
            system_time: Some(t2_amendment),
        }])
        .unwrap();
    assert_eq!(res2[0].version, Some(2));

    // =========================================================================
    // 3. Declarative SQL Query (Exact query requested in prompt):
    // "Quel était le statut du contrat pour la date du 1er février 2027,
    //  tel que nous le connaissions le 20 novembre 2026 ?"
    // =========================================================================
    let sql_as_of_nov20 = "SELECT id, json_extract(value, '$.amount') AS amount, \
                                  json_extract(value, '$.status') AS status \
                           FROM contracts \
                           AS OF TRANSACTION '2026-11-20' \
                           FOR VALID_TIME '2027-02-01' \
                           WHERE id = 'pol_881';";

    let rows_nov20 = db.sql_query(sql_as_of_nov20, &[]).unwrap();
    assert_eq!(rows_nov20.rows.len(), 1, "Must find exactly 1 contract record as known on 2026-11-20");
    assert_eq!(rows_nov20.rows[0][0], json!("pol_881"));
    assert_eq!(rows_nov20.rows[0][1], json!(1000), "Amount known on 2026-11-20 must be initial 1000");
    assert_eq!(rows_nov20.rows[0][2], json!("signed"));

    // 4. Query after the retroactive correction (e.g. as of 2026-12-20):
    let sql_as_of_dec20 = "SELECT id, json_extract(value, '$.amount') AS amount, \
                                  json_extract(value, '$.status') AS status \
                           FROM contracts \
                           AS OF TRANSACTION '2026-12-20' \
                           FOR VALID_TIME '2027-02-01' \
                           WHERE id = 'pol_881';";

    let rows_dec20 = db.sql_query(sql_as_of_dec20, &[]).unwrap();
    assert_eq!(rows_dec20.rows.len(), 1);
    assert_eq!(rows_dec20.rows[0][0], json!("pol_881"));
    assert_eq!(rows_dec20.rows[0][1], json!(1050), "Amount known on 2026-12-20 must be rectified 1050");
    assert_eq!(rows_dec20.rows[0][2], json!("rectified"));

    // 5. Test with SELECT * (returns bitemporal intervals)
    let sql_star = "SELECT * FROM contracts \
                    AS OF TRANSACTION '2026-11-20' \
                    FOR VALID_TIME '2027-02-01' \
                    WHERE id = 'pol_881';";
    let star_res = db.sql_query(sql_star, &[]).unwrap();
    assert_eq!(star_res.rows.len(), 1);
    assert_eq!(star_res.columns, vec!["id", "value", "version", "vt_from", "vt_to", "tt_from", "tt_to"]);

    // 6. Test with global view: FROM lilia_bitemporal AS OF ...
    let sql_global = "SELECT space, id, json_extract(value, '$.amount') AS amount \
                      FROM lilia_bitemporal \
                      AS OF TRANSACTION '2026-11-20' \
                      FOR VALID_TIME '2027-02-01' \
                      WHERE space = 'contracts' AND id = 'pol_881';";
    let global_res = db.sql_query(sql_global, &[]).unwrap();
    assert_eq!(global_res.rows.len(), 1);
    assert_eq!(global_res.rows[0][0], json!("contracts"));
    assert_eq!(global_res.rows[0][1], json!("pol_881"));
    assert_eq!(global_res.rows[0][2], json!(1000));

    // 7. Verify via Rust Fluent API: db.bitemporal().get(...)
    let get_nov20 = db
        .bitemporal()
        .get(
            "contracts",
            "pol_881",
            BitemporalPoint::at("2026-11-20").unwrap(),
            BitemporalPoint::at("2027-02-01").unwrap(),
        )
        .unwrap()
        .expect("must exist on 2026-11-20");
    assert_eq!(get_nov20.value["amount"], 1000);
    assert_eq!(get_nov20.version, 1);

    let get_dec20 = db
        .bitemporal()
        .get(
            "contracts",
            "pol_881",
            BitemporalPoint::at("2026-12-20").unwrap(),
            BitemporalPoint::at("2027-02-01").unwrap(),
        )
        .unwrap()
        .expect("must exist on 2026-12-20");
    assert_eq!(get_dec20.value["amount"], 1050);
    assert_eq!(get_dec20.version, 2);
}

#[test]
fn test_bitemporal_interval_slicing_and_carveouts() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("slicing_bitemporal.lilia"),
    ))
    .unwrap();

    // T1 (2026-11-10): Full year contract for 2027 with base premium 1000
    let t1 = parse_iso_to_ms("2026-11-10T12:00:00Z").unwrap();
    let full_year = BitemporalInterval::inclusive_days("2027-01-01", "2027-12-31").unwrap();
    db.bitemporal()
        .put_with_system_time(
            "policies",
            "pol_999",
            json!({ "rate": 1000, "tier": "standard" }),
            full_year,
            Some(t1),
        )
        .unwrap();

    // T2 (2026-12-01): Carve out Q3 only (2027-07-01 to 2027-10-01) with summer surcharge rate 1400
    let t2 = parse_iso_to_ms("2026-12-01T12:00:00Z").unwrap();
    let q3 = BitemporalInterval::inclusive_days("2027-07-01", "2027-09-30").unwrap();
    db.bitemporal()
        .put_with_system_time(
            "policies",
            "pol_999",
            json!({ "rate": 1400, "tier": "summer_peak" }),
            q3,
            Some(t2),
        )
        .unwrap();

    // Now inspect the timeline as of T2: should have 3 non-overlapping slices!
    // Slice 1: [2027-01-01, 2027-07-01) -> rate: 1000
    // Slice 2: [2027-07-01, 2027-10-01) -> rate: 1400
    // Slice 3: [2027-10-01, 2028-01-01) -> rate: 1000
    let timeline_t2 = db
        .bitemporal()
        .timeline("policies", "pol_999", BitemporalPoint::from_ms(t2 + 1000))
        .unwrap();

    assert_eq!(timeline_t2.len(), 3, "Timeline must be split into prefix, middle, suffix slices");
    assert_eq!(timeline_t2[0].value["rate"], 1000);
    assert_eq!(timeline_t2[1].value["rate"], 1400);
    assert_eq!(timeline_t2[2].value["rate"], 1000);

    // SQL queries at various valid times as of 2026-12-05:
    let q_may = db
        .sql_query(
            "SELECT json_extract(value, '$.rate') FROM policies \
             AS OF TRANSACTION '2026-12-05' FOR VALID_TIME '2027-05-01' WHERE id = 'pol_999';",
            &[],
        )
        .unwrap();
    assert_eq!(q_may.rows[0][0], json!(1000));

    let q_august = db
        .sql_query(
            "SELECT json_extract(value, '$.rate') FROM policies \
             AS OF TRANSACTION '2026-12-05' FOR VALID_TIME '2027-08-15' WHERE id = 'pol_999';",
            &[],
        )
        .unwrap();
    assert_eq!(q_august.rows[0][0], json!(1400));

    let q_november = db
        .sql_query(
            "SELECT json_extract(value, '$.rate') FROM policies \
             AS OF TRANSACTION '2026-12-05' FOR VALID_TIME '2027-11-01' WHERE id = 'pol_999';",
            &[],
        )
        .unwrap();
    assert_eq!(q_november.rows[0][0], json!(1000));

    // But as of 2026-11-20 (before the carve-out was introduced):
    // August had rate 1000!
    let q_august_past = db
        .sql_query(
            "SELECT json_extract(value, '$.rate') FROM policies \
             AS OF TRANSACTION '2026-11-20' FOR VALID_TIME '2027-08-15' WHERE id = 'pol_999';",
            &[],
        )
        .unwrap();
    assert_eq!(q_august_past.rows[0][0], json!(1000));
}

#[test]
fn test_bitemporal_coalescing_and_range_queries() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("coalesce_bitemporal.lilia"),
    ))
    .unwrap();

    let t1 = parse_iso_to_ms("2026-11-01").unwrap();

    // Insert first semester
    let h1 = BitemporalInterval::between("2027-01-01", "2027-07-01").unwrap();
    db.bitemporal()
        .put_with_system_time("tariffs", "power", json!({ "kwh": 0.25 }), h1, Some(t1))
        .unwrap();

    // Insert second semester with identical rate
    let h2 = BitemporalInterval::between("2027-07-01", "2028-01-01").unwrap();
    db.bitemporal()
        .put_with_system_time("tariffs", "power", json!({ "kwh": 0.25 }), h2, Some(t1 + 1000))
        .unwrap();

    // Thanks to temporal coalescing, adjacent slices with identical values are merged!
    let timeline = db
        .bitemporal()
        .timeline("tariffs", "power", BitemporalPoint::from_ms(t1 + 5000))
        .unwrap();
    assert_eq!(timeline.len(), 1, "Adjacent identical slices must coalesce into one");
    assert_eq!(timeline[0].vt_from, h1.from);
    assert_eq!(timeline[0].vt_to, h2.to);

    // Range query using SQL FOR VALID_TIME BETWEEN
    let range_res = db
        .sql_query(
            "SELECT id, json_extract(value, '$.kwh') AS kwh FROM tariffs \
             AS OF TRANSACTION '2026-11-15' \
             FOR VALID_TIME BETWEEN '2027-02-01' AND '2027-09-01' \
             WHERE id = 'power';",
            &[],
        )
        .unwrap();
    assert_eq!(range_res.rows.len(), 1);
    assert_eq!(range_res.rows[0][1], json!(0.25));
}

#[test]
fn test_bitemporal_valid_time_retraction_and_full_history() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("retract_bitemporal.lilia"),
    ))
    .unwrap();

    let t1 = parse_iso_to_ms("2026-11-01").unwrap();
    let full = BitemporalInterval::between("2027-01-01", "2028-01-01").unwrap();
    db.bitemporal()
        .put_with_system_time("memberships", "vip_1", json!({ "active": true }), full, Some(t1))
        .unwrap();

    // On 2026-12-01 (T2): Cancel membership early for second half of 2027 (retraction)
    let t2 = parse_iso_to_ms("2026-12-01").unwrap();
    let cancelled_period = BitemporalInterval::between("2027-07-01", "2028-01-01").unwrap();
    db.batch(&[BatchOperation::BitemporalDelete {
        space: "memberships".into(),
        id: "vip_1".into(),
        vt_from: Some(cancelled_period.from),
        vt_to: Some(cancelled_period.to),
        system_time: Some(t2),
    }])
    .unwrap();

    // Query valid time at 2027-08-01 as of T2 + 1 day: should return NONE (cancelled)
    let q_cancelled = db
        .sql_query(
            "SELECT id FROM memberships \
             AS OF TRANSACTION '2026-12-02' FOR VALID_TIME '2027-08-01' WHERE id = 'vip_1';",
            &[],
        )
        .unwrap();
    assert_eq!(q_cancelled.rows.len(), 0);

    // But as of 2026-11-15 (T1.5): it was still active!
    let q_past = db
        .sql_query(
            "SELECT id FROM memberships \
             AS OF TRANSACTION '2026-11-15' FOR VALID_TIME '2027-08-01' WHERE id = 'vip_1';",
            &[],
        )
        .unwrap();
    assert_eq!(q_past.rows.len(), 1);

    // Full 2D audit history includes all revisions
    let hist = db.bitemporal().history("memberships", "vip_1").unwrap();
    assert!(!hist.is_empty());
}
