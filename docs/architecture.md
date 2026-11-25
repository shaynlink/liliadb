# Architecture

LiliaDB v0.2 is a multimodel database on its own storage engine. The design and the measurements
behind it are in the [storage RFC](v0.2-storage-paradigm.md); what each binding exposes is fixed by
the [bindings contract](v0.2-bindings-contract.md). This document describes the code as it is.

## Layers

```text
 lilia (CLI) · lilia-daemon (IPC, HTTP) · Node · Swift/Kotlin · Flutter · React Native · wasm
                                    │  Operation → ResponseValue
                    ┌───────────────┴───────────────┐
                    │ lilia-protocol                │  operations, codecs, JS frames,
                    │ execute · open · close        │  one meaning per operation
                    └───────────────┬───────────────┘
                                    │  Session, Batch, model handles
                    ┌───────────────┴───────────────┐
                    │ lilia-db                      │  key space, catalog, versions,
                    │ models · guard · ledger · SQL │  security guard, history, undo
                    └───────────────┬───────────────┘
                                    │  WriteBatch, Snapshot
                    ┌───────────────┴───────────────┐
                    │ lilia-engine                  │  group commit, journal, memtables,
                    │                               │  sorted runs, compaction, MVCC
                    └───────────────┬───────────────┘
                                    │  Vfs
                    ┌───────────────┴───────────────┐
                    │ lilia-io                      │  native files, memory, OPFS pool,
                    │                               │  hardware probe, QoS, IPC security
                    └───────────────────────────────┘
```

| Crate | Role |
|---|---|
| `lilia-io` | Every system call of the storage stack: the `Vfs` trait and its file systems, `HardwareProfile`, background QoS, and (feature `ipc`) the daemon's operating-system access control. The only crate of the stack allowed `unsafe`. |
| `lilia-engine` | Ordered key-value store: group commit, journal, memtables, sorted runs, leveled compaction, snapshots, history. Knows nothing of the models. |
| `lilia-db` | The data models over the engine, the catalog (spaces, indexes, policies), sessions and transactions, the security guard, the ledger, history and undo, SQL (feature `sql`). |
| `lilia-protocol` | The operations of the daemon and the bindings, their execution on a `lilia-db` database, the codecs and the frames. |
| `lilia-core` | Shared contracts and algorithms: `LiliaError` and `ErrorCode`, the wire `BatchOperation`, the binary JSON format, security policy evaluation and masking, HNSW and quantization, Gorilla time-series compression, geo and H3, CRDT values, phonetic and fuzzy matching, CJK tokenization, ISO-8601 parsing. |
| `lilia-graph` | Graph algorithms, traversals and a Cypher subset over the graphs of `lilia-db`. |
| `lilia-analytics` | The DataFusion runtime that runs `lilia-db`'s SQL queries. |
| `lilia-zk` | Groth16 proofs over Poseidon commitments to document fields. |
| `lilia-embed` | Sentence-transformer embeddings on candle; the vectors are stored with `lilia-db`'s vector model. |
| `lilia-node-native`, `lilia-ffi`, `lilia-wasm` | The native bindings (Node-API; UniFFI and a C ABI; WebAssembly). |
| `apps/lilia-daemon`, `apps/lilia-cli` | The local daemon and the `lilia` command. |

The rules of RFC §5.2 hold across the workspace and `tools/lint.sh` checks them in CI: no warning,
every `#[allow]` with its reason, no `unwrap`/`expect` or discarded must-use value outside tests,
no source file over 600 lines, and `unsafe_code` lifted only in `lilia-io` and the native bindings.

## Storage engine (`lilia-engine`)

A database is a directory: write-ahead journals (`NNNNNN.log`), sorted runs (`NNNNNN.sst`), the
manifest (`MANIFEST-NNNNNN`, named by `CURRENT`) and a `LOCK` file that keeps a second process out.
LiliaDB is not supported on network file systems.

### Writes

A commit is a `WriteBatch`: atomic, optionally with version expectations (`WriteBatch::expect`).
The version of a key is the sequence number of the write that produced it.

1. The writer joins the commit queue. With no group in progress it becomes the leader at once (a
   lone writer waits for nobody); otherwise it waits as a follower.
