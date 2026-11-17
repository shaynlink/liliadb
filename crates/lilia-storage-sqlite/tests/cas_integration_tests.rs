use std::io::Read;

use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

#[test]
fn test_cas_engine_direct_deduplication_and_sealed_files() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test_cas.lilia");
    let blobs_dir = temp_dir.path().join("custom_blobs");

    let options = DatabaseOptions::durable(&db_path)
        .with_cas_enabled(true)
        .with_cas_threshold(64 * 1024)
        .with_cas_dir(&blobs_dir);

    let db = Database::open(options).unwrap();
    assert_eq!(db.cas().root_dir(), &blobs_dir);

    // Create 80 KB test payload
    let data = vec![0xAB; 80 * 1024];
    let desc1 = db.blob_store(&data).unwrap();
    assert_eq!(desc1.size_bytes, 80 * 1024);

    let file_path = db.blob_path(&desc1.hash);
    assert!(file_path.is_file());
    assert!(db.blob_exists(&desc1.hash));

    // Verify 2-level sharded directory structure: <root>/<hash[0..2]>/<hash[2..4]>/<hash>.blob
    let rel_path = file_path.strip_prefix(&blobs_dir).unwrap();
    let components: Vec<_> = rel_path.iter().map(|c| c.to_str().unwrap()).collect();
    assert_eq!(components.len(), 3);
    assert_eq!(components[0], &desc1.hash[0..2]);
    assert_eq!(components[1], &desc1.hash[2..4]);

    // Verify deduplication: storing identical content returns identical hash and keeps 1 physical file
    let desc2 = db.blob_store(&data).unwrap();
    assert_eq!(desc1.hash, desc2.hash);

    // Read back via get()
    let read_back = db.blob_get(&desc1.hash).unwrap().unwrap();
    assert_eq!(read_back, data);

    // Stream via open()
    let mut reader = db.blob_open(&desc1.hash).unwrap().unwrap();
    let mut stream_buf = Vec::new();
    reader.read_to_end(&mut stream_buf).unwrap();
    assert_eq!(stream_buf, data);
}

#[test]
fn test_kv_cas_transparent_offload_and_deduplication() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test_kv_cas.lilia");

    let options = DatabaseOptions::durable(&db_path)
        .with_cas_enabled(true)
        .with_cas_threshold(64 * 1024);

    let db = Database::open(options).unwrap();
    let kv = db.collection("assets").disk().kv().unwrap();

    // Small value (< 64 KB) must remain inline without CAS pointer
    let small_payload = vec![0x42; 1024];
    kv.set(b"small_file.txt", &small_payload).unwrap();
    let small_read = kv.get(b"small_file.txt").unwrap().unwrap();
    assert_eq!(small_read.value, small_payload);
    assert!(kv.get_blob_path(b"small_file.txt").unwrap().is_none());

    // Large value (> 64 KB, 100 KB photo)
    let photo = vec![0xEE; 100 * 1024];
    kv.set(b"photo1.jpg", &photo).unwrap();
    kv.set(b"photo2.jpg", &photo).unwrap();

    // Both keys must resolve transparently to the original 100 KB payload
    let read1 = kv.get(b"photo1.jpg").unwrap().unwrap();
    assert_eq!(read1.value, photo);
    let read2 = kv.get(b"photo2.jpg").unwrap().unwrap();
    assert_eq!(read2.value, photo);

    // Both keys must point to the EXACT same physical CAS file on disk
    let path1 = kv.get_blob_path(b"photo1.jpg").unwrap().unwrap();
    let path2 = kv.get_blob_path(b"photo2.jpg").unwrap().unwrap();
    assert_eq!(path1, path2);
    assert!(path1.is_file());

    // Stream directly via reader
    let mut stream_file = kv.get_blob_reader(b"photo1.jpg").unwrap().unwrap();
    let mut streamed = Vec::new();
    stream_file.read_to_end(&mut streamed).unwrap();
    assert_eq!(streamed, photo);

    // Verify ref_count in SQLite is 2
    {
        let reader_conn = db.lock_writer().unwrap();
        let mut stmt = reader_conn
            .prepare("SELECT ref_count FROM _lilia_blobs;")
            .unwrap();
        assert!(stmt.step().unwrap());
        assert_eq!(stmt.column_int64(0), 2);
    }

    // Delete photo1: ref_count becomes 1, file remains
    kv.delete(b"photo1.jpg").unwrap();
    assert!(path1.is_file());
    {
        let reader_conn = db.lock_writer().unwrap();
        let mut stmt = reader_conn
            .prepare("SELECT ref_count FROM _lilia_blobs;")
            .unwrap();
        assert!(stmt.step().unwrap());
        assert_eq!(stmt.column_int64(0), 1);
    }

    // Delete photo2: ref_count becomes 0
    kv.delete(b"photo2.jpg").unwrap();
    {
        let reader_conn = db.lock_writer().unwrap();
        let mut stmt = reader_conn
            .prepare("SELECT ref_count FROM _lilia_blobs;")
            .unwrap();
        assert!(stmt.step().unwrap());
        assert_eq!(stmt.column_int64(0), 0);
    }

    // Trigger WAL checkpoint: Atomic GC purges the orphaned file
    db.checkpoint().unwrap();
    assert!(!path1.is_file(), "orphaned blob file must be purged after checkpoint");

    // Verify record in _lilia_blobs is deleted
    {
        let reader_conn = db.lock_writer().unwrap();
        let mut stmt = reader_conn
            .prepare("SELECT count(*) FROM _lilia_blobs;")
            .unwrap();
        assert!(stmt.step().unwrap());
        assert_eq!(stmt.column_int64(0), 0);
    }
}

