#![allow(unsafe_code)]

use std::hint::black_box;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};
use lilia_core::json::{access_archived_doc_unchecked, JsonDocument};
use lilia_core::{BatchOperation, CollectionKind, OverflowPolicy, RingBufferConfig};
use lilia_shm::ShmOptions;
use lilia_storage_sqlite::{Database, DatabaseOptions};

#[derive(Debug, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq))]
pub struct BenchDeviceRecord {
    pub device_id: u64,
    pub location: String,
    pub active: bool,
    pub reading: f64,
}

fn bench_json_read_tiers(c: &mut Criterion) {
    let temp = tempfile::tempdir().expect("tempdir");

    // 1. Setup Disk Database (flushed to SQLite)
    let disk_path = temp.path().join("bench_disk.lilia");
    let db_disk = Database::open(DatabaseOptions::durable(&disk_path)).expect("db disk");
    db_disk
        .batch(&[BatchOperation::JsonPut {
            space: "json_space".into(),
            id: "doc:disk".into(),
            value: serde_json::json!({
                "device_id": 10001,
                "location": "datacenter-us-east-1",
                "telemetry": {
                    "reading": 98.6,
                    "active": true
                }
            }),
            if_version: None,
        }])
        .expect("put disk");
    db_disk.flush_writes().expect("flush disk");

    // 2. Setup Buffered Database (PendingOverlay in AlignedVec, NOT flushed)
    let buf_path = temp.path().join("bench_buf.lilia");
    let db_buffered = Database::open(
        DatabaseOptions::durable(&buf_path)
            .with_ring_buffer(RingBufferConfig {
                capacity: 16_384,
                max_batch_size: 2_048,
                flush_interval_ms: 120_000, // keep in overlay
                watermark_trigger: 16_384,
                overflow_policy: OverflowPolicy::Block {
                    timeout_ms: Some(2_000),
                },
                coalesce_timeseries: true,
                coalesce_kv_writes: true,
                enable_read_overlay: true,
            })
            .with_buffered_collection(CollectionKind::Json, "buf_space"),
    )
    .expect("db buffered");
    db_buffered
        .batch(&[BatchOperation::JsonPut {
            space: "buf_space".into(),
            id: "doc:buf".into(),
            value: serde_json::json!({
                "device_id": 10002,
                "location": "datacenter-eu-west-1",
                "telemetry": {
                    "reading": 98.6,
                    "active": true
                }
            }),
            if_version: None,
        }])
        .expect("put buf");

    // 3. Setup SHM Database (SHM engine cache hit)
    let shm_dir = temp.path().join("bench_shm.lilia");
    let shm_path = temp.path().join("bench_shm.lilia.shm");
    let db_shm = Database::open(
        DatabaseOptions::durable(&shm_dir).with_shared_memory(ShmOptions::file_backed(&shm_path)),
    )
    .expect("db shm");
    db_shm
        .batch(&[BatchOperation::JsonPut {
            space: "shm_space".into(),
            id: "doc:shm".into(),
            value: serde_json::json!({
                "device_id": 10003,
                "location": "datacenter-ap-northeast-1",
                "telemetry": {
                    "reading": 98.6,
                    "active": true
                }
            }),
            if_version: None,
        }])
        .expect("put shm");

    let mut group = c.benchmark_group("sqlite_engine_json_read_tiers");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));

    // Tier 1: Disk JSON Get (serde_json)
    group.bench_function("1_disk_json_get_serde", |b| {
        b.iter(|| {
            let entry = db_disk
                .json_get("json_space", black_box("doc:disk"))
                .unwrap()
                .unwrap();
            let reading = entry.value()["telemetry"]["reading"].as_f64().unwrap();
            black_box(reading);
        });
    });

    // Tier 2: Disk with_archived_json
    group.bench_function("2_disk_with_archived_json", |b| {
        b.iter(|| {
            let reading = db_disk
                .with_archived_json("json_space", black_box("doc:disk"), |archived| {
                    archived.extract_f64("telemetry.reading")
                })
                .unwrap()
                .unwrap();
            black_box(reading);
        });
    });

    // Tier 3: PendingOverlay (AlignedVec 0 ns cast)
    group.bench_function("3_overlay_with_archived_json_0ns", |b| {
        b.iter(|| {
            let reading = db_buffered
                .with_archived_json("buf_space", black_box("doc:buf"), |archived| {
                    archived.extract_f64("telemetry.reading")
                })
                .unwrap()
                .unwrap();
            black_box(reading);
        });
    });

    // Tier 3b: PendingOverlay view (0 ns)
    group.bench_function("3b_overlay_with_archived_json_view_0ns", |b| {
        b.iter(|| {
            let reading = db_buffered
                .with_archived_json_view("buf_space", black_box("doc:buf"), |view| {
                    view.extract_f64("telemetry.reading")
                })
                .unwrap()
                .unwrap();
            black_box(reading);
        });
    });

    // Tier 4: SHM Engine Fast-Path (< 1 ns)
    group.bench_function("4_shm_with_archived_json_0ns", |b| {
        b.iter(|| {
            let reading = db_shm
                .with_archived_json("shm_space", black_box("doc:shm"), |archived| {
                    archived.extract_f64("telemetry.reading")
                })
                .unwrap()
                .unwrap();
            black_box(reading);
        });
    });

    group.finish();
}

