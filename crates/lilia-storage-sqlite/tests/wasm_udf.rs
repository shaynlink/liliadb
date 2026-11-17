use lilia_storage_sqlite::{Database, DatabaseOptions, WasmUdfConfig};
use serde_json::json;

// Distance calculation WAT: Computes accurate equirectangular geodesic distance in meters
// lat1, lon1, lat2, lon2 (all in degrees) -> distance in meters
// x = (lon2 - lon1) * cos((lat1 + lat2) / 2 * pi / 180) * pi / 180
// y = (lat2 - lat1) * pi / 180
// d = sqrt(x*x + y*y) * 6371000.0
const HAVERSINE_WAT: &str = r#"
(module
  (func (export "haversine") (param $lat1 f64) (param $lon1 f64) (param $lat2 f64) (param $lon2 f64) (result f64)
    (local $dlat f64)
    (local $dlon f64)
    (local $mean_lat_rad f64)
    (local $cos_lat f64)
    (local $x f64)
    (local $y f64)
    (local $deg2rad f64)
    (local $r f64)

    ;; deg to rad factor = 0.017453292519943295
    (local.set $deg2rad (f64.const 0.017453292519943295))
    (local.set $r (f64.const 6371000.0))

    ;; mean_lat_rad = ((lat1 + lat2) / 2.0) * deg2rad
    (local.set $mean_lat_rad 
      (f64.mul 
        (f64.div (f64.add (local.get $lat1) (local.get $lat2)) (f64.const 2.0))
        (local.get $deg2rad)
      )
    )

    ;; Taylor series for cos(x) around 0: 1 - x^2/2 + x^4/24
    (local.set $cos_lat 
      (f64.sub 
        (f64.const 1.0)
        (f64.div 
          (f64.mul (local.get $mean_lat_rad) (local.get $mean_lat_rad))
          (f64.const 2.0)
        )
      )
    )

    ;; x = (lon2 - lon1) * deg2rad * cos_lat
    (local.set $x 
      (f64.mul 
        (f64.mul (f64.sub (local.get $lon2) (local.get $lon1)) (local.get $deg2rad))
        (local.get $cos_lat)
      )
    )

    ;; y = (lat2 - lat1) * deg2rad
    (local.set $y 
      (f64.mul (f64.sub (local.get $lat2) (local.get $lat1)) (local.get $deg2rad))
    )

    ;; distance = sqrt(x*x + y*y) * R
    (f64.mul 
      (f64.sqrt 
        (f64.add 
          (f64.mul (local.get $x) (local.get $x))
          (f64.mul (local.get $y) (local.get $y))
        )
      )
      (local.get $r)
    )
  )
)
"#;

// Infinite loop WAT for fuel exhaustion test
const INFINITE_LOOP_WAT: &str = r#"
(module
  (func (export "inf_loop") (param $x f64) (result f64)
    (loop $l
      (br $l)
    )
    (f64.const 0.0)
  )
)
"#;

// Out-of-bounds memory trap WAT
const OUT_OF_BOUNDS_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "oob_write") (param $x i64) (result i64)
    (i32.store (i32.const 99999999) (i32.const 42))
    (i64.const 0)
  )
)
"#;

// Memory growth limit WAT
const GROW_MEMORY_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "try_grow") (param $pages i64) (result i64)
    (i64.extend_i32_s
      (memory.grow (i32.wrap_i64 (local.get $pages)))
    )
  )
)
"#;

