use lilia_core::{BatchOperation, Result as LiliaResult, ValueCodec};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;
use std::sync::Arc;

#[derive(Debug)]
struct TestXorCodec {
    key: u8,
}

impl ValueCodec for TestXorCodec {
    fn name(&self) -> &'static str {
        "test-xor"
    }

    fn encode<'a>(&self, plaintext: &'a [u8]) -> LiliaResult<std::borrow::Cow<'a, [u8]>> {
        Ok(std::borrow::Cow::Owned(
            plaintext.iter().map(|b| b ^ self.key).collect(),
        ))
    }

    fn decode(&self, ciphertext: &[u8]) -> LiliaResult<Vec<u8>> {
        Ok(ciphertext.iter().map(|b| b ^ self.key).collect())
    }
}

#[test]
fn test_sql_basic_queries_and_json_extract() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("sql_test.lilia"),
    ))
    .unwrap();

    // Seed KV and JSON data
    db.batch(&[
        BatchOperation::KvSet {
            namespace: "settings".into(),
            key: b"theme".to_vec(),
            value: b"dark".to_vec(),
            if_version: None,
            expires_at_ms: None,
        },
        BatchOperation::KvSet {
            namespace: "settings".into(),
            key: b"lang".to_vec(),
            value: b"fr".to_vec(),
            if_version: None,
            expires_at_ms: None,
        },
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "user_1".into(),
            value: json!({ "name": "Ada", "role": "admin", "age": 36 }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "user_2".into(),
            value: json!({ "name": "Alan", "role": "dev", "age": 41 }),
            if_version: None,
        },
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "user_3".into(),
            value: json!({ "name": "Grace", "role": "admin", "age": 50 }),
            if_version: None,
        },
    ])
    .unwrap();

    // 1. SELECT with json_extract and WHERE
    let res = db
        .sql_query(
            "SELECT id, json_extract(value, '$.name') AS name, json_extract(value, '$.age') AS age
             FROM lilia_json
             WHERE space = 'users' AND json_extract(value, '$.role') = ?1
             ORDER BY age ASC",
            &[json!("admin")],
        )
        .unwrap();

    assert_eq!(res.columns, vec!["id", "name", "age"]);
    assert_eq!(res.rows.len(), 2);
    assert_eq!(res.rows[0], vec![json!("user_1"), json!("Ada"), json!(36)]);
    assert_eq!(
        res.rows[1],
        vec![json!("user_3"), json!("Grace"), json!(50)]
    );
    assert_eq!(res.rows_affected, 0);

    // 2. Aggregate query COUNT(*), AVG(age)
    let agg = db
        .sql_query(
            "SELECT count(*) as total, avg(json_extract(value, '$.age')) as avg_age
             FROM lilia_json
             WHERE space = 'users'",
            &[],
        )
        .unwrap();
    assert_eq!(agg.columns, vec!["total", "avg_age"]);
    assert_eq!(agg.rows.len(), 1);
    assert_eq!(agg.rows[0][0], json!(3));

    // 3. Query KV view
    let kv_res = db
        .sql_query(
            "SELECT namespace, hex(key) AS hex_key, hex(value) AS hex_val
             FROM lilia_kv
             WHERE namespace = 'settings'
             ORDER BY key ASC",
            &[],
        )
        .unwrap();
    assert_eq!(kv_res.rows.len(), 2);
    assert_eq!(kv_res.rows[0][0], json!("settings"));
}

