# LiliaDB Code Examples

This directory provides runnable, end-to-end examples demonstrating how to use the LiliaDB Node.js SDK (`@liliadb/node`).

## Requirements

- Node.js 22 or newer (for Node.js examples)
- Rust 1.80+ / Cargo (for native Rust examples)
- Project dependencies built (`pnpm build` or `cargo build`)

## Node.js Examples (`@liliadb/node`)

### 1. Basic Operations (`node-basic.mjs`)
Demonstrates core functionality in embedded mode:
- Initializing and opening an embedded database file
- Writing and reading Key/Value entries (binary `Uint8Array` support)
- Storing, reading, and updating structured JSON documents
- Multi-model atomic batch transactions combining KV mutations and JSON documents with optimistic concurrency checks (`ifVersion`)
- Database integrity checks and clean lifecycle closing

**Run:**
```sh
node examples/node-basic.mjs
```

---

### 2. Streaming & Large Scans (`node-streaming.mjs`)
Demonstrates high-performance data traversal on large datasets:
- Seeding hundreds of records using batch transactions
- Asynchronous iteration (`for await ... of`) using `db.kv.scanStream()` and `db.json.scanStream()`
- Customizing page chunking with `batchSize`
- Bounding results with a global `limit`
- Resuming scans using keyset pagination with cursor `after`
- Integrating with Node.js standard Streams (`stream.pipeline` and `createReadStream()`)

**Run:**
```sh
node examples/node-streaming.mjs
```

---

### 3. Local Daemon Mode (`node-daemon.mjs`)
Demonstrates client-daemon architecture over high-performance local IPC:
- Spawning a private local daemon
- Connecting to the daemon over Unix domain socket (or Windows named pipe) using bearer tokens
- Executing Key/Value and JSON operations remotely
- Streaming records over IPC

**Run:**
```sh
node examples/node-daemon.mjs
```

---

### 4. Declarative SQL Querying (`node-sql.mjs`)
Demonstrates SQL queries across structured views:
- Querying documents via `lilia_json` view using `json_extract()`
- Running aggregations (`COUNT`, `AVG`, `GROUP BY`)
- Typed record projections with `db.sql.query<T>()`
- Raw matrix projections (`columns`, `rows`) with `db.sql.raw()`
- Executing mutations and updates via `db.sql.execute()`

**Run:**
```sh
node examples/node-sql.mjs
```

---

### 5. Live Events & CDC Streaming (`node-watch.mjs`)
Demonstrates reactive Change Data Capture (CDC) with `db.watch()`:
- Subscribing to live mutations across KV, JSON, Graph, and FTS models
- Unified namespace filtering (`db.watch("orders", callback)`)
- Building reactive in-memory caches and real-time UI synchronization
- Unsubscribing cleanly via `subscription.unsubscribe()`

**Run:**
```sh
node examples/node-watch.mjs
```

---

## Native Rust Examples (`crates/lilia-storage-sqlite/examples`)

Native Rust applications can embed LiliaDB directly with zero FFI overhead.

### 1. Basic Embedded Usage (`basic.rs`)
Demonstrates:
- Opening an embedded SQLite-backed LiliaDB engine (`Database::open`)
- KV operations with TTL and conditional version matching (`if_version: Some(0)`)
- JSON document put/get with structured serde `json!` values
- Multi-model atomic transactions combining KV mutations and JSON documents
- Online snapshot backup (`db.backup(&backup_path)`) and storage integrity checks (`db.check_integrity()`)

**Run:**
```sh
cargo run --example basic -p lilia-storage-sqlite
```

---

### 2. Keyset Streaming & Bounded Pagination (`streaming.rs`)
Demonstrates:
- Efficient keyset pagination using the `after` cursor (`db.json_scan("telemetry", cursor, 50)`)
- Bounded scans without offset drift under concurrent insertions
- Low memory footprint streaming across hundreds of records in chunked batches

**Run:**
```sh
cargo run --example streaming -p lilia-storage-sqlite
```

---

### 3. Custom Encryption Codec & Raw Inspection (`encryption.rs`)
Demonstrates:
- Implementing the pluggable `ValueCodec` trait (`name`, `encode`, `decode`)
- Mounting a codec via `DatabaseOptions::durable(&path).with_codec(codec)`
- Transparent encryption on write and decryption on read for KV values and JSON documents
- Verifying ciphertext at rest by directly querying SQLite raw tables (`lilia_kv`, `lilia_json`)

**Run:**
```sh
cargo run --example encryption -p lilia-storage-sqlite
```

---

### 4. Declarative SQL Querying (`sql.rs`)
Demonstrates:
- Querying documents and KV pairs declaratively using `db.sql_query()`
- Parameterized queries with positional bindings (`?1`, `?2`)
- Aggregations and grouping on JSON fields with `json_extract()`
- Transparent on-the-fly decryption via custom SQLite scalar functions (`lilia_decode_kv`, `lilia_decode_json`)
- Routing read-only queries to the reader pool and write mutations to the exclusive writer queue

**Run:**
```sh
cargo run --example sql -p lilia-storage-sqlite
```

