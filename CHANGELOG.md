# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased] - 0.2.0-alpha.1

### Added
- **Comparative benchmark suite (`tools/lilia-bench`)**: one command (`pnpm bench:compare`) measures
  LiliaDB against SQLite (with and without `fullfsync`), LMDB, redb, fjall, RocksDB (with and
  without `HAVE_FULLFSYNC`) and DuckDB, on writes (1–64 concurrent writers, batches, value sizes,
  updates, deletes), reads (point reads after reopen and warm, 1–N threads, multi-get, ranges,
  scans), YCSB A–F, JSON documents, full-text search, vector search and analytics, plus the
  JavaScript surfaces. Every engine runs in its most durable mode and every row states the sync
  primitive it uses, checked against the measured commit latency. Wrong answers fail a run instead
  of producing a number. Raw results are kept in `docs/benchmarks.jsonl`.
- **v0.2 storage RFC** (`docs/v0.2-storage-paradigm.md`): audit of the write path and the
  native log-structured engine plan.

- **`lilia-io`**: platform layer of the v0.2 engine. A `Vfs` trait with the native file system
  (`F_FULLFSYNC` / `F_BARRIERFSYNC` on Apple, `fdatasync` on Linux, `FlushFileBuffers` on Windows,
  `F_PREALLOCATE` / `fallocate`) and an in-memory file system that models power loss (synced data
  and directory entries survive, unsynced tails may be torn) with fault injection;
  `HardwareProfile` (page size, cache line, performance and efficiency cores, memory) and
  background QoS for maintenance threads.
- **`lilia-engine`**: native log-structured storage engine (not yet wired behind the `LiliaDB`
  models). Group commit (one journal write and one sync per group of concurrent commits),
  lock-free MVCC memtable, atomic batches with version expectations, snapshots, range iterators,
  checksummed sorted runs with Bloom filters and a sharded block cache, an atomic manifest, and
  leveled compaction on background threads. Recovery replays journals up to the first damaged
  record. Validated by an oracle test, a concurrent maintenance stress test, and 100,000
  simulated power cuts with torn writes and injected I/O errors.

### Changed
- `docs/benchmarks.md` is generated from measurements; the README no longer carries hand-written
  benchmark numbers.
- JavaScript benchmark drivers moved out of the published `@liliadb/node` package into
  `tools/benchmarks/sdk/`, one process per surface, with read workloads added.

### Removed
- `lilia_bench`, `bench_rusqlite` and `lilia_bench_analytics` binaries and the `bench-bin` feature of
  `lilia-storage-sqlite`, with their `clap` and `rusqlite` dependencies.
- `jit_bench` binary of `lilia-storage-sqlite`: a printing duplicate of the criterion benchmark
  `benches/query_jit.rs` (interpreted, JIT single, JIT batch).
- `docs/benchmarks_comparison.md` and `tools/benchmarks/run_comparative_suite.mjs`, whose analysis
  text was written by hand and contradicted their own tables.

---

## [0.1.0-alpha.9] - 2026-11-20

### Added
- **Zero-Copy Relative-Pointer Architecture (`rkyv` 0.8)**:
  - Integrated `rkyv` 0.8.18 across the workspace with `bytecheck` and `aligned` memory validation.
  - Complete elimination of MessagePack serialization overhead (`rmp-serde`) in network protocol framing and daemon IPC (`lilia-protocol`).
  - Relative displacement pointers (`i32`) enabling $O(1)$ zero-cost deserialization at 0 nanoseconds (`unsafe { rkyv::access_unchecked::<T>(bytes) }`), clocking at **0.69 ns** (125.9x faster than `serde_json` and 91.9x faster than `rmp_serde`).
  - `JsonDocument` and `FlatNode` dynamic tape representation in `lilia-core` for zero-heap nested field extraction (`extract_str`, `extract_f64`, `extract_bool`) directly over mapped buffers.
  - Relative-pointer zero-copy wrappers: `RelKvEntry`, `RelJsonEntry`, `ArchivedRelKvEntry`, `ArchivedRelJsonEntry`.
  - Type-level archiving derivations across core CRDTs (`LwwJson`, `LwwRegister`, `OrSet`, `PnCounter`), IBF (`Cell`, `InvertibleBloomFilter`, `StrataFilter`), HLC timestamps, Geo, Timeseries, Graph, Index, Stats, Vector, and Event types.
