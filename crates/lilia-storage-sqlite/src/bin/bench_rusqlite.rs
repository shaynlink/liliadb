#![allow(
    unsafe_code,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::ffi::{c_int, c_void, CStr, CString};
use std::ptr;
use std::time::Instant;

use clap::Parser;
use rusqlite::ffi;
use rusqlite::{params, Connection};

#[derive(Debug, Parser)]
#[command(name = "bench_rusqlite", about = "Detailed audit benchmark for rusqlite & libsqlite3-sys")]
struct Args {
    #[arg(long, default_value_t = 100_000)]
    ops: usize,

    #[arg(long, default_value_t = 100)]
    batch_size: usize,

    #[arg(long, default_value_t = 3_000)]
    single_ops: usize,

    #[arg(long)]
    sync_full: bool,
}

struct Stats {
    ops_per_sec: f64,
    p50_us: f64,
    p95_us: f64,
    p99_us: f64,
    avg_us: f64,
}

#[inline(always)]
fn format_key(buf: &mut [u8; 16], id: usize) -> &[u8] {
    buf[..4].copy_from_slice(b"key_");
    let mut n = id;
    for i in (4..12).rev() {
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    &buf[..12]
}

fn compute_stats(mut latencies: Vec<f64>, total_ops: usize, total_time_ms: f64) -> Stats {
    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let len = latencies.len();
    let p50 = latencies[(len as f64 * 0.50) as usize];
    let p95 = latencies[(len as f64 * 0.95) as usize];
    let p99 = latencies[(len as f64 * 0.99) as usize];
    let sum: f64 = latencies.iter().sum();
    Stats {
        ops_per_sec: (total_ops as f64) / (total_time_ms / 1000.0),
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
        avg_us: sum / (len as f64),
    }
}

fn setup_db(conn: &Connection, sync_mode: &str) {
    conn.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.pragma_update(None, "synchronous", sync_mode).unwrap();
    conn.pragma_update(None, "temp_store", "MEMORY").unwrap();
    conn.pragma_update(None, "cache_size", -65536).unwrap();
    let _ = conn.pragma_update(None, "mmap_size", 1073741824i64);

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _lilia_kv (
           namespace TEXT NOT NULL,
           key BLOB NOT NULL,
           value BLOB NOT NULL,
           version INTEGER NOT NULL CHECK(version > 0),
           expires_at_ms INTEGER,
           PRIMARY KEY(namespace, key)
         ) STRICT, WITHOUT ROWID;",
    ).unwrap();
}

// 1. Direct FFI call from Rust (zero heap alloc in loop, stack buffers)
unsafe fn bench_direct_ffi(db: *mut ffi::sqlite3, total_ops: usize, batch_size: usize) -> Stats {
    let sql = CString::new(
        "INSERT INTO _lilia_kv(namespace, key, value, version, expires_at_ms) \
         VALUES(?1, ?2, ?3, 1, ?4) \
         ON CONFLICT(namespace, key) DO UPDATE SET \
           value = excluded.value, \
           version = _lilia_kv.version + 1, \
           expires_at_ms = excluded.expires_at_ms;"
    ).unwrap();

    let mut stmt: *mut ffi::sqlite3_stmt = ptr::null_mut();
    ffi::sqlite3_prepare_v2(db, sql.as_ptr(), -1, &mut stmt, ptr::null_mut());

    let begin_cmd = CString::new("BEGIN IMMEDIATE;").unwrap();
    let commit_cmd = CString::new("COMMIT;").unwrap();
    let ns = CString::new("default").unwrap();
    let val = [42u8; 128];

    let num_batches = total_ops / batch_size;
    let mut latencies = Vec::with_capacity(num_batches);
    let mut kbuf = [0u8; 16];

    let t0 = Instant::now();

    for b in 0..num_batches {
        let b_start = Instant::now();
        ffi::sqlite3_exec(db, begin_cmd.as_ptr(), None, ptr::null_mut(), ptr::null_mut());

        for i in 0..batch_size {
            let id = b * batch_size + i;
            let key = format_key(&mut kbuf, id);

            ffi::sqlite3_reset(stmt);
            ffi::sqlite3_bind_text(stmt, 1, ns.as_ptr(), -1, ffi::SQLITE_STATIC());
            ffi::sqlite3_bind_blob(stmt, 2, key.as_ptr().cast::<c_void>(), key.len() as c_int, ffi::SQLITE_STATIC());
            ffi::sqlite3_bind_blob(stmt, 3, val.as_ptr().cast::<c_void>(), val.len() as c_int, ffi::SQLITE_STATIC());
            ffi::sqlite3_bind_null(stmt, 4);

            ffi::sqlite3_step(stmt);
        }

        ffi::sqlite3_exec(db, commit_cmd.as_ptr(), None, ptr::null_mut(), ptr::null_mut());
        let b_dur = b_start.elapsed().as_nanos() as f64 / (batch_size as f64 * 1000.0);
        latencies.push(b_dur);
    }

    let total_time_ms = t0.elapsed().as_secs_f64() * 1000.0;
    ffi::sqlite3_finalize(stmt);
    compute_stats(latencies, total_ops, total_time_ms)
}

