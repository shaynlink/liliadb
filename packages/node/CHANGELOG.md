# @liliadb/node Changelog

All notable changes to the `@liliadb/node` SDK will be documented in this file.

---

## [Unreleased] - v0.2 (lilia-db)

### Changed
- The SDK runs on the native `lilia-db` engine instead of SQLite. `path` is a directory.
  Versions are engine sequences: they grow with every write, not per record from 1.
- One client for both modes: every call is a protocol operation, sent to the native addon
  (`execute`) or to the daemon (after an `LJS1` preface), so daemon mode works again with the
  `rkyv` daemon. Requests are MessagePack (`msgpackr`, replacing `@msgpack/msgpack`); results
  are JS frames: one JSON text written straight from the stored documents and read by V8's
  `JSON.parse`, then raw bytes. A `json.get` takes about half the time it took through
  MessagePack, synchronous calls reuse one output buffer, and asynchronous calls run on the
  libuv thread pool.
- `db.sync` has every call of the asynchronous API, not only KV, JSON, batches and SQL.
- SQL is DataFusion and read-only: parameters are `$1`, `$2`...; JSON is read with
  `value->>'field'` and `json_get_*`. Every JSON space is also a table.
- `json.find` terms are field paths or index names; an index serves the query when one
  covers a term, else the space is scanned.
- Open options: `durability`, `profile`, `ledger`, `historyRetentionMs`, `undoDepth`.
- `json.history` returns `{ version, document? }`; `undo`, `redo` and `undoHistory` return
  `{ version, keys }`; `storageStats` describes the engine's levels and memtables;
  `crdt.set.prune` returns whether anything was pruned; `crdt.json.merge` returns a
  `MutationResult`; `timeseries.applyRetention({ nowMs })` applies every policy.

### Added
- `fts.suggest`, `fts.fuzzySearch`, `fts.phoneticSearch`, `fts.loadDictionary`.

### Removed
- Retired with SQLite: `watch` and CDC events, `liveQuery`, materialized views, HLC writes
  and conflicts, dictionary training, automatic embeddings (`searchText`,
  `waitForEmbeddings`, `setAutoEmbed`), content-addressed blobs, sharding, `backup`,
  `checkpointStats`, `sql.execute`, and the SQLite open options (`busyTimeoutMs`,
  `writerQueueCapacity`, `readPoolSize`, `checkpointPolicy`, `jsonOptions`,
  `timeseriesOptions`, `casOptions`, `sharding`, `nodeId`, `maxDriftMs`, `encryptionKey`).

---

## [0.1.0-alpha.9] - 2026-11-20

### Added
- Native Node-API support for v0.1.0-alpha.9 engine core:
  - Zero-copy relative-pointer deserialization support via `rkyv`.
  - Shared memory direct archived readers for ultra-low latency IPC.
  - Aligned transaction write overlays for accelerated synchronous operations.
  - Updated native bindings and dependencies.

---

## [0.1.0-alpha.8] - 2026-11-20

### Added
- Native Node-API support for v0.1.0-alpha.8 engine core:
  - Cranelift JIT acceleration for query filters and batch operations.
  - Transparent support for Row-Level Security contexts and Dynamic Data Masking policies.
  - Flash wear-leveling endurance monitoring and telemetry hooks.
  - Automatic Reed-Solomon parity repair integration for corrupted pages.
  - Invertible Bloom Filter (IBF) set reconciliation primitives.
  - Multilingual CJK tokenization and phonetic matching in FTS search queries.
  - Zero-copy shared memory integration (`ShmOptions`) for ultra-low latency IPC reads.
  - Synchronous query execution and type-safe query extensions.

---

## [0.1.0-alpha.7] - 2026-11-10

### Added
- **Native Vector Search & HNSW (`db.vector`)**:
  - `db.vector.set()`: Store dense vectors in Float32, Int8, or 1-bit binary formats.
  - `db.vector.get()`: Retrieve vector dimension, element type, and vector data.
  - `db.vector.delete()`: Remove vectors from collections.
  - `db.vector.search()`: Approximate nearest neighbor (ANN) similarity search using Cosine, L2, Dot, or Hamming metrics.
- **ACID Hybrid Search (`db.hybrid`)**:
  - `db.hybrid.search()`: Unified reciprocal rank fusion (RRF) combining FTS5 BM25 text rank and HNSW vector distance with configurable weights and $k$ smoothing.
