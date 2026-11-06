use std::fs;
use std::sync::Arc;

use lilia_core::ValueCodec;
use lilia_plugin_api::{
    generate_keypair, install_verified, load_descriptor, load_plugin, pack_plugin, sign_package,
    Capability, PackOptions, WasmPluginConfig, WASM_TARGET_WASIP1,
};
use lilia_storage_sqlite::{Database, DatabaseOptions};

const VALID_CODEC_WAT: &str = r#"
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

  (func (export "codec_encode") (param $in_ptr i32) (param $in_len i32) (param $out_ptr i32) (param $out_cap i32) (param $out_len_ptr i32) (result i32)
    (local $i i32)
    (local $b i32)

    (i32.store (local.get $out_len_ptr) (local.get $in_len))
    (local.set $i (i32.const 0))
    (block $break
      (loop $loop
        (br_if $break (i32.ge_u (local.get $i) (local.get $in_len)))
        (local.set $b (i32.xor (i32.load8_u (i32.add (local.get $in_ptr) (local.get $i))) (i32.const 66)))
        (i32.store8 (i32.add (local.get $out_ptr) (local.get $i)) (local.get $b))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $loop)
      )
    )
    (i32.const 0)
  )

  (func (export "codec_decode") (param $in_ptr i32) (param $in_len i32) (param $out_ptr i32) (param $out_cap i32) (param $out_len_ptr i32) (result i32)
    (local $i i32)
    (local $b i32)

    (i32.store (local.get $out_len_ptr) (local.get $in_len))
    (local.set $i (i32.const 0))
    (block $break
      (loop $loop
        (br_if $break (i32.ge_u (local.get $i) (local.get $in_len)))
        (local.set $b (i32.xor (i32.load8_u (i32.add (local.get $in_ptr) (local.get $i))) (i32.const 66)))
        (i32.store8 (i32.add (local.get $out_ptr) (local.get $i)) (local.get $b))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $loop)
      )
    )
    (i32.const 0)
  )
)
"#;

const CRASH_TRAP_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "lilia_alloc") (param $size i32) (result i32)
    (i32.const 1024)
  )
  (func (export "lilia_dealloc") (param $ptr i32) (param $size i32))
  (func (export "codec_encode") (param $in_ptr i32) (param $in_len i32) (param $out_ptr i32) (param $out_cap i32) (param $out_len_ptr i32) (result i32)
    ;; Deliberately write to an out-of-bounds address (0x7FFFFFFF)
    (i32.store (i32.const 2147483647) (i32.const 42))
    (i32.const 0)
  )
  (func (export "codec_decode") (param $in_ptr i32) (param $in_len i32) (param $out_ptr i32) (param $out_cap i32) (param $out_len_ptr i32) (result i32)
    (i32.const 0)
  )
)
"#;

const PANIC_TRAP_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "lilia_alloc") (param $size i32) (result i32)
    (i32.const 1024)
  )
  (func (export "lilia_dealloc") (param $ptr i32) (param $size i32))
  (func (export "codec_encode") (param $in_ptr i32) (param $in_len i32) (param $out_ptr i32) (param $out_cap i32) (param $out_len_ptr i32) (result i32)
    (unreachable)
  )
  (func (export "codec_decode") (param $in_ptr i32) (param $in_len i32) (param $out_ptr i32) (param $out_cap i32) (param $out_len_ptr i32) (result i32)
    (i32.const 0)
  )
)
"#;

const INFINITE_LOOP_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "lilia_alloc") (param $size i32) (result i32)
    (i32.const 1024)
  )
  (func (export "lilia_dealloc") (param $ptr i32) (param $size i32))
  (func (export "codec_encode") (param $in_ptr i32) (param $in_len i32) (param $out_ptr i32) (param $out_cap i32) (param $out_len_ptr i32) (result i32)
    (loop (br 0))
    (i32.const 0)
  )
  (func (export "codec_decode") (param $in_ptr i32) (param $in_len i32) (param $out_ptr i32) (param $out_cap i32) (param $out_len_ptr i32) (result i32)
    (i32.const 0)
  )
)
"#;

const OOM_ALLOC_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "lilia_alloc") (param $size i32) (result i32)
    ;; Try to grow memory by 2000 pages (128 MB), which exceeds the 64 MB (or custom) quota
    (local $prev i32)
    (local.set $prev (memory.grow (i32.const 2000)))
    (if (i32.eq (local.get $prev) (i32.const -1))
      (then
        (return (i32.const 0))
      )
    )
    (i32.const 1024)
  )
  (func (export "lilia_dealloc") (param $ptr i32) (param $size i32))
  (func (export "codec_encode") (param $in_ptr i32) (param $in_len i32) (param $out_ptr i32) (param $out_cap i32) (param $out_len_ptr i32) (result i32)
    (i32.const 0)
  )
  (func (export "codec_decode") (param $in_ptr i32) (param $in_len i32) (param $out_ptr i32) (param $out_cap i32) (param $out_len_ptr i32) (result i32)
    (i32.const 0)
  )
)
"#;

