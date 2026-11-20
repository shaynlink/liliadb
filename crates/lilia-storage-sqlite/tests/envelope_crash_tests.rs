use std::fs::{self, File, OpenOptions};
use std::path::Path;
use std::time::Duration;

use lilia_core::envelope::{
    CipherAlgorithm, CompressionAlgorithm, CompressionProfile, DualHeaderEnvelope,
    EncryptionMetadata, EnvelopeConfig, EnvelopeHeader, EnvelopeRecoveryOutcome, KdfAlgorithm,
    ENVELOPE_HEADER_SIZE,
};
use lilia_core::{BatchOperation, ErrorCode};
use lilia_storage_sqlite::{Database, DatabaseOptions, EnvelopeManager};

fn create_sample_database(path: &Path) {
    let db = Database::open(DatabaseOptions::durable(path)).expect("open database");

    let batch = vec![
        BatchOperation::JsonPut {
            space: "users".into(),
            id: "user_42".into(),
            value: serde_json::json!({
                "name": "Ada Lovelace",
                "role": "Pioneer",
                "active": true,
                "balance": 1337.50
            }),
            if_version: None,
        },
        BatchOperation::KvSet {
            namespace: "system".into(),
            key: b"maintenance_mode".to_vec(),
            value: b"false".to_vec(),
            if_version: None,
            expires_at_ms: None,
        },
    ];

    db.batch(&batch).expect("batch succeeds");
    db.close(Duration::from_secs(5)).expect("close database");
}

#[test]
fn test_envelope_export_import_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let original_db_path = dir.path().join("original.lilia");
    let envelope_path = dir.path().join("backup.lenv");
    let restored_db_path = dir.path().join("restored.lilia");

    // 1. Create and populate original database
    create_sample_database(&original_db_path);

    // 2. Export database into a Zstandard compressed dual-header envelope
    let db = Database::open(DatabaseOptions::durable(&original_db_path)).expect("open db");
    let compression = CompressionProfile {
        algorithm: CompressionAlgorithm::Zstd,
        level: 3,
        flags: 0,
        dictionary_id: 0,
    };
    db.export_envelope(&envelope_path, compression)
        .expect("export envelope succeeds");
    db.close(Duration::from_secs(5)).expect("close db");

    assert!(envelope_path.exists());
    assert!(EnvelopeManager::is_enveloped_file(&envelope_path));

    // 3. Import envelope into a new database file
    let header = Database::import_envelope(&envelope_path, &restored_db_path)
        .expect("import envelope succeeds");
    assert_eq!(header.compression.algorithm, CompressionAlgorithm::Zstd);
    assert_eq!(header.schema_version, 1);

    // 4. Verify restored database has identical documents and keys
    let restored_db =
        Database::open(DatabaseOptions::durable(&restored_db_path)).expect("open restored db");
    let user_doc = restored_db
        .json_get("users", "user_42")
        .expect("json get succeeds")
        .expect("user exists");

    assert_eq!(user_doc.extract_str("name"), Some("Ada Lovelace"));
    assert_eq!(user_doc.extract_f64("balance"), Some(1337.50));

    let kv_val = restored_db
        .kv_get("system", b"maintenance_mode")
        .expect("kv get succeeds")
        .expect("key exists");
    assert_eq!(kv_val.value, b"false");
}

