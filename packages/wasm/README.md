# `@liliadb/wasm`

> WebAssembly & OPFS (Origin Private File System) distribution for **LiliaDB**, the local-first multimodel embedded database.

Run LiliaDB everywhere JavaScript runs:

- **Browsers**: client-side SQLite persisted in the Origin Private File System (OPFS), proxied through a dedicated Web Worker so the UI thread never blocks.
- **Edge runtimes**: Cloudflare Workers, Deno Deploy, and Vercel Edge with zero native binary dependencies.
- **Node.js / Bun / Deno**: pure WebAssembly with no native toolchain or prebuilt addon.

The SDK mirrors the `@liliadb/node` multimodel API as closely as the runtime allows: Key/Value, JSON documents, Graph, Full-Text Search, Vector, Hybrid search, and declarative SQL over a single transactional SQLite database.

---

## Features

- 🌸 **Multimodel API**: `db.kv`, `db.json`, `db.graph`, `db.fts`, `db.vector`, `db.hybrid`, `db.sql`, and `db.batch`.
- 📁 **OPFS SAHPool persistence**: Synchronous Access Handle Pool VFS for durable database files with no UI-thread blocking.
- 🧵 **Transparent Web Worker bridge**: On a browser main thread with OPFS, `Database.open()` automatically spawns a worker and proxies every operation through it.
- ⚡ **Full-text search (FTS5)**: BM25 ranking with snippet generation.
- 🧠 **Vector search**: Float32, Int8, and 1-bit vectors with cosine, L2, dot-product, and Hamming metrics, plus Int8/bit quantization helpers.
- 🔀 **Hybrid search**: Reciprocal Rank Fusion (RRF) over FTS + vector results.
- 🗂️ **Secondary JSON indexes**: Create, list, drop, and query dynamic indexes, including multikey array indexes.
- 🔒 **AES-256-GCM encryption**: Transparent page and payload encryption from a single passphrase.
- 🧮 **Atomic multi-model batches**: KV, JSON, Graph, FTS, and Vector writes commit or roll back together.
- 🎯 **Exact 64-bit versions**: `bigint` versions for optimistic concurrency control (`ifVersion`) and millisecond TTLs.
- 📦 **Zero native dependencies**: Ships pure `.wasm` plus ESM TypeScript bindings.

---

## Installation

```bash
pnpm add @liliadb/wasm
# or
npm install @liliadb/wasm
# or
bun add @liliadb/wasm
# or
deno add npm:@liliadb/wasm
```

---

## Quickstart

### 1. Browser with persistent OPFS

```typescript
import { Database } from "@liliadb/wasm";

// Opens or creates a persistent SQLite database in the browser's OPFS.
// On the main thread this transparently uses a Web Worker.
const db = await Database.open({ path: "app.lilia" });

// JSON document store
await db.json.put("users", "u-1", { name: "Alice", role: "admin" });
const user = await db.json.get("users", "u-1");
console.log(user?.value, user?.version);

// Full-text search
await db.fts.index("notes", "n-1", "LiliaDB runs high performance WebAssembly");
const hits = await db.fts.search("notes", "WebAssembly");
console.log(hits[0]?.snippet);

// Declarative SQL
const rows = await db.sql.query(
  "SELECT id, json_extract(value, '$.name') AS name FROM lilia_json WHERE space = ?",
  ["users"],
);
console.log(rows);

await db.close();
```

### 2. Edge runtimes and in-memory databases

```typescript
import { Database } from "@liliadb/wasm";

const db = await Database.open({ path: ":memory:", vfs: "memory" });

const key = new Uint8Array([1, 2]);
await db.kv.set("cache", key, new Uint8Array([99, 100]));
const cached = await db.kv.get("cache", key);
console.log(cached?.value);
```

### 3. Transparent AES-256-GCM encryption

```typescript
const db = await Database.open({
  path: "secure.lilia",
  encryptionKey: "my-secure-master-passphrase",
});
```