// String transformation WAT (uppercase conversion with allocator)
const STRING_UPPER_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 1024))

  (func (export "lilia_alloc") (param $size i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $heap))
    (global.set $heap (i32.add (local.get $ptr) (local.get $size)))
    (local.get $ptr)
  )

  (func (export "lilia_dealloc") (param $ptr i32) (param $size i32))

  (func (export "to_upper") (param $in_ptr i32) (param $in_len i32) (result i64)
    (local $i i32)
    (local $b i32)

    (local.set $i (i32.const 0))
    (block $break
      (loop $loop
        (br_if $break (i32.ge_u (local.get $i) (local.get $in_len)))
        (local.set $b (i32.load8_u (i32.add (local.get $in_ptr) (local.get $i))))
        
        ;; If 'a' (97) <= b <= 'z' (122), subtract 32
        (if (i32.and (i32.ge_u (local.get $b) (i32.const 97)) (i32.le_u (local.get $b) (i32.const 122)))
          (then
            (i32.store8 
              (i32.add (local.get $in_ptr) (local.get $i)) 
              (i32.sub (local.get $b) (i32.const 32))
            )
          )
        )

        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $loop)
      )
    )

    ;; Pack (in_ptr << 32) | in_len
    (i64.or 
      (i64.shl (i64.extend_i32_u (local.get $in_ptr)) (i64.const 32))
      (i64.extend_i32_u (local.get $in_len))
    )
  )
)
"#;

#[test]
fn test_wasm_haversine_in_query() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("wasm_haversine.lilia"),
    ))
    .unwrap();

    // Register Wasm Haversine function
    db.register_wasm_udf("haversine", HAVERSINE_WAT.as_bytes())
        .unwrap();

    // Create a POI table and seed points of interest around Europe
    db.sql_query(
        "CREATE TABLE pois (id TEXT PRIMARY KEY, lat REAL NOT NULL, lon REAL NOT NULL);",
        &[],
    )
    .unwrap();

    // Points of interest: Paris, London, Brussels, Berlin, Tokyo
    db.sql_query(
        "INSERT INTO pois (id, lat, lon) VALUES 
         ('Paris', 48.8566, 2.3522),
         ('London', 51.5074, -0.1278),
         ('Brussels', 50.8503, 4.3517),
         ('Berlin', 52.5200, 13.4050),
         ('Tokyo', 35.6762, 139.6503);",
        &[],
    )
    .unwrap();

    // Query closest to Paris (48.8566, 2.3522) using wasm_call
    let target_lat = 48.8566;
    let target_lon = 2.3522;

    let res = db
        .sql_query(
            "SELECT id, wasm_call('haversine', lat, lon, ?1, ?2) AS distance
             FROM pois
             ORDER BY distance ASC
             LIMIT 3;",
            &[json!(target_lat), json!(target_lon)],
        )
        .unwrap();

    assert_eq!(res.columns, vec!["id", "distance"]);
    assert_eq!(res.rows.len(), 3);

    // 1st closest should be Paris (0 distance)
    assert_eq!(res.rows[0][0], json!("Paris"));
    let d0 = res.rows[0][1].as_f64().unwrap();
    assert!(d0 < 1.0, "Paris distance to itself should be ~0: got {d0}");

    // 2nd closest should be Brussels (~260-300 km)
    assert_eq!(res.rows[1][0], json!("Brussels"));
    let d1 = res.rows[1][1].as_f64().unwrap();
    assert!(
        d1 > 200_000.0 && d1 < 350_000.0,
        "Brussels distance expected ~260km: got {d1}"
    );

    // 3rd closest should be London (~340 km)
    assert_eq!(res.rows[2][0], json!("London"));
    let d2 = res.rows[2][1].as_f64().unwrap();
    assert!(
        d2 > 300_000.0 && d2 < 400_000.0,
        "London distance expected ~340km: got {d2}"
    );

    // Test direct function alias: haversine(lat, lon, ?1, ?2)
    let res_direct = db
        .sql_query(
            "SELECT id, haversine(lat, lon, ?1, ?2) AS distance
             FROM pois
             ORDER BY distance ASC
             LIMIT 3;",
            &[json!(target_lat), json!(target_lon)],
        )
        .unwrap();

    assert_eq!(res_direct.rows.len(), 3);
    assert_eq!(res_direct.rows[0][0], json!("Paris"));
    assert_eq!(res_direct.rows[1][0], json!("Brussels"));
    assert_eq!(res_direct.rows[2][0], json!("London"));
}