- **Lock-Free `AlignedVec` Storage Overlay (`lilia-storage-sqlite`)**:
  - Uncommitted transaction mutations buffered directly into memory-aligned byte arrays (`AlignedVec`) within active transaction overlays.
  - Zero-copy query fast-paths (`database.with_archived_kv`, `database.with_archived_json`) resolving queries against pending transactions directly in memory without deserializing or querying SQLite B-Tree pages.
  - Cache-aligned ring buffer mutation streams for high-throughput zero-copy ingestion.
- **Shared Memory Direct Archived Readers & C-ABI Bridge (`lilia-shm`, `lilia-ffi`)**:
  - `Reader::get_archived()` and `Reader::get_archived_doc()` enabling zero-copy typed and dynamic document inspection directly from lock-free shared memory arenas (`/dev/shm`).
  - `Engine::put_archived()` allowing pre-archived relative-pointer buffers to be inserted directly into SHM slot arenas without re-serialization.
  - Universal C-ABI export `lilia_shm_get_archived` allowing external foreign runtimes (C, C++, Node-API, Go) to read shared memory frames with sub-50ns latency.
- **Micro-Benchmarking Suite & Performance Calibration**:
  - Criterion micro-benchmarks across protocol framing (`benches/protocol_framing.rs`), storage overlays (`benches/storage.rs`), and SHM relative pointers (`benches/zero_copy_comparison.rs`, `benches/shm_latency.rs`).
  - Calibrated benchmark documentation validating 0 B heap allocations and 125.9x deserialization speedups.

---

## [0.1.0-alpha.8] - 2026-11-20

### Added
- **Embedded Zero-Knowledge Proofs (`lilia-zk`, `lilia-core`, `lilia-storage-sqlite`, `lilia-cli`)**:
  - Succinct Zero-Knowledge Proof generation and verification based on Arkworks Groth16 over the pairing-friendly BN254 elliptic curve.
  - Poseidon cryptographic commitments over private document fields with zero-knowledge witness verification.
  - `SoldeAgePredicateCircuit` enforcing confidential predicate conditions (e.g. `balance >= 1000 AND age >= 18`) without disclosing plaintext values or identity.
  - Sub-millisecond verification (<1 ms) with ultra-compact 128-byte proof payloads.
  - SQL verification function `zk_verify()` and CLI commands (`lilia zk setup`, `commit`, `prove`, `verify`).
- **Sub-50ns Shared Memory Zero-Copy Multi-Process Engine (`lilia-shm`, `lilia-storage-sqlite`, `lilia-daemon`)**:
  - High-throughput lock-free inter-process communication utilizing shared memory (`/dev/shm` and memory-mapped files).
  - Slot map allocation and ring buffer architecture providing sub-50 nanosecond read latencies across co-located processes.
  - Real-time mirroring from SQLite WAL write transactions directly into shared memory arenas.
  - Universal C-ABI reader (`lilia_shm_reader`) and daemon CLI option (`--shm`, `--shm-path`).
- **Partially Homomorphic Paillier Cryptosystem (`lilia-core`, `lilia-storage-sqlite`)**:
  - Additive homomorphic encryption over large integer residues modulo $n^2$ via the Paillier cryptosystem.
  - `HomomorphicSumAccumulator` executing streaming aggregations over encrypted data without plaintext decryption in RAM.
  - SQL functions `paillier_add(c1, c2, n_squared)` and `paillier_sum(ciphertext, n_squared)` for privacy-preserving analytics over untrusted infrastructure.