---

## WebAssembly & OPFS Examples (`@liliadb/wasm`)

Zero-native-dependency SDK running in browsers, dedicated Web Workers, and Edge runtimes (Cloudflare Workers, Deno Deploy, Vercel Edge).

### 1. Browser & Origin Private File System (`examples/browser-opfs/`)
Interactive Vite application demonstrating:
- Persistent SQLite storage in the browser using OPFS (Origin Private File System) SAHPool
- Transparent Web Worker bridge ensuring synchronous OPFS handles run without blocking the UI thread
- Real-time full-text search with FTS5 highlighting
- Declarative SQL queries over JSON document collections
- Transparent AES-256-GCM encryption at rest

**Run:**
```sh
cd examples/browser-opfs
pnpm dev
```

### 2. Deno & Deno Deploy (`examples/deno-edge/`)
Deno script demonstrating:
- Direct execution of `@liliadb/wasm` with TypeScript
- Secondary index creation, lookups, and full-text search in Edge runtimes

---

## Plugin & Extension Development (`examples/custom-extension/`)

Learn how to build, package, cryptographically sign, and distribute custom LiliaDB extensions:
- Implementing the C-ABI v1.0 specification and `CodecDescriptorV1` in Rust
- Exposing pluggable storage capabilities with transparent at-rest data transformation
- Dual-target compilation: Native shared libraries (`.dylib` / `.so` / `.dll`) and WebAssembly sandboxed modules (`wasm32-wasip1`)
- Complete CLI distribution lifecycle: `keygen`, `pack`, `sign`, `verify`, `trust`, and `install`
- Transparent runtime usage in the standalone CLI (`--plugin`), background Daemon, and embedded Node.js

**Run:**
```sh
# Run automated lifecycle test
node examples/custom-extension/test-extension.mjs

# Or run the complete shell walkthrough
./examples/custom-extension/run-lifecycle.sh
```

---

## Full-Featured Local Micro-Projects

These runnable end-to-end applications showcase the power of LiliaDB by unifying all storage models (KV, JSON, Graph, FTS5, SQL, CDC, and Atomic Transactions) in concrete local-first scenarios.

### 1. Second Brain & Knowledge Graph Software (`examples/project-knowledge-graph/`)
An interactive, local-first software application (similar to Obsidian, Roam, or Notion):
- **Interactive REPL Terminal Console**: Menu-driven interactive explorer (`npm start` or `node bin/knowledge.mjs`)
- **Scriptable CLI**: Direct commands for automation (`knowledge-graph add`, `view`, `search`, `backlinks`, `reachability`, `stats`)
- **Automatic Wikilink & Tag Parser**: Extracts `[[note_id]]` links and `#tags` to automatically build graph edges and node indexes
- **Full-Text Search (FTS5)**: BM25 lexical search with snippet extraction across note contents
- **Graph Engine**: Bi-directional wikilinks, 1-hop backlinks (`direction: "incoming"`), and multi-hop concept reachability
- **KV with TTL**: Collaborative editor presence and ephemeral lock tokens
- **Atomic Multi-Model Transactions (`db.batch`)**: Updating note documents, FTS index, and graph edges in a single ACID commit
- **Declarative SQL**: Inbound graph degree analytics and author contribution reports

**Run:**
```sh
# Launch interactive REPL console
cd examples/project-knowledge-graph && npm start

# Or run automated walkthrough demo
node examples/project-knowledge-graph/bin/knowledge.mjs --demo

# Or invoke scriptable CLI commands
node examples/project-knowledge-graph/bin/knowledge.mjs search "ACID"
```

### 2. Smart Home Edge Hub & IoT Gateway (Low-Level Native Rust) (`examples/project-iot-smart-hub/`)
A high-performance, 100% offline edge gateway written in **low-level native Rust** for Raspberry Pi, local SBCs, and microcontrollers (Arduino via UART, ESP32 via WiFi/MQTT, GPIO sensors, relays):
- **Zero FFI / In-Process SQLite Engine**: Direct native Rust calls against `lilia-storage-sqlite` with balanced WAL durability
- **Hardware Device Registry (JSON)**: Structured device specifications and secondary indexes
- **Automation Graph**: Trigger pipelines (`Sensor -> TRIGGERS -> Rule -> CONTROLS -> Actuator`) and Zigbee/Matter mesh reachability
- **KV with TTL**: Microcontroller heartbeats (automatic offline detection) and PIR interrupt debouncing
- **Tokio Reactive CDC Worker (`db.subscribe()`)**: Sub-millisecond rule evaluation (< 200 µs) without polling loops
- **FTS5 Search**: Diagnostic hardware incident logging and failure pattern analysis
- **Atomic Batches (`db.batch()`)**: Synchronized relay actuation, event logging, and cycle counting in 1 commit
- **Declarative SQL**: Real-time power consumption (Watts) breakdown by room and protocol distribution

**Run:**
```sh
# Run in dev mode
cargo run -p project-iot-smart-hub

# Or run optimized release binary
cargo run --release -p project-iot-smart-hub
```
