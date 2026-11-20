//! Comparative benchmark suite: LiliaDB against embedded storage engines, on writes, reads,
//! YCSB mixes and model workloads (JSON, full-text, vectors, analytics).
//!
//! `lilia-bench suite` is the single entry point; `run` and `list` are what it calls in the
//! per-engine binaries it builds; `render` rebuilds the report from saved raw results.

use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod engines;
mod keys;
mod os;
mod record;
mod report;
mod suite;
mod sysinfo;
mod workloads;

use record::{Failure, Line};
use workloads::{Group, Settings};

#[derive(Debug, Parser)]
#[command(name = "lilia-bench", about = "LiliaDB comparative benchmark suite")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Build every engine binary, run all workloads, write the raw results and the report.
    Suite(suite::Args),
    /// Run one workload group against one engine compiled into this binary (JSON lines on stdout).
    Run {
        #[arg(long, value_enum)]
        group: Group,
        #[arg(long)]
        engine: String,
        /// `Settings` as JSON, as passed by `suite`.
        #[arg(long)]
        settings: String,
    },
    /// Engines compiled into this binary for a group (JSON array on stdout).
    List {
        #[arg(long, value_enum)]
        group: Group,
    },
    /// Render the report from raw results.
    Render {
        #[arg(long)]
        input: Option<PathBuf>,
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

fn main() -> std::process::ExitCode {
    match execute(Cli::parse().command) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lilia-bench: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Writes one result line to standard output and flushes it, so the suite sees it immediately.
fn print_line(line: &Line) -> engines::Result<()> {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer(&mut out, line)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

fn execute(command: Cmd) -> engines::Result<()> {
    match command {
        Cmd::Suite(args) => suite::run(&args),
        Cmd::List { group } => {
            println!("{}", serde_json::to_string(&group.engines())?);
            Ok(())
        }
        Cmd::Run { group, engine, settings } => {
            let settings: Settings = serde_json::from_str(&settings)?;
            let mut emit = |line: Line| {
                // A closed pipe means the suite is gone; there is nobody left to report to.
                if print_line(&line).is_err() {
                    std::process::exit(2);
                }
            };
            let group_name = format!("{group:?}").to_lowercase();
            let result = workloads::run(group, &engine, &settings, &mut emit);
            if let Err(error) = &result {
                emit(Line::Failure(Failure { suite: "engine".into(), engine: engine.clone(), workload: group_name, reason: error.to_string() }));
            }
            result
        }
        Cmd::Render { input, output } => {
            let repo = suite::repo_dir();
            let input = input.unwrap_or_else(|| repo.join("docs/benchmarks.jsonl"));
            let output = output.unwrap_or_else(|| repo.join("docs/benchmarks.md"));
            let lines: Vec<Line> = std::fs::read_to_string(&input)?
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(serde_json::from_str)
                .collect::<Result<_, _>>()?;
            let source = input.strip_prefix(&repo).unwrap_or(&input).display().to_string();
            std::fs::write(&output, report::render(&lines, &source))?;
            Ok(())
        }
    }
}
