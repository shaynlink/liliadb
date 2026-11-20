#![allow(unsafe_code)]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use lilia_core::json::{access_archived_doc, access_archived_doc_unchecked, JsonDocument};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq))]
pub struct BenchProfile {
    pub name: String,
    pub email: String,
    pub active: bool,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq))]
pub struct BenchUser {
    pub id: u64,
    pub profile: BenchProfile,
    pub tags: Vec<String>,
}

fn bench_zero_copy_comparison(c: &mut Criterion) {
    let user = BenchUser {
        id: 1001,
        profile: BenchProfile {
            name: "Ada Lovelace".to_string(),
            email: "ada@lovelace.org".to_string(),
            active: true,
            score: 99.75,
        },
        tags: vec![
            "algorithm".to_string(),
            "computing".to_string(),
            "mathematics".to_string(),
            "pioneer".to_string(),
        ],
    };

    // 1. Prepare JSON bytes
    let json_bytes = serde_json::to_vec(&user).expect("json serialize");

    // 2. Prepare MessagePack bytes
    let msgpack_bytes = rmp_serde::to_vec_named(&user).expect("msgpack serialize");

    // 3. Prepare Dynamic JsonDocument bytes
    let json_val = serde_json::to_value(&user).expect("json value");
    let dynamic_doc = JsonDocument::from_serde(&json_val);
    let dynamic_rkyv_bytes = dynamic_doc.to_archived_bytes().expect("dynamic rkyv bytes");

    // 4. Prepare Typed rkyv bytes
    let typed_rkyv_bytes = lilia_core::archive_to_bytes(&user).expect("typed rkyv bytes");

    let mut group = c.benchmark_group("deserialization_and_field_access");

    // Baseline: serde_json parse & extract
    group.bench_function("1_serde_json_typed", |b| {
        b.iter(|| {
            let u: BenchUser = serde_json::from_slice(black_box(&json_bytes)).unwrap();
            let email = black_box(&u.profile.email);
            black_box(email);
        });
    });

    group.bench_function("2_rmp_serde_typed", |b| {
        b.iter(|| {
            let u: BenchUser = rmp_serde::from_slice(black_box(&msgpack_bytes)).unwrap();
            let email = black_box(&u.profile.email);
            black_box(email);
        });
    });

    // Dynamic JsonDocument: validated bytecheck & path extract
    group.bench_function("3_rkyv_dynamic_doc_validated", |b| {
        b.iter(|| {
            let doc = access_archived_doc(black_box(&dynamic_rkyv_bytes)).unwrap();
            let email = doc.extract_str("profile.email").unwrap();
            black_box(email);
        });
    });

    // Dynamic JsonDocument: unchecked 0 ns cast & path extract
    group.bench_function("4_rkyv_dynamic_doc_unchecked", |b| {
        b.iter(|| {
            let doc = unsafe { access_archived_doc_unchecked(black_box(&dynamic_rkyv_bytes)) };
            let email = doc.extract_str("profile.email").unwrap();
            black_box(email);
        });
    });

    // Typed rkyv: validated bytecheck & direct relative pointer resolution
    group.bench_function("5_rkyv_typed_validated", |b| {
        b.iter(|| {
            let u = lilia_core::access_archived::<BenchUser>(black_box(&typed_rkyv_bytes)).unwrap();
            let email = u.profile.email.as_str();
            black_box(email);
        });
    });

    // Typed rkyv: 0 ns unchecked cast in trusted memory (1 machine instruction!)
    group.bench_function("6_rkyv_typed_unchecked_0ns", |b| {
        b.iter(|| {
            let u = unsafe { lilia_core::access_archived_unchecked::<BenchUser>(black_box(&typed_rkyv_bytes)) };
            let email = u.profile.email.as_str();
            black_box(email);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_zero_copy_comparison);
criterion_main!(benches);