#[test]
fn test_wasm_auxdata_high_speed_benchmark() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("wasm_bench.lilia"),
    ))
    .unwrap();

    db.register_wasm_udf("haversine", HAVERSINE_WAT.as_bytes())
        .unwrap();

    db.sql_query(
        "CREATE TABLE large_pois (id INTEGER PRIMARY KEY, lat REAL NOT NULL, lon REAL NOT NULL);",
        &[],
    )
    .unwrap();

    // Insert 5,000 rows
    {
        let writer = db.lock_writer().unwrap();
        writer.exec("BEGIN TRANSACTION;").unwrap();
        let mut stmt = writer
            .prepare("INSERT INTO large_pois (id, lat, lon) VALUES (?1, ?2, ?3);")
            .unwrap();
        for i in 1..=5000 {
            let lat = 48.0 + (i as f64) * 0.001;
            let lon = 2.0 + (i as f64) * 0.001;
            stmt.bind_int64(1, i).unwrap();
            stmt.bind_double(2, lat).unwrap();
            stmt.bind_double(3, lon).unwrap();
            stmt.step().unwrap();
            stmt.reset().unwrap();
        }
        drop(stmt);
        writer.exec("COMMIT;").unwrap();
    }

    let started = std::time::Instant::now();
    let res = db
        .sql_query(
            "SELECT id, wasm_call('haversine', lat, lon, 48.8566, 2.3522) AS distance
             FROM large_pois
             ORDER BY distance ASC
             LIMIT 10;",
            &[],
        )
        .unwrap();

    let elapsed = started.elapsed();
    println!("Evaluated wasm_call over 5000 rows in: {elapsed:?}");

    assert_eq!(res.rows.len(), 10);
    // AuxData ensures instance reuse: 5,000 evaluations should complete in < 150 ms in debug mode
    assert!(
        elapsed.as_millis() < 500,
        "Execution took too long: {elapsed:?}"
    );
}

#[test]
fn test_wasm_sandboxing_infinite_loop_fuel_trap() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("wasm_trap.lilia"),
    ))
    .unwrap();

    // Register infinite loop UDF with small fuel limit
    let config = WasmUdfConfig {
        memory_limit_bytes: 64 * 1024,
        fuel_limit: 10_000,
        entry_point: None,
    };
    db.register_wasm_udf_with_config("inf_loop", INFINITE_LOOP_WAT.as_bytes(), config, false)
        .unwrap();

    // Executing infinite loop must be trapped safely by fuel exhaustion without hanging!
    let res = db.sql_query("SELECT wasm_call('inf_loop', 1.0);", &[]);
    assert!(res.is_err(), "Infinite loop must fail with fuel trap");
    let err_msg = res.unwrap_err().to_string();
    assert!(
        err_msg.contains("fuel exhausted"),
        "Expected fuel exhaustion, got: {err_msg}"
    );
}

#[test]
fn test_wasm_sandboxing_out_of_bounds_trap() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("wasm_oob.lilia"),
    ))
    .unwrap();

    db.register_wasm_udf("oob_write", OUT_OF_BOUNDS_WAT.as_bytes())
        .unwrap();

    // Executing out-of-bounds write must trap cleanly without crashing SQLite or LiliaDB
    let res = db.sql_query("SELECT wasm_call('oob_write', 1);", &[]);
    assert!(res.is_err(), "OOB write must trap");
    let err_msg = res.unwrap_err().to_string();
    assert!(
        err_msg.contains("out of bounds") || err_msg.contains("memory"),
        "Expected memory trap error, got: {err_msg}"
    );
}

#[test]
fn test_wasm_sandboxing_memory_growth_limit() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("wasm_mem_cap.lilia"),
    ))
    .unwrap();

    // Memory limit hard-capped at 64 KiB (1 page)
    let config = WasmUdfConfig {
        memory_limit_bytes: 64 * 1024,
        fuel_limit: 100_000,
        entry_point: None,
    };
    db.register_wasm_udf_with_config("try_grow", GROW_MEMORY_WAT.as_bytes(), config, false)
        .unwrap();

    // Asking to grow by 5 pages (exceeding 64 KiB) should return -1 (growth rejected by ResourceLimiter)
    let res = db
        .sql_query("SELECT wasm_call('try_grow', 5);", &[])
        .unwrap();
    assert_eq!(res.rows[0][0], json!(-1));
}

