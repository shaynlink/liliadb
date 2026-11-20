//! Bulk load, then read workloads on the loaded database: point reads after reopening and warm,
//! concurrent point reads, multi-key reads, short range reads and a full ordered scan.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Barrier;
use std::time::Instant;

use serde_json::json;

use super::{expect_found, fresh_dir, nanos_since, open, Meter, Settings};
use crate::engines::{Engine, Result};
use crate::keys::{key, value, Rng};
use crate::os;
use crate::record::{params, Line, Record};

/// One latency sample every this many reads: timing every read would add the clock's own cost
/// to engines that answer in a few hundred nanoseconds. Throughput is always wall-clock.
const SAMPLE_EVERY: u64 = 8;

pub fn run(engine_id: &str, settings: &Settings, emit: &mut dyn FnMut(Line)) -> Result<()> {
    let dir = fresh_dir(settings)?;
    let n = settings.records;

    let engine = open(engine_id, dir.path())?;
    let mut load = bulk_load(&*engine, settings)?;
    drop(engine);
    load.disk_bytes = Some(os::dir_size(dir.path()));
    emit(Line::Record(Box::new(load)));

    // Reopened: the engine's own caches are cold. The OS page cache is not purged (that needs
    // root), so this measures engine warm-up, not device reads.
    let engine = open(engine_id, dir.path())?;
    let first = n.min(100_000);
    emit(Line::Record(Box::new(gets(&*engine, settings, 1, "reopened", Some(first))?)));
    emit(Line::Record(Box::new(gets(&*engine, settings, 1, "warm", None)?)));
    for &threads in settings.read_threads.iter().filter(|&&t| t > 1) {
        emit(Line::Record(Box::new(gets(&*engine, settings, threads, "warm", None)?)));
    }
    emit(Line::Record(Box::new(multi_gets(&*engine, settings)?)));
    emit(Line::Record(Box::new(ranges(&*engine, settings)?)));
    emit(Line::Record(Box::new(scan(&*engine, settings)?)));
    Ok(())
}

fn bulk_load(engine: &dyn Engine, settings: &Settings) -> Result<Record> {
    let mut session = engine.session()?;
    let size = settings.value_size;
    let mut latencies = Vec::new();
    let meter = Meter::start();
    let mut index = 0;
    while index < settings.records {
        let end = (index + 1_000).min(settings.records);
        let rows: Vec<([u8; 16], Vec<u8>)> = (index..end).map(|i| (key(i), value(i, size))).collect();
        let items: Vec<(&[u8], &[u8])> = rows.iter().map(|(k, v)| (&k[..], &v[..])).collect();
        let started = Instant::now();
        session.put(&items)?;
        latencies.push(nanos_since(started));
        index = end;
    }
    Ok(meter.finish(
        engine.info(),
        "load",
        params([("records", json!(settings.records)), ("batch", json!(1_000)), ("value_size", json!(size))]),
        settings.records,
        settings.records * (16 + size as u64),
        &mut latencies,
    ))
}

/// Uniform random point reads of existing keys by `threads` threads, until the budget runs out
/// or, when `limit` is set, after that many reads on one thread.
fn gets(engine: &dyn Engine, settings: &Settings, threads: usize, state: &str, limit: Option<u64>) -> Result<Record> {
    let n = settings.records;
    let barrier = Barrier::new(threads + 1);
    let stop = AtomicBool::new(false);
    let budget = settings.budget();

    let (meter, samples, reads, found) = std::thread::scope(|scope| -> Result<_> {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let (barrier, stop) = (&barrier, &stop);
                scope.spawn(move || -> Result<(Vec<u64>, u64, u64)> {
                    let mut session = engine.session()?;
                    let mut rng = Rng::new(0xABCD + t as u64);
                    let (mut samples, mut reads, mut found) = (Vec::new(), 0u64, 0u64);
                    barrier.wait();
                    while !stop.load(Ordering::Relaxed) && limit.is_none_or(|l| reads < l) {
                        let k = key(rng.below(n));
                        if reads % SAMPLE_EVERY == 0 {
                            let started = Instant::now();
                            found += u64::from(session.get(&k)?.is_some());
                            samples.push(nanos_since(started));
                        } else {
                            found += u64::from(session.get(&k)?.is_some());
                        }
                        reads += 1;
                    }
                    Ok((samples, reads, found))
                })
            })
            .collect();
        barrier.wait();
        let meter = Meter::start();
        if limit.is_none() {
            std::thread::sleep(budget);
            stop.store(true, Ordering::Relaxed);
        }
        let (mut samples, mut reads, mut found) = (Vec::new(), 0, 0);
        for handle in handles {
            let (s, r, f) = handle.join().map_err(|_| "reader thread panicked")??;
            samples.extend(s);
            reads += r;
            found += f;
        }
        Ok((meter, samples, reads, found))
    })?;

    let info = engine.info();
    expect_found(&info.id, "read.get", found, reads)?;
    let mut samples = samples;
    Ok(meter.finish(
        info,
        "read.get",
        params([("threads", json!(threads)), ("state", json!(state))]),
        reads,
        reads * settings.value_size as u64,
        &mut samples,
    ))
}

