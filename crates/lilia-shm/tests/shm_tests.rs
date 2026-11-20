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