2. The leader takes the waiting batches, up to `max_group_bytes`, and checks each batch's
   expectations on its own: a failed expectation rejects that batch, never the group.
3. It assigns contiguous sequence numbers, writes the group to the journal in one positional
   write and syncs once, as the durability mode asks.
4. It inserts the batches into the memtable, publishes the new visible sequence and wakes each
   follower with its own result.

| `Durability` | After a power cut | Sync per commit group |
|---|---|---|
| `Durable` (default) | Every acknowledged commit survives | `SyncLevel::Full` (`F_FULLFSYNC` on Apple platforms, `fdatasync` on Linux, `FlushFileBuffers` on Windows) |
| `Balanced` | Consistent; the latest acknowledged commits may be lost | `SyncLevel::Barrier` (`F_BARRIERFSYNC`; `fdatasync` elsewhere) |
| `Performance` | Consistent after a process crash, not after a power cut | none |

New runs and manifest records are synced before the manifest references them, in every mode.

The journal is a sequence of 32 KiB blocks of checksummed fragments (CRC32C, length, type). A zero
header ends the log, so a preallocated or torn tail stops recovery cleanly and a damaged fragment
is detected by its checksum.

An I/O failure of a journal write, a sync or a background flush or compaction (a full disk, for
instance) disables writes: later commits fail
with the original error until the database is reopened, so a write whose durability is unknown is
never followed by writes that depend on it. Reads continue.

### Memtables, runs and compaction

- **Memtable**: a lock-free ordered structure with a blocked Bloom filter, filled by the commit
  leader. When it reaches `memtable_bytes` it becomes immutable and a new one takes the writes;
  writes stall when `max_immutable_memtables` wait for their flush.
- **Sorted runs**: data blocks of `block_bytes` with prefix-compressed keys (a restart point every
  16 entries), an index block, a Bloom filter (10 bits per key) and a checksummed footer. Runs
  are immutable; reads go through a CLOCK block cache spread over 64 shards.
- **Compaction**: leveled. Four level-0 runs trigger a compaction into level 1; at twelve, writes
  stall until it catches up. Each level is ten times the previous one. Flushes and compactions
  run on background threads at background priority (`lilia_io::qos`).
- **Without background threads** (`Options::background_threads = false`, the default on `wasm32`),
  the writer that fills a memtable flushes it and compacts the levels that are due before its
  write completes.

`Options::for_hardware` sizes everything from the machine: memtables from memory (4 to 64 MiB),
blocks from the page size, the block cache from memory (8 to 512 MiB), and compaction threads from
the efficiency cores, so maintenance stays off the cores the application runs on.

### Reads, snapshots and history

Reads never block writes. A read sees an immutable view of the memtables and runs at one sequence
number; a `Snapshot` pins that sequence for repeatable reads and ranges.

With `Options::retain_history`, compactions keep every version visible at or after a history
horizon that the caller moves forward (`Db::set_history_horizon`). `Db::snapshot_at` reads at a past
sequence and `Db::history` lists the kept versions of a key.

### Recovery

Opening replays the journals written since the last flush, up to the first damaged record, then
deletes files the manifest does not reference. The engine is validated by an oracle test, a
concurrent maintenance stress test and 100,000 simulated power cuts on `MemVfs` (torn writes,
lost unsynced data, injected I/O errors); `lilia-db` adds 22,000 power cuts that check every model,
the ledger, the indexes, the bitemporal audit trail and undo together.

## Platform layer (`lilia-io`)

- **`Vfs`**: open, positional read and write, append, `sync(SyncLevel)`, preallocate, rename,
  remove, list, directory sync. Implementations:
  - `NativeVfs`: the operating system's file system, with `F_PREALLOCATE` / `fallocate`;
  - `MemVfs`: an in-memory file system that models what survives a power cut (synced data and
    directory entries, an optional torn tail of unsynced writes) and injects I/O errors;
  - `PoolVfs`: files over a fixed pool of storage slots, for the browser's origin private file
    system (`OpfsPool`, dedicated workers), whose files open only asynchronously.
- **`HardwareProfile`**: page size, cache line, performance and efficiency cores, memory.
- **`qos`**: lowers the priority of the engine's own threads (`QOS_CLASS_UTILITY` on Apple
  platforms); the caller's threads are never changed.