#[test]
fn test_sql_with_codec_transparent_decryption() {
    let temp_dir = tempfile::tempdir().unwrap();
    let codec = Arc::new(TestXorCodec { key: 0x42 });
    let options =
        DatabaseOptions::durable(temp_dir.path().join("encrypted_sql.lilia")).with_codec(codec);
    let db = Database::open(options).unwrap();

    // Insert sensitive documents
    db.batch(&[
        BatchOperation::KvSet {
            namespace: "secrets".into(),
            key: b"api_token".to_vec(),
            value: b"super-secret-token".to_vec(),
            if_version: None,
            expires_at_ms: None,
        },
        BatchOperation::JsonPut {
            space: "vault".into(),
            id: "card_1".into(),
            value: json!({ "pan": "4111-2222-3333-4444", "cvv": "123", "owner": "Satoshi" }),
            if_version: None,
        },
    ])
    .unwrap();

    // Query lilia_json view -> automatically transparently decrypted!
    let res = db
        .sql_query(
            "SELECT id, json_extract(value, '$.pan') as pan, json_extract(value, '$.owner') as owner
             FROM lilia_json
             WHERE space = 'vault'",
            &[],
        )
        .unwrap();

    assert_eq!(res.rows.len(), 1);
    assert_eq!(
        res.rows[0],
        vec![
            json!("card_1"),
            json!("4111-2222-3333-4444"),
            json!("Satoshi")
        ]
    );

    // Query raw _lilia_json table -> contains the ciphertext envelope
    let raw = db
        .sql_query("SELECT value FROM _lilia_json WHERE space = 'vault'", &[])
        .unwrap();
    let raw_str = raw.rows[0][0].as_str().unwrap();
    assert!(raw_str.contains("$codec"));
    assert!(raw_str.contains("$data"));
    assert!(!raw_str.contains("4111-2222-3333-4444"));

    // Query lilia_kv view -> automatically decrypted bytes
    let kv_res = db
        .sql_query(
            "SELECT namespace, cast(value as text) as plain_val FROM lilia_kv WHERE namespace = 'secrets'",
            &[],
        )
        .unwrap();
    assert_eq!(kv_res.rows.len(), 1);
    assert_eq!(kv_res.rows[0][1], json!("super-secret-token"));
}

#[test]
fn test_sql_mutations_and_rows_affected() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("mutation_sql.lilia"),
    ))
    .unwrap();

    // Mutating query via SQL
    let insert_res = db
        .sql_query(
            "INSERT INTO _lilia_json(space, id, value, version) VALUES('tasks', 'task_1', '{\"title\":\"Buy milk\"}', 1)",
            &[],
        )
        .unwrap();
    assert_eq!(insert_res.rows_affected, 1);

    // Read back via json_get
    let doc = db.json_get("tasks", "task_1").unwrap().expect("must exist");
    assert_eq!(doc.value["title"], "Buy milk");

    // Update via SQL
    let update_res = db
        .sql_query(
            "UPDATE _lilia_json SET value = '{\"title\":\"Buy coffee\"}', version = 2 WHERE space = 'tasks' AND id = 'task_1'",
            &[],
        )
        .unwrap();
    assert_eq!(update_res.rows_affected, 1);

    let doc_updated = db.json_get("tasks", "task_1").unwrap().unwrap();
    assert_eq!(doc_updated.value["title"], "Buy coffee");
    assert_eq!(doc_updated.version, 2);

    // Delete via SQL
    let delete_res = db
        .sql_query(
            "DELETE FROM _lilia_json WHERE space = 'tasks' AND id = 'task_1'",
            &[],
        )
        .unwrap();
    assert_eq!(delete_res.rows_affected, 1);
    assert!(db.json_get("tasks", "task_1").unwrap().is_none());
}

