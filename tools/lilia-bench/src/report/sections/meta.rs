//! Environment, engine list, summary, failures and method.

use crate::record::{EngineInfo, Record};
use crate::report::{count, duration_us, is_lilia, label, mib, param_str, param_u64, ratio, Data, Table};

pub fn environment(data: &Data) -> String {
    let Some(host) = data.host else { return String::new() };
    let mut t = Table::new(["", ""]);
    t.row(["Date (UTC)".into(), host.date.clone()]);
    t.row([
        "LiliaDB".into(),
        format!("{} @ `{}`{}", host.lilia_version, host.lilia_commit, if host.lilia_dirty { " (uncommitted changes)" } else { "" }),
    ]);
    let levels: Vec<String> = host.cores_by_level.iter().map(usize::to_string).collect();
    t.row(["CPU".into(), format!("{} — {} logical, physical by performance level: {}", host.cpu, host.logical_cpus, levels.join(" + "))]);
    t.row(["Memory".into(), format!("{:.0} GiB", host.memory_bytes as f64 / f64::from(1u32 << 30))]);
    t.row(["OS".into(), format!("{} ({})", host.os_version, host.arch)]);
    t.row(["VM page / cache line".into(), format!("{} B / {} B", host.page_size, host.cache_line)]);
    t.row(["Rust".into(), host.rustc.clone()]);
    let runtimes = [("Node", &host.node), ("Bun", &host.bun), ("Deno", &host.deno)]
        .iter()
        .map(|(name, v)| format!("{name} {}", v.as_deref().unwrap_or("absent")))
        .collect::<Vec<_>>()
        .join(", ");
    t.row(["JavaScript runtimes".into(), runtimes]);
    t.row(["Profile".into(), format!("`{}` — `{}`", host.profile, host.settings)]);
    format!("## Environment\n\n{}", t.render())
}

/// Single-writer commit record of an engine, used to check its declared sync.
fn commit_probe<'a>(data: &Data<'a>, engine: &EngineInfo) -> Option<&'a Record> {
    data.select("engine", "write.commit", |r| r.engine.id == engine.id && param_u64(r, "threads") == Some(1)).into_iter().next()
}

pub fn engines(data: &Data) -> String {
    let kv: Vec<&Record> = data.records.iter().copied().filter(|r| r.suite == "engine" && r.workload == "write.commit").collect();
    if kv.is_empty() {
        return String::new();
    }
    let apple = matches!(data.os(), "macos" | "ios");
    let mut t = Table::new(["Engine", "Version", "Commit sync", "Class on this OS", "1-writer commit p50", "Check"]);
    for engine in Data::engines(&kv) {
        let p50 = commit_probe(data, engine).map(|r| r.latency.p50_us);
        // Only a power-safe commit has a physical lower bound (one drive-cache flush); a slow
        // drive-cache commit proves nothing about its sync, so it is not flagged.
        let check = match (apple, data.class(engine), p50) {
            (true, "power-safe", Some(p)) if p < 1_000.0 => "⚠ faster than a drive-cache flush: declared sync not observed",
            (true, "power-safe", Some(_)) => "at least one flush per commit",
            (_, _, Some(_)) => "not verifiable from latency",
            (_, _, None) => "not measured",
        };
        t.row([
            label(engine),
            engine.version.clone(),
            format!("`{}`", engine.sync.label()),
            data.class(engine).to_owned(),
            p50.map_or("—".into(), duration_us),
            check.into(),
        ]);
    }
    let note = if apple {
        "On Apple platforms `fsync` returns once data reaches the drive's volatile cache; only \
         `F_FULLFSYNC` survives power loss. Engines are compared within a class, and against the \
         power-safe class. A power-safe commit cannot be faster than one drive-cache flush (about \
         4 ms on Apple SSDs), which the check column verifies.\n\n"
    } else {
        ""
    };
    format!("## Engines and durability\n\n{note}{}", t.render())
}

/// One summary line: how to find LiliaDB's record and its competitors'.
struct Metric {
    title: &'static str,
    workload: &'static str,
    keep: fn(&Record) -> bool,
    value: fn(&Record) -> f64,
    unit: &'static str,
    higher_is_better: bool,
}