- **`ipc`** (feature): the macOS ACL check of the daemon's paths, and the Windows security of its
  named pipes and token (below).

## Data models (`lilia-db`)

### Key space

Every engine key starts with a tag byte naming its model, so each model owns one ordered range
(`crates/lilia-db/src/keys.rs` has the full table): the catalog, key-value records, JSON
documents, secondary index entries, the ledger, graph nodes and both edge directions, full-text
documents, postings, statistics and dictionary, vectors, time-series chunks and watermarks,
geofences and their grid, GPS points and their H3 cells, CRDT entries, time marks, bitemporal
slices, and undo entries. Names never contain control characters, so the `\0` after a name keeps
collections apart and in name order.

### Writes, versions and transactions

- **Batches**: a `Batch` of `Write`s over any models commits atomically. Each write may require
  the version of its record (`0`: absent). A batch is planned against the current state and
  committed with those expectations; reads made only to maintain indexes or check policies are
  replanned on conflict instead of failing the caller.
- **Transactions**: optimistic, over a snapshot, with their own writes on top. Every record read
  is recorded with its version and must be unchanged at commit, otherwise the commit fails with a
  retryable conflict and writes nothing. Transactions are serializable for the records they read;
  phantoms inserted into a scanned range are not detected.
- **Wire batches**: the bindings send `lilia_core::BatchOperation`s (JSON or MessagePack);
  `lilia_db::wire` converts them into `Write`s in one place, so a batch means the same thing in
  every binding. An operation without an equivalent is refused with `UNSUPPORTED` and the batch
  fails.

### Sessions and security

Every call goes through a `Session`, which carries its caller's `SecurityContext`; there is no
thread-global context. One guard applies the collection's policies: records a read policy rejects
are skipped, masked fields are rewritten, and each write is checked against the write policies
with the record it replaces and the record it writes. Queries on a masked field see the masked
value. The bindings run on the system session.

### Models

| Model | Storage |
|---|---|
| Key-value | Namespaced byte keys and values, optional expiry, paged scans. |
| JSON documents | Documents by id in spaces, in the binary document format. Secondary indexes over paths or expressions, partial indexes, online builds resumed after a crash; `find` picks a complete index for one of its terms and rechecks each candidate. |
| Graph | Nodes, outbound edges and their inbound mirror; deleting a node deletes its edges. `lilia-graph` adds algorithms, traversals and Cypher. |
| Full-text search | Postings per term, statistics per shard, a dictionary; BM25, phrases, fuzzy and phonetic search, suggestions, CJK tokenization. |
| Vectors | `f32`, `i8` or bit vectors with exact search, or an HNSW index kept in memory: built from a snapshot on first search, then kept exact by every commit. |
| Time series | Gorilla-compressed chunks of up to 1,024 points keyed by their upper time bound; range statistics; lifecycle policies (tiered downsampling then expiry, resumable). |
| Geo | Geofences in a multi-level grid, then an exact polygon test; GPS tracks with exact H3 proximity. |
| CRDTs | PN-counters, add-wins OR-sets and LWW JSON documents, merged with a replica's state; one record per actor, element or field, so concurrent writers rarely meet. |
| Bitemporal | Values over valid-time intervals recorded at transaction times; current and superseded slices in separate ranges, a permanent audit trail. |

Composed searches (`compose`) restrict a vector search to a graph neighborhood or to the documents
an index selects, and fuse full-text and vector rankings (reciprocal rank fusion). Each part reads
through the caller's policies.

### SQL

With the `sql` feature, `Session::sql` runs read-only DataFusion queries (PostgreSQL-like dialect,
`$1` parameters) over tables built from one state of the database through the caller's session:
`lilia_kv`, `lilia_json`, one table per JSON space, `lilia_graph_nodes`, `lilia_graph_edges`,
`lilia_timeseries`. Policies and masking apply to every row, and a session in the past queries the
past. Statements that write are refused: writes go through the model APIs. The feature also builds
for the browser.

### History and undo

