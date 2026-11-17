#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Instant;

use clap::Parser;
use lilia_core::{
    Aes256GcmCodec, BatchOperation, CollectionKind, CollectionSpec, Durability, IdentityCodec,
    OverflowPolicy, RingBufferConfig, StorageTier, TimeSeriesPoint, ValueCodec,
};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(
    name = "lilia-bench",
    about = "Calibrated benchmark runner for LiliaDB native Rust engine"
)]
struct Arguments {
    /// Total number of mutations to execute
    #[arg(long, default_value_t = 10_000)]
    count: usize,

    /// Number of mutations per batch (1 = single-op transactions)
    #[arg(long, default_value_t = 1)]
    batch_size: usize,

    /// Codec to benchmark: "clear", "aes256", or "both"
    #[arg(long, default_value = "both")]
    codec: String,

    /// Durability profile: "durable", "balanced", or "performance"
    #[arg(long, default_value = "durable")]
    durability: String,

    /// Include raw `SQLite` native (C / rusqlite) baseline
    #[arg(long)]
    raw_sqlite: bool,

    /// Run comparative tiered storage suite (Disk vs Memory vs RingBuffer vs GroupSync vs IoT Coalescing)
    #[arg(long)]
    tier_suite: bool,

    /// Output results as JSON
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchMetrics {
    pub surface: String,
    pub codec: String,
    pub durability: String,
    pub total_mutations: usize,
    pub batch_size: usize,
    pub total_duration_ms: f64,
    pub throughput_ops_sec: f64,
    pub p50_us: f64,
    pub p95_us: f64,
    pub p99_us: f64,
    pub mean_us: f64,
    pub min_us: f64,
    pub max_us: f64,
    pub stddev_us: f64,
    pub disk_bytes: u64,
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (p / 100.0) * (sorted.len() - 1) as f64;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    let weight = rank - lower as f64;
    sorted[lower] * (1.0 - weight) + sorted[upper] * weight
}

fn run_single_benchmark(
    codec_name: &str,
    codec: Arc<dyn ValueCodec>,
    durability_name: &str,
    durability: Durability,
    count: usize,
    batch_size: usize,
) -> Result<BenchMetrics, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let db_path: PathBuf = temp_dir.path().join("bench.lilia");

    let mut options = DatabaseOptions::durable(&db_path).with_codec(codec);
    options.durability = durability;
    let db = Database::open(options)?;

    // 1. Warmup phase (300 mutations)
    let warmup_ops: Vec<BatchOperation> = (0..300)
        .map(|i| BatchOperation::KvSet {
            namespace: "warmup".into(),
            key: format!("k_{i}").into_bytes(),
            value: vec![42u8; 256],
            if_version: None,
            expires_at_ms: None,
        })
        .collect();
    for chunk in warmup_ops.chunks(batch_size.max(1)) {
        db.batch(chunk)?;
    }

    // 2. Timed benchmark phase
    let num_batches = count.div_ceil(batch_size);
    let mut batch_latencies_us = Vec::with_capacity(num_batches);

    let start_all = Instant::now();

    let mut generated = 0;
    for b in 0..num_batches {
        let current_batch_size = (count - generated).min(batch_size);
        let ops: Vec<BatchOperation> = (0..current_batch_size)
            .map(|i| {
                let id = b * batch_size + i;
                BatchOperation::KvSet {
                    namespace: "bench".into(),
                    key: format!("bench_key_{id:08}").into_bytes(),
                    value: vec![(id % 256) as u8; 256],
                    if_version: None,
                    expires_at_ms: None,
                }
            })
            .collect();

        let batch_start = Instant::now();
        db.batch(&ops)?;
        let elapsed = batch_start.elapsed();

        let op_latency_us = (elapsed.as_nanos() as f64) / (current_batch_size as f64 * 1_000.0);
        batch_latencies_us.push(op_latency_us);
        generated += current_batch_size;
    }

    let total_duration = start_all.elapsed();
    let total_duration_ms = total_duration.as_secs_f64() * 1_000.0;
    let throughput_ops_sec = (count as f64) / total_duration.as_secs_f64();

