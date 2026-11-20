//! Key-value sections: writes, reads (load, point, multi-key, range, scan) and YCSB.

use crate::record::Record;
use crate::report::{column, count, duration_us, label, mib, param_str, param_u64, pivot, Column, Data, Table};

fn sync_cell(data: &Data, r: &Record) -> String {
    format!("`{}` ({})", r.engine.sync.label(), data.class(&r.engine))
}

fn ops(r: Option<&Record>) -> String {
    r.map_or("—".into(), |r| count(r.ops_per_sec))
}

fn mb_per_sec(r: Option<&Record>) -> String {
    r.map_or("—".into(), |r| format!("{:.1}", r.logical_bytes as f64 / r.seconds.max(f64::MIN_POSITIVE) / 1e6))
}

fn p(r: Option<&Record>, pick: fn(&Record) -> f64) -> String {
    r.map_or("—".into(), |r| duration_us(pick(r)))
}

pub fn writes(data: &Data) -> String {
    let mut out = String::new();

    let commits = data.select("engine", "write.commit", |_| true);
    let mut threads: Vec<u64> = commits.iter().filter_map(|r| param_u64(r, "threads")).collect();
    threads.sort_unstable();
    threads.dedup();
    if !commits.is_empty() {
        let columns: Vec<Column> = threads.iter().map(|&t| column(move |r| param_u64(r, "threads") == Some(t))).collect();
        let mut headers = vec!["Engine".to_owned(), "Sync".to_owned()];
        headers.extend(threads.iter().map(|t| format!("{t} writer{}", if *t == 1 { "" } else { "s" })));
        headers.extend(["p50 (1 writer)".into(), "p99 (1 writer)".into()]);
        let mut t = Table::new(headers);
        for (engine, cells) in pivot(&commits, &columns) {
            let first = cells.first().copied().flatten();
            let mut row = vec![label(engine), first.map_or("—".into(), |r| sync_cell(data, r))];
            row.extend(cells.iter().map(|c| ops(*c)));
            row.push(p(first, |r| r.latency.p50_us));
            row.push(p(first, |r| r.latency.p99_us));
            t.row(row);
        }
        out.push_str(&format!(
            "### Single-key commits\n\nCommits per second; each writer thread commits one key per transaction and waits for it.\n\n{}\n",
            t.render()
        ));
    }

    let batches = data.select("engine", "write.batch", |r| param_u64(r, "value_size") == Some(256));
    if !batches.is_empty() {
        let sizes = [100u64, 1_000, 10_000];
        let columns: Vec<Column> = sizes.iter().map(|&b| column(move |r| param_u64(r, "batch") == Some(b))).collect();
        let mut t = Table::new(["Engine", "100 keys/commit", "1,000 keys/commit", "10,000 keys/commit", "p99 commit (1,000)"]);
        for (engine, cells) in pivot(&batches, &columns) {
            t.row([label(engine), ops(cells[0]), ops(cells[1]), ops(cells[2]), p(cells[1], |r| r.latency.p99_us)]);
        }
        out.push_str(&format!("### Batched writes\n\nKeys per second, 256-byte values, one writer.\n\n{}\n", t.render()));
    }

    let sizes = data.select("engine", "write.batch", |r| param_u64(r, "batch") == Some(100) && param_u64(r, "value_size") != Some(256));
    if !sizes.is_empty() {
        let values = [1_024u64, 4_096, 65_536];
        let columns: Vec<Column> = values.iter().map(|&v| column(move |r| param_u64(r, "value_size") == Some(v))).collect();
        let mut t = Table::new(["Engine", "1 KiB values", "4 KiB values", "64 KiB values"]);
        for (engine, cells) in pivot(&sizes, &columns) {
            t.row([label(engine), mb_per_sec(cells[0]), mb_per_sec(cells[1]), mb_per_sec(cells[2])]);
        }
        out.push_str(&format!("### Value size\n\nPayload MB/s, 100 keys per commit, one writer.\n\n{}\n", t.render()));
    }

    let updates = data.select("engine", "write.update", |_| true);
    let deletes = data.select("engine", "write.delete", |_| true);
    if !updates.is_empty() || !deletes.is_empty() {
        let both: Vec<&Record> = updates.iter().chain(&deletes).copied().collect();
        let mut t = Table::new(["Engine", "Updates (keys/s)", "p99 commit", "Deletes (keys/s)", "p99 commit"]);
        for (engine, cells) in pivot(&both, &[column(|r| r.workload == "write.update"), column(|r| r.workload == "write.delete")]) {
            t.row([label(engine), ops(cells[0]), p(cells[0], |r| r.latency.p99_us), ops(cells[1]), p(cells[1], |r| r.latency.p99_us)]);
        }
        out.push_str(&format!(
            "### Updates and deletes\n\nExisting keys (100,000 loaded), 1,000 per commit; updates pick keys uniformly at random.\n\n{}\n",
            t.render()
        ));
    }
    if out.is_empty() { out } else { format!("## Writes\n\n{out}") }
}