fn bench_kv_read_tiers(c: &mut Criterion) {
    let temp = tempfile::tempdir().expect("tempdir");

    let dev = BenchDeviceRecord {
        device_id: 10001,
        location: "datacenter-us-east-1".to_string(),
        active: true,
        reading: 98.6,
    };
    let dev_bytes = lilia_core::archive_to_bytes(&dev).unwrap().to_vec();

    // 1. Disk DB
    let disk_path = temp.path().join("bench_kv_disk.lilia");
    let db_disk = Database::open(DatabaseOptions::durable(&disk_path)).expect("db disk");
    db_disk
        .batch(&[BatchOperation::KvSet {
            namespace: "kv_space".into(),
            key: b"dev:1".to_vec(),
            value: dev_bytes.clone(),
            if_version: None,
            expires_at_ms: None,
        }])
        .expect("put kv disk");
    db_disk.flush_writes().expect("flush disk");

    // 2. Buffered DB
    let buf_path = temp.path().join("bench_kv_buf.lilia");
    let db_buffered = Database::open(
        DatabaseOptions::durable(&buf_path)
            .with_ring_buffer(RingBufferConfig {
                capacity: 16_384,
                max_batch_size: 2_048,
                flush_interval_ms: 120_000,
                watermark_trigger: 16_384,
                overflow_policy: OverflowPolicy::Block {
                    timeout_ms: Some(2_000),
                },
                coalesce_timeseries: true,
                coalesce_kv_writes: true,
                enable_read_overlay: true,
            })
            .with_buffered_kv("buf_kv"),
    )
    .expect("db buffered");
    db_buffered
        .batch(&[BatchOperation::KvSet {
            namespace: "buf_kv".into(),
            key: b"dev:1".to_vec(),
            value: dev_bytes.clone(),
            if_version: None,
            expires_at_ms: None,
        }])
        .expect("put kv buf");

    // 3. SHM DB
    let shm_dir = temp.path().join("bench_kv_shm.lilia");
    let shm_path = temp.path().join("bench_kv_shm.lilia.shm");
    let db_shm = Database::open(
        DatabaseOptions::durable(&shm_dir).with_shared_memory(ShmOptions::file_backed(&shm_path)),
    )
    .expect("db shm");
    db_shm
        .batch(&[BatchOperation::KvSet {
            namespace: "shm_kv".into(),
            key: b"dev:1".to_vec(),
            value: dev_bytes,
            if_version: None,
            expires_at_ms: None,
        }])
        .expect("put kv shm");

    let mut group = c.benchmark_group("sqlite_engine_kv_read_tiers");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));

    group.bench_function("1_disk_kv_get", |b| {
        b.iter(|| {
            let entry = db_disk.kv_get("kv_space", black_box(b"dev:1")).unwrap().unwrap();
            let archived = lilia_core::access_archived::<BenchDeviceRecord>(&entry.value).unwrap();
            black_box(archived.device_id.to_native());
        });
    });

    group.bench_function("2_disk_with_archived_kv", |b| {
        b.iter(|| {
            let id = db_disk
                .with_archived_kv::<BenchDeviceRecord, _, _>("kv_space", black_box(b"dev:1"), |archived| {
                    archived.device_id.to_native()
                })
                .unwrap()
                .unwrap();
            black_box(id);
        });
    });

    group.bench_function("3_overlay_with_archived_kv_0ns", |b| {
        b.iter(|| {
            let id = db_buffered
                .with_archived_kv::<BenchDeviceRecord, _, _>("buf_kv", black_box(b"dev:1"), |archived| {
                    archived.device_id.to_native()
                })
                .unwrap()
                .unwrap();
            black_box(id);
        });
    });

    group.bench_function("4_shm_with_archived_kv_0ns", |b| {
        b.iter(|| {
            let id = db_shm
                .with_archived_kv::<BenchDeviceRecord, _, _>("shm_kv", black_box(b"dev:1"), |archived| {
                    archived.device_id.to_native()
                })
                .unwrap()
                .unwrap();
            black_box(id);
        });
    });

    group.finish();
}

fn bench_in_situ_json_path_extraction(c: &mut Criterion) {
    let json_val = serde_json::json!({
        "device_id": 10001,
        "location": "datacenter-us-east-1",
        "telemetry": {
            "reading": 98.6,
            "active": true
        }
    });
    let json_bytes = serde_json::to_vec(&json_val).unwrap();
    let rkyv_doc = JsonDocument::from_serde(&json_val);
    let rkyv_bytes = rkyv_doc.to_archived_bytes().unwrap();

    let mut group = c.benchmark_group("in_situ_json_path_extraction");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));

    group.bench_function("1_serde_json_parse_and_pointer", |b| {
        b.iter(|| {
            let v: serde_json::Value = serde_json::from_slice(black_box(&json_bytes)).unwrap();
            let reading = v.pointer("/telemetry/reading").unwrap().as_f64().unwrap();
            black_box(reading);
        });
    });

    group.bench_function("2_rkyv_doc_extract_nested_0ns", |b| {
        b.iter(|| {
            let archived = unsafe { access_archived_doc_unchecked(black_box(&rkyv_bytes)) };
            let reading = archived.extract_f64("telemetry.reading").unwrap();
            black_box(reading);
        });
    });

    group.finish();
}

fn bench_kv_write(c: &mut Criterion) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let database = Database::open(DatabaseOptions::durable(directory.path().join("bench.lilia"))).expect("database");
    let mut sequence = 0_u64;

    let mut group = c.benchmark_group("storage_write");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));

    group.bench_function("kv_set_1k", |benchmark| {
        benchmark.iter(|| {
            sequence += 1;
            database
                .batch(&[BatchOperation::KvSet {
                    namespace: "bench".into(),
                    key: sequence.to_be_bytes().to_vec(),
                    value: vec![42; 1_024],
                    if_version: None,
                    expires_at_ms: None,
                }])
                .expect("write");
            black_box(sequence);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_json_read_tiers,
    bench_kv_read_tiers,
    bench_in_situ_json_path_extraction,
    bench_kv_write
);
criterion_main!(benches);