// 2. Direct FFI with RETURNING version
unsafe fn bench_direct_ffi_returning(db: *mut ffi::sqlite3, total_ops: usize, batch_size: usize) -> Stats {
    let sql = CString::new(
        "INSERT INTO _lilia_kv(namespace, key, value, version, expires_at_ms) \
         VALUES(?1, ?2, ?3, 1, ?4) \
         ON CONFLICT(namespace, key) DO UPDATE SET \
           value = excluded.value, \
           version = _lilia_kv.version + 1, \
           expires_at_ms = excluded.expires_at_ms \
         RETURNING version;"
    ).unwrap();

    let mut stmt: *mut ffi::sqlite3_stmt = ptr::null_mut();
    ffi::sqlite3_prepare_v2(db, sql.as_ptr(), -1, &mut stmt, ptr::null_mut());

    let begin_cmd = CString::new("BEGIN IMMEDIATE;").unwrap();
    let commit_cmd = CString::new("COMMIT;").unwrap();
    let ns = CString::new("default").unwrap();
    let val = [42u8; 128];

    let num_batches = total_ops / batch_size;
    let mut latencies = Vec::with_capacity(num_batches);
    let mut kbuf = [0u8; 16];

    let t0 = Instant::now();

    for b in 0..num_batches {
        let b_start = Instant::now();
        ffi::sqlite3_exec(db, begin_cmd.as_ptr(), None, ptr::null_mut(), ptr::null_mut());

        for i in 0..batch_size {
            let id = b * batch_size + i;
            let key = format_key(&mut kbuf, id);

            ffi::sqlite3_reset(stmt);
            ffi::sqlite3_bind_text(stmt, 1, ns.as_ptr(), -1, ffi::SQLITE_STATIC());
            ffi::sqlite3_bind_blob(stmt, 2, key.as_ptr().cast::<c_void>(), key.len() as c_int, ffi::SQLITE_STATIC());
            ffi::sqlite3_bind_blob(stmt, 3, val.as_ptr().cast::<c_void>(), val.len() as c_int, ffi::SQLITE_STATIC());
            ffi::sqlite3_bind_null(stmt, 4);

            let rc = ffi::sqlite3_step(stmt);
            if rc == ffi::SQLITE_ROW {
                let _v = ffi::sqlite3_column_int64(stmt, 0);
            }
            ffi::sqlite3_reset(stmt);
        }

        ffi::sqlite3_exec(db, commit_cmd.as_ptr(), None, ptr::null_mut(), ptr::null_mut());
        let b_dur = b_start.elapsed().as_nanos() as f64 / (batch_size as f64 * 1000.0);
        latencies.push(b_dur);
    }

    let total_time_ms = t0.elapsed().as_secs_f64() * 1000.0;
    ffi::sqlite3_finalize(stmt);
    compute_stats(latencies, total_ops, total_time_ms)
}

// 3. Rusqlite Tuned: Reused Statement + params![]
fn bench_rusqlite_reused_stmt(conn: &mut Connection, total_ops: usize, batch_size: usize) -> Stats {
    let num_batches = total_ops / batch_size;
    let mut latencies = Vec::with_capacity(num_batches);
    let val = [42u8; 128];
    let mut kbuf = [0u8; 16];

    let t0 = Instant::now();

    for b in 0..num_batches {
        let b_start = Instant::now();
        let tx = conn.transaction().unwrap();
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO _lilia_kv(namespace, key, value, version, expires_at_ms) \
                 VALUES(?1, ?2, ?3, 1, ?4) \
                 ON CONFLICT(namespace, key) DO UPDATE SET \
                   value = excluded.value, \
                   version = _lilia_kv.version + 1, \
                   expires_at_ms = excluded.expires_at_ms;"
            ).unwrap();

            for i in 0..batch_size {
                let id = b * batch_size + i;
                let key = format_key(&mut kbuf, id);
                stmt.execute(params!["default", key, val.as_slice(), None::<i64>]).unwrap();
            }
        }
        tx.commit().unwrap();
        let b_dur = b_start.elapsed().as_nanos() as f64 / (batch_size as f64 * 1000.0);
        latencies.push(b_dur);
    }

    let total_time_ms = t0.elapsed().as_secs_f64() * 1000.0;
    compute_stats(latencies, total_ops, total_time_ms)
}