#[test]
fn test_h3_native_indexing_and_carray_k_ring_query() {
    use lilia_core::h3;

    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("h3_carray_test.lilia"),
    ))
    .unwrap();

    // 1. Create table and index
    db.sql()
        .execute(
            "CREATE TABLE places(
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                category TEXT NOT NULL,
                latitude REAL NOT NULL,
                longitude REAL NOT NULL,
                h3_index INTEGER NOT NULL
            );",
        )
        .unwrap();

    db.sql()
        .execute("CREATE INDEX idx_places_h3 ON places(h3_index);")
        .unwrap();

    // 2. Insert POIs around Paris (Eiffel Tower area)
    // Eiffel Tower: 48.8584, 2.2945 -> res 9
    let eiffel_h3 = h3::lat_lng_to_cell(48.8584, 2.2945, 9).unwrap();
    let champ_de_mars_h3 = h3::lat_lng_to_cell(48.8556, 2.2986, 9).unwrap();
    let trocadero_h3 = h3::lat_lng_to_cell(48.8623, 2.2881, 9).unwrap();
    // Far point: Notre Dame (~4.2 km away)
    let notre_dame_h3 = h3::lat_lng_to_cell(48.8530, 2.3499, 9).unwrap();

    db.sql_query(
        "INSERT INTO places(id, name, category, latitude, longitude, h3_index) VALUES
            ('p1', 'Eiffel Tower', 'attraction', 48.8584, 2.2945, ?1),
            ('p2', 'Champ de Mars Cafe', 'restaurant', 48.8556, 2.2986, ?2),
            ('p3', 'Trocadero Gardens', 'park', 48.8623, 2.2881, ?3),
            ('p4', 'Notre Dame', 'monument', 48.8530, 2.3499, ?4);",
        &[
            json!(eiffel_h3),
            json!(champ_de_mars_h3),
            json!(trocadero_h3),
            json!(notre_dame_h3),
        ],
    )
    .unwrap();

    // 3. User request scenario:
    // Trouve tous les voisins dans un rayon de 2 hexagones (~500m)
    let current_h3_index = eiffel_h3;
    let nearby_hexagons = h3::k_ring(current_h3_index, 2).unwrap();

    // Recherche SQL instantanée avec l'extension carray native déjà compilée :
    let pois = db
        .sql()
        .query_with_carray(
            "SELECT id, name, h3_index FROM places WHERE h3_index IN carray(?1) ORDER BY id;",
            &nearby_hexagons,
        )
        .unwrap();

    assert_eq!(pois.columns, vec!["id", "name", "h3_index"]);
    // Should match Eiffel Tower, Champ de Mars Cafe, and Trocadero Gardens (within 2 hops)
    // but exclude Notre Dame (which is far away)
    assert_eq!(pois.rows.len(), 3);
    let ids: Vec<&str> = pois.rows.iter().map(|r| r[0].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["p1", "p2", "p3"]);

    // Test with additional parameters: query_with_carray_and_params
    let restaurants = db
        .sql()
        .query_with_carray_and_params(
            "SELECT name FROM places WHERE h3_index IN carray(?1) AND category = ?2;",
            &nearby_hexagons,
            &[json!("restaurant")],
        )
        .unwrap();
    assert_eq!(restaurants.rows.len(), 1);
    assert_eq!(restaurants.rows[0][0], "Champ de Mars Cafe");
}