const SQL_VALIDATOR_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "lilia_alloc") (param $size i32) (result i32)
    (i32.const 1024)
  )
  (func (export "lilia_dealloc") (param $ptr i32) (param $size i32))
  (func (export "sql_validate_query") (param $ptr i32) (param $len i32) (result i32)
    (if (i32.gt_u (local.get $len) (i32.const 0))
      (then
        ;; If first character is 'D' (ASCII 68), reject with code 1
        (if (i32.eq (i32.load8_u (local.get $ptr)) (i32.const 68))
          (then (return (i32.const 1)))
        )
      )
    )
    (i32.const 0)
  )
)
"#;

#[test]
fn wasm_plugin_codec_end_to_end_sqlite_storage() {
    let temp = tempfile::tempdir().expect("tempdir");
    let wasm_file = temp.path().join("codec_plugin.wasm");
    fs::write(&wasm_file, VALID_CODEC_WAT.as_bytes()).expect("write wasm file");

    let package_dir = temp.path().join("pkg");
    let pack_opts = PackOptions {
        name: "sandboxed-codec".into(),
        version: "1.0.0".into(),
        capabilities: vec![Capability::Codec],
        engine_requirement: None,
        target: Some(WASM_TARGET_WASIP1.into()),
    };
    let packed = pack_plugin(&wasm_file, &package_dir, &pack_opts).expect("pack wasm plugin");
    assert!(packed.is_dir());

    let (sk, vk) = generate_keypair();
    sign_package(&packed, &sk).expect("sign wasm package");

    let store_root = temp.path().join("plugins");
    let installed_path =
        install_verified(&packed, &store_root, &[vk], false, false).expect("install wasm plugin");

    // Load plugin descriptor
    let manifest_bytes = fs::read(installed_path.join("manifest.json")).expect("manifest");
    let manifest: lilia_plugin_api::PluginManifest =
        serde_json::from_slice(&manifest_bytes).expect("parse manifest");

    let loaded =
        unsafe { load_descriptor(&installed_path, &manifest).expect("load sandboxed plugin") };
    assert!(
        loaded.is_sandboxed(),
        "WASM plugin must report is_sandboxed() == true"
    );
    assert_eq!(loaded.name(), "sandboxed-codec");

    let codec = loaded.codec().expect("codec capability");
    assert_eq!(codec.name(), "sandboxed-codec");

    // Direct codec encode / decode validation
    let plaintext = b"Hello, sandboxed WebAssembly storage!";
    let encoded = codec.encode(plaintext).expect("encode");
    assert_ne!(
        encoded.as_ref(), plaintext,
        "encoded data must differ from plaintext"
    );
    let decoded = codec.decode(&encoded).expect("decode");
    assert_eq!(decoded, plaintext, "decoded data must match plaintext");

    // SQLite persistence integration
    let db_path = temp.path().join("database.lilia");
    {
        let options = DatabaseOptions::durable(&db_path).with_codec(Arc::new(codec.clone()));
        let db = Database::open(options).expect("open database with wasm codec");
        db.batch(&[lilia_core::BatchOperation::KvSet {
            namespace: "users".into(),
            key: b"42".to_vec(),
            value: b"secret-passphrase-data".to_vec(),
            if_version: None,
            expires_at_ms: None,
        }])
        .expect("put encrypted data");

        let entry = db.kv_get("users", b"42").expect("get data").expect("entry");
        assert_eq!(entry.value, b"secret-passphrase-data");
    }

    // Reopen database and verify persistent roundtrip
    {
        let options = DatabaseOptions::durable(&db_path).with_codec(Arc::new(codec));
        let db = Database::open(options).expect("reopen database");
        let entry = db
            .kv_get("users", b"42")
            .expect("get data after reopening")
            .expect("entry");
        assert_eq!(entry.value, b"secret-passphrase-data");
    }
}

