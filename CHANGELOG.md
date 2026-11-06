# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.1.0-alpha.6] - 2026-11-06

### Added
- **Interactive Micro-Projects & Real-World Use Cases**:
  - **Interactive Terminal Knowledge Graph** (`examples/project-knowledge-graph`): Complete CLI application and interactive terminal REPL demonstrating multimodel graph exploration, bidirectional edge tracking, semantic attribute queries, JSON serialization, and ASCII hierarchy visualization.
  - **Native Rust IoT Edge Hub** (`examples/project-iot-smart-hub`): High-throughput edge telemetry ingestion hub showcasing atomic batch writes, MCU heartbeat tracking with TTL expiration, threshold-triggered reactive alerts, and SQLite WAL storage integrity verification (`db.integrity_check()`).

- **Custom Extension Lifecycle & Plugin Tooling**:
  - Comprehensive reference guide and complete extension lifecycle implementation in `examples/custom-extension`.
  - Reference custom XOR/rotation codec plugin demonstrating C-ABI initialization (`lilia_plugin_init_v1`), dynamic memory allocation hooks (`lilia_alloc`, `lilia_dealloc`), and capability negotiation.
  - End-to-end automated lifecycle verification scripts (`run-lifecycle.sh`, `test-extension.mjs`) testing dynamic compilation, packaging, daemon registration, and live SQLite storage encryption/decryption.

- **Synchronous Accessors & Performance Benchmarking**:
  - Added synchronous method suite to `@liliadb/node` (`getSync`, `setSync`, `deleteSync`, `batchSync`, `batchMsgpackSync`) via N-API for latency-sensitive embedded workloads.
  - Dedicated keyset streaming performance benchmarks in `packages/node/src/benchmarks/drivers/node_embedded.ts` and automated runner integration for memory-constant async iterator evaluation.
  - Expanded cross-runtime test verification with a dedicated Bun compatibility test suite (`packages/node/tests/compat-bun.test.ts`).

### Changed & Performance
- **Native SQLite Transaction Engine Acceleration**:
  - Replaced high-overhead abstraction layers with direct SQLite FFI bindings in `crates/lilia-sqlite-native` and `crates/libsqlite3-sys`.
  - Zero-copy borrowing for codec transformation (`Cow<'a, [u8]>`) eliminating superfluous heap allocations during read/write cycles.
  - Accelerated transaction writes, prepared statement slot caching, and reduced per-operation latency overhead (benchmarked and validated in `crates/lilia-storage-sqlite/src/bin/bench_rusqlite.rs`).

---

## [0.1.0-alpha.5] - 2026-11-03

### Added
- **WebAssembly & Origin Private File System (OPFS) Support**:
  - New Rust crate `crates/lilia-wasm` compiling the core storage engine and SQLite to `wasm32-unknown-unknown` with zero native dependencies.
  - New universal npm package `@liliadb/wasm` (`packages/wasm`) providing 100% multimodel API parity with `@liliadb/node` (`kv`, `json`, `graph`, `fts`, `sql`, `batch`).
  - Browser OPFS persistence via Synchronous Access Handle Pool (`opfs-sahpool`) VFS with transparent dedicated Web Worker bridge to prevent blocking the UI thread.
  - Multi-runtime serverless Edge support for Cloudflare Workers, Deno Deploy, and Vercel Edge.
  - Runnable end-to-end examples for Browser OPFS (Vite), Cloudflare Workers (Wrangler), and Deno (`examples/browser-opfs`, `examples/cloudflare-worker`, `examples/deno-edge`).
  - Automated WASM build tooling (`tools/build-wasm.mjs`, `pnpm build:wasm`).

- **WebAssembly Plugin Sandboxing (Wasmtime / WASI)**:
  - Hardened execution runtime in `crates/lilia-plugin-api` powered by `wasmtime` 43.0 and `wasmtime-wasi` (preview 1).
  - 32-bit isolated linear memory preventing out-of-bounds segfaults and host memory corruption.
  - Linear memory quota enforcement via `wasmtime::ResourceLimiter` (`--plugin-memory-limit-mb`, default 64 MB).
  - CPU fuel metering preventing runaway loops and algorithmic DoS (`--plugin-fuel-limit`, default 10,000,000 units).
  - Thread-safe `WasmInstancePool` with fault-isolated instance eviction, ensuring guest traps or panics never crash the daemon or affect sibling requests.
  - CLI packaging support for WASM targets (`wasm32-wasip1`, `wasm32-wasi`, `wasm32-unknown-none`) via `lilia plugin pack --target <TARGET>`.
  - Fail-closed security policy flag on `lilia-daemon` (`--require-sandboxed-plugins`) rejecting native dynamic libraries before `dlopen` is attempted.
  - Adversarial test suite (`crates/lilia-plugin-api/tests/wasm_sandboxing.rs`) verifying segfault resistance, panic isolation, fuel exhaustion traps, and memory quota enforcement.

- **Documentation & Ecosystem**:
  - Overhauled `docs/plugins.md` detailing native C-ABI vs. WebAssembly WASI sandbox comparison, daemon security flags, and guest ABI export specifications.
  - Updated root `README.md` and `examples/README.md` with WebAssembly installation, quick start, and architectural guides.

---

## [0.1.0-alpha.4] - 2026-10-29

### Fixed
- **Homebrew Formula URL Alignment**:
  - Corrected archive download URLs in `Formula/lilia.rb` to point to official repository `https://github.com/shaynlink/lilia`.
  - Harmonized tap instructions to `brew tap liliadb/tap && brew install lilia`.