#[test]
fn test_h3_btree_range_scan_child_range() {
    use lilia_core::h3;

    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("h3_btree_range_test.lilia"),
    ))
    .unwrap();

    db.sql()
        .execute(
            "CREATE TABLE delivery_zones(
                driver_id TEXT PRIMARY KEY,
                h3_index INTEGER NOT NULL
            );",
        )
        .unwrap();
    db.sql()
        .execute("CREATE INDEX idx_delivery_h3 ON delivery_zones(h3_index);")
        .unwrap();

    // Parent cell at resolution 7 covering Eiffel Tower sector
    let parent_res7 = h3::lat_lng_to_cell(48.8584, 2.2945, 7).unwrap();

    // Insert 10 drivers at resolution 9 within or outside this parent
    let inside_pt_1 = h3::lat_lng_to_cell(48.8584, 2.2945, 9).unwrap();
    let inside_pt_2 = h3::lat_lng_to_cell(48.8600, 2.2950, 9).unwrap();
    // Outside driver in London
    let london_pt = h3::lat_lng_to_cell(51.5074, -0.1278, 9).unwrap();

    // Verify parentage
    assert_eq!(h3::cell_to_parent(inside_pt_1, 7).unwrap(), parent_res7);
    assert_eq!(h3::cell_to_parent(inside_pt_2, 7).unwrap(), parent_res7);
    assert_ne!(h3::cell_to_parent(london_pt, 7).unwrap(), parent_res7);

    db.sql_query(
        "INSERT INTO delivery_zones(driver_id, h3_index) VALUES
            ('driver_1', ?1),
            ('driver_2', ?2),
            ('driver_london', ?3);",
        &[json!(inside_pt_1), json!(inside_pt_2), json!(london_pt)],
    )
    .unwrap();

    // Proximité Spatiale = Plage d'Entiers [H3_min, H3_max]
    let (h3_min, h3_max) = h3::child_range(parent_res7, 9).unwrap();

    let drivers = db
        .sql()
        .query_with_params(
            "SELECT driver_id FROM delivery_zones WHERE h3_index BETWEEN ?1 AND ?2 ORDER BY driver_id;",
            &[json!(h3_min), json!(h3_max)],
        )
        .unwrap();

    assert_eq!(drivers.rows.len(), 2);
    assert_eq!(drivers.rows[0][0], "driver_1");
    assert_eq!(drivers.rows[1][0], "driver_2");

    // Also test pure SQL range calculation using native functions:
    let drivers_sql = db
        .sql()
        .query_with_params(
            "SELECT driver_id FROM delivery_zones 
             WHERE h3_index BETWEEN lilia_h3_child_range_min(?1, 9) AND lilia_h3_child_range_max(?1, 9)
             ORDER BY driver_id;",
            &[json!(parent_res7)],
        )
        .unwrap();
    assert_eq!(drivers_sql.rows.len(), 2);
}

#[test]
fn test_h3_sql_scalar_functions() {
    use lilia_core::h3;

    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("h3_functions_test.lilia"),
    ))
    .unwrap();

    // 1. lilia_h3_lat_lng_to_cell
    let res = db
        .sql()
        .query("SELECT lilia_h3_lat_lng_to_cell(48.8584, 2.2945, 9) AS cell;")
        .unwrap();
    let cell_val = res.rows[0][0].as_i64().unwrap() as u64;
    assert!(h3::is_valid(cell_val));
    assert_eq!(h3::resolution(cell_val).unwrap(), 9);

    // 2. lilia_h3_cell_to_lat and lilia_h3_cell_to_lng
    let lat_lng = db
        .sql()
        .query_with_params(
            "SELECT lilia_h3_cell_to_lat(?1) AS lat, lilia_h3_cell_to_lng(?1) AS lng;",
            &[json!(cell_val)],
        )
        .unwrap();
    let lat = lat_lng.rows[0][0].as_f64().unwrap();
    let lng = lat_lng.rows[0][1].as_f64().unwrap();
    assert!((lat - 48.8584).abs() < 0.01);
    assert!((lng - 2.2945).abs() < 0.01);

    // 3. lilia_h3_to_string and lilia_h3_from_string
    let str_res = db
        .sql()
        .query_with_params(
            "SELECT lilia_h3_to_string(?1) AS str, lilia_h3_from_string(lilia_h3_to_string(?1)) AS roundtrip;",
            &[json!(cell_val)],
        )
        .unwrap();
    let s = str_res.rows[0][0].as_str().unwrap();
    let rt = str_res.rows[0][1].as_i64().unwrap() as u64;
    assert_eq!(rt, cell_val);
    assert_eq!(s, h3::cell_to_string(cell_val));

    // 4. lilia_h3_resolution and lilia_h3_parent
    let parent_res = db
        .sql()
        .query_with_params(
            "SELECT lilia_h3_resolution(?1) AS res, lilia_h3_parent(?1, 7) AS parent;",
            &[json!(cell_val)],
        )
        .unwrap();
    assert_eq!(parent_res.rows[0][0], 9);
    let parent_val = parent_res.rows[0][1].as_i64().unwrap() as u64;
    assert_eq!(h3::resolution(parent_val).unwrap(), 7);

    // 5. lilia_h3_distance
    let dist_res = db
        .sql()
        .query_with_params(
            "SELECT lilia_h3_distance(?1, ?1) AS zero_dist, lilia_h3_is_valid(?1) AS valid;",
            &[json!(cell_val)],
        )
        .unwrap();
    assert_eq!(dist_res.rows[0][0], 0);
    assert_eq!(dist_res.rows[0][1], 1);

    // 6. lilia_h3_k_ring (returns JSON array)
    let kring_res = db
        .sql()
        .query_with_params(
            "SELECT lilia_h3_k_ring(?1, 1) AS disk;",
            &[json!(cell_val)],
        )
        .unwrap();
    let disk_json = kring_res.rows[0][0].as_str().unwrap();
    let disk_arr: Vec<u64> = serde_json::from_str(disk_json).unwrap();
    assert_eq!(disk_arr.len(), 7);

    // 7. lilia_h3_to_boundary (geopoly-compatible polygon)
    let boundary_res = db
        .sql()
        .query_with_params(
            "SELECT lilia_h3_to_boundary(?1) AS poly;",
            &[json!(cell_val)],
        )
        .unwrap();
    let poly_str = boundary_res.rows[0][0].as_str().unwrap();
    assert!(poly_str.starts_with('['));
    assert!(poly_str.ends_with(']'));
}

