# LiliaDB

[![CI](https://github.com/shaynlink/lilia/actions/workflows/ci.yml/badge.svg)](https://github.com/shaynlink/lilia/actions/workflows/ci.yml)
[![Compat Runtimes](https://github.com/shaynlink/lilia/actions/workflows/compat-runtimes.yml/badge.svg)](https://github.com/shaynlink/lilia/actions/workflows/compat-runtimes.yml)
[![npm version](https://img.shields.io/npm/v/@liliadb/node.svg)](https://www.npmjs.com/package/@liliadb/node)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

LiliaDB is a local-first multimodel database written in Rust, on its own storage engine: a
log-structured merge tree with a write-ahead journal, crash-tested by simulated power cuts. One
database holds key/value records, JSON documents with secondary indexes, graphs, full-text
indexes, vectors (HNSW), time series, geofences and GPS tracks, CRDTs and bitemporal entities;
one atomic batch writes any of them, and read-only SQL (DataFusion) reads them all.

It runs embedded in Rust, Node.js, Bun and Deno, iOS, Android, React Native and Flutter, and
in the browser (WebAssembly, on the origin private file system), or as a local daemon with a
CLI and an HTTP gateway. Every binding runs the same operations through one executor.

---

## Status

`v0.2` in progress: the storage engine and the data models (`lilia-engine`, `lilia-db`) replace
the SQLite engine of `v0.1`, which is gone. Record versions are the engine's sequence numbers of
the writes. See the [changelog](CHANGELOG.md).

---

## Installation

### 1. Homebrew (macOS & Linux)
Install the standalone CLI (`lilia`) and Daemon (`lilia-daemon`):

```bash
# Add the official tap and install
brew tap liliadb/tap
brew install lilia

# Or run the daemon as a background service
brew services start liliadb
```

### 2. JavaScript / TypeScript (Node.js, Bun, WebAssembly)

**Native Node-API (Node.js, Bun, Deno):**
```bash
# Node.js (npm / pnpm / yarn)
pnpm add @liliadb/node

# Bun
bun add @liliadb/node

# Deno
deno add npm:@liliadb/node
```

**WebAssembly, Browsers (OPFS) & Edge Runtimes (Cloudflare Workers, Deno Deploy, Vercel Edge):**
```bash
pnpm add @liliadb/wasm
# or
npm install @liliadb/wasm
```

---

## Quick Start

### CLI (Command-Line Interface)
```sh
# Create the database (a directory) and put a JSON document
lilia --database ./demo.lilia json put users ada '{"name":"Ada Lovelace","balance":1500,"age":25}' --output json

# Read it back, index a field, find by it
lilia --database ./demo.lilia json get users ada --output json
lilia --database ./demo.lilia json create-index users by_age '$.age'
lilia --database ./demo.lilia json find users '{"by_age": 25}'

# Read-only SQL: every JSON space is a table
lilia --database ./demo.lilia sql "SELECT id, value->>'name' AS name FROM users WHERE json_get_int(value, 'age') > \$1" --param 18

# Zero-knowledge proof: balance >= 1000 and age >= 18, without revealing either
lilia --database ./demo.lilia zk commit users ada
lilia --database ./demo.lilia zk prove users ada --condition "balance >= 1000 AND age >= 18" --output json > proof.json
lilia zk verify --proof proof.json

# Check the database and show its storage
lilia --database ./demo.lilia doctor --deep
```

### Node.js / Bun / Deno SDK (`@liliadb/node`)
```typescript
import { Database } from "@liliadb/node";

// Embedded in-process database (Node.js / Bun)
const db = await Database.open({
  path: "./app.lilia",
  durability: "durable", // "durable" | "balanced" | "performance"
});

// Key/Value operations
const enc = new TextEncoder();
await db.kv.set("session", enc.encode("tok_abc"), enc.encode("user_42"));
const session = await db.kv.get("session", enc.encode("tok_abc"));

// JSON document operations
await db.json.put("users", "usr_101", { name: "Alice", role: "admin" });
const user = await db.json.get("users", "usr_101");

// SQL analytical queries
const admins = await db.sql.query("SELECT id, value FROM lilia_json WHERE space = 'users'");

await db.close();
```

### WebAssembly & OPFS Browser SDK (`@liliadb/wasm`)
```typescript
import { Database } from "@liliadb/wasm";

// On the page: the database runs in the package's worker, on the origin private file system.
const db = await Database.open({ path: "app" });

await db.json.put("users", "usr_1", { name: "Ada Lovelace", role: "admin" });
const user = await db.json.get("users", "usr_1");

await db.fts.index("articles", "art_1", "LiliaDB runs in browser OPFS and Edge workers.");
const results = await db.fts.search("articles", "OPFS");

await db.close();
```

---

## Data Models

| Model | What it does |
| --- | --- |
| Key/value | Namespaced byte keys and values, optional expiry, ordered scans. |
| JSON documents | Documents in the engine's binary format; secondary indexes over paths or expressions, partial indexes; equality queries served by an index or a scan. |
| Graph | Nodes and weighted edges; neighbors, reachability, shortest paths (BFS, Dijkstra, A*), PageRank, centralities, cycles, components, impact analysis, a Cypher subset. |
| Full-text search | BM25 ranking with snippets, typo-tolerant suggestions and fuzzy search, Double Metaphone and Soundex phonetic search, CJK segmentation. |
| Vectors | `float32`, `int8` and 1-bit vectors; HNSW indexes (cosine, L2, dot, Hamming); searches restricted to a graph neighborhood or to the documents of an index; hybrid search fusing full-text and vector rankings (reciprocal rank fusion). |
| Time series | Gorilla-compressed points, range and statistics queries with automatic resolution, downsampling, retention policies. |
| Geo | Geofences (polygon containment, H3 cells), GPS tracks and their distances. |
| CRDTs | PN-counters, OR-sets and last-writer-wins JSON documents, with state exchange and merge. |
| Bitemporal entities | Values over valid-time intervals, read as of any transaction time. |
| SQL | Read-only DataFusion queries over every model; every JSON space is also a table. |

Every write of every model goes through one atomic batch, optionally conditional on the version
of each record it touches.

## Database Features

- **Durability modes**: `durable` (every acknowledged commit survives power loss), `balanced`
  (the journal stays consistent; the latest commits may be lost), `performance` (no sync on
  commit).
- **History and undo**: past versions stay readable within a retention window (a week by
  default): reads at a version or a time, a record's revisions, JSON diffs; the last commits can
  be undone and redone.
- **Ledger**: an optional tamper-evident record of every write (a Merkle Mountain Range), with
  proofs that a record or a commit is in it.
- **Row-level security and masking**: policies and masking rules applied to every read and write
  of a session, SQL included.
- **Integrity**: checksummed blocks, a deep integrity check, crash recovery validated by
  thousands of simulated power cuts.
- **Zero-knowledge proofs** (`lilia-zk`): Groth16 proofs that a document's `balance` and `age`
  meet thresholds without revealing them, against a Poseidon commitment.
- **Text embeddings** (`lilia-embed`): `all-MiniLM-L6-v2` on candle, for the vectors an
  application stores.

## Bindings

| Surface | Package | How it reaches the engine |
| --- | --- | --- |
| Rust | `lilia-db` | Directly. |
| Node.js, Bun, Deno | `@liliadb/node` | Node-API addon (asynchronous on the libuv pool, or synchronous), or the daemon. |
| Browsers, edge | `@liliadb/wasm` | WebAssembly, in a worker on OPFS, or in memory. |
| React Native | `@liliadb/react-native` | JSI over the C ABI (New Architecture). |
| iOS, macOS | `packages/apple-spm` | UniFFI Swift package. |
| Android | `packages/android-sdk` | UniFFI Kotlin package. |
| Flutter | `packages/flutter` | `dart:ffi` over the C ABI. |
| Shell | `lilia` | CLI, embedded or through the daemon. |
| Other processes | `lilia-daemon` | Local socket or named pipe (`rkyv` or JS frames), loopback HTTP. |

Every binding sends the operations of `lilia-protocol` to one executor; the JavaScript packages
share their client (`@liliadb/client`).

### Local Daemon & HTTP Gateway

```sh
lilia-daemon \
  --database ./data.lilia \
  --endpoint ./data.lilia.sock \
  --token-file ./data.lilia.token \
  --http-port 4444
```

See the [HTTP API reference](docs/http_api.md).

---

## Performance Benchmarks

All published numbers come from one reproducible suite, [`tools/lilia-bench`](tools/lilia-bench/README.md),
which measures LiliaDB against SQLite, LMDB, redb, fjall, RocksDB and DuckDB on the same machine:
durable commits (1 to 64 writers), batched and bulk writes, point, multi-key, range and scan reads,
YCSB A–F, JSON documents, full-text search, vector search, analytics, and the JavaScript surfaces
(Node-API, daemon IPC/HTTP, better-sqlite3, `node:sqlite`, `bun:sqlite`, Deno). Every engine runs
in its most durable mode, and each row states the sync primitive it actually uses.

The report, including where LiliaDB is slower, is [docs/benchmarks.md](docs/benchmarks.md). Reproduce it with:

```bash
pnpm bench:compare
```

---

## Workspace Structure

- `apps/`: CLI (`lilia-cli`), local daemon (`lilia-daemon`), VS Code extension (`lilia-vscode`).
- `crates/`:
  - `lilia-io`: the platform layer: file systems (native, in-memory with crash simulation, the
    browser's OPFS), durability primitives, hardware probe, and the access control of the
    daemon's endpoints (the `ipc` feature).
  - `lilia-engine`: the storage engine (LSM tree, journal, compaction, snapshots).
  - `lilia-db`: the data models, transactions, history, ledger, security, SQL.
  - `lilia-core`: shared types (records, batches, errors), the binary JSON format, expressions,
    CRDTs, HNSW, quantization, time-series compression, geo, phonetics.
  - `lilia-graph`: graph algorithms.
  - `lilia-analytics`: DataFusion over the models (the `sql` feature of `lilia-db`).
  - `lilia-protocol`: the operations, their executor, and the wire formats (rkyv frames, JS
    frames, MessagePack).
  - `lilia-node-native`, `lilia-ffi` (UniFFI and the C ABI), `lilia-wasm`: the native bindings.
  - `lilia-zk`: zero-knowledge proofs. `lilia-embed`: text embeddings.
- `packages/`: `@liliadb/client`, `@liliadb/node` (and its prebuilds `node-*`), `@liliadb/wasm`,
  `@liliadb/react-native`, `apple-spm`, `android-sdk`, `flutter`.
- `tools/`: mobile and WebAssembly build scripts (`mobile/`, `wasm/`), the conformance suite
  (`conformance/`), benchmarks (`lilia-bench/`, `benchmarks/`).
- `Formula/`: Homebrew packaging.

Documentation: [Architecture](docs/architecture.md) | [Storage design](docs/v0.2-storage-paradigm.md) | [Bindings contract](docs/v0.2-bindings-contract.md) | [HTTP API](docs/http_api.md) | [Benchmarks](docs/benchmarks.md) | [Security Policy](SECURITY.md)

---

## License

Apache-2.0.
