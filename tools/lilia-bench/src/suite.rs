//! `lilia-bench suite`: builds one binary per engine group, runs every (group, engine) pair in its
//! own process, adds the JavaScript SDK surfaces, then writes the raw results and the report.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::engines::Result;
use crate::record::{Failure, Line};
use crate::workloads::{Group, Settings};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Profile {
    /// Minutes: small data, one-second budgets. For CI and smoke checks; not for publication.
    Quick,
    /// The published configuration.
    Full,
}

#[derive(Debug, clap::Args)]
pub struct Args {
    #[arg(long, value_enum, default_value_t = Profile::Full)]
    pub profile: Profile,
    /// Engines to run (default: all). Example: `--engines lilia,sqlite-fullfsync,lmdb`.
    #[arg(long, value_delimiter = ',')]
    pub engines: Vec<String>,
    /// Workload groups to run (default: all).
    #[arg(long, value_delimiter = ',', value_enum)]
    pub groups: Vec<Group>,
    /// Skip the JavaScript SDK surfaces.
    #[arg(long)]
    pub no_sdk: bool,
    /// Where databases are created (default: the system temporary directory).
    #[arg(long)]
    pub work_dir: Option<PathBuf>,
    /// Raw results (JSON lines). Default: `docs/benchmarks.jsonl` in the repository.
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Rendered report. Default: `docs/benchmarks.md` in the repository.
    #[arg(long)]
    pub report: Option<PathBuf>,
}

/// One compiled binary and the engines it carries.
struct Build {
    name: &'static str,
    features: &'static [&'static str],
    engines: &'static [&'static str],
    /// Extra environment for the compilation.
    env: &'static [(&'static str, &'static str)],
    /// Only meaningful on Apple platforms.
    apple_only: bool,
}

const BUILDS: [Build; 3] = [
    Build {
        name: "core",
        features: &["lilia", "kvs", "rocksdb"],
        engines: &["lilia", "lilia-noledger", "lilia-aes", "lilia-engine", "lilia-engine-balanced", "lilia-db", "lilia-db-ledger", "redb", "fjall", "lmdb", "rocksdb"],
        env: &[],
        apple_only: false,
    },
    Build {
        name: "sqlite",
        features: &["sqlite", "duckdb"],
        engines: &["sqlite", "sqlite-fullfsync", "duckdb"],
        env: &[],
        apple_only: false,
    },
    Build {
        name: "rocksdb-fullfsync",
        features: &["rocksdb"],
        engines: &["rocksdb-fullfsync"],
        env: &[("LILIA_BENCH_ROCKSDB_FULLFSYNC", "1"), ("CXXFLAGS", "-DHAVE_FULLFSYNC")],
        apple_only: true,
    },
];

#[must_use]
pub fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[must_use]
pub fn repo_dir() -> PathBuf {
    crate_dir().join("../..").canonicalize().unwrap_or_else(|_| crate_dir().join("../.."))
}

#[must_use]
pub fn settings(profile: Profile, work_dir: PathBuf) -> Settings {
    let perf_cores = crate::sysinfo::probe(&repo_dir(), "", serde_json::Value::Null)
        .cores_by_level
        .first()
        .copied()
        .filter(|&c| c > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, usize::from));
    match profile {
        Profile::Quick => Settings {
            records: 50_000,
            value_size: 256,
            budget_ms: 1_000,
            threads: vec![1, 8],
            read_threads: vec![1, 4],
            documents: 5_000,
            vectors: 2_000,
            vector_dim: 128,
            analytics_rows: 50_000,
            work_dir,
        },
        Profile::Full => Settings {
            records: 1_000_000,
            value_size: 256,
            budget_ms: 5_000,
            threads: vec![1, 2, 4, 8, 16, 64],
            read_threads: {
                let mut t = vec![1, 4, perf_cores];
                t.dedup();
                t
            },
            documents: 100_000,
            vectors: 20_000,
            vector_dim: 768,
            analytics_rows: 500_000,
            work_dir,
        },
    }
}

fn wanted(args: &Args, engine: &str) -> bool {
    args.engines.is_empty() || args.engines.iter().any(|e| e == engine)
}

/// `LIBCLANG_PATH` for RocksDB's bindgen on macOS when the caller did not set one.
fn libclang_dir() -> Option<String> {
    if std::env::var_os("LIBCLANG_PATH").is_some() || !cfg!(target_os = "macos") {
        return None;
    }
    let developer = Command::new("xcode-select").arg("-p").output().ok()?;
    let developer = String::from_utf8_lossy(&developer.stdout).trim().to_owned();
    [
        format!("{developer}/usr/lib"),
        format!("{developer}/Toolchains/XcodeDefault.xctoolchain/usr/lib"),
        "/Library/Developer/CommandLineTools/usr/lib".to_owned(),
    ]
    .into_iter()
    .find(|dir| Path::new(dir).join("libclang.dylib").exists())
}

