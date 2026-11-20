#![allow(unsafe_code)]

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

    // Populate typed rkyv record
    #[derive(Debug, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
    #[rkyv(crate = rkyv, derive(Debug, PartialEq))]
    struct ShmBenchUser {
        pub name: String,
        pub role: String,
        pub status: String,
    }

    let bench_user = ShmBenchUser {
        name: "Ada Lovelace".to_string(),
        role: "Engineer".to_string(),
        status: "Active".to_string(),
    };
    engine
        .put_archived("shm_users", b"user:1001", &bench_user, 1, None)
        .expect("put archived user");

    group.bench_function("rkyv_shm_typed_get_0ns", |b| {
        b.iter(|| {
            let archived = reader
                .get_archived::<ShmBenchUser>("shm_users", black_box(b"user:1001"))
                .expect("must find archived user");
            let role = archived.role.as_str();
            black_box(role);
        });
    });

    // Populate dynamic rkyv JsonDocument
    let rkyv_doc = lilia_core::json::JsonDocument::from_serde(&json_val);
    engine
        .put_archived_doc("shm_docs", "1001", &rkyv_doc, 1, None)
        .expect("put archived doc");

    group.bench_function("rkyv_shm_doc_extract_str_0ns", |b| {
        b.iter(|| {
            let name = reader
                .json_extract_str_zero_copy("shm_docs", black_box("1001"), black_box("name"))
                .expect("must extract name");
            black_box(name);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_shm_read_latency);
criterion_main!(benches);
