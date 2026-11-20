#![allow(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use lilia_shm::{ShmBackend, ShmEngine, ShmOptions, ShmReader};

#[test]
fn test_shm_basic_crud_and_zero_copy() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let shm_path = temp_dir.path().join("test_crud.shm");

    let engine = ShmEngine::create(
        ShmOptions::file_backed(&shm_path)
            .with_size_bytes(4 * 1024 * 1024)
            .with_slot_capacity(1024),
    )
    .expect("create engine");

    // Insert KV
    let space = "users";
    let key = b"user:42";
    let val = b"Ada Lovelace";
    engine.put_kv(space, key, val, 1, None).expect("put kv");

    // Read zero copy
    let entry = engine.get_zero_copy(space, key).expect("find entry");
    assert_eq!(entry.key, key);
    assert_eq!(entry.value, val);
    assert_eq!(entry.version, 1);
    assert_eq!(entry.expires_at_ms, None);

    // Read via standalone ShmReader
    let reader = engine.create_reader().expect("create reader");
    let reader_entry = reader.get(space, key).expect("reader find entry");
    assert_eq!(reader_entry.value, val);

    // Update KV
    let new_val = b"Augusta Ada King";
    engine
        .put_kv(space, key, new_val, 2, None)
        .expect("update kv");
    let updated = reader.get(space, key).expect("reader updated entry");
    assert_eq!(updated.value, new_val);
    assert_eq!(updated.version, 2);

    // Delete KV
    assert!(engine.delete(space, key));
    assert!(reader.get(space, key).is_none());
}

#[test]
fn test_shm_json_zero_copy() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let shm_path = temp_dir.path().join("test_json.shm");

    let engine =
        ShmEngine::create(ShmOptions::file_backed(&shm_path).with_size_bytes(4 * 1024 * 1024))
            .expect("create engine");

    let json_val = serde_json::json!({
        "name": "Alan Turing",
        "role": "Computer Scientist",
        "active": true
    });
    let binary_bytes =
        lilia_core::json::parse_json_slice_to_binary(&serde_json::to_vec(&json_val).unwrap())
            .unwrap();

    engine
        .put_json("scientists", "turing", binary_bytes.as_bytes(), 1, None)
        .expect("put json");

    let reader = ShmReader::open(ShmBackend::FileBacked(shm_path)).expect("open reader");
    let entry = reader.json_get("scientists", "turing").expect("json entry");
    assert_eq!(entry.id, "turing");

    // Extract path
    let name = reader.json_extract_str("scientists", "turing", "name");
    assert_eq!(name.as_deref(), Some("Alan Turing"));
    let role = reader.json_extract_str("scientists", "turing", "role");
    assert_eq!(role.as_deref(), Some("Computer Scientist"));
}

#[test]
fn test_shm_page_pool() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let shm_path = temp_dir.path().join("test_pages.shm");

    let engine =
        ShmEngine::create(ShmOptions::file_backed(&shm_path).with_size_bytes(4 * 1024 * 1024))
            .expect("create engine");

    let page_0_data = [0xabu8; 4096];
    engine
        .publish_page(0, &page_0_data, 10)
        .expect("publish page 0");

    let page_1_data = [0xcdu8; 2048];
    engine
        .publish_page(1, &page_1_data, 20)
        .expect("publish page 1");

    let reader = engine.create_reader().expect("reader");

    let page_0 = reader.read_page(0).expect("read page 0");
    assert_eq!(page_0.version, 10);
    assert_eq!(page_0.data, &page_0_data[..]);

    let page_1 = reader.read_page(1).expect("read page 1");
    assert_eq!(page_1.version, 20);
    assert_eq!(page_1.data, &page_1_data[..]);

    // Out of bounds page
    assert!(reader.read_page(9999).is_none());
}

#[test]
fn test_shm_ttl_expiry() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let shm_path = temp_dir.path().join("test_ttl.shm");

    let engine =
        ShmEngine::create(ShmOptions::file_backed(&shm_path).with_size_bytes(4 * 1024 * 1024))
            .expect("create engine");

    // Expired record (expires_at_ms = 1 in the past)
    engine
        .put_kv("cache", b"stale", b"old_value", 1, Some(1))
        .expect("put expired");

    let reader = engine.create_reader().expect("reader");
    assert!(reader.get("cache", b"stale").is_none());

    // Future record (expires in 1 hour)
    let future = lilia_shm::header::now_millis() as i64 + 3_600_000;
    engine
        .put_kv("cache", b"fresh", b"fresh_value", 1, Some(future))
        .expect("put fresh");

    let fresh = reader.get("cache", b"fresh").expect("fresh entry");
    assert_eq!(fresh.value, b"fresh_value");
}