---

## Opening a Database

```typescript
import { Database } from "@liliadb/wasm";

const db = await Database.open(options);
```

| Option | Type | Description |
| --- | --- | --- |
| `path` | `string` | **Required.** Database file name or `:memory:`. |
| `vfs` | `"opfs" \| "opfs-sahpool" \| "memory"` | VFS to use. When omitted it is inferred (see below). |
| `durability` | `"durable" \| "balanced" \| "performance"` | SQLite durability profile. Defaults to `"durable"`. |
| `busyTimeoutMs` | `number` | SQLite busy timeout in milliseconds. |
| `writerQueueCapacity` | `number` | Capacity of the writer queue. |
| `encryptionKey` | `string` | Passphrase used by the AES-256-GCM codec. |
| `wasmModule` | `WebAssembly.Module \| BufferSource \| Response \| Promise<Response>` | Pre-loaded module used instead of the bundled `.wasm` file. |
| `worker` | `Worker \| string \| URL` | Custom worker instance or URL for the browser OPFS bridge. |
| `plugins` | `readonly ("kv" \| "json")[]` | Accepted for API parity with `@liliadb/node`; currently has no effect in the WASM build. |

### VFS resolution

When `vfs` is omitted, the SDK resolves it as follows:

1. A path containing `:memory:` uses the `memory` VFS.
2. A `?vfs=` query parameter in `path` selects that VFS.
3. If OPFS is available, `opfs-sahpool` is used.
4. Otherwise the in-memory VFS is used.

An explicit `vfs: "opfs"` or `vfs: "opfs-sahpool"` always maps to the SAHPool VFS, and `vfs: "memory"` always selects memory.

### Web Worker bridge

Browsers only expose synchronous OPFS access handles inside dedicated workers. When `Database.open()` is called from a browser main thread with an OPFS VFS, `@liliadb/wasm` automatically:

1. Spawns the bundled worker (`@liliadb/wasm/worker`) as an ES module.
2. Opens the database inside it.
3. Proxies every operation over `postMessage` RPC.

In a dedicated worker, Node.js, or with an in-memory VFS, the SDK calls the WASM bindings directly.

To control worker creation (bundler hints, custom module, CSP), pass `worker`:

```typescript
import { Database } from "@liliadb/wasm";

const db = await Database.open({
  path: "app.lilia",
  worker: new URL("@liliadb/wasm/worker", import.meta.url),
});

// Or reuse an existing Worker instance
const worker = new Worker(new URL("@liliadb/wasm/worker", import.meta.url), { type: "module" });
const db2 = await Database.open({ path: "app.lilia", worker });
```

### Explicit WASM initialization

The bindings initialize lazily on the first `Database.open()`. To load a custom module (bundler asset, embedded bytes, or a pre-fetched response), initialize explicitly:

```typescript
import { ensureWasmInitialized, Database } from "@liliadb/wasm";

await ensureWasmInitialized(fetch(new URL("@liliadb/wasm/pkg/lilia_wasm_bg.wasm", import.meta.url)));
await ensureWasmInitialized(new Uint8Array(buffer));
await ensureWasmInitialized(new WebAssembly.Module(bytes));

const db = await Database.open({ path: ":memory:" });
```

You can also pass the module through `wasmModule` on the first `open()`. Under Node.js, the SDK reads the bundled `.wasm` file from disk automatically.

---

## Key/Value Model (`db.kv`)

Keys and values are `Uint8Array` slices. Entries carry a `bigint` version and optional millisecond expiry.