    // Calculate percentiles
    batch_latencies_us.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p50_us = percentile(&batch_latencies_us, 50.0);
    let p95_us = percentile(&batch_latencies_us, 95.0);
    let p99_us = percentile(&batch_latencies_us, 99.0);
    let min_us = *batch_latencies_us.first().unwrap_or(&0.0);
    let max_us = *batch_latencies_us.last().unwrap_or(&0.0);

    let sum: f64 = batch_latencies_us.iter().sum();
    let mean_us = sum / batch_latencies_us.len() as f64;
    let variance: f64 = batch_latencies_us
        .iter()
        .map(|v| (v - mean_us).powi(2))
        .sum::<f64>()
        / batch_latencies_us.len() as f64;
    let stddev_us = variance.sqrt();

    // Check disk file size
    drop(db);
    let disk_bytes = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);

    Ok(BenchMetrics {
        surface: "Rust Native Embedded".into(),
        codec: codec_name.into(),
        durability: durability_name.into(),
        total_mutations: count,
        batch_size,
        total_duration_ms,
        throughput_ops_sec,
        p50_us,
        p95_us,
        p99_us,
        mean_us,
        min_us,
        max_us,
        stddev_us,
        disk_bytes,
    })
}

fn run_raw_sqlite_benchmark(
    durability_name: &str,
    synchronous: &str,
    count: usize,
    batch_size: usize,
) -> Result<BenchMetrics, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let db_path: PathBuf = temp_dir.path().join("bench_raw.db");

    let mut conn = rusqlite::Connection::open(&db_path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", synchronous)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "cache_size", -65536)?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _lilia_kv(
           namespace TEXT NOT NULL,
           key BLOB NOT NULL,
           value BLOB NOT NULL,
           version INTEGER NOT NULL CHECK(version > 0),
           expires_at_ms INTEGER,
           PRIMARY KEY(namespace,key)
         ) STRICT, WITHOUT ROWID;",
    )?;

    // 1. Warmup phase (300 mutations)
    let payload = vec![42u8; 256];
    let num_warmup_batches = 300usize.div_ceil(batch_size.max(1));
    let mut w_generated = 0;
    for b in 0..num_warmup_batches {
        let chunk_size = (300 - w_generated).min(batch_size);
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO _lilia_kv(namespace,key,value,version,expires_at_ms) VALUES(?1,?2,?3,?4,?5)
                 ON CONFLICT(namespace,key) DO UPDATE SET value=excluded.value,version=excluded.version,expires_at_ms=excluded.expires_at_ms",
            )?;
            for i in 0..chunk_size {
                let id = b * batch_size + i;
                let key = format!("k_{id}").into_bytes();
                stmt.execute(rusqlite::params!["warmup", key, payload, 1i64, None::<i64>])?;
            }
        }
        tx.commit()?;
        w_generated += chunk_size;
    }

    // 2. Timed benchmark phase
    let num_batches = count.div_ceil(batch_size.max(1));
    let mut batch_latencies_us = Vec::with_capacity(num_batches);

    let start_all = Instant::now();
    let mut generated = 0;

    for b in 0..num_batches {
        let current_batch_size = (count - generated).min(batch_size);
        let batch_start = Instant::now();

        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO _lilia_kv(namespace,key,value,version,expires_at_ms) VALUES(?1,?2,?3,?4,?5)
                 ON CONFLICT(namespace,key) DO UPDATE SET value=excluded.value,version=excluded.version,expires_at_ms=excluded.expires_at_ms",
            )?;
            for i in 0..current_batch_size {
                let id = b * batch_size + i;
                let key = format!("bench_key_{id:08}").into_bytes();
                let val = vec![(id % 256) as u8; 256];
                stmt.execute(rusqlite::params!["bench", key, val, 1i64, None::<i64>])?;
            }
        }
        tx.commit()?;

        let elapsed = batch_start.elapsed();
        let op_latency_us = (elapsed.as_nanos() as f64) / (current_batch_size as f64 * 1_000.0);
        batch_latencies_us.push(op_latency_us);
        generated += current_batch_size;
    }

    let total_duration = start_all.elapsed();
    let total_duration_ms = total_duration.as_secs_f64() * 1_000.0;
    let throughput_ops_sec = (count as f64) / total_duration.as_secs_f64();

    batch_latencies_us.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p50_us = percentile(&batch_latencies_us, 50.0);
    let p95_us = percentile(&batch_latencies_us, 95.0);
    let p99_us = percentile(&batch_latencies_us, 99.0);
    let min_us = *batch_latencies_us.first().unwrap_or(&0.0);
    let max_us = *batch_latencies_us.last().unwrap_or(&0.0);

    let sum: f64 = batch_latencies_us.iter().sum();
    let mean_us = sum / batch_latencies_us.len() as f64;
    let variance: f64 = batch_latencies_us
        .iter()
        .map(|v| (v - mean_us).powi(2))
        .sum::<f64>()
        / batch_latencies_us.len() as f64;
    let stddev_us = variance.sqrt();

    drop(conn);
    let disk_bytes = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);

    Ok(BenchMetrics {
        surface: "SQLite Native (C / Rusqlite)".into(),
        codec: "cleartext".into(),
        durability: durability_name.into(),
        total_mutations: count,
        batch_size,
        total_duration_ms,
        throughput_ops_sec,
        p50_us,
        p95_us,
        p99_us,
        mean_us,
        min_us,
        max_us,
        stddev_us,
        disk_bytes,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Arguments::parse();

    if args.tier_suite {
        return run_tier_suite(args.count, args.json);
    }

    let durability = match args.durability.to_lowercase().as_str() {
        "balanced" => Durability::Balanced,
        "performance" => Durability::Performance,
        _ => Durability::Durable,
    };

    let synchronous = match durability {
        Durability::Durable => "FULL",
        Durability::Balanced => "NORMAL",
        Durability::Performance => "OFF",
    };

    let run_clear = args.codec == "clear" || args.codec == "both";
    let run_aes = args.codec == "aes256" || args.codec == "both";

    let mut results = Vec::new();

    if args.raw_sqlite {
        let metrics =
            run_raw_sqlite_benchmark(&args.durability, synchronous, args.count, args.batch_size)?;
        results.push(metrics);
    }

    if run_clear {
        let metrics = run_single_benchmark(
            "cleartext",
            Arc::new(IdentityCodec),
            &args.durability,
            durability,
            args.count,
            args.batch_size,
        )?;
        results.push(metrics);
    }

    if run_aes {
        let metrics = run_single_benchmark(
            "aes-256-gcm",
            Arc::new(Aes256GcmCodec::from_passphrase(
                "liliadb-benchmark-official-key",
            )),
            &args.durability,
            durability,
            args.count,
            args.batch_size,
        )?;
        results.push(metrics);
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        println!(
            "\n=== LiliaDB Native Rust Benchmark ({}, count={}, batch={}) ===",
            args.durability, args.count, args.batch_size
        );
        println!(
            "{:<14} | {:<12} | {:>14} | {:>10} | {:>10} | {:>10} | {:>10}",
            "Codec", "Durability", "Throughput", "p50 (µs)", "p95 (µs)", "p99 (µs)", "Disk (KB)"
        );
        println!(
            "{:-<14}-|-{:-<12}-|-{:-<14}-|-{:-<10}-|-{:-<10}-|-{:-<10}-|-{:-<10}",
            "", "", "", "", "", "", ""
        );
        for m in &results {
            println!(
                "{:<14} | {:<12} | {:>10.0} ops/s | {:>10.2} | {:>10.2} | {:>10.2} | {:>10.1}",
                m.codec,
                m.durability,
                m.throughput_ops_sec,
                m.p50_us,
                m.p95_us,
                m.p99_us,
                m.disk_bytes as f64 / 1024.0
            );
        }

        if results.len() == 2 {
            let clear = &results[0];
            let aes = &results[1];
            let throughput_diff = (clear.throughput_ops_sec - aes.throughput_ops_sec)
                / clear.throughput_ops_sec
                * 100.0;
            let p50_diff = (aes.p50_us - clear.p50_us) / clear.p50_us * 100.0;
            let disk_diff =
                (aes.disk_bytes as f64 - clear.disk_bytes as f64) / clear.disk_bytes as f64 * 100.0;

            println!("\n--- AES-256-GCM vs Cleartext Impact ---");
            println!("  Throughput Impact: {:.1}% change", -throughput_diff);
            println!(
                "  Latency p50 Delta: {:+.2} µs ({:+.1}%)",
                aes.p50_us - clear.p50_us,
                p50_diff
            );
            println!("  Disk Expansion:    {disk_diff:+.1}%\n");
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize)]
struct TierBenchMetrics {
    scenario: String,
    tier: String,
    threads: usize,
    total_ops: usize,
    duration_ms: f64,
    throughput_ops_sec: f64,
    p50_us: f64,
    p95_us: f64,
    p99_us: f64,
    disk_bytes: u64,
    notes: String,
}

fn sqlite_footprint_bytes(db_path: &std::path::Path) -> u64 {
    let mut total = std::fs::metadata(db_path).map_or(0, |m| m.len());
    for suffix in ["-wal", "-shm"] {
        let mut side = db_path.as_os_str().to_os_string();
        side.push(suffix);
        total += std::fs::metadata(side).map_or(0, |m| m.len());
    }
    total
}

fn from_bench_metrics(
    scenario: &str,
    tier: &str,
    threads: usize,
    m: BenchMetrics,
    notes: impl Into<String>,
) -> TierBenchMetrics {
    TierBenchMetrics {
        scenario: scenario.into(),
        tier: tier.into(),
        threads,
        total_ops: m.total_mutations,
        duration_ms: m.total_duration_ms,
        throughput_ops_sec: m.throughput_ops_sec,
        p50_us: m.p50_us,
        p95_us: m.p95_us,
        p99_us: m.p99_us,
        disk_bytes: m.disk_bytes,
        notes: notes.into(),
    }
}

#[allow(clippy::too_many_arguments)]
fn finalize_tier_metrics(
    scenario: &str,
    tier: &str,
    threads: usize,
    total_ops: usize,
    elapsed: std::time::Duration,
    mut latencies_us: Vec<f64>,
    disk_bytes: u64,
    notes: String,
) -> TierBenchMetrics {
    latencies_us.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let secs = elapsed.as_secs_f64().max(1e-9);
    TierBenchMetrics {
        scenario: scenario.into(),
        tier: tier.into(),
        threads,
        total_ops,
        duration_ms: secs * 1000.0,
        throughput_ops_sec: total_ops as f64 / secs,
        p50_us: percentile(&latencies_us, 50.0),
        p95_us: percentile(&latencies_us, 95.0),
        p99_us: percentile(&latencies_us, 99.0),
        disk_bytes,
        notes,
    }
}

fn run_parallel_workload<F>(
    threads: usize,
    total_ops: usize,
    op_fn: &F,
) -> Result<(std::time::Duration, Vec<f64>), Box<dyn std::error::Error>>
where
    F: Fn(usize) -> Result<(), Box<dyn std::error::Error + Send + Sync>> + Sync,
{
    let workers = threads.max(1);
    let barrier = Barrier::new(workers + 1);
    let per_worker = total_ops / workers;
    let remainder = total_ops % workers;

    let mut all_latencies = Vec::with_capacity(total_ops);
    let wall_elapsed = thread::scope(
        |s| -> Result<std::time::Duration, Box<dyn std::error::Error + Send + Sync>> {
            let mut handles = Vec::with_capacity(workers);
            let mut offset = 0usize;
            for tid in 0..workers {
                let count = per_worker + usize::from(tid < remainder);
                let start_id = offset;
                offset += count;
                let barrier_ref = &barrier;
                handles.push(s.spawn(move || {
                    let mut lats = Vec::with_capacity(count);
                    barrier_ref.wait();
                    for id in start_id..(start_id + count) {
                        let t0 = Instant::now();
                        op_fn(id)?;
                        lats.push(t0.elapsed().as_nanos() as f64 / 1000.0);
                    }
                    Ok::<Vec<f64>, Box<dyn std::error::Error + Send + Sync>>(lats)
                }));
            }

            barrier.wait();
            let wall_start = Instant::now();
            for handle in handles {
                let lats = handle
                    .join()
                    .map_err(|_| std::io::Error::other("benchmark worker panicked"))??;
                all_latencies.extend(lats);
            }
            Ok(wall_start.elapsed())
        },
    )
    .map_err(|e| -> Box<dyn std::error::Error> { e })?;

    Ok((wall_elapsed, all_latencies))
}

fn bench_memory_tier(
    threads: usize,
    count: usize,
) -> Result<TierBenchMetrics, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("mem_bench.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path))?;
    db.set_collection_spec(CollectionSpec::new(
        CollectionKind::Kv,
        "mem_kv",
        StorageTier::Memory,
    ))?;

    for w in 0..200 {
        db.batch(&[BatchOperation::KvSet {
            namespace: "mem_kv".into(),
            key: format!("warm_{w}").into_bytes(),
            value: vec![1u8; 256],
            if_version: None,
            expires_at_ms: None,
        }])?;
    }
    let before_bytes = sqlite_footprint_bytes(&db_path);
    let payload = vec![7u8; 256];

    let (elapsed, latencies) = run_parallel_workload(threads, count, &|id| {
        db.batch(&[BatchOperation::KvSet {
            namespace: "mem_kv".into(),
            key: format!("mem_key_{id:08}").into_bytes(),
            value: payload.clone(),
            if_version: None,
            expires_at_ms: None,
        }])?;
        Ok(())
    })?;
    let after_bytes = sqlite_footprint_bytes(&db_path);
    let wal_delta = after_bytes.saturating_sub(before_bytes);
    let scenario = if threads == 1 {
        "Memory Tier KV (1 thread)"
    } else {
        "Memory Tier KV (8 threads)"
    };
    Ok(finalize_tier_metrics(
        scenario,
        "memory",
        threads,
        count,
        elapsed,
        latencies,
        wal_delta,
        format!("wal_delta={wal_delta} B"),
    ))
}

fn bench_ring_config(flush_interval_ms: u64, watermark_trigger: usize) -> RingBufferConfig {
    RingBufferConfig {
        capacity: 65_536,
        max_batch_size: 4096,
        flush_interval_ms,
        watermark_trigger,
        overflow_policy: OverflowPolicy::Block {
            timeout_ms: Some(5_000),
        },
        coalesce_timeseries: true,
        coalesce_kv_writes: false,
        enable_read_overlay: true,
    }
}

fn bench_buffered_ring_kv(count: usize) -> Result<TierBenchMetrics, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("ring_bench.lilia");
    let ring_cfg = bench_ring_config(5, 1024);
    let db = Database::open(
        DatabaseOptions::durable(&db_path)
            .with_ring_buffer(ring_cfg)
            .with_buffered_kv("buf_kv"),
    )?;
    let ring = db.ring_buffer();
    let payload = vec![5u8; 256];

    let t0 = Instant::now();
    let (enqueue_elapsed, latencies) = run_parallel_workload(8, count, &|id| {
        ring.push_kv("buf_kv", format!("buf_key_{id:08}").as_bytes(), &payload)?;
        Ok(())
    })?;
    let stats = db.flush_writes()?;
    let total_elapsed = t0.elapsed();
    let disk_bytes = sqlite_footprint_bytes(&db_path);
    let enqueue_rate = count as f64 / enqueue_elapsed.as_secs_f64().max(1e-9);
    Ok(finalize_tier_metrics(
        "Buffered RingBuffer Async KV (8 threads + flush)",
        "buffered",
        8,
        count,
        total_elapsed,
        latencies,
        disk_bytes,
        format!(
            "enqueue={enqueue_rate:.0} ops/s, batches={}, flushed={}",
            stats.batches_committed, stats.committed_total
        ),
    ))
}

fn bench_groupsync_kv(count: usize) -> Result<TierBenchMetrics, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("groupsync_bench.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path))?;
    let payload = vec![9u8; 256];

    let (elapsed, latencies) = run_parallel_workload(8, count, &|id| {
        db.commit_group_batch(
            vec![BatchOperation::KvSet {
                namespace: "sync_kv".into(),
                key: format!("gs_key_{id:08}").into_bytes(),
                value: payload.clone(),
                if_version: None,
                expires_at_ms: None,
            }],
            None,
        )?;
        Ok(())
    })?;
    let stats = db.ring_buffer_stats();
    let disk_bytes = sqlite_footprint_bytes(&db_path);
    Ok(finalize_tier_metrics(
        "GroupSync Multi-Thread KV (8 threads shared WAL)",
        "disk+groupsync",
        8,
        count,
        elapsed,
        latencies,
        disk_bytes,
        format!("wal_batches={}", stats.batches_committed),
    ))
}

fn bench_iot_ts_coalescing(count: usize) -> Result<TierBenchMetrics, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("iot_ts_bench.lilia");
    let ring_cfg = bench_ring_config(10, 2048);
    let db = Database::open(
        DatabaseOptions::durable(&db_path)
            .with_ring_buffer(ring_cfg)
            .with_buffered_series("iot_sensor"),
    )?;

    let t0 = Instant::now();
    let (_, latencies) = run_parallel_workload(1, count, &|id| {
        let point = TimeSeriesPoint::new(
            1_700_000_000_000 + id as i64 * 100,
            20.0 + (id % 100) as f64 * 0.1,
        );
        db.ts_append_buffered("iot_sensor", &[point])?;
        Ok(())
    })?;
    let stats = db.flush_writes()?;
    let total_elapsed = t0.elapsed();

    let chunk_res = db.sql_query(
        "SELECT COUNT(*) FROM main._lilia_timeseries_chunks WHERE series_id = 'iot_sensor'",
        &[],
    )?;
    let chunks = chunk_res
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    let disk_bytes = sqlite_footprint_bytes(&db_path);
    Ok(finalize_tier_metrics(
        "Buffered IoT Gorilla TS Coalescing (single-pt pushes)",
        "buffered",
        1,
        count,
        total_elapsed,
        latencies,
        disk_bytes,
        format!(
            "chunks={chunks}, batches={}, coalesced_pts={}",
            stats.batches_committed, stats.coalesced_ts_points
        ),
    ))
}

