//! A database in the browser: on the origin private file system, in a dedicated worker, without
//! background threads, with SQL. Run with a C compiler that targets wasm32 (for the zstd of
//! `DataFusion`):
//!
//! ```text
//! CC_wasm32_unknown_unknown=clang cargo test -p lilia-db --features sql \
//!     --target wasm32-unknown-unknown --test browser
//! ```
#![cfg(all(target_arch = "wasm32", target_os = "unknown"))]

use std::sync::Arc;

use lilia_db::{Database, Options};
use lilia_io::{HardwareProfile, OpfsPool, Vfs};
use serde_json::json;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_dedicated_worker);

/// Small buffers, so a few thousand writes flush and compact.
fn options() -> Options {
    let mut options = Options::for_hardware(&HardwareProfile::minimal());
    options.engine.memtable_bytes = 64 << 10;
    options.engine.target_file_bytes = 64 << 10;
    options.engine.level1_bytes = 256 << 10;
    options
}

async fn open(directory: &str) -> (OpfsPool, Database) {
    let pool = OpfsPool::open(directory, 256).await.unwrap();
    let vfs: Arc<dyn Vfs> = Arc::new(pool.vfs());
    let db = Database::open_with(vfs, "/db", options()).unwrap();
    (pool, db)
}

#[wasm_bindgen_test]
async fn a_database_persists_in_opfs_and_answers_sql() {
    let directory = format!("lilia-test-{}", js_sys::Math::random());
    let (pool, db) = open(&directory).await;
    let s = db.system();
    for i in 0..3_000u32 {
        s.kv_put("ns", format!("k{i:05}").as_bytes(), &[b'x'; 200])
            .unwrap();
    }
    s.json_put("docs", "a", &json!({"title": "NDA", "cost": 500}))
        .unwrap();
    let rows = s
        .sql(
            "SELECT value->>'title', json_get_int(value, 'cost') FROM docs",
            &[],
        )
        .unwrap()
        .rows;
    assert_eq!(rows, [vec![json!("NDA"), json!(500)]]);
    let used = pool.vfs().capacity() - pool.vfs().free_slots();
    assert!(used > 3, "flushes wrote sorted runs: {used} slots in use");
    drop(s);
    db.close().unwrap();
    drop(pool);

    let (_pool, db) = open(&directory).await;
    let s = db.system();
    assert_eq!(s.kv_get("ns", b"k02999").unwrap().unwrap().value.len(), 200);
    let count = s.sql("SELECT count(*) FROM lilia_kv", &[]).unwrap().rows;
    assert_eq!(count, [vec![json!(3_000)]]);
    assert_eq!(
        s.json_get("docs", "a").unwrap().unwrap().value()["cost"],
        500
    );
}