#[test]
fn test_shm_concurrent_readers_and_writer() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let shm_path = temp_dir.path().join("test_concurrent.shm");

    let engine = Arc::new(
        ShmEngine::create(
            ShmOptions::file_backed(&shm_path)
                .with_size_bytes(8 * 1024 * 1024)
                .with_slot_capacity(4096),
        )
        .expect("create engine"),
    );

    let stop = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();

    // 4 reader threads
    for _ in 0..4 {
        let reader = engine.create_reader().expect("reader");
        let stop_clone = Arc::clone(&stop);
        handles.push(thread::spawn(move || {
            let mut reads = 0;
            while !stop_clone.load(Ordering::Relaxed) {
                for i in 0..100 {
                    let key = format!("key:{i}");
                    if let Some(entry) = reader.get("stress", key.as_bytes()) {
                        // Value must be consistent with format val:{i}:{version}
                        let s = std::str::from_utf8(entry.value).unwrap();
                        assert!(s.starts_with(&format!("val:{i}:")));
                    }
                }
                reads += 1;
            }
            reads
        }));
    }

    // Writer writes 1000 times
    for v in 1..=500 {
        for i in 0..100 {
            let key = format!("key:{i}");
            let val = format!("val:{i}:{v}");
            engine
                .put_kv("stress", key.as_bytes(), val.as_bytes(), v as u64, None)
                .expect("put kv");
        }
    }

    stop.store(true, Ordering::Release);

    for h in handles {
        let reads = h.join().expect("reader thread completed");
        assert!(reads > 0);
    }
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq))]
struct UserProfile {
    pub name: String,
    pub email: String,
    pub score: f64,
}

#[test]
fn test_shm_rkyv_zero_copy() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("rkyv.shm");

    let engine = ShmEngine::create(
        ShmOptions::file_backed(&path)
            .with_size_bytes(4 * 1024 * 1024)
            .with_slot_capacity(1024),
    )
    .expect("create engine");

    // 1. Put typed struct
    let profile = UserProfile {
        name: "Ada Lovelace".to_string(),
        email: "ada@lovelace.org".to_string(),
        score: 99.5,
    };
    engine
        .put_archived("users", b"ada", &profile, 1, None)
        .expect("put archived profile");

    let reader = engine.create_reader().expect("create reader");

    // Reader reads typed struct directly with 0 ns cast
    let archived_profile: &ArchivedUserProfile = reader
        .get_archived::<UserProfile>("users", b"ada")
        .expect("must find archived profile");
    assert_eq!(archived_profile.name.as_str(), "Ada Lovelace");
    assert_eq!(archived_profile.email.as_str(), "ada@lovelace.org");
    assert_eq!(archived_profile.score.to_native(), 99.5);

    // 2. Put dynamic JSON document
    let json_val = serde_json::json!({
        "order_id": 4242,
        "customer": {
            "name": "Charles Babbage",
            "vip": true
        },
        "items": ["gear", "cam", "differential"]
    });
    let doc = lilia_core::json::JsonDocument::from_serde(&json_val);
    engine
        .put_archived_doc("orders", "4242", &doc, 1, None)
        .expect("put archived doc");

    // Zero-copy extraction directly from shared memory
    let archived_doc = reader
        .get_archived_doc("orders", "4242")
        .expect("must find archived order");
    assert_eq!(archived_doc.extract_i64("order_id"), Some(4242));
    assert_eq!(
        archived_doc.extract_str("customer.name"),
        Some("Charles Babbage")
    );
    assert_eq!(archived_doc.extract_bool("customer.vip"), Some(true));
    assert_eq!(archived_doc.extract_str("items.1"), Some("cam"));

    // In-situ string extraction method
    assert_eq!(
        reader.json_extract_str_zero_copy("orders", "4242", "customer.name"),
        Some("Charles Babbage")
    );
}

