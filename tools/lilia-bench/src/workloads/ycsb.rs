//! YCSB core workloads A–F on a loaded database, Zipfian (θ = 0.99) request distribution.
//! Every update or insert is its own durable commit, as in the reference YCSB client.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Barrier;
use std::time::Instant;

use serde_json::json;

use super::{fresh_dir, nanos_since, open, preload, Meter, Settings};
use crate::engines::{Engine, Result, Session};
use crate::keys::{key, scramble, value, Rng, Zipf};
use crate::record::{params, Line};

#[derive(Debug, Clone, Copy)]
struct Mix {
    name: &'static str,
    read: f64,
    update: f64,
    insert: f64,
    scan: f64,
    /// Read-modify-write share (workload F).
    rmw: f64,
    /// Reads favour the most recent inserts (workload D).
    latest: bool,
}

const MIXES: [Mix; 6] = [
    Mix { name: "a", read: 0.5, update: 0.5, insert: 0.0, scan: 0.0, rmw: 0.0, latest: false },
    Mix { name: "b", read: 0.95, update: 0.05, insert: 0.0, scan: 0.0, rmw: 0.0, latest: false },
    Mix { name: "c", read: 1.0, update: 0.0, insert: 0.0, scan: 0.0, rmw: 0.0, latest: false },
    Mix { name: "d", read: 0.95, update: 0.0, insert: 0.05, scan: 0.0, rmw: 0.0, latest: true },
    Mix { name: "e", read: 0.0, update: 0.0, insert: 0.05, scan: 0.95, rmw: 0.0, latest: false },
    Mix { name: "f", read: 0.5, update: 0.0, insert: 0.0, scan: 0.0, rmw: 0.5, latest: false },
];

/// Maximum length of a workload E scan (uniform in `1..=MAX_SCAN`).
const MAX_SCAN: u64 = 100;

pub fn run(engine_id: &str, settings: &Settings, emit: &mut dyn FnMut(Line)) -> Result<()> {
    let dir = fresh_dir(settings)?;
    let engine = open(engine_id, dir.path())?;
    preload(&*engine, settings.records, settings.value_size)?;
    let inserted = AtomicU64::new(settings.records);
    let mut thread_counts = vec![1];
    if let Some(&most) = settings.threads.iter().max() {
        if most > 1 {
            thread_counts.push(most.min(8));
        }
    }
    for mix in MIXES {
        for &threads in &thread_counts {
            emit(Line::Record(Box::new(run_mix(&*engine, settings, mix, threads, &inserted)?)));
        }
    }
    Ok(())
}

fn run_mix(
    engine: &dyn Engine,
    settings: &Settings,
    mix: Mix,
    threads: usize,
    inserted: &AtomicU64,
) -> Result<crate::record::Record> {
    let barrier = Barrier::new(threads + 1);
    let stop = AtomicBool::new(false);
    let zipf = Zipf::new(settings.records);
    let budget = settings.budget();

    let (meter, latencies, counts) = std::thread::scope(|scope| -> Result<_> {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let (barrier, stop, mut zipf) = (&barrier, &stop, zipf.clone());
                scope.spawn(move || -> Result<(Vec<u64>, [u64; 2])> {
                    let mut session = engine.session()?;
                    let mut rng = Rng::new(0x1CEB_00DA ^ ((t as u64) << 20));
                    let (mut latencies, mut counts) = (Vec::new(), [0u64; 2]);
                    barrier.wait();
                    while !stop.load(Ordering::Relaxed) {
                        let started = Instant::now();
                        let wrote = step(&mut *session, settings, mix, &mut zipf, &mut rng, inserted)?;
                        latencies.push(nanos_since(started));
                        counts[usize::from(wrote)] += 1;
                    }
                    Ok((latencies, counts))
                })
            })
            .collect();
        barrier.wait();
        let meter = Meter::start();
        std::thread::sleep(budget);
        stop.store(true, Ordering::Relaxed);
        let (mut latencies, mut counts) = (Vec::new(), [0u64; 2]);
        for handle in handles {
            let (l, c) = handle.join().map_err(|_| "ycsb thread panicked")??;
            latencies.extend(l);
            counts[0] += c[0];
            counts[1] += c[1];
        }
        Ok((meter, latencies, counts))
    })?;

    let mut latencies = latencies;
    let ops = latencies.len() as u64;
    let mut record = meter.finish(
        engine.info(),
        &format!("ycsb.{}", mix.name),
        params([("threads", json!(threads)), ("records", json!(settings.records))]),
        ops,
        ops * settings.value_size as u64,
        &mut latencies,
    );
    record.quality = Some(json!({ "reads": counts[0], "writes": counts[1] }));
    Ok(record)
}

/// One YCSB operation; returns whether it wrote.
fn step(
    session: &mut dyn Session,
    settings: &Settings,
    mix: Mix,
    zipf: &mut Zipf,
    rng: &mut Rng,
    inserted: &AtomicU64,
) -> Result<bool> {
    let existing = inserted.load(Ordering::Relaxed);
    let pick = |zipf: &mut Zipf, rng: &mut Rng| {
        let rank = zipf.next(rng);
        if mix.latest {
            existing - 1 - rank.min(existing - 1)
        } else {
            scramble(rank, settings.records)
        }
    };
    let roll = rng.unit();
    if roll < mix.read {
        let i = pick(zipf, rng);
        // A key inserted by another thread may still be committing; loaded keys must be there.
        if session.get(&key(i))?.is_none() && i < settings.records {
            return Err("ycsb read of a loaded key found nothing".into());
        }
        return Ok(false);
    }
    if roll < mix.read + mix.update {
        let i = pick(zipf, rng);
        session.put(&[(&key(i), &value(i ^ rng.next_u64(), settings.value_size))])?;
        return Ok(true);
    }
    if roll < mix.read + mix.update + mix.insert {
        let i = inserted.fetch_add(1, Ordering::Relaxed);
        session.put(&[(&key(i), &value(i, settings.value_size))])?;
        return Ok(true);
    }
    if roll < mix.read + mix.update + mix.insert + mix.scan {
        let start = key(pick(zipf, rng));
        session.range_after(&start, usize::try_from(1 + rng.below(MAX_SCAN))?)?;
        return Ok(false);
    }
    debug_assert!(mix.rmw > 0.0);
    let i = pick(zipf, rng);
    let k = key(i);
    session.get(&k)?.ok_or("ycsb read-modify-write of a loaded key found nothing")?;
    session.put(&[(&k, &value(i ^ rng.next_u64(), settings.value_size))])?;
    Ok(true)
}