`Options::history_retention` (a week by default, zero keeps none) keeps old versions in the engine.
A time mark per millisecond of commit maps times to versions. `Session::at_version` and `at_time`
read every model as it was; `json_history`, `kv_history` and `json_diff` list and compare the
versions of a record. With `Options::undo_depth`, each commit also records its keys; `undo` writes
them back to their state before the commit, read from the history, and `redo` to their state
after. The space history takes grows with how often records are rewritten within the window (RFC
§7.3).

### Ledger

With `Options::ledger`, every batch that writes records is also a ledger commit (deltas, Merkle
root, chained hash) appended to a Merkle Mountain Range stored in the same engine batch as the
data: appends and proofs are `O(log n)`. A database that keeps a ledger must always be opened with
it. `verify_ledger` checks it fast (the commit chain and the latest root) or strictly (also every
live record against its commitment), as the database does when it opens (`verify_ledger` option);
`prove_*` give record and commit proofs.

### Health

`storage_stats` describes the levels, the memtables and the sequence. `integrity_check` reads every
stored block back through its checksum and verifies the ledger strictly; `quick_check` verifies
the ledger only. `flush` writes the memtable to a run; `close` makes every acknowledged write
durable (it syncs, which matters in the `Balanced` and `Performance` modes).

## Protocol and bindings

`lilia_protocol::execute` runs an `Operation` on a database through its system session and answers
with a `ResponseValue`. The daemon, its HTTP gateway, Node, the FFI, WebAssembly and the CLI all
call it, so an operation has one meaning everywhere; the shared conformance suite
(`tools/conformance`) runs one fixture corpus through each surface. `lilia_protocol::open` opens a
database from `OpenOptions` (durability, hardware profile, ledger, history retention, undo depth),
and `close` closes it once the calls still holding it have returned, waiting at most a timeout;
later calls fail with `CLOSED`.

### Codecs and frames

- **`LRK1`**: size-prefixed `rkyv` archives both ways, for Rust clients.
- **`LJS1`**: MessagePack requests and JS-frame responses, for JavaScript clients. A JS frame is
  one JSON text, written straight from the stored binary documents and read by the runtime's
  native `JSON.parse`, followed by the raw bytes (keys, values, vectors) it refers to by segment
  number. On a 400-byte document, V8 parses the JSON twice as fast as a JavaScript MessagePack
  decoder, and writing it from the binary view is 2.7 times faster than through a value tree.

Frames are limited to 16 MiB. The protocol version is `2.0`; a handshake with another major
version is refused.

### Exact versions

Versions and expectations are `u64`. JS frames carry versions as decimal strings, which
`@liliadb/client` reads as `bigint`, so a version never goes through a JavaScript `number`;
`ifVersion` must be a `bigint` in `0..=18446744073709551615`, and other values are rejected rather
than coerced. Numbers inside JSON documents remain IEEE-754 numbers. Expiry inputs must be safe
integer milliseconds.

### Bindings

| Surface | Path |
|---|---|
| Node (`@liliadb/node`) | `lilia-node-native`: `execute` on the libuv pool, `executeSync` into a reused buffer; or the daemon over `LJS1`. One client for both, from `@liliadb/client`, with keyset-paginated streams (`scanStream`, `createReadStream`, `batchSize`, `limit`) whose page fetches `close({ timeoutMs })` drains. |
| Swift, Kotlin (`packages/apple-spm`, `packages/android-sdk`) | `lilia-ffi`'s typed UniFFI API (`LiliaDatabase`, synchronous and asynchronous methods), generated with `uniffi-bindgen`. |
| Flutter, React Native | `lilia-ffi`'s C ABI: `lilia_c_open` (options in JSON), `lilia_c_execute` (MessagePack or JSON in, JS frame out), `lilia_c_close`. React Native installs a JSI binding from a TurboModule: asynchronous calls on native worker threads, the frame's bytes as an `ArrayBuffer` over the engine's memory. |
| Browser (`@liliadb/wasm`) | `lilia-wasm` on an `OpfsPool` (dedicated workers) or in memory, without background threads, in the calling thread or in the package's worker. Built by `tools/wasm/build.sh`. |
| CLI (`lilia`) | Each command the protocol knows is one operation; bitemporal reads, the ledger and zero-knowledge proofs call `lilia-db` and `lilia-zk`. Output is human, JSON or JSONL; execution errors print the structured error on stderr with exit code 1. |