#[test]
fn test_shm_phase3_archived_json_view_and_extractors() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("phase3.shm");

    let engine = ShmEngine::create(
        ShmOptions::file_backed(&path)
            .with_size_bytes(4 * 1024 * 1024)
            .with_slot_capacity(1024),
    )
    .expect("create engine");

    let order = serde_json::json!({
        "order_id": 9999,
        "amount": 149.99,
        "customer": {
            "name": "Ada Lovelace",
            "vip": true
        },
        "tags": ["urgent", "priority", "direct"]
    });

    // 1. Put JSON directly using put_archived_json with serde_json::Value
    engine
        .put_archived_json("orders", "9999", &order, 1, None)
        .expect("put archived json");

    // 2. Read via ShmReader::get_archived_json (returns ArchivedJsonView)
    let reader = engine.create_reader().expect("create reader");
    let view = reader
        .get_archived_json("orders", "9999")
        .expect("must find archived json view");

    // 3. Navigate in-situ via dot and pointer paths
    assert_eq!(
        view.get("customer")
            .and_then(|c| c.get("name"))
            .and_then(|n| n.as_str()),
        Some("Ada Lovelace")
    );
    assert_eq!(view.extract("/customer/vip").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(view.extract("tags.1").and_then(|t| t.as_str()), Some("priority"));

    // 4. In-situ zero-copy typed extractors on ShmReader
    assert_eq!(
        reader.json_extract_str_zero_copy("orders", "9999", "/customer/name"),
        Some("Ada Lovelace")
    );
    assert_eq!(
        reader.json_extract_i64_zero_copy("orders", "9999", "/order_id"),
        Some(9999)
    );
    assert_eq!(
        reader.json_extract_f64_zero_copy("orders", "9999", "/amount"),
        Some(149.99)
    );
    assert_eq!(
        reader.json_extract_bool_zero_copy("orders", "9999", "/customer/vip"),
        Some(true)
    );

    // 5. Test checked reader with bytecheck validation
    let profile = UserProfile {
        name: "Charles Babbage".into(),
        email: "charles@diff-engine.uk".into(),
        score: 100.0,
    };
    engine
        .put_archived("profiles", b"charles", &profile, 1, None)
        .expect("put archived struct");

    let checked_profile = reader
        .get_archived_checked::<UserProfile>("profiles", b"charles")
        .expect("validation passed")
        .expect("found profile");
    assert_eq!(checked_profile.name.as_str(), "Charles Babbage");
    assert_eq!(checked_profile.score.to_native(), 100.0);

    // 6. Test ShmEngine::get_archived_json on writer
    let engine_view = engine
        .get_archived_json("orders", "9999")
        .expect("engine find json view");
    assert_eq!(
        engine_view.extract_str("/customer/name"),
        Some("Ada Lovelace")
    );
}

#[test]
fn test_nanosecond_comparison_metrics() {
    use std::hint::black_box;
    use std::time::Instant;

    let profile = UserProfile {
        name: "Ada Lovelace".to_string(),
        email: "ada@lovelace.org".to_string(),
        score: 99.5,
    };

    let json_bytes = serde_json::to_vec(&profile).unwrap();
    let rkyv_bytes = lilia_core::archive_to_bytes(&profile).unwrap();

    const ITERS: u32 = 100_000;

    // 1. serde_json
    let start = Instant::now();
    for _ in 0..ITERS {
        let u: UserProfile = serde_json::from_slice(black_box(&json_bytes)).unwrap();
        let email = black_box(&u.email);
        black_box(email);
    }
    let serde_duration = start.elapsed();
    let serde_ns_per_op = serde_duration.as_nanos() as f64 / ITERS as f64;

    // 2. rkyv unchecked 0ns
    let start = Instant::now();
    for _ in 0..ITERS {
        let u: &ArchivedUserProfile =
            unsafe { lilia_core::access_archived_unchecked::<UserProfile>(black_box(&rkyv_bytes)) };
        let email = black_box(u.email.as_str());
        black_box(email);
    }
    let rkyv_duration = start.elapsed();
    let rkyv_ns_per_op = rkyv_duration.as_nanos() as f64 / ITERS as f64;

    println!("\n========================================================");
    println!(">>> MICRO-BENCHMARK: 100,000 ITERATIONS COMPARISON <<<");
    println!("serde_json latency : {:.2} ns / op", serde_ns_per_op);
    println!("rkyv 0-ns latency  : {:.2} ns / op", rkyv_ns_per_op);
    println!(
        "Speedup factor     : {:.1}x FASTER",
        serde_ns_per_op / rkyv_ns_per_op
    );
    println!("========================================================\n");

    assert!(rkyv_ns_per_op < serde_ns_per_op);
}
