use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use lilia_shm::{ShmEngine, ShmOptions};

fn bench_shm_read_latency(c: &mut Criterion) {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let shm_path = temp_dir.path().join("bench.shm");

    let engine = ShmEngine::create(
        ShmOptions::file_backed(&shm_path)
            .with_size_bytes(8 * 1024 * 1024)
            .with_slot_capacity(4096),
    )
    .expect("create shm engine");

    // Populate keys
    let key = b"user:1001";
    let value = b"{\"name\":\"Ada Lovelace\",\"role\":\"Engineer\",\"status\":\"Active\"}";
    engine.put_kv("users", key, value, 1, None).expect("put kv");

    let reader = engine.create_reader().expect("create reader");

    let mut group = c.benchmark_group("shm_zero_copy");

    group.bench_function("kv_get_zero_copy", |b| {
        b.iter(|| {
            let entry = reader.get("users", black_box(key));
            let val = entry.expect("must find entry").value;
            black_box(val);
        });
    });

    // Populate JSON binary doc
    let json_val = serde_json::json!({
        "id": "1001",
        "name": "Ada Lovelace",
        "nested": {
            "score": 99.5,
            "active": true
        }
    });
    let binary_bytes =
        lilia_core::json::parse_json_slice_to_binary(&serde_json::to_vec(&json_val).unwrap())
            .unwrap();

    engine
        .put_json("users_json", "1001", binary_bytes.as_bytes(), 1, None)
        .expect("put json");

    group.bench_function("json_get_zero_copy", |b| {
        b.iter(|| {
            let entry = reader.json_get("users_json", black_box("1001"));
            let val = entry.expect("must find json").value_bytes;
            black_box(val);
        });
    });

    // Publish a 4K snapshot page
    let page_data = vec![0x42u8; 4096];
    engine.publish_page(0, &page_data, 1).expect("publish page");

    group.bench_function("page_read_zero_copy", |b| {
        b.iter(|| {
            let page = reader.read_page(black_box(0));
            let data = page.expect("must find page").data;
            black_box(data);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_shm_read_latency);
criterion_main!(benches);