fn summary_metrics(max_writers: u64, max_readers: u64) -> Vec<Metric> {
    let throughput = |r: &Record| r.ops_per_sec;
    let mut metrics = vec![
        Metric { title: "Single-key commits, 1 writer", workload: "write.commit", keep: |r| param_u64(r, "threads") == Some(1), value: throughput, unit: "commits/s", higher_is_better: true },
        Metric { title: "Single-key commits, 8 writers", workload: "write.commit", keep: |r| param_u64(r, "threads") == Some(8), value: throughput, unit: "commits/s", higher_is_better: true },
        Metric { title: "Batched writes, 1,000 keys per commit", workload: "write.batch", keep: |r| param_u64(r, "batch") == Some(1_000) && param_u64(r, "value_size") == Some(256), value: throughput, unit: "keys/s", higher_is_better: true },
        Metric { title: "Bulk load", workload: "load", keep: |_| true, value: throughput, unit: "keys/s", higher_is_better: true },
        Metric { title: "Point reads, 1 thread", workload: "read.get", keep: |r| param_u64(r, "threads") == Some(1) && param_str(r, "state") == Some("warm"), value: throughput, unit: "reads/s", higher_is_better: true },
        Metric { title: "Range reads, 100 keys", workload: "read.range", keep: |_| true, value: throughput, unit: "keys/s", higher_is_better: true },
        Metric { title: "Full ordered scan", workload: "read.scan", keep: |_| true, value: throughput, unit: "keys/s", higher_is_better: true },
        Metric { title: "YCSB A (50% update), 1 thread", workload: "ycsb.a", keep: |r| param_u64(r, "threads") == Some(1), value: throughput, unit: "ops/s", higher_is_better: true },
        Metric { title: "YCSB C (read only), 1 thread", workload: "ycsb.c", keep: |r| param_u64(r, "threads") == Some(1), value: throughput, unit: "ops/s", higher_is_better: true },
        Metric { title: "Disk size after bulk load (lower is better)", workload: "load", keep: |r| r.disk_bytes.is_some(), value: |r| r.disk_bytes.unwrap_or(0) as f64, unit: "bytes", higher_is_better: false },
        Metric { title: "Write amplification of bulk load (lower is better)", workload: "load", keep: |r| r.io.device_bytes_written > 0, value: |r| r.io.device_bytes_written as f64 / r.logical_bytes.max(1) as f64, unit: "×", higher_is_better: false },
    ];
    if max_writers > 8 {
        metrics.insert(2, Metric { title: "Single-key commits, most writers measured", workload: "write.commit", keep: |_| true, value: throughput, unit: "commits/s", higher_is_better: true });
    }
    if max_readers > 1 {
        let after = metrics.iter().position(|m| m.title == "Point reads, 1 thread").map_or(metrics.len(), |i| i + 1);
        metrics.insert(after, Metric { title: "Point reads, most threads measured", workload: "read.get", keep: |r| param_str(r, "state") == Some("warm"), value: throughput, unit: "reads/s", higher_is_better: true });
    }
    metrics
}

fn show(value: f64, unit: &str) -> String {
    match unit {
        "bytes" => mib(value as u64),
        "×" => format!("{value:.2}×"),
        _ => format!("{} {unit}", count(value)),
    }
}

