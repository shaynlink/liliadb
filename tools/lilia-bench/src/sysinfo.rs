//! Description of the machine and build a report was measured on.

use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Host {
    /// UTC timestamp, RFC 3339.
    pub date: String,
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub cpu: String,
    pub logical_cpus: usize,
    /// Physical cores per performance level, fastest first (e.g. `[10, 4]` on M3 Max).
    pub cores_by_level: Vec<usize>,
    pub memory_bytes: u64,
    pub page_size: u64,
    pub cache_line: u64,
    pub rustc: String,
    pub node: Option<String>,
    pub bun: Option<String>,
    pub deno: Option<String>,
    pub lilia_version: String,
    pub lilia_commit: String,
    /// Uncommitted changes in the LiliaDB checkout when the suite ran.
    pub lilia_dirty: bool,
    pub profile: String,
    pub settings: serde_json::Value,
}

fn output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn sysctl(name: &str) -> Option<String> {
    output("sysctl", &["-n", name])
}

fn sysctl_u64(name: &str) -> u64 {
    sysctl(name).and_then(|v| v.parse().ok()).unwrap_or(0)
}

/// Workspace version from the repository manifest (`[workspace.package] version`).
fn workspace_version(repo: &std::path::Path) -> String {
    std::fs::read_to_string(repo.join("Cargo.toml"))
        .ok()
        .and_then(|text| {
            let section = text.split("[workspace.package]").nth(1)?;
            let line = section.lines().find(|l| l.trim_start().starts_with("version"))?;
            Some(line.split('"').nth(1)?.to_owned())
        })
        .unwrap_or_else(|| "unknown".into())
}

#[must_use]
pub fn probe(repo: &std::path::Path, profile: &str, settings: serde_json::Value) -> Host {
    let os = std::env::consts::OS.to_owned();
    let (cpu, cores_by_level, memory_bytes, page_size, cache_line, os_version) = if cfg!(target_os = "macos") {
        let levels = sysctl_u64("hw.nperflevels").max(1);
        let cores = (0..levels)
            .map(|l| usize::try_from(sysctl_u64(&format!("hw.perflevel{l}.physicalcpu"))).unwrap_or(0))
            .collect();
        (
            sysctl("machdep.cpu.brand_string").unwrap_or_default(),
            cores,
            sysctl_u64("hw.memsize"),
            sysctl_u64("hw.pagesize"),
            sysctl_u64("hw.cachelinesize"),
            format!(
                "macOS {} (Darwin {})",
                output("sw_vers", &["-productVersion"]).unwrap_or_default(),
                sysctl("kern.osrelease").unwrap_or_default()
            ),
        )
    } else {
        let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        let cpu = cpuinfo
            .lines()
            .find_map(|l| l.strip_prefix("model name").and_then(|r| r.split(':').nth(1)))
            .unwrap_or("")
            .trim()
            .to_owned();
        let mem_kib = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|m| m.lines().next().and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok()))
            .unwrap_or(0);
        (
            cpu,
            vec![std::thread::available_parallelism().map_or(0, usize::from)],
            mem_kib * 1024,
            output("getconf", &["PAGESIZE"]).and_then(|v| v.parse().ok()).unwrap_or(0),
            output("getconf", &["LEVEL1_DCACHE_LINESIZE"]).and_then(|v| v.parse().ok()).unwrap_or(0),
            output("uname", &["-sr"]).unwrap_or_default(),
        )
    };
    let git = |args: &[&str]| output("git", &[&["-C", &repo.to_string_lossy()], args].concat());
    Host {
        date: utc_now(),
        os,
        os_version,
        arch: std::env::consts::ARCH.to_owned(),
        cpu,
        logical_cpus: std::thread::available_parallelism().map_or(0, usize::from),
        cores_by_level,
        memory_bytes,
        page_size,
        cache_line,
        rustc: output("rustc", &["--version"]).unwrap_or_default(),
        node: output("node", &["--version"]),
        bun: output("bun", &["--version"]),
        deno: output("deno", &["--version"]).and_then(|v| v.lines().next().map(str::to_owned)),
        lilia_version: workspace_version(repo),
        lilia_commit: git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into()),
        lilia_dirty: git(&["status", "--porcelain", "--untracked-files=no"]).is_some(),
        profile: profile.to_owned(),
        settings,
    }
}

/// Current UTC time as `YYYY-MM-DDTHH:MM:SSZ`.
fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil-from-days (Howard Hinnant), valid for the proleptic Gregorian calendar.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}