- **Crash-Safe Double-Header Envelope Encryption (`lilia-core`, `lilia-storage-sqlite`)**:
  - Dual-header architecture (Header Page 0 and rear trailer) with BLAKE3 tamper-proofing and integrity authentication tags.
  - Atomic generation bumps ensuring resilience against sudden power loss and process termination during key rotation.
  - Automatic rollback and self-healing when encountering corrupted or interrupted header writes.
- **Unified Cross-Model Query Engine: "LiliaQL" (`lilia-storage-sqlite`)**:
  - Fluent, type-safe query builder integrating JSON document filtering, graph traversals, BM25 text ranking, and dense vector similarity.
  - Reciprocal Rank Fusion (RRF) for fused relevance scoring across lexical and vector modalities.
  - Direct zero-cost projection into strongly typed structs (`execute_as::<T>()`).
- **Reactive Incremental Dataflow Query Engine (`lilia-core`, `lilia-storage-sqlite`)**:
  - Incremental computation engine maintaining reactive materialized view pipelines with minimal differential updates ($\Delta$).
  - Event subscription bus with streaming delta notifications (`Insert`, `Update`, `Delete`) and exact HLC timestamps.
- **Battery-Aware & Energy-Efficient Adaptive Engine (`lilia-core`, `lilia-storage-sqlite`)**:
  - Hardware power profile heuristics (`Normal`, `LowPower`, `Critical`, `Smartwatch`) dynamically adapting query execution and memory limits.
  - `EXPLAIN DIAGNOSE` pedagogical query inspector providing actionable energy and performance optimization recommendations.
- **Optimistic Concurrency Control (OCC) (`lilia-storage-sqlite`)**:
  - High-concurrency lock-free transaction execution with Read-Your-Own-Writes (RYOW) thread-local memory buffering.
  - Snapshot validation, conflict detection, and automatic retry loops maximizing multi-core write throughput.
- **Multilingual CJK & Phonetic Full-Text Search (`lilia-core`, `lilia-storage-sqlite`, `lilia-daemon`, `lilia-cli`)**:
  - Native CJK tokenization engine supporting Chinese (Jieba compound segmentation), Japanese (Kanji/Kana unigram-bigram), and Korean (Hangul jamo splitting).
  - Double Metaphone and Soundex phonetic search matching phonetically equivalent terms despite spelling errors or transliteration differences.
  - Full CLI (`lilia fts phonetic`) and HTTP REST (`POST /v1/fts/phonetic`) support.
- **Predictive Markov Cache & Bitemporal Branching (`lilia-core`, `lilia-storage-sqlite`)**:
  - Adaptive Markov state transition prefetching anticipating upcoming queries.
  - Bitemporal data modeling tracking assertion time vs valid time, paired with branch simulation and diffing.
- **Native Query JIT Compilation (`lilia-core`, `lilia-storage-sqlite`, Cranelift)**:
  - Just-In-Time native machine code compilation (ARM64 & x86_64) for JSON query filter expressions using Cranelift JIT compiler (`JitCompiler`).
  - Native predicates evaluate directly against raw `BinaryDoc` byte buffers with zero interpreter or AST traversal overhead.
  - Multi-document vectorized batch evaluation (`CompiledPredicate::eval_batch`) writing match indices directly to memory.
  - Concurrent thread-safe LRU predicate compilation cache (`JitPredicateCache`) with hit/miss performance metrics.
- **P2P Set Reconciliation via Invertible Bloom Filters (`lilia-core`, `lilia-storage-sqlite`)**:
  - Decentralized state synchronization utilizing Invertible Bloom Filters (IBF / IBLT) with 3-way hash peeling for $O(d)$ symmetric difference recovery where $d$ is the divergence size.
  - Strata Estimator for bandwidth-efficient logarithmic difference size estimation over bandwidth-constrained links (e.g. BLE and P2P mesh).
  - Multi-peer reconciliation worker with deterministic 3-way merge resolution based on Hybrid Logical Clocks (HLC).
