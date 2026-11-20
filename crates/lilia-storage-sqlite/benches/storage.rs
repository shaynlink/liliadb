use criterion::{criterion_group, criterion_main, Criterion};
use lilia_core::BatchOperation;
use lilia_storage_sqlite::{Database, DatabaseOptions};
use std::hint::black_box;

fn kv_write(c: &mut Criterion) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let database = Database::open(DatabaseOptions::durable(
        directory.path().join("bench.lilia"),
    ))
    .expect("database");
    let mut sequence = 0_u64;
    c.bench_function("kv_set_1k", |benchmark| {
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

    #[derive(Debug, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
    #[rkyv(crate = rkyv, derive(Debug, PartialEq))]
    struct BenchDeviceRecord {
        pub device_id: u64,
        pub location: String,
        pub active: bool,
        pub reading: f64,
    }

    let dev = BenchDeviceRecord {
        device_id: 10001,
        location: "datacenter-us-east-1".to_string(),
        active: true,
        reading: 98.6,
    };

    database
        .typed_put("bench_typed", b"dev:1", &dev, None)
        .expect("typed put initial");

    c.bench_function("with_archived_kv_0ns", |b| {
        b.iter(|| {
            let res = database
                .with_archived_kv::<BenchDeviceRecord, _, _>(
                    "bench_typed",
                    black_box(b"dev:1"),
                    |archived| {
                        let loc_len = archived.location.as_str().len();
                        let reading = archived.reading.to_native();
                        black_box((loc_len, reading))
                    },
                )
                .expect("read archived kv");
            black_box(res);
        });
    });

    database
        .batch(&[BatchOperation::JsonPut {
            space: "bench_json".into(),
            id: "doc:1".into(),
            value: serde_json::json!({
                "sensor": "accelerometer",
                "telemetry": {
                    "x": 0.12,
                    "y": -0.84,
                    "z": 9.81
                }
            }),
            if_version: None,
        }])
        .expect("json put");

    c.bench_function("with_archived_json_0ns", |b| {
        b.iter(|| {
            let val = database
                .with_archived_json("bench_json", black_box("doc:1"), |archived| {
                    let z = archived.extract_f64("telemetry.z");
                    black_box(z)
                })
                .expect("with archived json");
            black_box(val);
        });
    });
}

criterion_group!(benches, kv_write);
criterion_main!(benches);