## Daemon

`lilia-daemon` serves one database to the processes of its user over a Unix socket or a Windows
named pipe, and optionally over an HTTP gateway (`--http-port` binds loopback; `--http-bind` takes
another address only with `--development`; see [the HTTP API](http_api.md)).

- **Authentication**: a random token, published with user-only permissions, authenticates every
  request before dispatch, shutdown included; HTTP takes it as a `Bearer` token. Tokens are
  compared in constant time.
- **Limits**: 64 concurrent connections, a client silent for 30 seconds is dropped, frames up to
  16 MiB.
- **Deadlines**: `deadline_ms` is an optional admission budget that starts once the frame is
  decoded. It is checked before and after waiting for a blocking worker; an expired request returns
  a retryable `TIMEOUT` without running. Once an operation starts, its actual outcome is returned,
  so a timeout never hides a commit.
- **Shutdown**: an authenticated shutdown stops accepting connections and closes the database,
  waiting at most five seconds for running requests. The response acknowledges the request, not
  the completed close.

### Unix IPC filesystem policy

The daemon requires an owned, user-only parent directory for each socket and token. Missing
directories are created with mode `0700`; existing directory permissions are never changed.
Ancestors must be owned by root or the current user and not writable by others, except root-owned
sticky temporary directories. Paths with parent traversal or symlinks are rejected, directory
symlinks included: resolve aliases such as macOS `/var` or `/tmp` with `realpath` first. Relative
paths without traversal are supported. On macOS, extended ACL grants on ancestors, IPC directories,
existing tokens or staging files are refused even when the mode bits are private
(`lilia_io::ipc::reject_acl_grants`); deny-only ACLs, such as the usual home-directory delete
restriction, are accepted.

The socket is claimed before the token rotates, so a second daemon never invalidates the token of
a running one. Every existing endpoint is refused, even a stale socket: remove stale endpoints by
hand once no daemon uses them. Cleanup removes only the socket inode this instance created.
Accepted connections must report the current effective UID through the OS peer credentials; a
failed lookup is refused.

Tokens are written to a mode-`0600` staging file, synced, then atomically published. Rotation
accepts only an owned private regular file with one hard link; symlinks, hard-linked files, public
files and other entry types are rejected unmodified. These checks isolate unprivileged users from
each other, not from root or from hostile processes of the same user.

### Windows IPC security

`lilia_io::ipc::UserSecurity` creates the named pipes with a protected DACL granting access only to
the current user's SID. The first instance claims the name before the token rotates; replacement
listeners are created while the connected instance stays alive. Remote pipe names and remote
clients are rejected.

Token paths must be on a local drive, without traversal, alternate data streams or reparse points.
The final parent must have a protected current-user-only DACL; missing directories receive it at
creation, and existing files with broader or inherited ACLs are refused, not repaired. Directory
handles deny delete sharing and stay open for the daemon's lifetime, so an ancestor cannot be
replaced. Ancestors must be owned by the current user, SYSTEM, Administrators or the
TrustedInstaller service SID and must not let other principals modify data, attributes, ACLs,
ownership or children; unknown ACE types are refused. Tokens are staged with the private DACL,
synced, renamed over the validated token, and the parent directory is flushed
(`NativeVfs::sync_dir`). Hard links and non-regular entries are refused.

These guarantees exclude administrators with privilege overrides and hostile same-user processes.
`tools/test-windows-accounts.ps1` checks that a second account can open neither the pipe nor the
token; it runs on Windows CI and cannot be inferred from a cross-compilation.

## Not in v0.2 yet

Features of v0.1 without a `lilia-db` equivalent were removed rather than left half-wired:
content-addressed blobs, sharding, live queries and change streams, materialized views, hybrid
logical clocks, P2P sync, dictionary compression, WebAssembly UDFs, automatic embeddings,
encryption at rest, backups and vacuum, the shared-memory reader and plugins. The [bindings
contract](v0.2-bindings-contract.md) §3 lists each one and what brings it back. A v0.1 database
does not open in v0.2; `lilia import` (alpha.6) will read it.