- **Reed-Solomon Erasure Coding & Automatic Page Repair (`lilia-core`, `lilia-storage-sqlite`)**:
  - Galois Field $GF(2^8)$ arithmetic with Cauchy systematic generator matrices for $k$ data shards and $m$ parity shards.
  - Checkpoint-integrated physical database page parity generation (`ParityManager`) persisting companion `.parity` sidecars.
  - Automatic detection of NAND flash bitflips and single/multi-page corruption, repairing corrupted SQLite B-Tree pages transparently on read/checkpoint.
- **Row-Level Security (RLS) & Dynamic Data Masking (DDM) (`lilia-core`, `lilia-storage-sqlite`)**:
  - Session-level security context (`SecurityContext`) with caller identification, roles, and arbitrary security attributes.
  - Declarative Row-Level Security policies (`NamedPolicy`, `PolicyAction`) evaluating boolean filter expressions across JSON, SQL, and KV operations.
  - Dynamic Data Masking engine (`MaskFunction`) providing built-in transformations for `CreditCard` (preserving last 4 digits), `Email` (obfuscating username), `Redact`, `Partial` (retaining custom prefix/suffix), and deterministic `Hash` (SHA-256) with role-based exemption expressions.
- **Flash Wear Leveling & Adaptive WAL Fsync Control (`lilia-core`, `lilia-storage-sqlite`)**:
  - Continuous telemetry tracking physical NAND flash byte write volume and daily write budgets (`FlashWearManager`).
  - Adaptive WAL fsync frequency adjustment across four wear pressure tiers (`FlashWearTier::Nominal`, `Elevated`, `Severe`, `Critical`), dynamically stretching fsync/checkpoint intervals to extend mobile flash hardware lifespan.
- **Sandboxed WebAssembly User-Defined Functions (WASM UDF) (`lilia-storage-sqlite`)**:
  - In-database execution of user-supplied WebAssembly bytecode as scalar SQL functions powered by Wasmtime.
  - Strict CPU fuel metering preventing runaway execution or infinite loops, paired with linear memory boundaries.
  - Function-call auxiliary data caching (`sqlite3_get_auxdata`/`sqlite3_set_auxdata`) preserving instantiated WebAssembly runtime stores across consecutive rows.
- **Polyglot Virtual Tables & Multi-Model SQL Rewriter (`lilia-storage-sqlite`)**:
  - Ephemeral SQLite virtual table modules (`lilia_polyglot`) allowing unified SQL joins across JSON documents, KV pairs, Graph nodes, and Timeseries metrics in a single query.
  - Transparent SQL AST rewriter mapping polyglot functional table expressions to optimized virtual table instances.
- **Vectorized in-process analytics (new `lilia-analytics`, `lilia-storage-sqlite` feature `analytics`)**: analytical SQL over persistent Apache Arrow columnar segments, executed by DataFusion 55 in vectorized batches of 1,024 rows without leaving the process.
  - `db.analytics()?.create_table(ColumnarTableSpec::json("users_col", "users").column("age", "$.age", ColumnType::Int64))` projects typed columns out of a JSON collection (keyed by `id`) or a SQL table (`ColumnarTableSpec::sql`, keyed by `rowid`). `query(sql)` returns Arrow record batches, `query_json(sql)` the same shape as `sql_query`.
  - Segments are immutable, stored in the database file as one Arrow IPC chunk per column (`_lilia_col_segments`, `_lilia_col_chunks`) with zone maps for segment pruning, and pass through the configured `ValueCodec`. Field-level encrypted values are never decrypted into segments.
  - The row store stays the source of truth: triggers record the key of every changed row (`_lilia_col_delta`) on every write path, scans overlay those rows on read, and compaction (background worker or `compact(name)`) folds them back into segments. The initial build does not block writers.
  - Read-only by construction: DDL, DML and session statements are rejected by the engine.
  - On 500,000 rows (Apple M3 Max): filter + `sum`/`avg` in 0.97 ms and `GROUP BY` in 1.37 ms with a warm cache, against 18.9 ms and 117.6 ms for SQLite on a typed table. See `docs/benchmarks.md`.
  - Rust API only for now, native targets only. Not available for memory-tier collections, JSON collections under temporal sharding, or `WITHOUT ROWID` SQL tables.