- **Quantization Utilities**:
  - `quantization.toInt8()`: Scalar quantization from Float32 to Int8 with scaling factor.
  - `quantization.toBit()`: 1-bit binary packing for 32x memory compression.
  - `quantization.cosineDistance()`: High-performance cosine distance calculation.
- **Synchronous Multimodel Accessors (`db.sync`)**:
  - Synchronous KV, JSON, SQL, and Batch operations for ultra-low latency embedded usage in Node.js and Bun.
- **AbortSignal Support**:
  - Native cancellation support via `options.signal` on all asynchronous queries and streams.
- **WHATWG ReadableStream Support**:
  - Export standard web streams for browser and universal streaming environments.

---

## [0.1.0-alpha.6] - 2026-11-06

### Added
- Synchronous embedded accessors (`getSync`, `setSync`, `deleteSync`, `batchSync`, `batchMsgpackSync`) via Node-API for zero-async-overhead embedded workloads.
- Keyset streaming driver benchmark in `packages/node/src/benchmarks/drivers/node_embedded.ts`.
- Dedicated Bun compatibility test suite in `packages/node/tests/compat-bun.test.ts`.

### Changed
- Native bindings optimization via direct SQLite statement evaluation and BigInt 64-bit decoding alignment.

---

## [0.1.0-alpha.5] - 2026-11-03

### Changed
- Workspace version alignment alongside new `@liliadb/wasm` release.
- Added support for WebAssembly sandboxed plugin verification in daemon client sessions.

---

## [0.1.0-alpha.4] - 2026-10-29

### Changed
- Comprehensive documentation synchronization with dedicated `docs/node_sdk.md`.
- Workspace cleanup, telemetry disabling, and Homebrew tap formula path alignment.

---

## [0.1.0-alpha.3] - 2026-10-28

### Added
- **Multi-Platform Prebuilds**:
  - Direct npm publication of multi-platform native tarball prebuilds for macOS (arm64/x64), Linux (arm64/x64), and Windows MSVC.
  - Native Deno 2 auto node_modules resolution via `deno.json`.
- **Packaging Hardening**:
  - Automatic prebuild artifact packaging via `tools/build-prebuilds.mjs`.

---

## [0.1.0-alpha.2] - 2026-10-27

### Added
- **Multi-Runtime Compatibility**:
  - Full native support for **Node.js** (>= 22), **Bun** (>= 1.0), and **Deno** (>= 2.0).
  - Automated cross-runtime test verification in CI (`compat-runtimes.yml`).
- **Real-Time CDC & Live Event Streaming**:
  - New `db.watch(namespace?, callback)` method to subscribe to live mutations across KV, JSON, Graph, and FTS collections.
  - Streaming MessagePack framing over local Unix domain sockets and Windows named pipes.
  - Graceful unsubscription via `subscription.unsubscribe()`.
- **Embedded Graph Operations**:
  - New `db.graph` namespace exposing `setNode`, `addEdge`, `neighbors(graph, node, options)`, and `reachability(graph, startNodes, options)`.
- **Full-Text Search (FTS5)**:
  - New `db.fts` namespace providing BM25-ranked full-text indexing and querying (`db.fts.index`, `db.fts.search`) with highlights and snippet extraction.
- **Keyset Streaming & Async Iteration**:
  - `db.kv.scanStream()` and `db.json.scanStream()` async generator iteration.
  - `db.kv.createReadStream()` and `db.json.createReadStream()` for standard Node.js object-mode streams.
- **Declarative SQL Queries**:
  - `db.sql.query<T>()`, `db.sql.execute()`, and `db.sql.raw()` querying preconfigured views (`lilia_kv`, `lilia_json`, `lilia_graph_nodes`, `lilia_graph_edges`) with transparent decryption.
- **Precompiled Native Addons**:
  - Prebuilt Node-API binaries for `darwin-arm64`, `darwin-x64`, `linux-arm64-gnu`, `linux-x64-gnu`, and `win32-x64-msvc`.

---

## [0.1.0-alpha.1] - 2026-09-10

### Added
- Initial release of `@liliadb/node` featuring embedded SQLite storage and daemon IPC modes.
- Key/Value and JSON document operations with optimistic concurrency control (`ifVersion`).
