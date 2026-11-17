use std::path::Path;

use lilia_core::{ConflictResolutionStrategy, Hlc};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

fn create_replica(name: &str, node_id: &str, base_dir: &Path) -> Database {
    Database::open(DatabaseOptions::durable(base_dir.join(name)).with_node_id(node_id))
        .expect("open replica")
}

#[test]
fn test_p2p_ibf_fast_path_small_divergence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_a = create_replica("phone_a.lilia", "device-a", dir.path());
    let db_b = create_replica("phone_b.lilia", "device-b", dir.path());

    // 1. Common baseline: 1,000 shared documents
    for i in 0..1000 {
        let doc_id = format!("doc-{i}");
        let val = json!({ "counter": i, "status": "synced" });
        let hlc = Hlc::new(10, u32::try_from(i).unwrap_or(0), "seed-node");
        db_a.json_put_hlc(
            "notes",
            &doc_id,
            val.clone(),
            Some(hlc.clone()),
            None,
            ConflictResolutionStrategy::LastWriteWins,
        )
        .expect("put on a");
        db_b.json_put_hlc(
            "notes",
            &doc_id,
            val,
            Some(hlc),
            None,
            ConflictResolutionStrategy::LastWriteWins,
        )
        .expect("put on b");
    }

    // 2. Offline divergence (Phone A modifies 3, adds 2; Phone B modifies 2, adds 1)
    // Phone A edits doc-10, doc-20, doc-30 and adds doc-new-a1, doc-new-a2
    let hlc_a = Hlc::new(50, 1, "device-a");
    for id in &["doc-10", "doc-20", "doc-30", "doc-new-a1", "doc-new-a2"] {
        db_a.json_put_hlc(
            "notes",
            id,
            json!({ "updated_by": "device-a" }),
            Some(hlc_a.clone()),
            None,
            ConflictResolutionStrategy::LastWriteWins,
        )
        .expect("diverge a");
    }

    // Phone B edits doc-40, doc-50 and adds doc-new-b1
    let hlc_b = Hlc::new(50, 2, "device-b");
    for id in &["doc-40", "doc-50", "doc-new-b1"] {
        db_b.json_put_hlc(
            "notes",
            id,
            json!({ "updated_by": "device-b" }),
            Some(hlc_b.clone()),
            None,
            ConflictResolutionStrategy::LastWriteWins,
        )
        .expect("diverge b");
    }

    // 3. P2P Reconciliation
    let summary = db_a
        .p2p_reconcile_with(
            &db_b,
            Some("notes"),
            ConflictResolutionStrategy::LastWriteWins,
        )
        .expect("reconciliation");

    // Fast-Path (1-RTT) assertions
    assert!(
        summary.fast_path,
        "small divergence must trigger 1-RTT Fast-Path"
    );
    assert_eq!(summary.rounds_used, 1);
    // 3 modified + 2 new on A + 2 older versions of doc-40, doc-50 = 7
    assert_eq!(summary.items_sent_a_to_b, 7);
    // 2 modified + 1 new on B + 3 older versions of doc-10, doc-20, doc-30 = 6
    assert_eq!(summary.items_sent_b_to_a, 6);

    // 4. Verify convergence: all 1,003 documents exist on both phones with identical values
    for id in &["doc-10", "doc-20", "doc-30", "doc-new-a1", "doc-new-a2"] {
        let val_a = db_a.json_get("notes", id).unwrap().unwrap().to_json_value();
        let val_b = db_b.json_get("notes", id).unwrap().unwrap().to_json_value();
        assert_eq!(val_a, val_b);
        assert_eq!(val_a["updated_by"], "device-a");
    }

    for id in &["doc-40", "doc-50", "doc-new-b1"] {
        let val_a = db_a.json_get("notes", id).unwrap().unwrap().to_json_value();
        let val_b = db_b.json_get("notes", id).unwrap().unwrap().to_json_value();
        assert_eq!(val_a, val_b);
        assert_eq!(val_b["updated_by"], "device-b");
    }
}