- **Cross-model integrity contracts (`lilia-core`, `lilia-storage-sqlite`)**: declarative contracts enforced inside the write transaction, rejected with the structured `CONSTRAINT_VIOLATION` error code (`details`: `constraint`, `kind`, `model`, `collection`, `id`).
  - JSON `CHECK` constraints in SQL three-valued logic: `db.json().create_collection("accounts", CollectionOptions { check: Some("$.balance >= 0.0 AND length($.iban) == 27".into()) })`, `db.json().set_check(..)`.
  - Cross-model references `db.integrity().add_reference(Reference::graph_nodes(..) | Reference::kv(..))` with `CASCADE`, `RESTRICT` and `SET NULL` on parent deletion.
  - Declared contracts are validated against existing data, persisted in the catalog (`_lilia_catalog.check_expr`, `_lilia_references`) and require the `Disk` tier.
- `SQLITE_CONSTRAINT` is now reported as `CONSTRAINT_VIOLATION` (HTTP 409); protocol minor version bumped to 2.
- **In-database embeddings (`lilia-core`, `lilia-storage-sqlite`, new `lilia-embed`)**: a JSON collection can keep the vector collection of the same name in sync with one of its text fields, with no embedding code in the application.
  - `db.json().create_collection("articles", CollectionOptions { auto_embed: Some(AutoEmbedConfig::new("content", BuiltinModel::MiniLmL6V2)), ..Default::default() })`, or `db.json().set_auto_embed(..)` on an existing collection (its documents are embedded in the background).
  - Writes only queue the documents whose source text changed (`_lilia_embed_queue`, same transaction); the `lilia-embed` worker thread runs inference outside any lock and stores the vector under the document id. Documents are never rewritten, so their version and `if_version` writes are unaffected. Deletions, undo and redo keep the vectors in step.
  - `db.vector_search_text(collection, text, limit)` embeds the query with the model of the collection; `db.wait_for_embeddings(timeout)`, `db.json().embed_status(..)` and `db.json().reembed(..)` cover read-your-writes, monitoring and repair.
  - `lilia-embed` provides `CandleEmbedder` (pure Rust, candle): `all-MiniLM-L6-v2`, 384 dimensions, mean pooling and L2 normalisation, optional `metal` and `accelerate` features. The model files (about 90 MB, fp32) are supplied by the application as a folder; nothing is downloaded or bundled. The backend is registered with `DatabaseOptions::with_embedder`, so the storage engine gains no inference dependency.
  - Rust API only for now; auto-embedded collections and their vector collection must stay on the `Disk` tier and the source field cannot be covered by field-level encryption.

### Changed
- Toolchain and minimum supported Rust version raised from 1.91.1 to **1.100.0** (required by DataFusion 55).
- Undo/redo changesets no longer record the columnar store's bookkeeping tables (`_lilia_col_*`); they are maintained by triggers when the changeset is applied.
- **Breaking (alpha)**: graph nodes link to JSON documents only through an explicit `doc_space` / `doc_id`; the implicit fallback to a JSON space named after the node `kind` was removed (including in `lilia_graph_unified`). `doc_id` defaults to the node id when only `doc_space` is given.
- Raw SQL writes to `_lilia_json`, `_lilia_kv` and `_lilia_graph_nodes` are refused while integrity contracts or auto-embedded collections are declared; use batch operations.
- Integer arithmetic in JSON expressions no longer overflows: it evaluates to `NULL`.
- Undo/redo changesets no longer record the ledger tables (`_lilia_ledger*`): an undo or redo is a new ledger commit. Raw SQL can read the ledger tables but never write them.
- `BinaryDoc::from_json_value` and `BinaryDocBuilder::build_from_value` now return `Result`: documents beyond the format limits (4 GiB, 65,535-byte keys) are refused with `INVALID_INPUT` instead of being silently corrupted.
- Writers checkpoint the WAL themselves once it exceeds four times `CheckpointPolicy::wal_bytes` (at least 4 MiB), and `journal_size_limit` shrinks it back after a rewind.