pub fn reads(data: &Data) -> String {
    let mut out = String::new();

    let loads = data.select("engine", "load", |_| true);
    if !loads.is_empty() {
        let mut t = Table::new(["Engine", "Load (keys/s)", "Disk after load", "Space amplification", "Write amplification"]);
        let mut rows: Vec<&Record> = loads.clone();
        rows.sort_by(|a, b| b.ops_per_sec.total_cmp(&a.ops_per_sec));
        for r in rows {
            let disk = r.disk_bytes.unwrap_or(0);
            t.row([
                label(&r.engine),
                count(r.ops_per_sec),
                mib(disk),
                format!("{:.2}×", disk as f64 / r.logical_bytes.max(1) as f64),
                if r.io.device_bytes_written > 0 { format!("{:.2}×", r.io.device_bytes_written as f64 / r.logical_bytes.max(1) as f64) } else { "—".into() },
            ]);
        }
        out.push_str(&format!(
            "### Bulk load and space\n\n{} keys in commits of 1,000. Space amplification is disk size over payload bytes; write amplification is bytes the process wrote to the device over payload bytes.\n\n{}\n",
            loads.first().and_then(|r| param_u64(r, "records")).map_or("?".into(), |n| count(n as f64)),
            t.render()
        ));
    }

    let gets = data.select("engine", "read.get", |_| true);
    if !gets.is_empty() {
        let mut threads: Vec<u64> = gets.iter().filter(|r| param_str(r, "state") == Some("warm")).filter_map(|r| param_u64(r, "threads")).collect();
        threads.sort_unstable();
        threads.dedup();
        let mut columns = vec![
            column(|r| param_str(r, "state") == Some("warm") && param_u64(r, "threads") == Some(1)),
            column(|r| param_str(r, "state") == Some("reopened")),
        ];
        for &t in threads.iter().filter(|&&t| t > 1) {
            columns.push(column(move |r| param_str(r, "state") == Some("warm") && param_u64(r, "threads") == Some(t)));
        }
        let mut headers = vec!["Engine".to_owned(), "1 thread".into(), "After reopen".into()];
        headers.extend(threads.iter().filter(|&&t| t > 1).map(|t| format!("{t} threads")));
        headers.extend(["p50 (1 thread)".into(), "p99 (1 thread)".into()]);
        let mut t = Table::new(headers);
        for (engine, cells) in pivot(&gets, &columns) {
            let mut row = vec![label(engine)];
            row.extend(cells.iter().map(|c| ops(*c)));
            row.push(p(cells[0], |r| r.latency.p50_us));
            row.push(p(cells[0], |r| r.latency.p99_us));
            t.row(row);
        }
        out.push_str(&format!(
            "### Point reads\n\nReads per second of uniformly random existing keys. \"After reopen\" is the first 100,000 reads after closing and reopening the engine.\n\n{}\n",
            t.render()
        ));
    }

    let others: Vec<&Record> = data.records.iter().copied().filter(|r| r.suite == "engine" && matches!(r.workload.as_str(), "read.multi_get" | "read.range" | "read.scan")).collect();
    if !others.is_empty() {
        let mut t = Table::new(["Engine", "Multi-get, 100 keys (keys/s)", "Range of 100 (keys/s)", "p50 per range", "Full scan (keys/s)"]);
        for (engine, cells) in pivot(&others, &[column(|r| r.workload == "read.multi_get"), column(|r| r.workload == "read.range"), column(|r| r.workload == "read.scan")]) {
            t.row([label(engine), ops(cells[0]), ops(cells[1]), p(cells[1], |r| r.latency.p50_us), ops(cells[2])]);
        }
        out.push_str(&format!("### Multi-key, range and scan reads\n\n{}\n", t.render()));
    }
    if out.is_empty() { out } else { format!("## Reads\n\n{out}") }
}

pub fn ycsb(data: &Data) -> String {
    let all: Vec<&Record> = data.records.iter().copied().filter(|r| r.suite == "engine" && r.workload.starts_with("ycsb.")).collect();
    if all.is_empty() {
        return String::new();
    }
    let mut threads: Vec<u64> = all.iter().filter_map(|r| param_u64(r, "threads")).collect();
    threads.sort_unstable();
    threads.dedup();
    let mut out = String::from(
        "## YCSB\n\nOperations per second. A: 50% read / 50% update. B: 95/5. C: read only. D: 95% read of recent keys / 5% insert. E: 95% short range (1–100 keys) / 5% insert. F: 50% read / 50% read-modify-write. Every write is its own commit.\n\n",
    );
    for t in threads {
        let records: Vec<&Record> = all.iter().copied().filter(|r| param_u64(r, "threads") == Some(t)).collect();
        let columns: Vec<Column> = ["a", "b", "c", "d", "e", "f"]
            .iter()
            .map(|w| {
                let name = format!("ycsb.{w}");
                column(move |r| r.workload == name)
            })
            .collect();
        let mut table = Table::new(["Engine", "A", "B", "C", "D", "E", "F", "p99 (A)"]);
        for (engine, cells) in pivot(&records, &columns) {
            let mut row = vec![label(engine)];
            row.extend(cells.iter().map(|c| ops(*c)));
            row.push(p(cells[0], |r| r.latency.p99_us));
            table.row(row);
        }
        out.push_str(&format!("### {t} client thread{}\n\n{}\n", if t == 1 { "" } else { "s" }, table.render()));
    }
    out
}