#[test]
fn test_json_cas_reference_sharing_and_garbage_collection() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test_json_cas.lilia");

    let options = DatabaseOptions::durable(&db_path)
        .with_cas_enabled(true)
        .with_cas_threshold(64 * 1024);

    let db = Database::open(options).unwrap();
    let json_col = db.collection("users").disk().json().unwrap();

    // Store a 90 KB image blob
    let image_bytes = vec![0x33; 90 * 1024];
    let blob_desc = db.blob_store(&image_bytes).unwrap();
    let blob_path = db.blob_path(&blob_desc.hash);
    assert!(blob_path.is_file());

    // Alice and Bob share the exact same avatar photo
    let alice_doc = json!({
        "name": "Alice",
        "avatar": blob_desc.to_json_value()
    });
    let bob_doc = json!({
        "name": "Bob",
        "avatar": blob_desc.to_json_value()
    });

    json_col.put("alice", alice_doc).unwrap();
    json_col.put("bob", bob_doc).unwrap();

    // Check ref_count is 2 and 2 references exist in _lilia_blob_refs
    {
        let conn = db.lock_writer().unwrap();
        let mut stmt_ref = conn.prepare("SELECT ref_count FROM _lilia_blobs WHERE hash = ?1;").unwrap();
        stmt_ref.bind_text_static(1, &blob_desc.hash).unwrap();
        assert!(stmt_ref.step().unwrap());
        assert_eq!(stmt_ref.column_int64(0), 2);

        let mut stmt_count = conn.prepare("SELECT count(*) FROM _lilia_blob_refs WHERE blob_hash = ?1;").unwrap();
        stmt_count.bind_text_static(1, &blob_desc.hash).unwrap();
        assert!(stmt_count.step().unwrap());
        assert_eq!(stmt_count.column_int64(0), 2);
    }

    // Delete Alice: ref_count becomes 1, file remains
    json_col.delete("alice").unwrap();
    assert!(blob_path.is_file());
    db.checkpoint().unwrap();
    assert!(blob_path.is_file(), "file must not be purged while Bob still references it");

    // Delete Bob: ref_count becomes 0
    json_col.delete("bob").unwrap();
    {
        let conn = db.lock_writer().unwrap();
        let mut stmt_ref = conn.prepare("SELECT ref_count FROM _lilia_blobs WHERE hash = ?1;").unwrap();
        stmt_ref.bind_text_static(1, &blob_desc.hash).unwrap();
        assert!(stmt_ref.step().unwrap());
        assert_eq!(stmt_ref.column_int64(0), 0);
    }

    // Checkpoint purges the orphaned file
    let gc_stats = db.blob_gc().unwrap();
    assert_eq!(gc_stats.orphans_purged, 1);
    assert_eq!(gc_stats.bytes_reclaimed, 90 * 1024);
    assert!(!blob_path.is_file(), "orphaned blob must be purged from disk");
}

#[test]
fn test_transaction_rollback_preserves_cas_references() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test_rollback_cas.lilia");

    let options = DatabaseOptions::durable(&db_path)
        .with_cas_enabled(true)
        .with_cas_threshold(64 * 1024);

    let db = Database::open(options).unwrap();
    let json_col = db.collection("docs").disk().json().unwrap();

    let pdf_bytes = vec![0x77; 75 * 1024];
    let blob_desc = db.blob_store(&pdf_bytes).unwrap();
    let blob_path = db.blob_path(&blob_desc.hash);
    assert!(blob_path.is_file());

    json_col.put("doc1", json!({ "file": blob_desc.to_json_value() })).unwrap();

    // Rollback a deletion attempt
    {
        let mut writer = db.lock_writer().unwrap();
        let mut tx = writer.transaction().unwrap();
        let _ = lilia_core::cas::extract_blob_hashes_from_value(&json!({ "file": blob_desc.to_json_value() }));
        // Manually delete references inside transaction then rollback
        lilia_storage_sqlite::cas::delete_json_blob_refs(&mut tx, "docs", "doc1").unwrap();
        tx.rollback().unwrap();
    }

    // Checkpoint must NOT purge the file because transaction rolled back
    db.checkpoint().unwrap();
    assert!(blob_path.is_file(), "file must not be purged after transaction rollback");

    {
        let conn = db.lock_writer().unwrap();
        let mut stmt = conn.prepare("SELECT ref_count FROM _lilia_blobs WHERE hash = ?1;").unwrap();
        stmt.bind_text_static(1, &blob_desc.hash).unwrap();
        assert!(stmt.step().unwrap());
        assert_eq!(stmt.column_int64(0), 1);
    }
}