#[test]
fn test_h3_spatiotemporal_gps_points_and_carray() {
    use lilia_core::GpsPoint;

    let temp_dir = tempfile::tempdir().unwrap();
    let db = Database::open(DatabaseOptions::durable(
        temp_dir.path().join("h3_spatiotemporal_gps.lilia"),
    ))
    .unwrap();

    // Record GPS fixes for a courier
    // Point 1: Eiffel Tower at t=1000
    // Point 2: 150m south at t=2000
    // Point 3: Far away (Notre Dame) at t=3000
    let pts = vec![
        GpsPoint::with_details(1000, 48.8584, 2.2945, Some(35.0), Some(4.5), Some(3.0)),
        GpsPoint::with_details(2000, 48.8570, 2.2945, Some(35.0), Some(4.8), Some(3.0)),
        GpsPoint::with_details(3000, 48.8530, 2.3499, Some(30.0), Some(5.0), Some(3.0)),
    ];

    db.geo_record_gps_points("courier_1", &pts).unwrap();

    // Verify track points have h3_index populated
    let track = db.geo_get_track_points("courier_1", None, None).unwrap();
    assert_eq!(track.len(), 3);
    for pt in &track {
        assert!(pt.h3_index.is_some());
        assert!(lilia_core::h3::is_valid(pt.h3_index.unwrap()));
    }

    // Query using geo_find_points_in_h3
    let eiffel_h3 = lilia_core::h3::lat_lng_to_cell(48.8584, 2.2945, 9).unwrap();

    // Find points within 2 hex hops (~500m) during t=[500, 2500]
    let nearby_fixes = db
        .geo_find_points_in_h3(eiffel_h3, 2, Some(500), Some(2500))
        .unwrap();
    assert_eq!(nearby_fixes.len(), 2);
    assert_eq!(nearby_fixes[0].timestamp, 1000);
    assert_eq!(nearby_fixes[1].timestamp, 2000);

    // Filter by narrower time window: t=[1500, 2500] -> only point 2
    let narrow_fixes = db
        .geo_find_points_in_h3(eiffel_h3, 2, Some(1500), Some(2500))
        .unwrap();
    assert_eq!(narrow_fixes.len(), 1);
    assert_eq!(narrow_fixes[0].timestamp, 2000);
}