#[test]
fn test_crash_recovery_corrupted_front_header_page_0() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("source.lilia");
    let env_path = dir.path().join("corrupted_front.lenv");
    let restored_path = dir.path().join("healed_front.lilia");

    create_sample_database(&db_path);

    let db = Database::open(DatabaseOptions::durable(&db_path)).expect("open");
    db.export_envelope(
        &env_path,
        CompressionProfile {
            algorithm: CompressionAlgorithm::Deflate,
            level: 6,
            flags: 0,
            dictionary_id: 0,
        },
    )
    .expect("export succeeds");
    db.close(Duration::from_secs(5)).expect("close");

    // SIMULATE BRUTAL POWER LOSS: First sector (Page 0, bytes 0..4096) is zeroed out
    let mut file_bytes = fs::read(&env_path).expect("read envelope");
    assert!(file_bytes.len() >= 8192);
    file_bytes[0..ENVELOPE_HEADER_SIZE].fill(0);
    fs::write(&env_path, &file_bytes).expect("write corrupted file");

    // The front header is now completely destroyed
    assert!(!EnvelopeManager::is_enveloped_file(&env_path));

    // Run self-healing recovery: the engine must detect the invalid Front Header,
    // locate the intact Rear Header sealed at the end of the file, verify its BLAKE3 hash,
    // and restore Sector 0 cleanly!
    let outcome =
        EnvelopeManager::verify_and_heal_file(&env_path).expect("verify and heal succeeds");

    assert!(matches!(
        outcome,
        Some(EnvelopeRecoveryOutcome::FrontRepairedFromRear { .. })
    ));

    // Sector 0 is now revived!
    assert!(EnvelopeManager::is_enveloped_file(&env_path));

    // Confirm database unpacks and runs cleanly with zero data loss
    let header = Database::import_envelope(&env_path, &restored_path).expect("import succeeds");
    assert_eq!(header.compression.algorithm, CompressionAlgorithm::Deflate);

    let healed_db = Database::open(DatabaseOptions::durable(&restored_path)).expect("open db");
    let doc = healed_db
        .json_get("users", "user_42")
        .expect("get succeeds")
        .expect("doc exists");
    assert_eq!(doc.extract_str("name"), Some("Ada Lovelace"));
}

#[test]
fn test_crash_recovery_corrupted_rear_header() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("source.lilia");
    let env_path = dir.path().join("corrupted_rear.lenv");
    let restored_path = dir.path().join("healed_rear.lilia");

    create_sample_database(&db_path);

    let db = Database::open(DatabaseOptions::durable(&db_path)).expect("open");
    db.export_envelope(&env_path, CompressionProfile::default())
        .expect("export succeeds");
    db.close(Duration::from_secs(5)).expect("close");

    // SIMULATE CRASH DURING TRAILER WRITE: Last 4096 bytes corrupted with random bytes
    let mut file_bytes = fs::read(&env_path).expect("read envelope");
    let len = file_bytes.len();
    file_bytes[(len - ENVELOPE_HEADER_SIZE)..len].fill(0xDE);
    fs::write(&env_path, &file_bytes).expect("write corrupted file");

    // Self-healing recovery: Engine validates Front Header, detects corrupted Rear Header,
    // and automatically mirrors Front into Rear!
    let outcome =
        EnvelopeManager::verify_and_heal_file(&env_path).expect("verify and heal succeeds");

    assert!(matches!(
        outcome,
        Some(EnvelopeRecoveryOutcome::RearRepairedFromFront { .. })
    ));

    // Confirm that the healed file opens and restores with full fidelity
    let header = Database::import_envelope(&env_path, &restored_path).expect("import succeeds");
    assert_eq!(header.compression.algorithm, CompressionAlgorithm::None);

    let healed_db = Database::open(DatabaseOptions::durable(&restored_path)).expect("open db");
    let val = healed_db
        .kv_get("system", b"maintenance_mode")
        .expect("get succeeds")
        .expect("key exists");
    assert_eq!(val.value, b"false");
}

#[test]
fn test_crash_recovery_interrupted_generation_bump() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file_path = dir.path().join("generation_desync.lenv");

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&file_path)
        .expect("create file");

    let mut envelope = DualHeaderEnvelope::create(
        file,
        EnvelopeConfig {
            schema_version: 4,
            database_id: Some("gen-desync-test".into()),
            ..Default::default()
        },
    )
    .expect("create envelope");

    envelope
        .write_payload(0, b"interrupted write")
        .expect("write payload");
    envelope.commit_sync(17, None).expect("commit sync"); // generation 2

    drop(envelope);

    // Simulate crash after Front header was updated to generation 3, but before Rear was written
    let mut data = fs::read(&file_path).expect("read file");
    let mut front_buf = [0u8; ENVELOPE_HEADER_SIZE];
    front_buf.copy_from_slice(&data[0..ENVELOPE_HEADER_SIZE]);
    let mut header = EnvelopeHeader::deserialize_from(&front_buf).expect("parse front");
    header.generation = 3;
    header.serialize_into(&mut front_buf);
    data[0..ENVELOPE_HEADER_SIZE].copy_from_slice(&front_buf);
    fs::write(&file_path, &data).expect("write desync file");

    // On boot, verify_and_heal detects Front generation 3 > Rear generation 2,
    // and synchronizes Rear to generation 3!
    let outcome =
        EnvelopeManager::verify_and_heal_file(&file_path).expect("verify and heal succeeds");

    assert_eq!(
        outcome,
        Some(EnvelopeRecoveryOutcome::RearSyncedToNewerFront {
            old_generation: 2,
            new_generation: 3,
        })
    );

    // Verify both headers are now at generation 3
    let verified_file = File::open(&file_path).expect("open verified");
    let (env, final_outcome) =
        DualHeaderEnvelope::open_and_recover(verified_file).expect("open and recover succeeds");
    assert_eq!(
        final_outcome,
        EnvelopeRecoveryOutcome::NominalSynced { generation: 3 }
    );
    assert_eq!(env.header().generation, 3);
}

