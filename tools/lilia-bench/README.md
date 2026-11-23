# lilia-bench

The comparative benchmark suite of LiliaDB. It measures LiliaDB against embedded storage engines
on writes and reads, under the same data, the same machine and each engine's most durable
configuration, and writes the published report [`docs/benchmarks.md`](../../docs/benchmarks.md)
from the raw results in [`docs/benchmarks.jsonl`](../../docs/benchmarks.jsonl).

## Run

From the repository root:

```bash
pnpm bench:compare
```

That builds the Node addon, `@liliadb/node` and `lilia-daemon`, then runs
`cargo run --release --manifest-path tools/lilia-bench/Cargo.toml -- suite`. The `full` profile
takes about an hour on an M3 Max, mostly in time-boxed workloads. `pnpm bench:compare:quick`
runs the small `quick` profile into `target/` without touching the published report.

Useful options of `suite`:

```bash
cargo run --release --manifest-path tools/lilia-bench/Cargo.toml -- suite \
  --profile quick --engines lilia,sqlite-fullfsync,lmdb --groups write,read --no-sdk
```

Re-render the report from saved results (for example after changing the report code):

```bash
cargo run --release --manifest-path tools/lilia-bench/Cargo.toml -- render
```

Prerequisites: Rust (the workspace toolchain), a C/C++ toolchain, and `libclang` for RocksDB's
bindings (found automatically under the Xcode or Command Line Tools directory on macOS). The
JavaScript surfaces need Node 22+, and optionally Bun and Deno; a missing runtime is reported as a
run without results, never silently skipped.

## Engines

| Id | Engine | Commit sync |
| :--- | :--- | :--- |
| `lilia` | LiliaDB: the data models (`crates/lilia-db`) over `lilia-engine`, defaults (a week of history, no ledger), `Durability::Durable`; the key-value workloads use its key-value model, the model workloads run the protocol's operations | `F_FULLFSYNC` on Apple |
| `lilia-ledger` | same, ledger on | same |
| `lilia-nohistory` | same, no history window | same |
| `lilia-engine` | the storage engine alone (`crates/lilia-engine`), `Durability::Durable` | `F_FULLFSYNC` on Apple (`File::sync_data`) |
| `lilia-engine-balanced` | same, `Durability::Balanced` | `F_BARRIERFSYNC` on Apple |
| `sqlite` | SQLite WAL, `synchronous=FULL` (via `rusqlite`) | `fsync` |
| `sqlite-fullfsync` | same, plus `fullfsync` and `checkpoint_fullfsync` | `F_FULLFSYNC` on Apple |
| `redb` | redb, `Durability::Immediate` | `File::sync_data` |
| `fjall` | fjall, `PersistMode::SyncAll` | `File::sync_all` |
| `lmdb` | LMDB through `heed`, default flags | `F_FULLFSYNC` on Apple |
| `rocksdb` | RocksDB, `WriteOptions::sync` | `fsync` (the crate omits `HAVE_FULLFSYNC`) |
| `rocksdb-fullfsync` | RocksDB built with `-DHAVE_FULLFSYNC` (Apple only) | `F_FULLFSYNC` |
| `duckdb` | DuckDB (analytics only) | — |

Rust's `File::sync_all`/`sync_data` issue `F_FULLFSYNC` on Apple platforms. On Linux every engine's
`fsync`/`fdatasync` is power-safe, so they all share one class.

## Workload groups

| Group | Measures |
| :--- | :--- |
| `write` | single-key commits with 1–64 writers, batches of 100/1,000/10,000 keys, 1/4/64 KiB values, updates, deletes |
| `read` | bulk load (with disk size and write amplification), point reads after reopen and warm (1–N threads), multi-get, ranges of 100, full scans |
| `ycsb` | YCSB A–F, Zipfian θ = 0.99, 1 and 8 client threads |
| `json` | indexed document inserts (1/4/64 KiB), get, field extraction, indexed find |
| `fts` | full-text indexing and top-10 BM25 queries |
| `vector` | HNSW inserts, top-10 cosine search with recall@10 against an exact scan |
| `analytics` | filtered aggregate and group-by, answers checked against the generator |
| `sdk` | the JavaScript surfaces (`tools/benchmarks/sdk_suite.mjs`) |

## Layout

- `src/engines/`: one adapter per engine behind `Engine`/`Session`.
- `src/workloads/`: one module per group; every group runs in its own process.
- `src/suite.rs`: builds the engine binaries (one per group of engines, so that no engine's native
  libraries are linked into another's measurements), runs the plan, writes the results.
- `src/report/`: renders the Markdown report from the result lines; no text depends on a
  particular outcome.