// 4. Rusqlite with prepare_cached inside the item loop (Lilia's current batch pattern)
fn bench_rusqlite_cached_per_item(conn: &mut Connection, total_ops: usize, batch_size: usize) -> Stats {
    let num_batches = total_ops / batch_size;
    let mut latencies = Vec::with_capacity(num_batches);
    let val = [42u8; 128];
    let mut kbuf = [0u8; 16];

    let t0 = Instant::now();

    for b in 0..num_batches {
        let b_start = Instant::now();
        let tx = conn.transaction().unwrap();
        for i in 0..batch_size {
            let id = b * batch_size + i;
            let key = format_key(&mut kbuf, id);
            let mut stmt = tx.prepare_cached(
                "INSERT INTO _lilia_kv(namespace, key, value, version, expires_at_ms) \
                 VALUES(?1, ?2, ?3, 1, ?4) \
                 ON CONFLICT(namespace, key) DO UPDATE SET \
                   value = excluded.value, \
                   version = _lilia_kv.version + 1, \
                   expires_at_ms = excluded.expires_at_ms;"
            ).unwrap();
            stmt.execute(params!["default", key, val.as_slice(), None::<i64>]).unwrap();
        }
        tx.commit().unwrap();
        let b_dur = b_start.elapsed().as_nanos() as f64 / (batch_size as f64 * 1000.0);
        latencies.push(b_dur);
    }

    let total_time_ms = t0.elapsed().as_secs_f64() * 1000.0;
    compute_stats(latencies, total_ops, total_time_ms)
}

// 5. Rusqlite Atomic UPSERT with RETURNING (statement reused across batch)
fn bench_rusqlite_returning(conn: &mut Connection, total_ops: usize, batch_size: usize) -> Stats {
    let num_batches = total_ops / batch_size;
    let mut latencies = Vec::with_capacity(num_batches);
    let val = [42u8; 128];
    let mut kbuf = [0u8; 16];

    let t0 = Instant::now();

    for b in 0..num_batches {
        let b_start = Instant::now();
        let tx = conn.transaction().unwrap();
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO _lilia_kv(namespace, key, value, version, expires_at_ms) \
                 VALUES(?1, ?2, ?3, 1, ?4) \
                 ON CONFLICT(namespace, key) DO UPDATE SET \
                   value = excluded.value, \
                   version = _lilia_kv.version + 1, \
                   expires_at_ms = excluded.expires_at_ms \
                 RETURNING version;"
            ).unwrap();

            for i in 0..batch_size {
                let id = b * batch_size + i;
                let key = format_key(&mut kbuf, id);
                let _v: i64 = stmt.query_row(params!["default", key, val.as_slice(), None::<i64>], |r| r.get(0)).unwrap();
            }
        }
        tx.commit().unwrap();
        let b_dur = b_start.elapsed().as_nanos() as f64 / (batch_size as f64 * 1000.0);
        latencies.push(b_dur);
    }

    let total_time_ms = t0.elapsed().as_secs_f64() * 1000.0;
    compute_stats(latencies, total_ops, total_time_ms)
}