pub fn summary(data: &Data) -> String {
    let max_of = |workload: &str| data.select("engine", workload, |_| true).iter().filter_map(|r| param_u64(r, "threads")).max().unwrap_or(1);
    let (max_writers, max_readers) = (max_of("write.commit"), max_of("read.get"));
    let mut t = Table::new(["Metric", "LiliaDB", "Best competitor, same sync class", "LiliaDB ÷ it", "Best power-safe competitor", "LiliaDB ÷ it"]);
    for metric in summary_metrics(max_writers, max_readers) {
        let all = data.select("engine", metric.workload, metric.keep);
        // "Most threads" metrics keep only the highest thread count present.
        let all: Vec<&Record> = if metric.title.contains("most") {
            let top = all.iter().filter_map(|r| param_u64(r, "threads")).max();
            all.into_iter().filter(|r| param_u64(r, "threads") == top).collect()
        } else {
            all
        };
        let Some(lilia) = all.iter().find(|r| r.engine.id == "lilia") else { continue };
        let lilia_class = data.class(&lilia.engine);
        let best = |class: &str| {
            all.iter()
                .filter(|r| !is_lilia(&r.engine.id) && data.class(&r.engine) == class)
                .max_by(|a, b| {
                    let order = (metric.value)(a).total_cmp(&(metric.value)(b));
                    if metric.higher_is_better { order } else { order.reverse() }
                })
                .copied()
        };
        let cell = |r: Option<&Record>| -> (String, String) {
            r.map_or_else(
                || ("—".into(), "—".into()),
                |r| (format!("{} ({})", show((metric.value)(r), metric.unit), r.engine.name), ratio((metric.value)(lilia) / (metric.value)(r))),
            )
        };
        let (same, same_ratio) = cell(best(lilia_class));
        let (safe, safe_ratio) = if lilia_class == "power-safe" { ("same class".into(), "—".into()) } else { cell(best("power-safe")) };
        let title = match metric.workload {
            "write.commit" | "read.get" if metric.title.contains("most") => {
                format!("{} ({})", metric.title, param_u64(lilia, "threads").unwrap_or(0))
            }
            _ => metric.title.to_owned(),
        };
        t.row([title, show((metric.value)(lilia), metric.unit), same, same_ratio, safe, safe_ratio]);
    }
    if t.is_empty() {
        return String::new();
    }
    let lilia_class = data.records.iter().find(|r| r.engine.id == "lilia").map_or("", |r| data.class(&r.engine));
    format!(
        "## Summary\n\nLiliaDB (default configuration: `Durability::Durable`, ledger on) against the fastest \
         other engine of its sync class (`{lilia_class}`), and against the fastest power-safe engine. \
         A ratio below 1 means LiliaDB is slower (or, for lower-is-better rows, smaller).\n\n{}",
        t.render()
    )
}

pub fn failures(data: &Data) -> String {
    if data.failures.is_empty() {
        return String::new();
    }
    let mut t = Table::new(["Suite", "Engine", "Workload", "Reason"]);
    for f in &data.failures {
        t.row([f.suite.clone(), f.engine.clone(), f.workload.clone(), format!("`{}`", f.reason.replace('|', "\\|").replace('\n', " "))]);
    }
    format!("## Runs without results\n\nThese planned measurements produced no data; their engines are missing from the tables above.\n\n{}", t.render())
}

pub fn method(data: &Data) -> String {
    let Some(host) = data.host else { return String::new() };
    let s = &host.settings;
    let get = |k: &str| s.get(k).map_or_else(|| "?".into(), ToString::to_string);
    format!(
        "## Method\n\n\
         - **Harness.** `tools/lilia-bench` builds one binary per engine group (LiliaDB and SQLite each \
         embed a SQLite amalgamation, so they cannot share one) and runs every workload group of every \
         engine in its own process. Each write workload starts from an empty database directory.\n\
         - **Data.** 16-byte keys whose first 8 bytes are a hash (inserts land at random positions in \
         every ordered index), {value} B values of mixed text, {records} records for reads and YCSB.\n\
         - **Durability.** Every engine runs in its most durable configuration; a write call returns \
         after the engine's commit sync. The sync primitive is declared per engine and checked against \
         the observed single-commit latency.\n\
         - **Time boxes.** Throughput workloads run for {budget} ms of wall-clock time each; \
         throughput is operations completed divided by elapsed time. Point-read latency is sampled \
         every 8th read so the clock does not dominate sub-microsecond reads.\n\
         - **Caches.** \"Reopened\" reads run right after closing and reopening the engine (cold \
         engine caches); the OS page cache is not purged, which needs root.\n\
         - **Correctness.** Reads verify that every loaded key is found, scans verify the record \
         count, JSON reads verify extracted values, analytics answers are compared with the value \
         computed from the generator, and vector search reports recall@10 against an exact scan. A \
         wrong answer fails the run instead of producing a number.\n\
         - **Resources.** Device bytes come from the process's own I/O counters \
         (`proc_pid_rusage` on macOS, `/proc/self/io` on Linux) and include background threads; \
         write amplification is device bytes written divided by payload bytes (keys + values). \
         Disk size is measured with the engine closed.\n\
         - **YCSB.** Workloads A–F with a Zipfian (θ = 0.99) request distribution over the loaded \
         records; every update or insert is its own commit.\n",
        value = get("value_size"),
        records = get("records"),
        budget = get("budget_ms"),
    )
}
