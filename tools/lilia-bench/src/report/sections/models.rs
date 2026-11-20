//! Model sections (JSON, full-text, vectors, analytics) and the JavaScript SDK surfaces.

use crate::record::Record;
use crate::report::{column, count, duration_us, label, mib, param_str, param_u64, pivot, Data, Table};

fn ops(r: Option<&Record>) -> String {
    r.map_or("—".into(), |r| count(r.ops_per_sec))
}

fn p50(r: Option<&Record>) -> String {
    r.map_or("—".into(), |r| duration_us(r.latency.p50_us))
}

fn p99(r: Option<&Record>) -> String {
    r.map_or("—".into(), |r| duration_us(r.latency.p99_us))
}

fn quality(r: Option<&Record>, field: &str) -> Option<f64> {
    r.and_then(|r| r.quality.as_ref()).and_then(|q| q.get(field)).and_then(serde_json::Value::as_f64)
}

pub fn json(data: &Data) -> String {
    let puts = data.select("engine", "json.put", |_| true);
    let reads: Vec<&Record> = data.records.iter().copied().filter(|r| matches!(r.workload.as_str(), "json.get" | "json.extract" | "json.find")).collect();
    if puts.is_empty() && reads.is_empty() {
        return String::new();
    }
    let mut out = String::from("## JSON documents\n\nOne secondary index on `$.city`, maintained by every insert. SQLite stores documents as `jsonb` with an expression index.\n\n");
    let mut t = Table::new(["Engine", "1 KiB docs/s", "4 KiB docs/s", "64 KiB docs/s", "Disk per 1 KiB doc"]);
    for (engine, cells) in pivot(&puts, &[1_024, 4_096, 65_536].map(|size| column(move |r| param_u64(r, "doc_size") == Some(size)))) {
        let per_doc = cells[0].and_then(|r| r.disk_bytes.map(|d| d as f64 / r.ops.max(1) as f64));
        t.row([label(engine), ops(cells[0]), ops(cells[1]), ops(cells[2]), per_doc.map_or("—".into(), |b| format!("{b:.0} B"))]);
    }
    out.push_str(&format!("### Indexed inserts (100 documents per commit)\n\n{}\n", t.render()));

    let mut t = Table::new(["Engine", "Get by id (ops/s)", "p50", "Extract `$.user.name` (ops/s)", "p50", "Indexed find, 100 docs (queries/s)", "p50"]);
    for (engine, cells) in pivot(&reads, &["json.get", "json.extract", "json.find"].map(|w| column(move |r| r.workload == w))) {
        t.row([label(engine), ops(cells[0]), p50(cells[0]), ops(cells[1]), p50(cells[1]), ops(cells[2]), p50(cells[2])]);
    }
    let docs = reads.first().and_then(|r| param_u64(r, "documents")).map_or("?".into(), |n| count(n as f64));
    out.push_str(&format!("### Reads ({docs} documents of 1 KiB, every result decoded)\n\n{}\n", t.render()));
    out
}

pub fn fts(data: &Data) -> String {
    let all: Vec<&Record> = data.records.iter().copied().filter(|r| r.workload.starts_with("fts.")).collect();
    if all.is_empty() {
        return String::new();
    }
    let mut t = Table::new(["Engine", "Index (docs/s)", "Index size", "1-term top-10 (q/s)", "p50", "2-term top-10 (q/s)", "p50", "Mean hits (2 terms)"]);
    for (engine, cells) in pivot(
        &all,
        &[
            column(|r| r.workload == "fts.index"),
            column(|r| r.workload == "fts.search" && param_u64(r, "terms") == Some(1)),
            column(|r| r.workload == "fts.search" && param_u64(r, "terms") == Some(2)),
        ],
    ) {
        t.row([
            label(engine),
            ops(cells[0]),
            cells[0].and_then(|r| r.disk_bytes).map_or("—".into(), mib),
            ops(cells[1]),
            p50(cells[1]),
            ops(cells[2]),
            p50(cells[2]),
            quality(cells[2], "mean_hits").map_or("—".into(), |h| format!("{h:.1}")),
        ]);
    }
    let docs = all.first().and_then(|r| param_u64(r, "documents")).map_or("?".into(), |n| count(n as f64));
    format!(
        "## Full-text search\n\n{docs} synthetic documents of 60 words drawn from a Zipf-distributed 20,000-word vocabulary, indexed 100 per commit. Queries use mid-frequency terms and return the 10 best-ranked documents (BM25). Engines differ in how they combine two terms; mean hits shows what each returned.\n\n{}",
        t.render()
    )
}

