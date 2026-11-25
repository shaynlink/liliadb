//! Write workloads: durable single-op commits, concurrent writers, batches, value sizes,
//! overwrites and deletes. Each starts from a fresh database.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Barrier;
use std::time::Instant;

use serde_json::json;

use super::{fresh_dir, nanos_since, open, preload, Meter, Settings};
use crate::engines::Result;
use crate::keys::{key, value, Rng};
use crate::record::{params, Line, Record};
use crate::os;

/// Keys written by the workloads that need distinct key ranges start here.
const FRESH_KEYS: u64 = 1 << 40;

pub fn run(engine: &str, settings: &Settings, emit: &mut dyn FnMut(Line)) -> Result<()> {
    emit(Line::Record(Box::new(concurrent(engine, settings, 1)?)));
    for &threads in settings.threads.iter().filter(|&&t| t > 1) {
        emit(Line::Record(Box::new(concurrent(engine, settings, threads)?)));
    }
    for batch in [100, 1_000, 10_000] {
        emit(Line::Record(Box::new(batched(engine, settings, batch, settings.value_size)?)));
    }
    for size in [1_024, 4_096, 65_536] {
        emit(Line::Record(Box::new(batched(engine, settings, 100, size)?)));
    }
    emit(Line::Record(Box::new(overwrite(engine, settings)?)));
    emit(Line::Record(Box::new(delete(engine, settings)?)));
    Ok(())
}

/// `threads` writers, each committing one key per transaction until the budget runs out.
fn concurrent(engine_id: &str, settings: &Settings, threads: usize) -> Result<Record> {
    let dir = fresh_dir(settings)?;
    let engine = open(engine_id, dir.path())?;
    let barrier = Barrier::new(threads + 1);
    let stop = AtomicBool::new(false);
    let budget = settings.budget();
    let size = settings.value_size;

    let (meter, per_thread) = std::thread::scope(|scope| -> Result<_> {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let (engine, barrier, stop) = (&*engine, &barrier, &stop);
                scope.spawn(move || -> Result<Vec<u64>> {
                    let mut session = engine.session()?;
                    let payload = value(t as u64, size);
                    let mut latencies = Vec::new();
                    barrier.wait();
                    let mut i = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        let k = key(FRESH_KEYS + ((t as u64) << 32) + i);
                        let started = Instant::now();
                        session.put(&[(&k, &payload)])?;
                        latencies.push(nanos_since(started));
                        i += 1;
                    }
                    Ok(latencies)
                })
            })
            .collect();
        barrier.wait();
        let meter = Meter::start();
        std::thread::sleep(budget);
        stop.store(true, Ordering::Relaxed);
        let mut all = Vec::new();
        for handle in handles {
            all.extend(handle.join().map_err(|_| "writer thread panicked")??);
        }
        Ok((meter, all))
    })?;

    let mut latencies = per_thread;
    let ops = latencies.len() as u64;
    let info = engine.info();
    let mut record = meter.finish(
        info,
        "write.commit",
        params([("threads", json!(threads)), ("value_size", json!(size))]),
        ops,
        ops * (16 + size as u64),
        &mut latencies,
    );
    drop(engine);
    record.disk_bytes = Some(os::dir_size(dir.path()));
    Ok(record)
}

/// One writer committing `batch` keys per transaction, until the budget or the record count.
fn batched(engine_id: &str, settings: &Settings, batch: usize, size: usize) -> Result<Record> {
    let dir = fresh_dir(settings)?;
    let engine = open(engine_id, dir.path())?;
    let mut session = engine.session()?;
    // At most 1 GiB of payload per measurement, so large values stay bounded.
    let cap = settings.records.min((1u64 << 30) / (16 + size as u64)).max(batch as u64);
    let budget = settings.budget();
    let mut latencies = Vec::new();
    let meter = Meter::start();
    let mut written = 0u64;
    while written < cap && meter.elapsed() < budget {
        let end = (written + batch as u64).min(cap);
        let rows: Vec<([u8; 16], Vec<u8>)> = (written..end).map(|i| (key(i), value(i, size))).collect();
        let items: Vec<(&[u8], &[u8])> = rows.iter().map(|(k, v)| (&k[..], &v[..])).collect();
        let started = Instant::now();
        session.put(&items)?;
        latencies.push(nanos_since(started));
        written = end;
    }
    drop(session);
    let mut record = meter.finish(
        engine.info(),
        "write.batch",
        params([("batch", json!(batch)), ("value_size", json!(size))]),
        written,
        written * (16 + size as u64),
        &mut latencies,
    );
    drop(engine);
    record.disk_bytes = Some(os::dir_size(dir.path()));
    Ok(record)
}

/// Updates of existing keys, chosen uniformly, 1,000 per transaction.
fn overwrite(engine_id: &str, settings: &Settings) -> Result<Record> {
    let dir = fresh_dir(settings)?;
    let engine = open(engine_id, dir.path())?;
    let existing = settings.records.min(100_000);
    preload(&*engine, existing, settings.value_size)?;
    let mut session = engine.session()?;
    let mut rng = Rng::new(7);
    let budget = settings.budget();
    let mut latencies = Vec::new();
    let meter = Meter::start();
    let mut written = 0u64;
    while meter.elapsed() < budget {
        let rows: Vec<([u8; 16], Vec<u8>)> = (0..1_000)
            .map(|n| {
                let i = rng.below(existing);
                (key(i), value(i ^ (written + n), settings.value_size))
            })
            .collect();
        let items: Vec<(&[u8], &[u8])> = rows.iter().map(|(k, v)| (&k[..], &v[..])).collect();
        let started = Instant::now();
        session.put(&items)?;
        latencies.push(nanos_since(started));
        written += 1_000;
    }
    drop(session);
    let mut record = meter.finish(
        engine.info(),
        "write.update",
        params([("batch", json!(1_000)), ("existing", json!(existing)), ("value_size", json!(settings.value_size))]),
        written,
        written * (16 + settings.value_size as u64),
        &mut latencies,
    );
    drop(engine);
    record.disk_bytes = Some(os::dir_size(dir.path()));
    Ok(record)
}

/// Deletes of existing keys, 1,000 per transaction, until all are gone or the budget runs out.
fn delete(engine_id: &str, settings: &Settings) -> Result<Record> {
    let dir = fresh_dir(settings)?;
    let engine = open(engine_id, dir.path())?;
    let existing = settings.records.min(100_000);
    preload(&*engine, existing, settings.value_size)?;
    let mut session = engine.session()?;
    let budget = settings.budget();
    let mut latencies = Vec::new();
    let meter = Meter::start();
    let mut deleted = 0u64;
    while deleted < existing && meter.elapsed() < budget {
        let end = (deleted + 1_000).min(existing);
        let keys: Vec<[u8; 16]> = (deleted..end).map(key).collect();
        let refs: Vec<&[u8]> = keys.iter().map(|k| &k[..]).collect();
        let started = Instant::now();
        session.delete(&refs)?;
        latencies.push(nanos_since(started));
        deleted = end;
    }
    let remaining = session.get(&key(deleted.saturating_sub(1)))?;
    if deleted > 0 && remaining.is_some() {
        return Err(format!("{engine_id} write.delete: a deleted key is still readable").into());
    }
    drop(session);
    let mut record = meter.finish(
        engine.info(),
        "write.delete",
        params([("batch", json!(1_000)), ("existing", json!(existing))]),
        deleted,
        deleted * 16,
        &mut latencies,
    );
    drop(engine);
    record.disk_bytes = Some(os::dir_size(dir.path()));
    Ok(record)
}
