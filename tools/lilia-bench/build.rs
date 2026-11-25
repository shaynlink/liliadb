//! Embeds the versions under test, read from the manifests, so every record names exactly what ran.

use std::path::Path;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let root = Path::new(&manifest_dir);
    let workspace = root.join("../../Cargo.toml");
    let lock = root.join("Cargo.lock");
    for path in [&workspace, &lock] {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-env-changed=LILIA_BENCH_ROCKSDB_FULLFSYNC");

    let lilia = std::fs::read_to_string(&workspace)
        .ok()
        .and_then(|text| {
            let section = text.split("[workspace.package]").nth(1)?.to_owned();
            let line = section.lines().find(|l| l.trim_start().starts_with("version"))?.to_owned();
            Some(line.split('"').nth(1)?.to_owned())
        })
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=LILIA_BENCH_LILIA_VERSION={lilia}");

    let locked = std::fs::read_to_string(&lock).unwrap_or_default();
    for (crate_name, var) in [
        ("redb", "REDB"),
        ("fjall", "FJALL"),
        ("heed", "HEED"),
        ("lmdb-master-sys", "LMDB_SYS"),
        ("rocksdb", "ROCKSDB"),
        ("librocksdb-sys", "LIBROCKSDB_SYS"),
        ("rusqlite", "RUSQLITE"),
        ("duckdb", "DUCKDB"),
    ] {
        println!("cargo:rustc-env=LILIA_BENCH_DEP_{var}={}", locked_version(&locked, crate_name));
    }

    let fullfsync = std::env::var("LILIA_BENCH_ROCKSDB_FULLFSYNC").is_ok_and(|v| v == "1");
    println!("cargo:rustc-env=LILIA_BENCH_ROCKSDB_FULLFSYNC={}", u8::from(fullfsync));
}

/// Version of `name` in a `Cargo.lock`, or `unknown`.
fn locked_version(lock: &str, name: &str) -> String {
    let needle = format!("name = \"{name}\"\n");
    lock.split("[[package]]")
        .find(|block| block.trim_start().starts_with(needle.trim_end()) && block.contains(&needle))
        .and_then(|block| block.lines().find_map(|l| l.strip_prefix("version = \"")))
        .map_or_else(|| "unknown".into(), |v| v.trim_end_matches('"').to_owned())
}