// 6. Rusqlite Double Query (SELECT + UPSERT per item, Lilia's current logic)
fn bench_rusqlite_double_query(conn: &mut Connection, total_ops: usize, batch_size: usize) -> Stats {
    let num_batches = total_ops / batch_size;
    let mut latencies = Vec::with_capacity(num_batches);
    let val = [42u8; 128];
    let mut kbuf = [0u8; 16];

    let t0 = Instant::now();

    for b in 0..num_batches {
        let b_start = Instant::now();
        let tx = conn.transaction().unwrap();
        for i in 0..batch_size {
            let id = b * batch_size + i;
            let key = format_key(&mut kbuf, id);

            // 1. SELECT
            let mut sel_stmt = tx.prepare_cached(
                "SELECT version FROM _lilia_kv WHERE namespace = ?1 AND key = ?2;"
            ).unwrap();
            let current: Option<i64> = sel_stmt.query_row(
                params!["default", key],
                |r| r.get(0)
            ).ok();
            let next_ver = current.unwrap_or(0) + 1;

            // 2. UPSERT
            let mut upsert_stmt = tx.prepare_cached(
                "INSERT INTO _lilia_kv(namespace, key, value, version, expires_at_ms) \
                 VALUES(?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(namespace, key) DO UPDATE SET \
                   value = excluded.value, \
                   version = excluded.version, \
                   expires_at_ms = excluded.expires_at_ms;"
            ).unwrap();
            upsert_stmt.execute(params!["default", key, val.as_slice(), next_ver, None::<i64>]).unwrap();
        }
        tx.commit().unwrap();
        let b_dur = b_start.elapsed().as_nanos() as f64 / (batch_size as f64 * 1000.0);
        latencies.push(b_dur);
    }

    let total_time_ms = t0.elapsed().as_secs_f64() * 1000.0;
    compute_stats(latencies, total_ops, total_time_ms)
}

// 7. Rusqlite Single-Op Writes (1 op per transaction)
fn bench_rusqlite_single_op(conn: &mut Connection, total_ops: usize) -> Stats {
    let mut latencies = Vec::with_capacity(total_ops);
    let val = [42u8; 128];
    let mut kbuf = [0u8; 16];

    let t0 = Instant::now();

    for i in 0..total_ops {
        let op_start = Instant::now();
        let tx = conn.transaction().unwrap();
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO _lilia_kv(namespace, key, value, version, expires_at_ms) \
                 VALUES(?1, ?2, ?3, 1, ?4) \
                 ON CONFLICT(namespace, key) DO UPDATE SET \
                   value = excluded.value, \
                   version = _lilia_kv.version + 1, \
                   expires_at_ms = excluded.expires_at_ms;"
            ).unwrap();
            let key = format_key(&mut kbuf, i);
            stmt.execute(params!["default", key, val.as_slice(), None::<i64>]).unwrap();
        }
        tx.commit().unwrap();
        latencies.push(op_start.elapsed().as_nanos() as f64 / 1000.0);
    }

    let total_time_ms = t0.elapsed().as_secs_f64() * 1000.0;
    compute_stats(latencies, total_ops, total_time_ms)
}