pub fn vectors(data: &Data) -> String {
    let all: Vec<&Record> = data.records.iter().copied().filter(|r| r.workload.starts_with("vector.")).collect();
    if all.is_empty() {
        return String::new();
    }
    let mut t = Table::new(["Engine", "Insert (vectors/s)", "Disk", "Top-10 search (q/s)", "p50", "p99", "Recall@10"]);
    for (engine, cells) in pivot(&all, &["vector.insert", "vector.search"].map(|w| column(move |r| r.workload == w))) {
        t.row([
            label(engine),
            ops(cells[0]),
            cells[0].and_then(|r| r.disk_bytes).map_or("—".into(), mib),
            ops(cells[1]),
            p50(cells[1]),
            p99(cells[1]),
            quality(cells[1], "recall_at_10").map_or("—".into(), |r| format!("{:.3}", r)),
        ]);
    }
    let shape = all.first().map_or(String::new(), |r| {
        format!("{} vectors of dimension {}", param_u64(r, "vectors").map_or("?".into(), |n| count(n as f64)), param_u64(r, "dim").unwrap_or(0))
    });
    format!(
        "## Vector search\n\n{shape}, clustered, unit length, cosine distance, inserted 100 per commit with the index in place. The exact scan is the in-process reference that defines recall; an index is only useful where it is faster.\n\n{}",
        t.render()
    )
}

pub fn analytics(data: &Data) -> String {
    let all: Vec<&Record> = data.records.iter().copied().filter(|r| r.workload.starts_with("analytics.")).collect();
    if all.is_empty() {
        return String::new();
    }
    let mut t = Table::new(["Engine", "Mode", "Load (rows/s)", "Build", "Filter + aggregate (median)", "Group by (median)"]);
    let mut modes: Vec<(&Record, &str)> =
        all.iter().filter(|r| r.workload == "analytics.filter_aggregate").filter_map(|r| param_str(r, "mode").map(|m| (*r, m))).collect();
    modes.sort_by(|a, b| a.0.latency.p50_us.total_cmp(&b.0.latency.p50_us));
    for (record, mode) in modes {
        let same = |workload: &str| all.iter().copied().find(|r| r.engine.name == record.engine.name && r.workload == workload && (workload == "analytics.load" || param_str(r, "mode") == Some(mode)));
        t.row([
            label(&record.engine),
            mode.to_owned(),
            ops(same("analytics.load")),
            same("analytics.build").map_or("—".into(), |r| duration_us(r.seconds * 1e6)),
            p50(Some(record)),
            p50(same("analytics.group_by")),
        ]);
    }
    let rows = all.first().and_then(|r| param_u64(r, "rows")).map_or("?".into(), |n| count(n as f64));
    format!(
        "## Analytics\n\n{rows} events (age, city, amount). Medians of 7 runs; every answer is checked against the generator.\n\n{}",
        t.render()
    )
}

pub fn sdk(data: &Data) -> String {
    let all: Vec<&Record> = data.records.iter().copied().filter(|r| r.suite == "sdk").collect();
    if all.is_empty() {
        return String::new();
    }
    let mut t = Table::new(["Surface", "Version", "Commits/s (1 key)", "p50", "Keys/s (100 per commit)", "Reads/s", "p50 read"]);
    for (engine, cells) in pivot(&all, &["write.commit", "write.batch", "read.get"].map(|w| column(move |r| r.workload == w))) {
        t.row([label(engine), engine.version.clone(), ops(cells[0]), p50(cells[0]), ops(cells[1]), ops(cells[2]), p50(cells[2])]);
    }
    let records = all.iter().find(|r| r.workload == "read.get").and_then(|r| param_u64(r, "records")).map_or("?".into(), |n| count(n as f64));
    format!(
        "## JavaScript surfaces\n\nThe same key-value operations through each runtime's binding, durable configuration (`synchronous=FULL` for SQLite drivers, `durability: \"durable\"` for LiliaDB), 256-byte values; reads over {records} loaded keys. Each surface pays its own call overhead (Node-API, IPC framing, HTTP).\n\n{}",
        t.render()
    )
}