/// 100 random existing keys read together, from one snapshot when the engine offers one.
fn multi_gets(engine: &dyn Engine, settings: &Settings) -> Result<Record> {
    let mut session = engine.session()?;
    let mut rng = Rng::new(0x5EED);
    let budget = settings.budget();
    let (mut latencies, mut reads, mut found) = (Vec::new(), 0u64, 0u64);
    let meter = Meter::start();
    while meter.elapsed() < budget {
        let keys: Vec<[u8; 16]> = (0..100).map(|_| key(rng.below(settings.records))).collect();
        let refs: Vec<&[u8]> = keys.iter().map(|k| &k[..]).collect();
        let started = Instant::now();
        found += session.multi_get(&refs)? as u64;
        latencies.push(nanos_since(started));
        reads += 100;
    }
    expect_found(&engine.info().id, "read.multi_get", found, reads)?;
    Ok(meter.finish(
        engine.info(),
        "read.multi_get",
        params([("keys", json!(100))]),
        reads,
        reads * settings.value_size as u64,
        &mut latencies,
    ))
}

/// The 100 entries following a random existing key.
fn ranges(engine: &dyn Engine, settings: &Settings) -> Result<Record> {
    let mut session = engine.session()?;
    let mut rng = Rng::new(0xFACE);
    let budget = settings.budget();
    let (mut latencies, mut rows) = (Vec::new(), 0u64);
    let meter = Meter::start();
    while meter.elapsed() < budget {
        let k = key(rng.below(settings.records));
        let started = Instant::now();
        rows += session.range_after(&k, 100)? as u64;
        latencies.push(nanos_since(started));
    }
    let ranges = latencies.len() as u64;
    // Only ranges starting among the last 100 keys may be short (100 in `records` of them), so a
    // mean below 95 rows per range is a wrong answer, not bad luck.
    if rows < ranges * 95 {
        return Err(format!("{} read.range: ranges returned too few rows ({rows} for {ranges})", engine.info().id).into());
    }
    Ok(meter.finish(
        engine.info(),
        "read.range",
        params([("length", json!(100)), ("ranges", json!(ranges))]),
        rows,
        rows * (16 + settings.value_size as u64),
        &mut latencies,
    ))
}

/// Full ordered scans, repeated until the budget is spent (at least one).
fn scan(engine: &dyn Engine, settings: &Settings) -> Result<Record> {
    let mut session = engine.session()?;
    let budget = settings.budget();
    let (mut latencies, mut rows) = (Vec::new(), 0u64);
    let meter = Meter::start();
    loop {
        let started = Instant::now();
        let seen = session.scan_all()?;
        latencies.push(nanos_since(started));
        expect_found(&engine.info().id, "read.scan", seen, settings.records)?;
        rows += seen;
        if meter.elapsed() >= budget {
            break;
        }
    }
    Ok(meter.finish(
        engine.info(),
        "read.scan",
        params([("records", json!(settings.records)), ("passes", json!(latencies.len()))]),
        rows,
        rows * (16 + settings.value_size as u64),
        &mut latencies,
    ))
}