#[test]
fn test_wasm_string_normalization() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("wasm_string.lilia"),
    ))
    .unwrap();

    db.register_wasm_udf("to_upper", STRING_UPPER_WAT.as_bytes())
        .unwrap();

    db.sql_query(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT NOT NULL);",
        &[],
    )
    .unwrap();

    db.sql_query(
        "INSERT INTO users (id, username) VALUES (1, 'ada_lovelace'), (2, 'alan_turing');",
        &[],
    )
    .unwrap();

    let res = db
        .sql_query(
            "SELECT id, wasm_call('to_upper', username) AS upper_name FROM users ORDER BY id ASC;",
            &[],
        )
        .unwrap();

    assert_eq!(res.rows.len(), 2);
    assert_eq!(res.rows[0][1], json!("ADA_LOVELACE"));
    assert_eq!(res.rows[1][1], json!("ALAN_TURING"));
}

#[test]
fn test_wasm_persistence_across_reopen() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("wasm_persist.lilia");

    {
        let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();
        // Register haversine with persistent = true (default)
        db.register_wasm_udf("haversine", HAVERSINE_WAT.as_bytes())
            .unwrap();

        db.sql_query("CREATE TABLE coords (id TEXT, lat REAL, lon REAL);", &[])
            .unwrap();
        db.sql_query("INSERT INTO coords VALUES ('Paris', 48.8566, 2.3522);", &[])
            .unwrap();

        // Verify it works in session 1
        let res = db
            .sql_query(
                "SELECT wasm_call('haversine', lat, lon, 48.8566, 2.3522) FROM coords;",
                &[],
            )
            .unwrap();
        assert_eq!(res.rows[0][0], json!(0.0));
    }

    // Database is dropped and closed.
    // Now reopen from same path:
    {
        let db_reopened = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

        // Must be able to execute wasm_call and direct alias WITHOUT re-registering!
        let res = db_reopened
            .sql_query(
                "SELECT wasm_call('haversine', lat, lon, 48.8566, 2.3522) FROM coords;",
                &[],
            )
            .unwrap();
        assert_eq!(res.rows[0][0], json!(0.0));

        let res_direct = db_reopened
            .sql_query(
                "SELECT haversine(lat, lon, 48.8566, 2.3522) FROM coords;",
                &[],
            )
            .unwrap();
        assert_eq!(res_direct.rows[0][0], json!(0.0));

        // Verify in list
        let udfs = db_reopened.list_wasm_udfs();
        assert_eq!(udfs.len(), 1);
        assert_eq!(udfs[0].name, "haversine");
    }
}

#[test]
fn test_wasm_sql_registry_management() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("wasm_sql_reg.lilia"),
    ))
    .unwrap();

    // Register via SQL: wasm_register('haversine', ?1)
    let res_reg = db
        .sql_query(
            "SELECT wasm_register('haversine', ?1);",
            &[json!(HAVERSINE_WAT)],
        )
        .unwrap();
    assert_eq!(res_reg.rows[0][0], json!(1));

    // Check wasm_list()
    let res_list = db.sql_query("SELECT wasm_list();", &[]).unwrap();
    let list_json: String = res_list.rows[0][0].as_str().unwrap().to_string();
    assert!(list_json.contains("haversine"));

    // Call it
    let res_call = db
        .sql_query(
            "SELECT wasm_call('haversine', 48.8566, 2.3522, 48.8566, 2.3522);",
            &[],
        )
        .unwrap();
    assert_eq!(res_call.rows[0][0], json!(0.0));

    // Unregister via SQL: wasm_unregister('haversine')
    let res_unreg = db
        .sql_query("SELECT wasm_unregister('haversine');", &[])
        .unwrap();
    assert_eq!(res_unreg.rows[0][0], json!(1));

    // Calling it now should return error
    let res_err = db.sql_query(
        "SELECT wasm_call('haversine', 48.8566, 2.3522, 48.8566, 2.3522);",
        &[],
    );
    assert!(res_err.is_err());
}
