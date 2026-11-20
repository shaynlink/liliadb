#![allow(unsafe_code)]

use std::hint::black_box;
use std::io::Cursor;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};
use lilia_core::BatchOperation;
use lilia_protocol::{read_archived_frame, read_archived_frame_unchecked, read_frame, Operation, Request};
use rkyv::util::AlignedVec;
use tokio::runtime::Runtime;

fn create_bench_request() -> Request {
    Request {
        id: "req_018f0000-0000-7000-8000-000000000001".into(),
        token: "bearer_token_secret_12345".into(),
        deadline_ms: Some(5_000),
        operation: Operation::Batch {
            operations: vec![BatchOperation::JsonPut {
                space: "devices".into(),
                id: "sensor-alpha-99".into(),
                value: serde_json::json!({
                    "hardware_rev": "v2.4",
                    "firmware": "1.0.8-prod",
                    "telemetry": {
                        "temperature_c": 41.8,
                        "pressure_kpa": 101.325,
                        "active": true
                    },
                    "tags": ["datacenter", "critical", "gateway"]
                }),
                if_version: None,
            }],
        },
    }
}

fn bench_protocol_framing(c: &mut Criterion) {
    let req = create_bench_request();

    // 1. Pre-serialize for deserialization benchmarks
    let rkyv_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&req).expect("serialize rkyv");
    let rmp_bytes = rmp_serde::to_vec_named(&req).expect("serialize rmp");
    let json_bytes = serde_json::to_vec(&req).expect("serialize json");

    // Pre-build stream frame buffers (4-byte length prefix + payload)
    let mut rkyv_framed = Vec::with_capacity(rkyv_bytes.len() + 4);
    rkyv_framed.extend_from_slice(&(rkyv_bytes.len() as u32).to_be_bytes());
    rkyv_framed.extend_from_slice(&rkyv_bytes);

    let mut rmp_framed = Vec::with_capacity(rmp_bytes.len() + 4);
    rmp_framed.extend_from_slice(&(rmp_bytes.len() as u32).to_be_bytes());
    rmp_framed.extend_from_slice(&rmp_bytes);

    let mut json_framed = Vec::with_capacity(json_bytes.len() + 4);
    json_framed.extend_from_slice(&(json_bytes.len() as u32).to_be_bytes());
    json_framed.extend_from_slice(&json_bytes);

    // ==========================================
    // Group 1: Frame Serialization
    // ==========================================
    let mut g1 = c.benchmark_group("protocol_frame_serialization");
    g1.warm_up_time(Duration::from_millis(500));
    g1.measurement_time(Duration::from_secs(2));

    g1.bench_function("1_serde_json_serialize", |b| {
        b.iter(|| {
            let encoded = serde_json::to_vec(black_box(&req)).unwrap();
            black_box(encoded);
        });
    });

    g1.bench_function("2_rmp_serde_serialize", |b| {
        b.iter(|| {
            let encoded = rmp_serde::to_vec_named(black_box(&req)).unwrap();
            black_box(encoded);
        });
    });

    g1.bench_function("3_rkyv_frame_serialize", |b| {
        b.iter(|| {
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(black_box(&req)).unwrap();
            black_box(bytes);
        });
    });

    g1.finish();

    // ==========================================
    // Group 2: Frame Deserialization & Field Read
    // ==========================================
    let mut g2 = c.benchmark_group("protocol_frame_deserialization");
    g2.warm_up_time(Duration::from_millis(500));
    g2.measurement_time(Duration::from_secs(2));

    g2.bench_function("1_serde_json_read_and_field_access", |b| {
        b.iter(|| {
            let decoded: Request = serde_json::from_slice(black_box(&json_bytes)).unwrap();
            let token = black_box(&decoded.token);
            black_box(token);
        });
    });

    g2.bench_function("2_rmp_serde_read_and_field_access", |b| {
        b.iter(|| {
            let decoded: Request = rmp_serde::from_slice(black_box(&rmp_bytes)).unwrap();
            let token = black_box(&decoded.token);
            black_box(token);
        });
    });

    g2.bench_function("3_rkyv_read_frame_owned", |b| {
        b.iter(|| {
            let decoded: Request = rkyv::from_bytes::<Request, rkyv::rancor::Error>(black_box(&rkyv_bytes)).unwrap();
            let token = black_box(&decoded.token);
            black_box(token);
        });
    });

    g2.bench_function("4_rkyv_read_archived_validated", |b| {
        b.iter(|| {
            let archived = lilia_core::access_archived::<Request>(black_box(&rkyv_bytes)).unwrap();
            let token = archived.token.as_str();
            black_box(token);
        });
    });

    g2.bench_function("5_rkyv_read_archived_unchecked_0ns", |b| {
        b.iter(|| {
            let archived = unsafe { lilia_core::access_archived_unchecked::<Request>(black_box(&rkyv_bytes)) };
            let token = archived.token.as_str();
            black_box(token);
        });
    });

    g2.finish();

    // ==========================================
    // Group 3: Async Stream Frame Ingestion
    // ==========================================
    let rt = Runtime::new().expect("tokio runtime");
    let mut g3 = c.benchmark_group("protocol_async_stream_read");
    g3.warm_up_time(Duration::from_millis(500));
    g3.measurement_time(Duration::from_secs(2));

    g3.bench_function("1_async_read_frame_owned", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(&rkyv_framed);
            rt.block_on(async {
                let req: Request = read_frame(&mut cursor).await.unwrap();
                black_box(req.token);
            });
        });
    });

    g3.bench_function("2_async_read_archived_frame_validated", |b| {
        let mut buffer = AlignedVec::new();
        b.iter(|| {
            let mut cursor = Cursor::new(&rkyv_framed);
            rt.block_on(async {
                let archived = read_archived_frame::<Request>(&mut cursor, &mut buffer).await.unwrap();
                let token = archived.token.as_str();
                black_box(token);
            });
        });
    });

    g3.bench_function("3_async_read_archived_frame_unchecked_0ns", |b| {
        let mut buffer = AlignedVec::new();
        b.iter(|| {
            let mut cursor = Cursor::new(&rkyv_framed);
            rt.block_on(async {
                let archived = read_archived_frame_unchecked::<Request>(&mut cursor, &mut buffer).await.unwrap();
                let token = archived.token.as_str();
                black_box(token);
            });
        });
    });

    g3.finish();
}

criterion_group!(benches, bench_protocol_framing);
criterion_main!(benches);