#[test]
fn test_tamper_proofing_blake3_strict_rejection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file_path = dir.path().join("tampered.lenv");

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&file_path)
        .expect("create file");

    let mut envelope = DualHeaderEnvelope::create(file, EnvelopeConfig::default()).unwrap();
    envelope.commit_sync(0, None).unwrap();
    drop(envelope);

    let mut data = fs::read(&file_path).expect("read");
    let len = data.len();

    // Tamper with both headers by flipping a bit in the schema version
    data[88] ^= 0x01;
    data[len - ENVELOPE_HEADER_SIZE + 88] ^= 0x01;
    fs::write(&file_path, &data).expect("write");

    // Both headers fail BLAKE3 integrity check: must strictly refuse with ErrorCode::Corrupt
    let res = EnvelopeManager::verify_and_heal_file(&file_path);
    assert!(res.is_err());
    let err = res.err().unwrap();
    assert_eq!(err.code, ErrorCode::Corrupt);
}

#[test]
fn test_encryption_metadata_zero_exposure() {
    let derived_key = b"cryptographically-derived-aes-key";
    let salt = [0x55u8; 32];
    let nonce = [0x99u8; 16];
    let check_val = EncryptionMetadata::compute_check_value(derived_key);

    let encryption = EncryptionMetadata {
        cipher: CipherAlgorithm::Aes256Gcm,
        kdf: KdfAlgorithm::Pbkdf2HmacSha256,
        kdf_iterations: 150_000,
        salt,
        nonce_prefix: nonce,
        key_check_value: check_val,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let file_path = dir.path().join("secure.lenv");

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&file_path)
        .expect("create file");

    let mut envelope = DualHeaderEnvelope::create(
        file,
        EnvelopeConfig {
            encryption,
            ..Default::default()
        },
    )
    .expect("create");
    envelope.commit_sync(0, None).expect("commit");
    drop(envelope);

    let file_bytes = fs::read(&file_path).expect("read file");

    // CRITICAL: Ensure the private derived key NEVER appears anywhere in the 4K header!
    assert!(
        !file_bytes
            .windows(derived_key.len())
            .any(|w| w == derived_key),
        "derived encryption key must never be persisted in plaintext"
    );

    // Verify key check validation logic
    let opened = File::open(&file_path).expect("open");
    let (env, _) = DualHeaderEnvelope::open_and_recover(opened).expect("recover");
    assert!(env.header().encryption.verify_key(derived_key));
    assert!(!env.header().encryption.verify_key(b"wrong-password-key"));
}

#[test]
fn test_backward_compatibility_raw_sqlite() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("legacy.lilia");

    create_sample_database(&db_path);

    // Standard SQLite file: starts with "SQLite format 3\0", NOT "LILIADBF"
    assert!(!EnvelopeManager::is_enveloped_file(&db_path));

    // verify_and_heal_file must gracefully ignore standard SQLite files without modifying them
    let outcome = EnvelopeManager::verify_and_heal_file(&db_path).expect("verify succeeds");
    assert!(outcome.is_none());

    // Standard opening succeeds without issue
    let db = Database::open(DatabaseOptions::durable(&db_path)).expect("open db");
    let user = db
        .json_get("users", "user_42")
        .expect("get")
        .expect("user exists");
    assert_eq!(user.extract_str("name"), Some("Ada Lovelace"));
}
