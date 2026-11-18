# LiliaDB

[![CI](https://github.com/shaynlink/lilia/actions/workflows/ci.yml/badge.svg)](https://github.com/shaynlink/lilia/actions/workflows/ci.yml)
[![Compat Runtimes](https://github.com/shaynlink/lilia/actions/workflows/compat-runtimes.yml/badge.svg)](https://github.com/shaynlink/lilia/actions/workflows/compat-runtimes.yml)
[![npm version](https://img.shields.io/npm/v/@liliadb/node.svg)](https://www.npmjs.com/package/@liliadb/node)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

LiliaDB is a local-first multimodel embedded & daemon database written in Rust. It provides
durable Key/Value, JSON documents, Graph, Full-Text Search (FTS5), Declarative SQL, and Native Vector Search (HNSW + Quantization) with Geofencing and Time-Series through the same SQLite ACID
transaction boundary, across embedded Rust, WebAssembly (browser OPFS & Edge), Node.js / Bun / Deno (Node-API), Mobile (iOS, Android, React Native, Flutter), CLI, and Daemon entry points.

---

## Status: v0.2.0

This repository contains the `0.2.0` release featuring:
- **Vectorized Columnar Analytics (`lilia-analytics`, DataFusion 55 & Apache Arrow)**: In-process analytical SQL engine executing on persistent immutable Arrow columnar segments in batches of 1,024 rows. Sub-millisecond aggregations on 500k rows (0.97ms filter/sum/avg) with zone-map pruning and merge-on-read rowstore deltas.
- **Native Query JIT Compilation (`lilia-core`, Cranelift JIT)**: Direct compilation of JSON query expressions into native host machine code (ARM64 / x86_64) for zero-interpreter batch document filtering.
- **Sandboxed WebAssembly User-Defined Functions (WASM UDF via Wasmtime)**: Safe in-database execution of user-supplied WebAssembly bytecode as scalar SQL functions with strict CPU fuel metering and linear memory limits.
- **Reed-Solomon Erasure Coding & Automatic Page Repair**: Transparent NAND flash bitflip detection and automatic recovery of corrupted SQLite B-Tree pages using Cauchy systematic generator matrices in Galois Field $GF(2^8)$.
- **P2P Set Reconciliation via Invertible Bloom Filters (IBF)**: Decentralized peer-to-peer state synchronization with $O(d)$ symmetric difference recovery, Strata Estimators for bandwidth-constrained connections (BLE/mesh), and HLC 3-way conflict resolution.
- **Row-Level Security (RLS) & Dynamic Data Masking (DDM)**: Context-aware security sessions with declarative row-level isolation policies and built-in dynamic data masking (CreditCard, Email, Redact, Partial, Hash) with role exemptions.
- **Flash Wear Leveling & Adaptive WAL Fsync Control**: Physical NAND flash write volume telemetry and daily budget tracking with dynamic multi-tier WAL checkpoint/fsync interval stretching to maximize mobile hardware lifespan.
- **Cross-Model Integrity Contracts**: Declarative JSON `CHECK` constraints in three-valued SQL logic and cross-model references (`CASCADE`, `RESTRICT`, `SET NULL`) enforced inside write transactions.
- **In-Database Vector Embeddings (`lilia-embed`)**: Synchronous and asynchronous embedding generation using Candle (`all-MiniLM-L6-v2`) keeping JSON text fields and vector collections in lockstep.
- **Polyglot Virtual Tables**: Unified SQL query capabilities joining JSON documents, KV namespaces, Graph nodes, and Timeseries metrics in single SQL statements.
- **Native Vector Engine & HNSW Search**: Pure Rust Hierarchical Navigable Small World (HNSW) graph index with zero external C/C++ dependencies. Fully ACID persistent in SQLite (`_lilia_hnsw_meta`, `_lilia_hnsw_edges`, `_lilia_vectors`) and committed atomically within WAL transactions.
- **Hardware-Accelerated Quantization (SQ8 & 1-Bit Binary)**: Scalar Int8 quantization (4x memory reduction) and 1-bit binary packing (32x memory reduction with fast POPCOUNT Hamming distance) with coarse-to-fine candidate re-ranking.
- **Hybrid Search (Reciprocal Rank Fusion - RRF)**: Single-query fusion of lexical FTS5 BM25 ranking and semantic vector similarity with configurable $k$ smoothing and weights.
- **Spatial Indexing & Geofencing**: Microsecond Haversine distance, bounding box filters, Geopoly polygon containment, and automated CDC geofence breach event generation.
- **Time-Series Compression**: Gorilla-style delta-of-delta timestamp and XOR float compression (80–90% reduction), downsampling rollups (`min`, `max`, `avg`, `count`, `sum`), and automated retention pruning.
- **Universal C-ABI FFI (`crates/lilia-ffi`) & Mobile SDKs**: React Native (JSI), Apple SPM (XCFramework), Android SDK (Kotlin/JNI), and Flutter (Dart FFI).
- **Official Visual Studio Code Extension (`apps/lilia-vscode`)**: Interactive multimodel data explorer, schema inspector, and live SQL query runner.
- **WebAssembly & OPFS (Origin Private File System)**: `@liliadb/wasm` bringing 100% multimodel database capabilities directly into web browsers and Edge runtimes.
- **Real-Time CDC & Live Event Streaming**: Sub-millisecond reactive subscriptions via `db.watch()` and Server-Sent Events (`/v1/watch` SSE) over HTTP.
- **Encryption at Rest**: Native authenticated AES-256-GCM AEAD encryption, protecting KV, JSON, vectors, and WAL frames on disk.

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
# Initialize database and insert JSON document
lilia --database ./demo.lilia json put users ada '{"name":"Ada Lovelace","active":true}' --output json

# Query document
lilia --database ./demo.lilia json get users ada --output json

# Query using SQL analytics
lilia --database ./demo.lilia sql "SELECT id, json_extract(value, '$.name') AS name FROM lilia_json WHERE space = 'users'"

# Create atomic online backup snapshot
lilia --database ./demo.lilia backup ./snapshots/demo.lilia --output json
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

// Opens persistent database in the browser's Origin Private File System (OPFS)
const db = await Database.open({
  path: "app.lilia",
  vfs: "opfs", // automatically uses synchronous access handle pool (SAHPool)
});

// JSON documents, KV, Graph, FTS5, and SQL with 100% API parity
await db.json.put("users", "usr_1", { name: "Ada Lovelace", role: "admin" });
const user = await db.json.get("users", "usr_1");

// BM25 full-text search
await db.fts.index("articles", "art_1", "LiliaDB runs in browser OPFS and Edge workers.");
const results = await db.fts.search("articles", "OPFS");

await db.close();
```

---

## Key Features

### 1. Real-Time CDC & Live Watch Subscriptions
Subscribe to live change events in real time with namespace-level filtering.

**TypeScript (Node / Bun / Deno):**
```typescript
const sub = await db.watch("orders", (event) => {
  console.log(`[CDC] ${event.type}: ${event.id || event.key} (deleted: ${event.deleted})`);
});

// Mutate data -> trigger reactive event
await db.json.put("orders", "ord_100", { total: 49.99 });

// Cleanup
sub.unsubscribe();
```

**HTTP Server-Sent Events (SSE):**
```bash
curl -N http://127.0.0.1:4444/v1/watch?namespace=orders \
  -H "Authorization: Bearer <TOKEN>"
```

---

### 2. Embedded Graph & Full-Text Search (FTS5)

**Graph Traversal:**
```typescript
// Directed relationship graph
await db.graph.setNode("social", "user_1", "User", { name: "Alice" });
await db.graph.setNode("social", "user_2", "User", { name: "Bob" });
await db.graph.addEdge("social", "user_1", "user_2", "FOLLOWS");

// Find neighbors and reachability
const friends = await db.graph.neighbors("social", "user_1", { direction: "outgoing" });
const network = await db.graph.reachability("social", ["user_1"], { direction: "outgoing" });
```

**Full-Text Search:**
```typescript
// Index text document
await db.fts.index("articles", "art_1", "LiliaDB delivers unified SQLite multi-model storage.");

// Search with BM25 ranking & snippet extraction
const hits = await db.fts.search("articles", "multi-model storage", { limit: 5 });
for (const hit of hits) {
  console.log(`Hit ${hit.docId} (rank: ${hit.rank}): ${hit.snippet}`);
}
```

---

### 3. Encryption at Rest (AES-256-GCM AEAD)
Start the CLI or Daemon with native authenticated encryption at rest:

```sh
# 256-bit encryption key (hex or passphrase)
export LILIA_CODEC_KEY="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"

# Stored KV and JSON values are encrypted on disk and in WAL frames
lilia --database ./secure.lilia --plugin ./target/release/liblilia_plugin_codec.dylib \
  kv set vault secret_key "confidential_token"

# Decrypt transparently via SQL views
lilia --database ./secure.lilia --plugin ./target/release/liblilia_plugin_codec.dylib \
  sql "SELECT key, lilia_decode_kv(value) FROM lilia_kv"
```

---

### 4. Local Daemon & HTTP REST Gateway
Run the background daemon on Unix with both IPC socket and loopback HTTP gateway:

```sh
lilia-daemon \
  --database ./data.lilia \
  --endpoint ./data.lilia.sock \
  --token-file ./data.lilia.token \
  --http-port 4444
```

See [HTTP REST API Reference](docs/http_api.md) for endpoint details.

---

### 5. WebAssembly Plugin Sandboxing (Wasmtime / WASI)
Safely execute untrusted third-party plugins with out-of-bounds protection, fault isolation, and resource caps:
- **Linear Memory Boundaries**: Guest plugins operate inside isolated 32-bit linear memory—panics or traps never crash the host daemon.
- **Resource Limiter**: Memory bounded via `--plugin-memory-limit-mb` (default 64MB).
- **CPU Fuel Metering**: Execution limits prevent runaway CPU loops via `--plugin-fuel-limit` (default 10,000,000 units).
- **Strict Enforcement Policy**: `--require-sandboxed-plugins` strictly forbids native dynamic libraries and requires WASM modules.

```sh
lilia-daemon \
  --database ./app.lilia \
  --require-sandboxed-plugins \
  --plugin ./dist/my-wasm-plugin
```

See [Plugin Architecture & Sandboxing Guide](docs/plugins.md).

---

### 6. WebAssembly & OPFS Storage for Browsers & Edge (`@liliadb/wasm`)
Run identical LiliaDB storage engines across web browsers and serverless edge functions:
- **Browser Origin Private File System (OPFS)**: High-speed zero-copy persistence using Synchronous Access Handle Pool (`opfs-sahpool`) without locking the browser's UI thread.
- **Serverless Edge Runtimes**: Zero-native-dependency deployment on Cloudflare Workers, Deno Deploy, and Vercel Edge.
- **100% Multimodel API Parity**: Identical `kv`, `json`, `graph`, `fts`, and `sql` surfaces across native Node-API and WebAssembly.

---

### 7. In-Database Embeddings (`lilia-embed`, Rust API)
Declare which text field to embed and the engine keeps the HNSW index up to date; the application never computes a vector:

```rust
let embedder = CandleEmbedder::load(BuiltinModel::MiniLmL6V2, "assets/all-MiniLM-L6-v2", DevicePreference::Auto)?;
let db = Database::open(DatabaseOptions::durable("app.lilia").with_embedder(Arc::new(embedder)))?;

db.json().create_collection("articles", CollectionOptions {
    auto_embed: Some(AutoEmbedConfig::new("content", BuiltinModel::MiniLmL6V2)),
    ..CollectionOptions::default()
})?;

// Writes return immediately; a background thread embeds the documents whose text changed.
let hits = db.vector_search_text("articles", "how do I cook noodles?", 5)?;
```

- **Background and crash-safe**: writes queue the document in the same transaction; inference runs outside any lock and the queue is drained again after a restart.
- **Documents are never rewritten**: vectors live in the vector collection of the same name, under the document id, so versions and optimistic writes are untouched.
- **No network, no bundled blob**: the application ships the `all-MiniLM-L6-v2` files (about 90 MB) and passes the folder. Inference is pure Rust (candle), with optional Metal and Accelerate support on Apple platforms.
- Available from Rust today; the other SDK surfaces do not expose it yet.

---

### 8. Cross-Model Integrity Contracts (Foreign Keys & JSON CHECK, Rust API)
Enforce strong consistency across KV, JSON, and Graph models with declarative contracts evaluated inside the SQLite WAL write transaction:

```rust
// 1. Declarative CHECK constraints on JSON collections (evaluated in SQL 3-valued Kleene logic)
db.json().create_collection("accounts", CollectionOptions {
    check: Some("$.balance >= 0.0 AND length($.iban) == 27".into()),
    ..CollectionOptions::default()
})?;

// 2. Cross-Model Foreign Keys: Graph Nodes -> JSON Documents with CASCADE
db.integrity().add_reference(
    Reference::graph_nodes("social", "users")
        .on_delete(RefAction::Cascade)
)?;

// 3. Cross-Model Foreign Keys: KV Keys -> JSON Documents with RESTRICT
db.integrity().add_reference(
    Reference::kv("sessions", "users")
        .key(KeyMapping::Prefix { separator: ':' })
        .on_delete(RefAction::Restrict)
)?;
```

- **Single Transactional Funnel**: Constraints are verified inside `transaction::apply`. A `json.delete("users", "user_42")` automatically cascades to delete the associated graph node and all its incident edges in one atomic commit.
- **Fail-Fast Structured Errors**: Any mutation violating a CHECK or reference contract is immediately rejected with `CONSTRAINT_VIOLATION` (HTTP 409) containing detailed error context.
- **Anti-Bypass Protection**: Direct SQL writes (`INSERT`, `UPDATE`, `DELETE`) to governed tables (`_lilia_json`, `_lilia_kv`, `_lilia_graph_nodes`) are automatically blocked when integrity contracts are declared.
- **Strict Tier Pinning**: All collections participating in integrity contracts must live on the durable `StorageTier::Disk` tier.

### 9. Vectorized Analytics (Arrow Columnar Engine, Rust API)

SQLite is a row store: aggregating 500,000 rows walks the B-tree one row at a time. With the `analytics` feature, LiliaDB keeps opt-in **columnar projections** of JSON collections and SQL tables as Apache Arrow segments inside the database file and runs analytical SQL on them with DataFusion, in vectorized batches of 1,024 rows:

```rust
use lilia_storage_sqlite::{ColumnType, ColumnarTableSpec, Database, DatabaseOptions};

let db = Database::open(DatabaseOptions::durable("app.lilia"))?;
let analytics = db.analytics()?;
analytics.create_table(
    ColumnarTableSpec::json("users_col", "users")
        .column("age", "$.age", ColumnType::Int64)
        .column("city", "$.city", ColumnType::Utf8),
)?;

// Arrow record batches, or JSON rows with `query_json`.
let output = analytics.query(
    "SELECT city, count(*), avg(age) FROM users_col WHERE age > 30 GROUP BY city",
)?;
```

- **Always current**: the row store remains the source of truth. Triggers track every changed row on every write path; queries overlay them on read and a background worker compacts them into segments.
- **Columnar I/O**: one Arrow IPC chunk per column and per segment, zone maps to skip segments, a byte-bounded cache of decoded chunks. Only the columns a query references are read.
- **Safe by construction**: read-only SQL, segments encoded through the database codec, field-level encrypted values left encrypted.
- **Opt-in**: `lilia-storage-sqlite = { features = ["analytics"] }`, native targets. SIMD follows the build's target features: NEON on aarch64; on x86_64 compile with `-C target-cpu=x86-64-v3` (AVX2) or `x86-64-v4` (AVX-512).

---

## Performance Benchmarks

Calibrated throughput and latency distributions measured on Apple M3 Max (14 cores, macOS 25.6, SQLite WAL `Durability::Durable`, November 2026):

### 1. Multi-Tier Storage Engine & Concurrency Benchmarks
| Storage Tier / Architecture | Threads | Workload | Throughput (ops/sec) | Latency $p_{50}$ | Latency $p_{99}$ | Architectural Highlights |
| :--- | :---: | :---: | :---: | :---: | :---: | :--- |
| **Memory Tier (`StorageTier::Memory`)** | 1 | 10,000 | **1,570,126 ops/s** | **0.50 µs** | 4.63 µs | Sub-microsecond pure RAM engine, zero disk I/O, zero WAL |
| **Memory Tier (`StorageTier::Memory`)** | 8 | 10,000 | **972,136 ops/s** | **3.75 µs** | 44.92 µs | Concurrent multi-threaded lock-free in-memory storage |
| **Buffered RingBuffer (`StorageTier::Buffered`)** | 8 | 10,000 | **386,462 ops/s** | **3.00 µs** | 33.04 µs | Enqueue rate **1,399,474 ops/s**, async micro-batch flush to SQLite |
| **GroupSync Multi-Thread Commit** | 8 | 10,000 | **19,481 ops/s** | **372.83 µs** | 929.17 µs | Shared WAL commit (**10.8x faster** under concurrent write contention) |
| **Buffered IoT Gorilla TS Stream** | 1 | 10,000 | **3,446,790 pts/s** | **0.25 µs** | 0.50 µs | Auto-coalescing single-point pushes into Gorilla float chunks |

### 2. Standard ACID Disk Persistence Across Execution Surfaces (`Durability::Durable`)
| Surface | Workload | Batch Size | Throughput (ops/sec) | Latency $p_{50}$ | Latency $p_{99}$ |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **Node.js (Node-API Sync)** | 10,000 | 1 | **4,347 ops/s** | 193.42 µs | 437.42 µs |
| **Daemon via HTTP Loopback** | 10,000 | 1 | **3,556 ops/s** | 268.60 µs | 456.80 µs |
| **Node.js (Node-API Async)** | 10,000 | 1 | **3,518 ops/s** | 220.83 µs | 496.97 µs |
| **Daemon via IPC (Socket)** | 10,000 | 1 | **3,353 ops/s** | 253.79 µs | 454.42 µs |
| **Rust Native Embedded** | 10,000 | 1 | **1,797 ops/s** | 211.33 µs | 3807.74 µs |
| **Node.js (Node-API Sync)** | 10,000 | 100 | **117,816 ops/s** | 7.67 µs | 10.85 µs |
| **Daemon via IPC (Socket)** | 10,000 | 100 | **104,071 ops/s** | 9.06 µs | 14.26 µs |
| **Node.js (Node-API Async)** | 10,000 | 100 | **98,157 ops/s** | 8.15 µs | 42.75 µs |
| **Daemon via HTTP Loopback** | 10,000 | 100 | **97,907 ops/s** | 9.87 µs | 12.27 µs |
| **Rust Native Embedded** | 10,000 | 100 | **94,867 ops/s** | 8.19 µs | 44.05 µs |
| **Rust Native Bulk Ingest** | 100,000 | 500 | **145,547 ops/s** | 6.69 µs | 8.75 µs |
| **Node.js (Node-API Sync)** | 100,000 | 500 | **131,563 ops/s** | 6.59 µs | 18.98 µs |
| **Node.js (Node-API Async)** | 100,000 | 500 | **127,768 ops/s** | 6.62 µs | 30.87 µs |
| **Daemon via HTTP Bulk Ingest** | 100,000 | 500 | **122,225 ops/s** | 7.57 µs | 22.07 µs |
| **Daemon via IPC Bulk Ingest** | 100,000 | 500 | **117,370 ops/s** | 7.17 µs | 25.98 µs |

### 3. Vectorized Analytics vs Row Store (500,000 rows, `analytics` feature)
| Query | SQLite on JSON documents | SQLite on a typed table | Columnar, warm cache | Columnar, no cache |
| :--- | :---: | :---: | :---: | :---: |
| `count`/`sum`/`avg` with a filter | 5,637 ms | 18.9 ms | **0.97 ms** (513 M rows/s) | 2.60 ms |
| `GROUP BY` (16 groups) with `count`/`avg` | 5,167 ms | 117.6 ms | **1.37 ms** (366 M rows/s) | 3.87 ms |

Read the full report in [Official Performance Benchmarks](docs/benchmarks.md) and the [Comparative Benchmark: LiliaDB vs Native SQLite & Ecosystems](docs/benchmarks_comparison.md).

---

## Workspace Structure

- `apps/`: CLI (`lilia-cli`), local daemon (`lilia-daemon`), and VSCode extension (`apps/lilia-vscode`).
- `crates/`:
  - `lilia-core`: Storage primitives, vector types, HNSW algorithm, quantization, time-series, and geofencing.
  - `lilia-storage-sqlite`: SQLite engine adapter, HNSW vector storage, hybrid RRF search, migrations, and views.
  - `lilia-sqlite-native`: Direct SQLite amalgamation integration, extension loading, backup, and session API.
  - `lilia-embed`: Text embedding backend (candle) for auto-embedded JSON collections.
  - `lilia-analytics`: Vectorized analytics engine (Apache Arrow columnar segments, DataFusion), used by the `analytics` feature of `lilia-storage-sqlite`.
  - `lilia-ffi`: Universal C-ABI FFI wrapper for mobile and foreign language bindings.
  - `lilia-protocol`: Binary MessagePack IPC framing, handshake, and streaming request/response envelopes.
  - `lilia-plugin-api`: Plugin C-ABI, trust store, and ed25519 signature verification.
  - `lilia-graph`: Graph reachability and adjacency engine.
  - `lilia-wasm`: WebAssembly core and OPFS/in-memory VFS bindings.
  - `lilia-node-native`: High-performance Node-API native bindings.
- `plugins/`: Native & WebAssembly plugins (`lilia-plugin-codec`, `lilia-plugin-sql`, `lilia-plugin-kv`, `lilia-plugin-json`).
- `packages/`:
  - `@liliadb/node`: Universal Node.js, Bun, and Deno SDK (`packages/node`) and native prebuilds (`packages/node-*`).
  - `@liliadb/wasm`: WebAssembly and OPFS browser / edge SDK (`packages/wasm`).
  - `@liliadb/react-native`: React Native and Expo module (`packages/react-native`).
  - `packages/apple-spm`: Swift Package Manager module with universal XCFramework for iOS, macOS, watchOS, and tvOS.
  - `packages/android-sdk`: Android Kotlin SDK and JNI/NDK prebuilds.
  - `packages/flutter`: Flutter Dart FFI plugin.
- `tools/`: Cross-platform mobile build scripts (`tools/mobile/`), conformance test suite (`tools/conformance/`), and benchmarks (`tools/benchmarks/`).
- `Formula/`: Homebrew packaging (`Formula/liliadb.rb`, `Formula/lilia.rb`).

Documentation: [Architecture](docs/architecture.md) | [Node.js SDK](docs/node_sdk.md) | [HTTP REST API](docs/http_api.md) | [Benchmarks](docs/benchmarks.md) | [Comparative Benchmarks](docs/benchmarks_comparison.md) | [Plugins](docs/plugins.md) | [Security Policy](SECURITY.md)

---

## License

Apache-2.0.
