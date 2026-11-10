# LiliaDB

[![CI](https://github.com/shaynlink/lilia/actions/workflows/ci.yml/badge.svg)](https://github.com/shaynlink/lilia/actions/workflows/ci.yml)
[![Compat Runtimes](https://github.com/shaynlink/lilia/actions/workflows/compat-runtimes.yml/badge.svg)](https://github.com/shaynlink/lilia/actions/workflows/compat-runtimes.yml)
[![npm version](https://img.shields.io/npm/v/@liliadb/node.svg)](https://www.npmjs.com/package/@liliadb/node)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

LiliaDB is a local-first multimodel embedded & daemon database written in Rust. It provides
durable Key/Value, JSON documents, Graph, Full-Text Search (FTS5), Declarative SQL, and Native Vector Search (HNSW + Quantization) with Geofencing and Time-Series through the same SQLite ACID
transaction boundary, across embedded Rust, WebAssembly (browser OPFS & Edge), Node.js / Bun / Deno (Node-API), Mobile (iOS, Android, React Native, Flutter), CLI, and Daemon entry points.

---

## Status: v0.1.0-alpha.7

This repository contains the `0.1.0-alpha.7` release featuring:
- **Native Vector Engine & HNSW Search**: Pure Rust Hierarchical Navigable Small World (HNSW) graph index with zero external C/C++ dependencies. Fully ACID persistent in SQLite (`_lilia_hnsw_meta`, `_lilia_hnsw_edges`, `_lilia_vectors`) and committed atomically within WAL transactions.
- **Hardware-Accelerated Quantization (SQ8 & 1-Bit Binary)**: Scalar Int8 quantization (4x memory reduction) and 1-bit binary packing (32x memory reduction with fast POPCOUNT Hamming distance) with coarse-to-fine candidate re-ranking.
- **Hybrid Search (Reciprocal Rank Fusion - RRF)**: Single-query fusion of lexical FTS5 BM25 ranking and semantic vector similarity with configurable $k$ smoothing and weights.
- **Pre-Filtered Vector Queries**: Eliminates empty-result post-filtering anomalies by evaluating HNSW beam traversal against secondary JSON indexes and Graph neighborhood constraints.
- **Spatial Indexing & Geofencing**: Microsecond Haversine distance, bounding box filters, Geopoly polygon containment, and automated CDC geofence breach event generation.
- **Time-Series Compression**: Gorilla-style delta-of-delta timestamp and XOR float compression (80–90% reduction), downsampling rollups (`min`, `max`, `avg`, `count`, `sum`), and automated retention pruning.
- **Universal C-ABI FFI (`crates/lilia-ffi`) & Mobile SDKs**:
  - **React Native & Expo (`@liliadb/react-native`)**: Native C++ JSI bindings for zero-bridge execution.
  - **Apple Ecosystem (`packages/apple-spm`)**: Swift Package Manager module with universal XCFramework for iOS, macOS, watchOS, and tvOS.
  - **Android SDK (`packages/android-sdk`)**: Kotlin / JNI bindings with NDK prebuilds.
  - **Flutter (`packages/flutter`)**: Dart FFI plugin.
- **Official Visual Studio Code Extension (`apps/lilia-vscode`)**: Interactive multimodel data explorer, schema inspector, and live SQL query runner.
- **Engine De-bloating & SQLite Native Integration**: Direct integration of SQLite amalgamation in `crates/lilia-sqlite-native` with extension loading, backup API, session tracking, and removal of external 600K-line `libsqlite3-sys` fork.
- **WebAssembly & OPFS (Origin Private File System)**: `@liliadb/wasm` bringing 100% multimodel database capabilities directly into web browsers and Edge runtimes.
- **Real-Time CDC & Live Event Streaming**: Sub-millisecond reactive subscriptions via `db.watch()` and Server-Sent Events (`/v1/watch` SSE) over HTTP.
- **Encryption at Rest**: Native authenticated AES-256-GCM AEAD encryption, protecting KV, JSON, vectors, and WAL frames on disk.
- **Calibrated Performance Benchmarks**: Formal throughput and latency metrics (up to 298k ops/s in Rust native and 203k ops/s via HTTP gateway).

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

## Performance Benchmarks

Calibrated throughput and latency distributions measured on Apple M3 Max (14 cores, macOS 25.6, SQLite WAL `Durability::Durable`, November 2026):

| Surface | Workload | Batch Size | Throughput (ops/sec) | Latency $p_{50}$ | Latency $p_{99}$ |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **Node.js (Node-API Sync)** | 10,000 | 1 | **9,799 ops/s** | 59.58 µs | 1453.57 µs |
| **Daemon via IPC (Socket)** | 10,000 | 1 | **7,519 ops/s** | 101.29 µs | 205.71 µs |
| **Daemon via HTTP Loopback** | 10,000 | 1 | **7,075 ops/s** | 109.92 µs | 241.76 µs |
| **Rust Native Embedded** | 10,000 | 1 | **5,466 ops/s** | 56.27 µs | 3121.45 µs |
| **Node.js (Node-API Sync)** | 10,000 | 100 | **207,250 ops/s** | 4.47 µs | 5.35 µs |
| **Rust Native Embedded** | 10,000 | 100 | **172,503 ops/s** | 4.61 µs | 23.10 µs |
| **Daemon via IPC (Socket)** | 10,000 | 100 | **172,747 ops/s** | 5.34 µs | 7.25 µs |
| **Daemon via HTTP Loopback** | 10,000 | 100 | **149,918 ops/s** | 6.30 µs | 7.86 µs |
| **Rust Native Bulk Ingest** | 100,000 | 500 | **298,308 ops/s** | 3.23 µs | 3.92 µs |
| **Node.js (Node-API Sync)** | 100,000 | 500 | **256,193 ops/s** | 3.53 µs | 5.15 µs |
| **Daemon via HTTP Bulk Ingest** | 100,000 | 500 | **203,038 ops/s** | 4.59 µs | 6.15 µs |
| **Daemon via IPC Bulk Ingest** | 100,000 | 500 | **196,967 ops/s** | 4.48 µs | 9.43 µs |

Read the full report in [Official Performance Benchmarks](docs/benchmarks.md) and the [Comparative Benchmark: LiliaDB vs Native SQLite & Ecosystems](docs/benchmarks_comparison.md).

---

## Workspace Structure

- `apps/`: CLI (`lilia-cli`), local daemon (`lilia-daemon`), and VSCode extension (`apps/lilia-vscode`).
- `crates/`:
  - `lilia-core`: Storage primitives, vector types, HNSW algorithm, quantization, time-series, and geofencing.
  - `lilia-storage-sqlite`: SQLite engine adapter, HNSW vector storage, hybrid RRF search, migrations, and views.
  - `lilia-sqlite-native`: Direct SQLite amalgamation integration, extension loading, backup, and session API.
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