#[test]
fn wasm_plugin_crash_trap_resistance() {
    let temp = tempfile::tempdir().expect("tempdir");
    let wasm_file = temp.path().join("crash.wasm");
    fs::write(&wasm_file, CRASH_TRAP_WAT.as_bytes()).expect("write wasm file");

    let loaded = unsafe { load_plugin(&wasm_file).expect("load wasm plugin") };
    assert!(loaded.is_sandboxed());
    let codec = loaded.codec().expect("codec");

    // Calling encode triggers out-of-bounds memory write inside WASM sandbox.
    // In native C-ABI this would SIGSEGV and terminate the host daemon.
    // In WASM sandbox, host intercepts the trap and returns an error without crashing.
    let error = codec
        .encode(b"trigger segfault trap")
        .expect_err("out of bounds memory access must trap");

    assert_eq!(error.code, lilia_core::ErrorCode::Corrupt);
    assert!(
        error.to_string().contains("out of bounds") || error.to_string().contains("memory"),
        "error message should indicate memory trap: {error}"
    );

    // Verify host thread is alive and subsequent calls are handled cleanly
    let error_again = codec
        .encode(b"second call after trap")
        .expect_err("second call should also safely fail");
    assert_eq!(error_again.code, lilia_core::ErrorCode::Corrupt);
}

#[test]
fn wasm_plugin_panic_trap_resistance() {
    let temp = tempfile::tempdir().expect("tempdir");
    let wasm_file = temp.path().join("panic.wasm");
    fs::write(&wasm_file, PANIC_TRAP_WAT.as_bytes()).expect("write wasm file");

    let loaded = unsafe { load_plugin(&wasm_file).expect("load wasm plugin") };
    assert!(loaded.is_sandboxed());
    let codec = loaded.codec().expect("codec");

    // Calling encode executes `unreachable` instruction (guest panic).
    // Host intercepts without aborting.
    let error = codec
        .encode(b"trigger guest panic")
        .expect_err("guest unreachable must trap");

    assert!(
        error.to_string().contains("unreachable") || error.to_string().contains("trap"),
        "error should indicate unreachable/trap: {error}"
    );
}

#[test]
fn wasm_plugin_infinite_loop_fuel_exhaustion() {
    let temp = tempfile::tempdir().expect("tempdir");
    let wasm_file = temp.path().join("infinite.wasm");
    fs::write(&wasm_file, INFINITE_LOOP_WAT.as_bytes()).expect("write wasm file");

    // Use a small fuel limit for quick interruption test
    let config = WasmPluginConfig {
        memory_limit_bytes: 64 * 1024 * 1024,
        fuel_limit: 50_000,
    };

    let loaded = unsafe {
        lilia_plugin_api::load_plugin_with_config(&wasm_file, config)
            .expect("load wasm plugin with fuel limit")
    };
    let codec = loaded.codec().expect("codec");

    let start = std::time::Instant::now();
    let error = codec
        .encode(b"trigger infinite loop")
        .expect_err("infinite loop must run out of fuel");

    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "fuel exhaustion should interrupt in < 500ms, took {:?}",
        elapsed
    );
    assert_eq!(error.code, lilia_core::ErrorCode::Timeout);
    assert!(
        error.to_string().contains("fuel"),
        "error should mention fuel: {error}"
    );
}

#[test]
fn wasm_plugin_memory_limiter_oom_prevention() {
    let temp = tempfile::tempdir().expect("tempdir");
    let wasm_file = temp.path().join("oom.wasm");
    fs::write(&wasm_file, OOM_ALLOC_WAT.as_bytes()).expect("write wasm file");

    // Configure 64 KB memory limit (1 WebAssembly page)
    let config = WasmPluginConfig {
        memory_limit_bytes: 64 * 1024,
        fuel_limit: 1_000_000,
    };

    let loaded = unsafe {
        lilia_plugin_api::load_plugin_with_config(&wasm_file, config)
            .expect("load wasm plugin with memory limit")
    };
    let codec = loaded.codec().expect("codec");

    // The module's lilia_alloc attempts to grow memory beyond the quota.
    // The ResourceLimiter denies growth, alloc returns 0, and host reports allocation failure.
    let error = codec
        .encode(b"large data exceeding memory quota")
        .expect_err("oom memory allocation must fail");

    assert!(
        error.to_string().contains("allocation") || error.to_string().contains("memory"),
        "error should indicate allocation failure: {error}"
    );
}

#[test]
fn wasm_plugin_sql_query_validation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let wasm_file = temp.path().join("sql.wasm");
    fs::write(&wasm_file, SQL_VALIDATOR_WAT.as_bytes()).expect("write wasm file");

    let loaded = unsafe { load_plugin(&wasm_file).expect("load wasm plugin") };
    assert!(loaded.is_sandboxed());
    let sql = loaded.sql().expect("sql capability");

    // Allowed query
    assert!(sql.validate_query("SELECT * FROM users").is_ok());

    // Disallowed query starting with 'D' (DROP TABLE)
    let err = sql
        .validate_query("DROP TABLE users")
        .expect_err("DROP query must be rejected");
    assert!(err.to_string().contains("rejected with code 1"));
}