```typescript
const encoder = new TextEncoder();
const decoder = new TextDecoder();

const write = await db.kv.set("sessions", encoder.encode("sess_01"), encoder.encode("active"), {
  ifVersion: 0n,                      // Insert only if the key does not exist
  expiresAtMs: Date.now() + 3_600_000, // 1 hour TTL
});
console.log(write.version, write.deleted);

const entry = await db.kv.get("sessions", encoder.encode("sess_01"));
if (entry) {
  console.log(entry.namespace, entry.key, decoder.decode(entry.value), entry.version, entry.expiresAtMs);
}

await db.kv.delete("sessions", encoder.encode("sess_01"), { ifVersion: write.version });

const entries = await db.kv.scan("sessions", { limit: 100 });
```

| Method | Signature |
| --- | --- |
| `get` | `(namespace, key: Uint8Array) => Promise<KvEntry \| undefined>` |
| `set` | `(namespace, key, value, options?: { ifVersion?: bigint; expiresAtMs?: number }) => Promise<MutationResult>` |
| `delete` | `(namespace, key, options?: { ifVersion?: bigint }) => Promise<MutationResult>` |
| `scan` | `(namespace, options?: { after?: Uint8Array; limit?: number }) => Promise<KvEntry[]>` |
| `scanStream` / `stream` | `(namespace, options?: ScanStreamOptions<Uint8Array>) => AsyncIterableIterator<KvEntry>` |

`KvEntry` is `{ namespace, key: Uint8Array, value: Uint8Array, version: bigint, expiresAtMs?: number }`.
`scan` defaults to `limit: 100`.

---

## JSON Document Model (`db.json`)

Documents are stored as structured JSON values. Each entry has a `space`, a string `id`, the decoded `value`, and a `bigint` `version`.

```typescript
const put = await db.json.put(
  "customers",
  "cust_100",
  { name: "Grace Hopper", plan: "enterprise", tags: ["vip", "early-adopter"] },
  { ifVersion: 0n },
);

const doc = await db.json.get("customers", "cust_100");
console.log(doc?.id, doc?.value, doc?.version);

await db.json.delete("customers", "cust_100", { ifVersion: put.version });

const list = await db.json.scan("customers", { limit: 50 });
```

| Method | Signature |
| --- | --- |
| `get` | `(space, id) => Promise<JsonEntry \| undefined>` |
| `put` | `(space, id, value, options?: { ifVersion?: bigint }) => Promise<MutationResult>` |
| `delete` | `(space, id, options?: { ifVersion?: bigint }) => Promise<MutationResult>` |
| `scan` | `(space, options?: { after?: string; limit?: number }) => Promise<JsonEntry[]>` |
| `scanStream` / `stream` | `(space, options?: ScanStreamOptions<string>) => AsyncIterableIterator<JsonEntry>` |

`JsonEntry` is `{ space: string, id: string, value: JsonValue, version: bigint }`.

### Secondary indexing

`createIndex` backfills existing documents, keeps the index in sync for future writes, and automatically indexes array elements (multikey indexes).

```typescript
// Field name shorthand (path defaults to $.field)
await db.json.createIndex("customers", "plan");

// Explicit JSON path
await db.json.createIndex("customers", "email", "$.email");

// Named index
await db.json.createIndex("customers", "email", { name: "idx_customer_email", path: "$.email" });

const indexes = await db.json.listIndexes("customers");
// [{ space: "customers", indexName: "idx_customer_email", jsonPath: "$.email" }]

const dropped = await db.json.dropIndex("customers", "idx_customer_email");
```

### Finding documents

```typescript
// Match on one or more fields (AND semantics). Uses an index when available.
const enterprise = await db.json.find("customers", { plan: "enterprise" }, { limit: 10 });
const vip = await db.json.find("customers", { tags: "vip" });

// Exact index lookup
const byEmail = await db.json.findByIndex("customers", "idx_customer_email", "grace@example.com");

// Streaming filtered queries
for await (const customer of db.json.findStream("customers", { plan: "enterprise" }, { batchSize: 100 })) {
  console.log(customer.id);
}
```