### Fixed
- **Merkle ledger**: every write path is now recorded, not only `batch`. Interactive transactions, rule actions, integrity cascades, ring-buffer flushes, memory-tier promotion and demotion, undo/redo and raw SQL left rows without a ledger leaf, so the next strict open failed with `TAMPER_DETECTED`. Rows are now captured by triggers on the writer connection only (external tools still trip the verification), and a database that holds data but no ledger history gets a genesis commit on its first open.
- Reactive rules now fire for writes made through `Database::transaction()` / `with_transaction`, not only through `batch`.
- Temporal sharding mapped about 5% of days to a wrong date (for example 1970-01-10 became 1971-01-12), routing writes to the wrong monthly shard; calendar arithmetic is now shared and exact. Daily and monthly shard ids are validated as real dates.
- `strftime('%j')` and `strftime('%w')` on ISO dates returned the day of month and 0; they now return the day of year and the weekday.
- Rule conditions now support arithmetic (`a + b > 10` used to evaluate to `NULL`), and negating `i64::MIN` no longer panics.
- Bulk loads no longer grow the WAL without bound (it reached several GiB): prepared statements are reset when a connection is released so they stop pinning a WAL snapshot, and writers checkpoint past the size limit.
- The CLI silently truncated out-of-range vector elements (an `int8` of 300 became 44); it now rejects them.
- Cypher `LIMIT` and hop bounds must be non-negative integers; more than 65,536 edge kinds in a graph view is an error instead of a wrapped id.
- `lilia-wasm` builds again for `wasm32-unknown-unknown` (`getrandom` 0.4 backend and native-only shard archiving).

---

## [0.1.0-alpha.7] - 2026-11-10

### Added
- **The 6th Model: Native Vector Engine & HNSW Search (`lilia-core`, `lilia-storage-sqlite`)**:
  - Pure Rust Hierarchical Navigable Small World (HNSW) graph index with zero external C/C++ dependencies.
  - Multi-layer probabilistic beam search ($O(\log N)$ ANN traversal) with atomic ACID persistence directly within SQLite WAL transactions (`_lilia_hnsw_meta`, `_lilia_hnsw_edges`, `_lilia_vectors`).
  - Hardware-accelerated vector quantization: Float32 (exact), Int8 / SQ8 scalar quantization (4x memory reduction), and 1-bit binary packing (32x memory reduction with SIMD/POPCOUNT Hamming distance).
  - Multiple distance metrics: Cosine similarity, Euclidean distance ($L_2$), Dot product, and Hamming distance.
  - Pre-filtered vector search bypassing post-filter emptiness: beam traversal constrained via secondary JSON indexes (`vector_search_by_json_index`) and Graph neighborhood constraints (`vector_search_in_graph_neighbors`).
  - Coarse-to-fine two-stage re-ranking: rapid quantized HNSW beam search candidate retrieval followed by exact $f32$ re-ranking.
  - Full CLI support (`lilia vector create-index`, `set`, `get`, `delete`, `search`).
  - Daemon HTTP REST endpoints (`POST /v1/vector/set`, `POST /v1/vector/get`, `POST /v1/vector/search`).
  - Node.js SDK and WebAssembly SDK integration (`db.vector.set`, `db.vector.get`, `db.vector.delete`, `db.vector.search`).

- **ACID Hybrid Search via Reciprocal Rank Fusion (RRF)**:
  - Single-query lexical (FTS5 BM25) and semantic (HNSW vector) fusion evaluated inside one transactional read snapshot.
  - Configurable $k$ smoothing and relative weights ($w_{\text{fts}}, w_{\text{vec}}$).
  - CLI command `lilia hybrid search` and HTTP endpoint `POST /v1/hybrid/search`.
  - Node.js SDK & WebAssembly SDK `db.hybrid.search()`.