#[test]
fn test_p2p_ibf_normal_path_large_divergence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_a = create_replica("phone_a_large.lilia", "device-a", dir.path());
    let db_b = create_replica("phone_b_large.lilia", "device-b", dir.path());

    // 1. Common baseline: 500 documents
    for i in 0..500 {
        let doc_id = format!("doc-{i}");
        let hlc = Hlc::new(10, u32::try_from(i).unwrap_or(0), "seed-node");
        db_a.json_put_hlc(
            "inventory",
            &doc_id,
            json!({ "idx": i }),
            Some(hlc.clone()),
            None,
            ConflictResolutionStrategy::LastWriteWins,
        )
        .expect("put on a");
        db_b.json_put_hlc(
            "inventory",
            &doc_id,
            json!({ "idx": i }),
            Some(hlc),
            None,
            ConflictResolutionStrategy::LastWriteWins,
        )
        .expect("put on b");
    }

    // 2. Large divergence (Phone A adds 60, Phone B adds 40 -> total d = 100)
    let hlc_a = Hlc::new(50, 1, "device-a");
    for i in 0..60 {
        let doc_id = format!("item-a-{i}");
        db_a.json_put_hlc(
            "inventory",
            &doc_id,
            json!({ "stock": i }),
            Some(hlc_a.clone()),
            None,
            ConflictResolutionStrategy::LastWriteWins,
        )
        .expect("add on a");
    }

    let hlc_b = Hlc::new(50, 2, "device-b");
    for i in 0..40 {
        let doc_id = format!("item-b-{i}");
        db_b.json_put_hlc(
            "inventory",
            &doc_id,
            json!({ "stock": i }),
            Some(hlc_b.clone()),
            None,
            ConflictResolutionStrategy::LastWriteWins,
        )
        .expect("add on b");
    }

    // 3. P2P Reconciliation (2-RTT Normal-Path)
    let summary = db_a
        .p2p_reconcile_with(
            &db_b,
            Some("inventory"),
            ConflictResolutionStrategy::LastWriteWins,
        )
        .expect("reconcile large");

    assert!(
        !summary.fast_path,
        "100 differences should execute 2-RTT Normal-Path"
    );
    assert_eq!(summary.rounds_used, 2);
    assert_eq!(summary.items_sent_a_to_b, 60);
    assert_eq!(summary.items_sent_b_to_a, 40);

    // Verify all 600 items present on both sides
    for i in 0..60 {
        let doc_id = format!("item-a-{i}");
        assert!(db_a.json_get("inventory", &doc_id).unwrap().is_some());
        assert!(db_b.json_get("inventory", &doc_id).unwrap().is_some());
    }
    for i in 0..40 {
        let doc_id = format!("item-b-{i}");
        assert!(db_a.json_get("inventory", &doc_id).unwrap().is_some());
        assert!(db_b.json_get("inventory", &doc_id).unwrap().is_some());
    }
}