/// Compiles `build`; returns its binary or the reason it could not be built.
fn compile(build: &Build, features: &[&str]) -> std::result::Result<PathBuf, String> {
    let target = crate_dir().join("target").join(build.name);
    let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.args(["build", "--release", "--no-default-features", "--manifest-path"])
        .arg(crate_dir().join("Cargo.toml"))
        .arg("--features")
        .arg(features.join(","))
        .arg("--target-dir")
        .arg(&target)
        .envs(build.env.iter().copied())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(dir) = libclang_dir() {
        cmd.env("LIBCLANG_PATH", &dir).env("DYLD_FALLBACK_LIBRARY_PATH", &dir);
    }
    eprintln!("· building `{}` ({})", build.name, features.join(", "));
    let output = cmd.output().map_err(|e| format!("cargo could not start: {e}"))?;
    if output.status.success() {
        Ok(target.join("release").join("lilia-bench"))
    } else {
        Err(tail(&String::from_utf8_lossy(&output.stderr), 12))
    }
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

fn failure(suite: &str, engine: &str, workload: &str, reason: impl Into<String>) -> Line {
    Line::Failure(Failure { suite: suite.into(), engine: engine.into(), workload: workload.into(), reason: reason.into() })
}

/// Runs `program` and collects the result lines it prints; anything else on stdout is ignored.
fn collect(mut cmd: Command, suite: &str, engine: &str, workload: &str, lines: &mut Vec<Line>) {
    let started = Instant::now();
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return lines.push(failure(suite, engine, workload, format!("could not start: {e}"))),
    };
    let stderr = child.stderr.take().map(|s| std::thread::spawn(move || std::io::read_to_string(s).unwrap_or_default()));
    let (mut records, mut failed) = (0, false);
    if let Some(stdout) = child.stdout.take() {
        for text in BufReader::new(stdout).lines().map_while(std::io::Result::ok) {
            if let Ok(line) = serde_json::from_str::<Line>(&text) {
                failed |= matches!(line, Line::Failure(_));
                records += usize::from(matches!(line, Line::Record(_)));
                lines.push(line);
            }
        }
    }
    let status = child.wait();
    let stderr = stderr.and_then(|h| h.join().ok()).unwrap_or_default();
    match status {
        Ok(s) if s.success() || failed => {}
        Ok(s) => lines.push(failure(suite, engine, workload, format!("exited with {s}: {}", tail(&stderr, 8)))),
        Err(e) => lines.push(failure(suite, engine, workload, format!("could not be awaited: {e}"))),
    }
    eprintln!("  {workload:<10} {engine:<18} {records:>3} records in {:>6.1}s{}", started.elapsed().as_secs_f64(), if failed { "  FAILED" } else { "" });
}

pub fn run(args: &Args) -> Result<()> {
    let repo = repo_dir();
    let work_dir = args.work_dir.clone().unwrap_or_else(std::env::temp_dir).join(format!("lilia-bench-{}", std::process::id()));
    let settings = settings(args.profile, work_dir.clone());
    let profile = match args.profile {
        Profile::Quick => "quick",
        Profile::Full => "full",
    };
    let groups: Vec<Group> = if args.groups.is_empty() { Group::ALL.to_vec() } else { args.groups.clone() };
    let mut lines = vec![Line::Host(Box::new(crate::sysinfo::probe(&repo, profile, serde_json::to_value(&settings)?)))];

    let mut binaries = Vec::new();
    for build in &BUILDS {
        let engines: Vec<&str> = build.engines.iter().copied().filter(|e| wanted(args, e)).collect();
        if engines.is_empty() || (build.apple_only && !cfg!(any(target_os = "macos", target_os = "ios"))) {
            continue;
        }
        let features: Vec<&str> = build
            .features
            .iter()
            .copied()
            .filter(|f| *f != "duckdb" || engines.contains(&"duckdb"))
            .collect();
        match compile(build, &features) {
            Ok(binary) => binaries.push(binary),
            Err(reason) => {
                for engine in engines {
                    lines.push(failure("engine", engine, "build", reason.clone()));
                }
            }
        }
    }

    let settings_json = serde_json::to_string(&settings)?;
    for group in groups {
        let group_name = format!("{group:?}").to_lowercase();
        eprintln!("· group {group_name}");
        for binary in &binaries {
            let listed = Command::new(binary).args(["list", "--group", &group_name]).output()?;
            let engines: Vec<String> = serde_json::from_slice(&listed.stdout)?;
            for engine in engines.iter().filter(|e| wanted(args, e)) {
                let mut cmd = Command::new(binary);
                cmd.args(["run", "--group", &group_name, "--engine", engine, "--settings", &settings_json]);
                collect(cmd, "engine", engine, &group_name, &mut lines);
            }
        }
    }

    if !args.no_sdk {
        sdk(&repo, profile, &mut lines);
    }
    std::fs::remove_dir_all(&work_dir).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) })?;

    let out = args.out.clone().unwrap_or_else(|| repo.join("docs/benchmarks.jsonl"));
    let mut file = std::io::BufWriter::new(std::fs::File::create(&out)?);
    for line in &lines {
        serde_json::to_writer(&mut file, line)?;
        file.write_all(b"\n")?;
    }
    file.flush()?;
    let report = args.report.clone().unwrap_or_else(|| repo.join("docs/benchmarks.md"));
    std::fs::write(&report, crate::report::render(&lines, &relative(&repo, &out)))?;
    eprintln!("· wrote {} and {}", out.display(), report.display());
    Ok(())
}

/// The JavaScript surfaces: Node-API, daemon IPC/HTTP and the SQLite drivers of Node, Bun, Deno.
fn sdk(repo: &Path, profile: &str, lines: &mut Vec<Line>) {
    eprintln!("· group sdk");
    let entry = repo.join("tools/benchmarks/sdk_suite.mjs");
    let built = repo.join("packages/node/dist/index.js");
    if !built.exists() {
        lines.push(failure("sdk", "*", "sdk", "packages/node is not built: run `pnpm --filter @liliadb/node build` first"));
        return;
    }
    let mut cmd = Command::new("node");
    cmd.arg(&entry).args(["--profile", profile]).current_dir(repo);
    collect(cmd, "sdk", "*", "sdk", lines);
}

fn relative(base: &Path, path: &Path) -> String {
    path.strip_prefix(base).unwrap_or(path).display().to_string()
}