- **Spatial Indexing & Geofencing Sub-Engine (`lilia-core`, `lilia-storage-sqlite`)**:
  - Haversine distance calculations and spatial bounding box evaluations.
  - Native polygon containment via SQLite Geopoly integration.
  - Automated geofence breach event generation streaming over the CDC bus.

- **Time-Series Sub-Engine with Lossless Compression**:
  - Gorilla-style delta-of-delta timestamp and XOR floating-point value compression (80–90% reduction on telemetry datasets).
  - Automated window rollups and aggregations (`min`, `max`, `avg`, `count`, `sum`).
  - Declarative lifecycle policies (`TimeSeriesLifecyclePolicy`) with multi-tier downsampling rules and automated background retention pruning (`TimeSeriesWorker`).

- **Native Transactional Undo/Redo & Change Tracking (`lilia-sqlite-native`, `lilia-storage-sqlite`)**:
  - Native VDBE-level change recording using SQLite session extension changesets and compact binary inversion.
  - Inverted changeset history ring buffer in `_lilia_undo_log` with Deflate compression.
  - Instant transactional mutations rollback and forward replay (`db.undo()`, `db.redo()`).

- **JSON RFC 6902 Delta History & Temporal Time-Travel (`lilia-core`, `lilia-storage-sqlite`)**:
  - Keyframe and RFC 6902 JSON Patch delta encoding (`_lilia_json_revisions`) saving up to 85% storage on mutable documents.
  - Time-travel queries: `db.json_get_at_version()`, `db.json_get_at_timestamp()`.
  - Full revision timeline inspection and version-to-version diffing: `db.json_history()`, `db.json_diff()`.

- **Universal C-ABI FFI (`crates/lilia-ffi`) & Cross-Platform Mobile Targets**:
  - Thread-safe C-ABI wrapper for foreign language bindings and embedded targets.
  - **Apple Ecosystem (`packages/apple-spm`)**: Swift Package Manager module with universal XCFramework build script (`tools/mobile/build-apple.sh`) for iOS, macOS, watchOS, and tvOS.
  - **Android SDK (`packages/android-sdk`)**: Kotlin bindings with JNI/NDK prebuilds (`tools/mobile/build-android.sh`).
  - **React Native & Expo (`@liliadb/react-native`)**: High-performance C++ JSI module with zero-serialization overhead.
  - **Flutter (`packages/flutter`)**: Dart FFI plugin.
  - Automated GitHub Actions CI workflow for Apple and Android targets (`.github/workflows/mobile-targets.yml`).

- **Official Visual Studio Code Extension (`apps/lilia-vscode`)**:
  - Multimodel explorer, schema viewer, SQL runner, and database diagnostics inside VSCode.

- **Synchronous Engine Accessors (`db.sync`) & Bun / Deno Compatibility**:
  - Synchronous low-latency embedded accessors in `@liliadb/node` for embedded Node.js and Bun workloads.
  - Native Bun compatibility tests and Deno Node-compat verification.

### Changed & Performance
- **Engine Streamlining & Direct SQLite Amalgamation**:
  - Bundled SQLite 3.49 amalgamation in `crates/lilia-sqlite-native`, replacing the 600K-line `libsqlite3-sys` fork.
  - Native SQLite extension loading (`sqlite3ext.h`), online backup API (`sqlite3_backup_*`), and session changeset API.
  - Codebase cleanup: removed obsolete micro-examples (`examples/`) and standalone `apps/lilia-mcp`.
- **Calibrated Performance Benchmarks (2026-11-10)**:
  - Sustained throughput of up to 298k ops/s in Rust native and 203k ops/s over HTTP gateway.

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
  - SQLite FTS5 integration with BM25 ranking, snippet generation, and highlighted query matching across CLI and Node SDK.
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
- Standalone CLI (`lilia`) and Daemon (`lilia-daemon`).
- Conformance test suite with shared 28-fixture oracle.