| Method | Signature |
| --- | --- |
| `createIndex` | `(space, fieldOrPath, pathOrOptions?: string \| { name?: string; path?: string }) => Promise<void>` |
| `dropIndex` | `(space, indexName) => Promise<boolean>` |
| `listIndexes` | `(space) => Promise<JsonIndexInfo[]>` |
| `find` | `(space, query, options?: { after?: string; limit?: number }) => Promise<JsonEntry[]>` |
| `findByIndex` | `(space, indexName, value: string \| number \| boolean, options?: { after?: string; limit?: number }) => Promise<JsonEntry[]>` |
| `findStream` | `(space, query, options?: ScanStreamOptions<string>) => AsyncIterableIterator<JsonEntry>` |

`limit` defaults to `100` for scans, finds, and index lookups.

---

## Graph Engine (`db.graph`)

Directed graphs with typed nodes, typed edges, neighbor traversal, and transitive reachability.

```typescript
await db.graph.setNode("social", "user_1", "User", { name: "Alice" });
await db.graph.setNode("social", "user_2", "User", { name: "Bob" });
await db.graph.addEdge("social", "user_1", "user_2", "FOLLOWS");

const friends = await db.graph.neighbors("social", "user_1", {
  direction: "outgoing", // "outgoing" | "incoming" | "both"
  kind: "FOLLOWS",
});

const reachable = await db.graph.reachability("social", ["user_1"], {
  direction: "outgoing",
});
```

| Method | Signature |
| --- | --- |
| `setNode` | `(graph, id, kind, metadata?: JsonValue) => Promise<MutationResult>` |
| `addEdge` | `(graph, source, target, kind) => Promise<MutationResult>` |
| `neighbors` | `(graph, node, options?: { direction?: GraphDirection; kind?: string }) => Promise<string[]>` |
| `reachability` | `(graph, startNodes: string[], options?: { direction?: GraphDirection }) => Promise<string[]>` |

`direction` defaults to `"outgoing"`. Node and edge deletion is available through `db.batch()` with `graph_delete_node` and `graph_remove_edge`.

---

## Full-Text Search (`db.fts`)

SQLite FTS5 with BM25 ranking and snippet generation.

```typescript
await db.fts.index("articles", "art_1", "LiliaDB delivers unified SQLite multimodel storage.");

const hits = await db.fts.search("articles", "multimodel storage", { limit: 5 });
for (const hit of hits) {
  console.log(hit.docId, hit.rank, hit.snippet);
}
```

| Method | Signature |
| --- | --- |
| `index` | `(collection, docId, content) => Promise<MutationResult>` |
| `search` | `(collection, query, options?: { limit?: number }) => Promise<FtsHit[]>` |

`FtsHit` is `{ collection, docId, snippet, rank }`. `limit` defaults to `10`. Remove indexed text with the `fts_delete` batch operation.

---

## Vector Search (`db.vector`)

High-dimensional vector storage and nearest-neighbor search with Float32, Int8, and 1-bit encodings.

```typescript
// Float32 vectors (dimension inferred from the array length)
await db.vector.set("catalog", "item_1", [0.15, -0.42, 0.91, 0.05]);

// Explicit element type and dimension
await db.vector.set("catalog", "item_2", new Float32Array([0.1, 0.2, 0.3]), {
  dim: 3,
  elementType: "float32", // "float32" | "int8" | "bit"
});

const raw = await db.vector.get("catalog", "item_1");
// Uint8Array containing the encoded vector, or null when absent.

const hits = await db.vector.search("catalog", [0.12, -0.4, 0.88, 0.02], {
  metric: "cosine", // "cosine" | "l2" | "dot" | "hamming"
  limit: 5,
});
for (const hit of hits) {
  console.log(hit.id, hit.distance);
}

await db.vector.delete("catalog", "item_2");
```