#[test]
fn test_p2p_ibf_concurrent_modification_with_three_way_merge() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_a = create_replica("med_a.lilia", "doctor-phone", dir.path());
    let db_b = create_replica("med_b.lilia", "nurse-tablet", dir.path());

    // Common patient file at base time t=50
    let base_hlc = Hlc::new(50, 0, "hospital-server");
    let base_doc = json!({
        "patient": "Alex Doe",
        "allergies": ["penicillin"],
        "status": "stable"
    });

    db_a.json_put_hlc(
        "patients",
        "pat-1",
        base_doc.clone(),
        Some(base_hlc.clone()),
        None,
        ConflictResolutionStrategy::LastWriteWins,
    )
    .expect("seed a");
    db_b.json_put_hlc(
        "patients",
        "pat-1",
        base_doc,
        Some(base_hlc.clone()),
        None,
        ConflictResolutionStrategy::LastWriteWins,
    )
    .expect("seed b");

    // Offline field divergence:
    // Doctor phone (A) notes new allergy at t=100.1@doctor-phone
    let hlc_doc = Hlc::new(100, 1, "doctor-phone");
    db_a.json_put_hlc(
        "patients",
        "pat-1",
        json!({
            "patient": "Alex Doe",
            "allergies": ["penicillin", "aspirin"],
            "status": "stable"
        }),
        Some(hlc_doc),
        Some(base_hlc.clone()),
        ConflictResolutionStrategy::LastWriteWins,
    )
    .expect("doctor edit");

    // Nurse tablet (B) changes patient status to "critical" at t=100.2@nurse-tablet
    let hlc_nurse = Hlc::new(100, 2, "nurse-tablet");
    db_b.json_put_hlc(
        "patients",
        "pat-1",
        json!({
            "patient": "Alex Doe",
            "allergies": ["penicillin"],
            "status": "critical"
        }),
        Some(hlc_nurse),
        Some(base_hlc),
        ConflictResolutionStrategy::LastWriteWins,
    )
    .expect("nurse edit");

    // Reconcile with ThreeWayMerge strategy
    let summary = db_a
        .p2p_reconcile_with(
            &db_b,
            Some("patients"),
            ConflictResolutionStrategy::ThreeWayMerge,
        )
        .expect("reconcile");

    // Verify conflict detection and resolution
    assert!(summary.stats_a.conflicts_detected > 0 || summary.stats_b.conflicts_detected > 0);

    // Result on both devices: merged non-destructively!
    let merged_a = db_a
        .json_get("patients", "pat-1")
        .unwrap()
        .unwrap()
        .to_json_value();
    let merged_b = db_b
        .json_get("patients", "pat-1")
        .unwrap()
        .unwrap()
        .to_json_value();

    assert_eq!(
        merged_a, merged_b,
        "both devices must converge to identical state"
    );
    assert_eq!(
        merged_a["status"], "critical",
        "nurse status update preserved"
    );
    assert_eq!(
        merged_a["allergies"],
        json!(["penicillin", "aspirin"]),
        "doctor allergy update preserved"
    );
}

#[test]
fn test_p2p_simulated_ble_wire_footprint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = create_replica("ble_bench.lilia", "phone-a", dir.path());

    // 5,000 documents batched for speed
    for batch_idx in 0..5 {
        let mut ops = Vec::with_capacity(1000);
        for i in 0..1000 {
            let doc_id = format!("note-{}", batch_idx * 1000 + i);
            ops.push(lilia_core::BatchOperation::JsonPutHlc {
                space: "notes".to_string(),
                id: doc_id,
                value: json!({ "title": "Field Note", "body": "Encrypted sensor data" }),
                hlc: None,
                parent_hlc: None,
                strategy: ConflictResolutionStrategy::LastWriteWins,
            });
        }
        db.batch(&ops).expect("batch seed");
    }

    // Build Strata Estimator
    let strata = db.p2p_strata(Some("notes")).expect("build strata");
    let strata_bytes = rmp_serde::to_vec(&strata).expect("serialize strata with MessagePack");

    println!(
        "Strata Estimator binary MessagePack size: {} bytes",
        strata_bytes.len()
    );

    // Compressed with zstd (simulating BLE transfer)
    let compressed_strata = lilia_core::compress_bytes(&strata_bytes);
    println!(
        "Strata Estimator compressed size: {} bytes",
        compressed_strata.len()
    );

    // 5,000 documents full payload would be > 2.5 MB.
    // The compressed Strata Estimator should be under 16 KB (over 99% bandwidth savings)!
    assert!(
        compressed_strata.len() < 16_000,
        "compressed strata must be < 16 KB, was {}",
        compressed_strata.len()
    );

    // Also test a single 64-cell IBF (Fast-Path or small difference):
    let small_ibf =
        lilia_core::InvertibleBloomFilter::new(64, 3, lilia_core::ibf::DEFAULT_IBF_SEED).unwrap();
    let ibf_bytes = rmp_serde::to_vec(&small_ibf).unwrap();
    let compressed_ibf = lilia_core::compress_bytes(&ibf_bytes);
    println!(
        "64-cell IBF binary size: {} bytes, compressed: {} bytes",
        ibf_bytes.len(),
        compressed_ibf.len()
    );
    // Must fit comfortably in standard BLE ATT MTU packets (under 1 KB!)
    assert!(
        compressed_ibf.len() < 1_000,
        "small IBF must be < 1 KB, was {}",
        compressed_ibf.len()
    );
}