fn main() {
    let args = Args::parse();
    let sync_mode = if args.sync_full { "FULL" } else { "NORMAL" };

    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("bench_rusqlite.db");

    let mut conn = Connection::open(&db_path).unwrap();
    setup_db(&conn, sync_mode);

    let raw_handle = unsafe { conn.handle() };
    let libver = unsafe { CStr::from_ptr(ffi::sqlite3_libversion()).to_str().unwrap() };
    let sourceid = unsafe { CStr::from_ptr(ffi::sqlite3_sourceid()).to_str().unwrap() };

    println!("=== Rusqlite & libsqlite3-sys Audit Benchmark ===");
    println!("SQLite Version: {libver} | Source: {sourceid}");
    println!("Synchronous: {sync_mode} | Batch Ops: {} | Batch Size: {} | Single Ops: {}\n",
        args.ops, args.batch_size, args.single_ops);

    // 1. Direct FFI Direct UPSERT
    let s_ffi = unsafe { bench_direct_ffi(raw_handle, args.ops, args.batch_size) };
    println!("[1] Direct FFI (sqlite3_* raw calls from Rust, 0 rusqlite wrapper):");
    println!("    Throughput : {:.0} op/s", s_ffi.ops_per_sec);
    println!("    Latency p50: {:.2} us | p95: {:.2} us | p99: {:.2} us | avg: {:.2} us\n",
        s_ffi.p50_us, s_ffi.p95_us, s_ffi.p99_us, s_ffi.avg_us);

    // 2. Direct FFI with RETURNING
    let s_ffi_ret = unsafe { bench_direct_ffi_returning(raw_handle, args.ops, args.batch_size) };
    println!("[2] Direct FFI with RETURNING (raw C API reading sqlite3_column_int64):");
    println!("    Throughput : {:.0} op/s", s_ffi_ret.ops_per_sec);
    println!("    Latency p50: {:.2} us | p95: {:.2} us | p99: {:.2} us | avg: {:.2} us",
        s_ffi_ret.p50_us, s_ffi_ret.p95_us, s_ffi_ret.p99_us, s_ffi_ret.avg_us);
    println!("    VDBE OP_ResultRow overhead in raw SQLite: -{:.1}% vs Direct UPSERT ({:.2}x)\n",
        (1.0 - s_ffi_ret.ops_per_sec / s_ffi.ops_per_sec) * 100.0, s_ffi.ops_per_sec / s_ffi_ret.ops_per_sec);

    // 3. Rusqlite Tuned (Reused statement across batch)
    let s_reused = bench_rusqlite_reused_stmt(&mut conn, args.ops, args.batch_size);
    println!("[3] Rusqlite Reused Statement (prepare once per batch + params![]):");
    println!("    Throughput : {:.0} op/s", s_reused.ops_per_sec);
    println!("    Latency p50: {:.2} us | p95: {:.2} us | p99: {:.2} us | avg: {:.2} us",
        s_reused.p50_us, s_reused.p95_us, s_reused.p99_us, s_reused.avg_us);
    println!("    Rusqlite Abstraction Tax vs Direct FFI: -{:.1}% ({:.2}x slower)\n",
        (1.0 - s_reused.ops_per_sec / s_ffi.ops_per_sec) * 100.0, s_ffi.ops_per_sec / s_reused.ops_per_sec);

    // 4. Rusqlite prepare_cached per item (LruCache hashmap lookup on each row)
    let s_per_item = bench_rusqlite_cached_per_item(&mut conn, args.ops, args.batch_size);
    println!("[4] Rusqlite prepare_cached PER ITEM (LruCache hashmap lookup on each row):");
    println!("    Throughput : {:.0} op/s", s_per_item.ops_per_sec);
    println!("    Latency p50: {:.2} us | p95: {:.2} us | p99: {:.2} us | avg: {:.2} us",
        s_per_item.p50_us, s_per_item.p95_us, s_per_item.p99_us, s_per_item.avg_us);
    println!("    Overhead of per-item LRU lookup: -{:.1}% vs reused statement ({:.2}x slower)\n",
        (1.0 - s_per_item.ops_per_sec / s_reused.ops_per_sec) * 100.0, s_reused.ops_per_sec / s_per_item.ops_per_sec);

    // 5. Rusqlite Atomic RETURNING
    let s_ret = bench_rusqlite_returning(&mut conn, args.ops, args.batch_size);
    println!("[5] Rusqlite Atomic UPSERT with RETURNING (query_row + r.get):");
    println!("    Throughput : {:.0} op/s", s_ret.ops_per_sec);
    println!("    Latency p50: {:.2} us | p95: {:.2} us | p99: {:.2} us | avg: {:.2} us",
        s_ret.p50_us, s_ret.p95_us, s_ret.p99_us, s_ret.avg_us);
    println!("    Rusqlite query_row + FromSql Tax vs FFI RETURNING: -{:.1}% ({:.2}x slower)\n",
        (1.0 - s_ret.ops_per_sec / s_ffi_ret.ops_per_sec) * 100.0, s_ffi_ret.ops_per_sec / s_ret.ops_per_sec);

    // 6. Rusqlite Double Query (SELECT + UPSERT per item - exact Lilia logic)
    let s_dq = bench_rusqlite_double_query(&mut conn, args.ops, args.batch_size);
    println!("[6] Rusqlite Double Query (SELECT + UPSERT per item, Lilia's current logic):");
    println!("    Throughput : {:.0} op/s", s_dq.ops_per_sec);
    println!("    Latency p50: {:.2} us | p95: {:.2} us | p99: {:.2} us | avg: {:.2} us",
        s_dq.p50_us, s_dq.p95_us, s_dq.p99_us, s_dq.avg_us);
    println!("    Total Cost of Double Query + LRU Lookup: -{:.1}% vs Direct FFI ({:.2}x slower)\n",
        (1.0 - s_dq.ops_per_sec / s_ffi.ops_per_sec) * 100.0, s_ffi.ops_per_sec / s_dq.ops_per_sec);

    // 7. Rusqlite Single-Op Writes
    let s_single = bench_rusqlite_single_op(&mut conn, args.single_ops);
    println!("[7] Rusqlite Single-Op Writes (1 op per transaction / COMMIT):");
    println!("    Throughput : {:.0} op/s", s_single.ops_per_sec);
    println!("    Latency p50: {:.2} us | p95: {:.2} us | p99: {:.2} us | avg: {:.2} us\n",
        s_single.p50_us, s_single.p95_us, s_single.p99_us, s_single.avg_us);
}