| Method | Signature |
| --- | --- |
| `set` | `(collection, id, vector: Float32Array \| number[] \| Uint8Array, options?: { dim?: number; elementType?: VectorElementType }) => Promise<MutationResult>` |
| `get` | `(collection, id) => Promise<Uint8Array \| null>` |
| `delete` | `(collection, id) => Promise<MutationResult>` |
| `search` | `(collection, query, options?: { dim?: number; elementType?: VectorElementType; metric?: VectorMetric; limit?: number }) => Promise<VectorHit[]>` |

`VectorHit` is `{ collection, id, distance }`. For `int8` and `bit` encodings, pass `dim` explicitly when the input is a raw `Uint8Array`; otherwise the dimension is inferred from the array length. `metric` defaults to `"cosine"` and `limit` to `10`.

`int8` and `bit` inputs supplied as `Float32Array`/`number[]` are quantized automatically. To quantize yourself and reuse the result, use the helpers below.

### Quantization helpers (`quantization`)

```typescript
import { quantization } from "@liliadb/wasm";

const floats = [0.15, -0.42, 0.91, 0.05];

// Int8 scalar quantization (returns { quantized: Int8Array, scale: number })
const { quantized, scale } = quantization.toInt8(floats);

// 1-bit binary packing
const packed = quantization.toBit(floats);

// Cosine distance, clamped to [0, 2]
const distance = quantization.cosineDistance(floats, [0.1, -0.4, 0.9, 0.0]);
```

---

## Hybrid Search (`db.hybrid`)

Combine BM25 full-text search and vector similarity with Reciprocal Rank Fusion (RRF). Provide `query`, `vector`, or both.

```typescript
const hits = await db.hybrid.search("catalog", {
  query: "wireless headphones",
  vector: [0.12, -0.4, 0.88, 0.02],
  metric: "cosine",
  rrfK: 60,          // RRF smoothing constant
  ftsWeight: 1.0,    // weight applied to the FTS ranking
  vectorWeight: 1.0, // weight applied to the vector ranking
  limit: 10,
});

for (const hit of hits) {
  console.log(hit.id, hit.rrfScore, hit.ftsRank, hit.ftsSnippet, hit.vectorRank, hit.vectorDistance);
}
```

`HybridHit` is `{ id, rrfScore, ftsRank?, ftsSnippet?, vectorRank?, vectorDistance? }`.
Defaults: `rrfK` `60`, `ftsWeight` `1.0`, `vectorWeight` `1.0`, `limit` `10`.

---

## Streaming & Large Scans

Loading an entire dataset with `.scan()` keeps every record in memory. Streaming APIs paginate transparently with keyset cursors.

```typescript
for await (const doc of db.json.scanStream("telemetry", { batchSize: 200 })) {
  console.log(doc.id, doc.value);
}

for await (const entry of db.kv.stream("analytics", {
  batchSize: 500,
  limit: 2500, // stop after 2,500 entries
})) {
  console.log(entry.key, entry.value);
}

// Resume from a cursor
for await (const doc of db.json.scanStream("events", { after: "event_0500" })) {
  console.log(doc.id);
}
```

`ScanStreamOptions` is `{ after?, limit?, batchSize? }`; `batchSize` defaults to `100`. Early `break` stops fetching further batches. `scanStream` and `stream` are aliases on `db.kv` and `db.json`; `findStream` is available on `db.json` for filtered queries.

---

## Atomic Multi-Model Batches (`db.batch`)

Every write helper (`kv.set`, `json.put`, `graph.addEdge`, `fts.index`, `vector.set`, …) is a single-operation batch. Combine multiple models into one atomic commit:

```typescript
const results = await db.batch([
  {
    model: "json_put",
    space: "users",
    id: "ada",
    value: { name: "Ada", status: "verified" },
    ifVersion: 2n,
  },
  {
    model: "kv_set",
    namespace: "audit",
    key: new TextEncoder().encode("log:ada:verified"),
    value: new TextEncoder().encode(new Date().toISOString()),
  },
  {
    model: "graph_add_edge",
    graph: "social",
    source: "ada",
    target: "grace",
    kind: "FOLLOWS",
  },
]);
// results[i] corresponds to operations[i]
```