fn run_tier_suite(count: usize, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let disk_durable = run_single_benchmark(
        "cleartext",
        Arc::new(IdentityCodec),
        "durable",
        Durability::Durable,
        count,
        1,
    )?;
    let disk_balanced = run_single_benchmark(
        "cleartext",
        Arc::new(IdentityCodec),
        "balanced",
        Durability::Balanced,
        count,
        1,
    )?;

    let results = vec![
        from_bench_metrics("Disk Durable KV (batch=1)", "disk", 1, disk_durable, "WAL FULL"),
        from_bench_metrics("Disk Balanced KV (batch=1)", "disk", 1, disk_balanced, "WAL NORMAL"),
        bench_memory_tier(1, count)?,
        bench_memory_tier(8, count)?,
        bench_buffered_ring_kv(count)?,
        bench_groupsync_kv(count)?,
        bench_iot_ts_coalescing(count)?,
    ];

    if json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        println!("\n=== LiliaDB Tiered Storage Benchmark Suite (count={count}) ===");
        println!(
            "{:<50} | {:<14} | {:>3} | {:>14} | {:>9} | {:>9} | {:>9} | {:>10} | Notes",
            "Scenario", "Tier", "Thr", "Throughput", "p50 (µs)", "p95 (µs)", "p99 (µs)", "Disk (KB)"
        );
        println!(
            "{:-<50}-|-{:-<14}-|-{:-<3}-|-{:-<14}-|-{:-<9}-|-{:-<9}-|-{:-<9}-|-{:-<10}-|-{:-<28}",
            "", "", "", "", "", "", "", "", ""
        );
        for m in &results {
            println!(
                "{:<50} | {:<14} | {:>3} | {:>8.0} ops/s | {:>9.2} | {:>9.2} | {:>9.2} | {:>10.1} | {}",
                m.scenario,
                m.tier,
                m.threads,
                m.throughput_ops_sec,
                m.p50_us,
                m.p95_us,
                m.p99_us,
                m.disk_bytes as f64 / 1024.0,
                m.notes
            );
        }
        println!();
    }

    Ok(())
}