### Added
- **Exhaustive Documentation**:
  - Completed HTTP REST API specification (`docs/http_api.md`) with full endpoints documentation for Graph (`/v1/graph/*`), FTS5 (`/v1/fts/*`), and real-time Change Data Capture SSE streams (`/v1/watch`).
  - Completed Python SDK reference (`docs/python_sdk.md`) with Graph (`client.graph`), FTS5 (`client.fts`), and dynamic JSON secondary indexing (`client.json.create_index`).
  - Added dedicated comprehensive Node.js SDK documentation (`docs/node_sdk.md`) covering embedded and daemon modes, streaming, OCC, and cross-runtime compatibility.

### Changed
- **Workspace & CI Hygiene**:
  - Disabled telemetry in Nx workspace configuration to eliminate interactive CLI prompts in CI.
  - Purged stale alpha.2 build artifacts and verified 100% test pass rate across Rust, Node, and Python test suites.

---

## [0.1.0-alpha.3] - 2026-10-28

### Added
- **Public Homebrew Tap**:
  - Official public tap repository at `shaynlink/homebrew-tap` supporting `brew tap shaynlink/tap && brew install lilia` (and custom alias `brew tap liliadb/tap`).
  - Automated formula synchronization with SHA256 checksums on release dispatch.
- **Packaging & CI Hardening**:
  - Direct npm publication of multi-platform native tarball prebuilds for macOS (arm64/x64), Linux (arm64/x64), and Windows MSVC.
  - Dual PyPI authentication support (OIDC Trusted Publishing and API token fallback).
  - Native Deno 2 auto node_modules resolution via `deno.json`.
  - Windows chaos recovery permission handling.
- **Daemon HTTP & Storage Enhancements**:
  - Dedicated REST endpoints for batch, transactions, doctor, and live SSE event streams.

## [0.1.0-alpha.2] - 2026-10-27

### Added
- **Global Packaging & Package Managers**:
  - Official Homebrew formula (`Formula/liliadb.rb`, `Formula/lilia.rb`) for macOS (Apple Silicon / Intel) and Linux (x86_64 / arm64) with background daemon management via `brew services start liliadb`.
  - Automated PyPI publishing workflow (`.github/workflows/publish-pypi.yml`) with PyPA Trusted Publishing (OIDC).
  - Multi-runtime server compatibility in `@liliadb/node` with automated CI matrix for Node.js (22, 24, 26), Bun 1.x, and Deno 2.x.
  - Automated checksum calculator tool (`tools/update-homebrew-formula.mjs`) synchronizing release assets with Homebrew formula.
- **Real-Time CDC & Live Watch Subscriptions**:
  - Live reactive change event bus across all storage mutations (KV, JSON, Graph, FTS5).
  - SDK subscription API: `db.watch(namespace, callback)` in `@liliadb/node` and `client.watch(namespace, callback)` in Python SDK.
  - HTTP Server-Sent Events endpoint `GET /v1/watch` on `lilia-daemon` for cross-platform event streaming.
  - Binary protocol streaming request framing (`RequestPayload::SubscribeWatch`, `ResponsePayload::WatchEvent`).
- **Graph Engine & Full-Text Search (FTS5)**:
  - Directed graph representation (`lilia-graph`), adjacency storage, neighborhood traversal (`graphNeighbors`), and transitive reachability analysis (`graphReachability`).
  - SQLite FTS5 integration with BM25 ranking, snippet generation, and highlighted query matching across CLI, Node SDK, and MCP server.
- **Encryption at Rest (AES-256-GCM AEAD)**:
  - Hardware-accelerated native encryption via `plugins/lilia-plugin-codec` protecting KV and JSON values on disk and in WAL frames with zero plaintext leakage.
- **Declarative SQL Engine**:
  - Integrated SQL analytical engine via `plugins/lilia-plugin-sql` exposing views (`lilia_kv`, `lilia_json`, `lilia_graph_nodes`, `lilia_graph_edges`) and transparent decryption functions (`lilia_decode_kv`, `lilia_decode_json`).
- **Keyset Streaming & Async Iteration**:
  - Native async generator `scanStream()` and Node.js `createReadStream()` in `@liliadb/node` for unbounded, memory-constant key scans.
- **Multi-Platform Prebuilds**:
  - Cross-compilation builder script (`tools/build-prebuilds.mjs`) and release packaging for `darwin-arm64`, `darwin-x64`, `linux-arm64-gnu`, `linux-x64-gnu`, and `win32-x64-msvc`.

### Security
- **Windows Access Control Hardening**:
  - Enforced restrictive DACLs (`O:{sid}D:P(...)`) on IPC named pipes, authentication tokens, database files, and backup staging, establishing strict multi-user access denial parity with Unix (`0700`/`0600`).
- **Crash & Chaos Resilience**:
  - Formal tolerance verification against power loss, torn WAL frames, bit-flips in checksums, abrupt `SIGKILL` termination, and `ENOSPC` disk exhaustion with automatic atomic self-healing.
- **Constant-Time Authentication**:
  - Constant-time Bearer token comparison (`subtle::ConstantTimeEq`) on `lilia-daemon` HTTP gateway to prevent timing side-channel attacks.

### Performance
- **Calibrated Benchmarks**:
  - Dedicated benchmark tool `lilia-bench` in `crates/lilia-storage-sqlite` and TypeScript runner in `packages/node/src/benchmarks/`.
  - Comprehensive performance metrics documented in `docs/benchmarks.md` demonstrating up to 281,016 ops/sec in native Rust and 171,051 ops/sec over HTTP loopback.

---

## [0.1.0-alpha.1] - 2026-09-10

### Added
- Initial project release with SQLite WAL multi-model engine (KV, JSON).
- Node-API native bindings in `@liliadb/node`.
- Standalone CLI (`lilia`), Daemon (`lilia-daemon`), and MCP server (`lilia-mcp`).
- Conformance test suite with shared 28-fixture oracle.