Supported operations:

| Model | Fields |
| --- | --- |
| `kv_set` | `namespace`, `key`, `value`, `ifVersion?`, `expiresAtMs?` |
| `kv_delete` | `namespace`, `key`, `ifVersion?` |
| `json_put` | `space`, `id`, `value`, `ifVersion?` |
| `json_delete` | `space`, `id`, `ifVersion?` |
| `graph_set_node` | `graph`, `id`, `kind`, `metadata?` |
| `graph_delete_node` | `graph`, `id` |
| `graph_add_edge` | `graph`, `source`, `target`, `kind` |
| `graph_remove_edge` | `graph`, `source`, `target`, `kind` |
| `fts_index` | `collection`, `docId`, `content` |
| `fts_delete` | `collection`, `docId` |
| `vector_set` | `collection`, `id`, `dim`, `elementType?`, `vector` |
| `vector_delete` | `collection`, `id` |

If any operation fails, the whole batch rolls back and `batch()` rejects.

---

## Declarative SQL (`db.sql`)

LiliaDB exposes SQLite views over its multimodel storage. When encryption is enabled, the views and decoding functions return plaintext transparently.

| View | Columns |
| --- | --- |
| `lilia_json` | `(space, id, value, version)` — `value` is plaintext JSON |
| `lilia_kv` | `(namespace, key, value, version, expires_at_ms)` — `value` is decoded bytes |
| `lilia_graph_nodes` | `(graph, id, kind, metadata, doc_space, doc_id)` |
| `lilia_graph_edges` | `(graph, source, target, kind, weight)` |
| `lilia_graph_unified` | `(graph, id, kind, metadata, doc_space, doc_id, document)` — nodes joined with their JSON metadata |
| `lilia_vectors` | `(collection, id, dim, element_type, vector)` — decoded vectors |
| `lilia_vector_indexes` | `(collection, entry_id, max_level, m, ef_construction, ef_search, metric, element_type, dim)` |

The underlying tables (`_lilia_json`, `_lilia_kv`, `_lilia_graph_nodes`, …) remain queryable as well.

### Typed queries (`db.sql.query`)

```typescript
interface Developer {
  id: string;
  name: string;
  points: number;
}

const devs = await db.sql.query<Developer>(
  `SELECT
     id,
     json_extract(value, '$.name') AS name,
     json_extract(value, '$.points') AS points
   FROM lilia_json
   WHERE space = ? AND json_extract(value, '$.points') >= ?
   ORDER BY points DESC`,
  ["developers", 900],
);
```

### Raw projections (`db.sql.raw`)

```typescript
const raw = await db.sql.raw(
  "SELECT id, json_extract(value, '$.role') AS role FROM lilia_json WHERE space = ?",
  ["users"],
);
console.log(raw.columns); // ["id", "role"]
console.log(raw.rows);    // [["ada", "admin"], ...]
console.log(raw.records); // [{ id: "ada", role: "admin" }, ...]
console.log(raw.rowsAffected);
```

### Mutations (`db.sql.execute`)

```typescript
const { rowsAffected } = await db.sql.execute(
  "UPDATE _lilia_json SET version = version + 1 WHERE space = ?",
  ["users"],
);
```

| Method | Signature |
| --- | --- |
| `query<T>` | `(sql, params?: JsonValue[]) => Promise<T[]>` |
| `raw<T>` | `(sql, params?: JsonValue[]) => Promise<SqlQueryResult<T>>` |
| `execute` | `(sql, params?: JsonValue[]) => Promise<{ rowsAffected: number }>` |

`SqlQueryResult<T>` is `{ columns: string[], rows: JsonValue[][], rowsAffected: number, records: T[] }`. Parameters are positional and passed as JSON values.

---

## Versioning & Concurrency

- Versions are always `bigint`, from reads, scans, and mutations.
- `ifVersion: 0n` means "insert only if the entry does not exist".
- A mismatched `ifVersion` fails with a `CONFLICT` error and changes nothing.
- Out-of-range or non-`bigint` versions throw `LiliaError` with code `INVALID_INPUT`.
- The stored version range is `0..=18446744073709551615` (SQLite `u64`).
- `expiresAtMs` must be a safe integer.

`MutationResult` is `{ version?: bigint; deleted: boolean }`.

---

## Error Handling

All operations reject with structured `LiliaError` instances when the WASM layer reports one. Other failures reject with a plain `Error`.

```typescript
import { LiliaError } from "@liliadb/wasm";

try {
  await db.json.put("users", "ada", { status: "updated" }, { ifVersion: 99n });
} catch (error) {
  if (error instanceof LiliaError) {
    console.error(`Code: ${error.code}`);          // e.g. "CONFLICT", "CLOSED", "TIMEOUT", "INVALID_INPUT"
    console.error(`Message: ${error.message}`);
    console.error(`Retryable: ${error.retryable}`);
    console.error(`RequestId: ${error.requestId}`); // may be undefined
  }
}
```

`LiliaError` extends `Error` with `code`, `retryable`, and an optional `requestId`. Errors crossing the worker boundary preserve `code` and `retryable`.

---

## Lifecycle & Graceful Shutdown

`close()` drains operations already accepted by the SDK, releases the database, and terminates the worker if one was spawned. It is idempotent.

```typescript
await db.close({ timeoutMs: 5_000 });
```

- `timeoutMs` defaults to `5_000` and must be an integer in `0..=2147483647`.
- A timeout rejects with code `TIMEOUT` (`retryable: true`) but does not cancel accepted work.
- New operations after `close()` reject with code `CLOSED`.

---

## Maintenance Operations

```typescript
// SQLite integrity check
const ok: boolean = await db.integrityCheck();
```

---

## Exported API

The package root exports:

- `Database` — the main class and all model namespaces.
- `ensureWasmInitialized` — manual WASM module loading.
- `quantization` — `toInt8`, `toBit`, and `cosineDistance` helpers.
- `LiliaError` and its `LiliaErrorShape`.
- Types: `WasmOpenOptions`, `OpenOptions`, `WasmVfs`, `Durability`, `JsonValue`, `KvEntry`, `JsonEntry`, `JsonIndexInfo`, `CreateIndexOptions`, `JsonFindOptions`, `ScanStreamOptions`, `GraphDirection`, `FtsHit`, `VectorElementType`, `VectorMetric`, `VectorHit`, `HybridHit`, `BatchOperation`, `MutationResult`, `SqlQueryResult`, `DatabaseEvent`, and `WatchSubscription`.

The worker entry point is available at `@liliadb/wasm/worker`, and the raw bindings are exposed under `@liliadb/wasm/pkg/*`.

---

## Architecture

```
┌────────────────────────────────────────────────────────┐
│                   Main Browser Thread                  │
│                                                        │
│   db.json.put() ──►  Database  ──► WorkerClient (RPC)  │
└───────────────────────────┬────────────────────────────┘
                            │ postMessage
┌───────────────────────────▼────────────────────────────┐
│                  Dedicated Web Worker                  │
│                                                        │
│  worker.ts ──► DirectWasmClient ──► WasmDatabase       │
│                                      │ (Rust/WASM)    │
│                                      ▼                 │
│                         sqlite-wasm-vfs (SAHPool)      │
│                                      │                 │
│                                      ▼                 │
│                     FileSystemSyncAccessHandle         │
│                                      │                 │
│                                      ▼                 │
│                       Origin Private File System       │
└────────────────────────────────────────────────────────┘
```

In a dedicated worker, Node.js, or with the memory VFS, `Database` talks to `DirectWasmClient` without the RPC layer.

---

## License

Apache-2.0
