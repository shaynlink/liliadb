# @liliadb/node

Official Node.js SDK for **LiliaDB**, a local-first multimodel database with durable
Key/Value, JSON Document, Graph, Full-Text Search, Vector, and SQL storage backed by
SQLite and native plugins.

## Features

- **Embedded & Daemon modes**: Run in-process through Node-API bindings or connect to a
  local `lilia-daemon` over Unix domain sockets / Windows named pipes.
- **Multimodel, transactional**: Key/Value, JSON, Graph, FTS, and Vector operations share
  a single ACID commit through `db.batch()`.
- **Secondary JSON indexes**: Create, list, and drop dynamic indexes, then query with
  `find()` (index-accelerated when possible) or `findByIndex()`.
- **Streaming everywhere**: Keyset-paginated async iterators (`scanStream`), Node.js
  `Readable` streams (`createReadStream`), and WHATWG `ReadableStream` (`readableStream`).
- **Real-time CDC**: Live mutation subscriptions with namespace/space filtering.
- **Exact 64-bit versions**: `bigint` versions for optimistic concurrency control
  (`ifVersion`) without IEEE-754 precision loss.
- **TTL support**: Millisecond expiration on Key/Value entries.
- **Cancellation**: `AbortSignal` support on every asynchronous operation and stream.
- **Synchronous access**: Zero-async-overhead embedded API via `db.sync`.
- **Multi-runtime**: Node.js `>=22`, Bun `>=1.0`, and Deno `>=2.0`.

---

## Installation

```sh
pnpm add @liliadb/node
# or
npm install @liliadb/node
# or
bun add @liliadb/node
# or
deno add npm:@liliadb/node
```

> **Requirements**: Node.js `>=22` and a platform-supported native addon:
> `@liliadb/node-darwin-arm64`, `@liliadb/node-darwin-x64`,
> `@liliadb/node-linux-x64-gnu`, `@liliadb/node-linux-arm64-gnu`, or
> `@liliadb/node-win32-x64-msvc`.
>
> To load a locally built addon instead, set `LILIA_NATIVE_PATH` to the compiled
> `liblilia_node_native` / `lilia_node_native` artifact.

---

## Quick Start

```ts
import { Database } from "@liliadb/node";

const db = await Database.open({ path: "app.lilia" });

// 1. JSON document
await db.json.put("users", "ada", { name: "Ada Lovelace", role: "admin" });
const user = await db.json.get("users", "ada");
console.log(user?.value, user?.version);

// 2. Key/Value
const encoder = new TextEncoder();
await db.kv.set("cache", "token", encoder.encode("xyz123"));
const token = await db.kv.get("cache", "token");
console.log(new TextDecoder().decode(token?.value));

// 3. Graph
await db.graph.setNode("social", "ada", "User", { name: "Ada" });
await db.graph.setNode("social", "grace", "User", { name: "Grace" });
await db.graph.addEdge("social", "ada", "grace", "FOLLOWS");
console.log(await db.graph.neighbors("social", "ada", { direction: "outgoing" }));

// 4. Full-text search
await db.fts.index("articles", "a1", "LiliaDB is a local-first multimodel database");
console.log(await db.fts.search("articles", "multimodel"));

// 5. Vector search
await db.vector.set("embeddings", "ada", [0.1, 0.2, 0.3]);
console.log(await db.vector.search("embeddings", [0.1, 0.2, 0.29], { limit: 3 }));

// 6. Clean shutdown
await db.close();
```

---

## Opening a Database

`Database.open(options)` accepts the following options:

| Option | Type | Description |
| --- | --- | --- |
| `path` | `string` | **Required.** Database file path. |
| `mode` | `"embedded" \| "daemon"` | Execution mode. Defaults to embedded. |
| `durability` | `"durable" \| "balanced" \| "performance"` | SQLite durability profile. |
| `plugins` | `readonly ("kv" \| "json")[]` | Enabled plugins. The v0.1 distribution requires both `kv` and `json`; this is the default. |
| `endpoint` | `string` | Daemon IPC endpoint. Defaults to `${path}.sock` (or the Windows named-pipe equivalent). |
| `tokenFile` | `string` | Daemon authentication token file. Defaults to `${path}.token`. |
| `busyTimeoutMs` | `number` | SQLite busy timeout. |
| `writerQueueCapacity` | `number` | Size of the native writer queue. |
| `readPoolSize` | `number` | Number of native read connections. |
| `checkpointPolicy` | `{ walBytes?: number; intervalMs?: number }` | WAL checkpoint thresholds; values must be integers in `1..=4294967295`. |
| `casOptions` | `{ enabled?: boolean; thresholdBytes?: number; hashAlgorithm?: "sha256" \| "blake3"; blobsDir?: string; autoPurgeOnCheckpoint?: boolean }` | Content Addressable Storage (CAS) options for large BLOB deduplication. |
| `encryptionKey` | `string` | Key used by the encryption codec at rest. |
| `signal` | `AbortSignal` | Cancels opening. |

```ts
const db = await Database.open({
  path: "app.lilia",
  durability: "durable",
  busyTimeoutMs: 5000,
  writerQueueCapacity: 64,
  readPoolSize: 4,
  checkpointPolicy: { walBytes: 4 * 1024 * 1024, intervalMs: 1000 },
});
```

### Daemon Mode

```ts
const db = await Database.open({
  mode: "daemon",
  path: "app.lilia",
  endpoint: "app.lilia.sock",   // or \\.\pipe\lilia on Windows
  tokenFile: "app.lilia.token",
});
```

The daemon is a separate `lilia-daemon` process that owns the database file. The client
speaks a length-prefixed MessagePack protocol over local IPC.

### Embedded Mode

Embedded mode runs in-process. It exposes `db.sync` for synchronous access and supports
SQLite WAL background operations without blocking the JavaScript event loop.

---

## Key/Value Model (`db.kv`)

Keys and values are binary `Uint8Array` slices. For convenience, `string` keys and values
are UTF-8 encoded automatically. Entries carry a `bigint` version and optional millisecond
expiry.

```ts
const encoder = new TextEncoder();
const decoder = new TextDecoder();

const write = await db.kv.set(
  "sessions",
  "sess_01",
  encoder.encode("active"),
  {
    ifVersion: 0n,                      // Insert only if the key does not exist
    expiresAtMs: Date.now() + 3600_000, // 1 hour TTL
  },
);
console.log(write.version, write.deleted);

const entry = await db.kv.get("sessions", "sess_01");
if (entry) {
  console.log(entry.namespace, entry.key, decoder.decode(entry.value), entry.version, entry.expiresAtMs);
}

await db.kv.delete("sessions", "sess_01", { ifVersion: write.version });

// Bounded in-memory scan
const entries = await db.kv.scan("sessions", { limit: 100 });
```

| Method | Signature |
| --- | --- |
| `get` | `(namespace, key, options?: { signal? }) => Promise<KvEntry \| undefined>` |
| `set` | `(namespace, key, value, options?: { ifVersion?: bigint; expiresAtMs?: number; signal? }) => Promise<MutationResult>` |
| `delete` | `(namespace, key, options?: { ifVersion?: bigint; signal? }) => Promise<MutationResult>` |
| `scan` | `(namespace, options?: { after?: Uint8Array \| string; limit?: number; signal? }) => Promise<KvEntry[]>` |
| `watch` | `(namespace, callback, options?: { signal? }) => Promise<WatchSubscription>` |

`KvEntry` is `{ namespace, key: Uint8Array, value: Uint8Array, version: bigint, expiresAtMs?: number }`.

---

## JSON Document Model (`db.json`)

Documents are stored as structured JSON values. Each entry has a `space`, a string `id`,
the decoded `value`, and a `bigint` `version`.

```ts
const put = await db.json.put("customers", "cust_100", {
  name: "Grace Hopper",
  plan: "enterprise",
  credits: 5000,
  tags: ["vip", "early-adopter"],
}, { ifVersion: 0n });

const doc = await db.json.get<{ name: string }>("customers", "cust_100");
console.log(doc?.id, doc?.value.name, doc?.version);

await db.json.delete("customers", "cust_100", { ifVersion: put.version });

const list = await db.json.scan("customers", { limit: 50 });
```

| Method | Signature |
| --- | --- |
| `get<T>` | `(space, id, options?: { signal? }) => Promise<JsonEntry<T> \| undefined>` |
| `put<T>` | `(space, id, value, options?: { ifVersion?: bigint; signal? }) => Promise<MutationResult>` |
| `delete` | `(space, id, options?: { ifVersion?: bigint; signal? }) => Promise<MutationResult>` |
| `scan<T>` | `(space, options?: { after?: string; limit?: number; signal? }) => Promise<JsonEntry<T>[]>` |
| `watch` | `(space, callback, options?: { signal? }) => Promise<WatchSubscription>` |

`JsonEntry<T>` is `{ space: string, id: string, value: T, version: bigint }`.

### Secondary Indexing

`createIndex` backfills existing documents, keeps the index in sync for future writes,
and automatically indexes array elements (multikey indexes).

```ts
// Index through a field name (path defaults to $.field)
await db.json.createIndex("customers", "plan");

// Explicit path
await db.json.createIndex("customers", "email", "$.email");

// Named index
await db.json.createIndex("customers", "email", { name: "idx_custom_email", path: "$.email" });

const indexes = await db.json.listIndexes("customers");
// [{ space: "customers", indexName: "idx_custom_email", jsonPath: "$.email" }]

const dropped = await db.json.dropIndex("customers", "idx_custom_email");
```

### Finding Documents

```ts
// Match on one or more fields (AND semantics). Uses an index when available,
// otherwise falls back to a scan.
const enterprise = await db.json.find("customers", { plan: "enterprise" }, { limit: 10 });
const activeAdmins = await db.json.find("customers", { plan: "enterprise", active: true });

// Multikey match: matches any document whose `tags` array contains "vip"
const vip = await db.json.find("customers", { tags: "vip" });

// Low-level exact index lookup
const byEmail = await db.json.findByIndex("customers", "idx_custom_email", "grace@example.com");
```

| Method | Signature |
| --- | --- |
| `createIndex` | `(space, fieldOrPath, pathOrOptions?: string \| { name?: string; path?: string }, options?: { signal? }) => Promise<void>` |
| `dropIndex` | `(space, indexName, options?: { signal? }) => Promise<boolean>` |
| `listIndexes` | `(space, options?: { signal? }) => Promise<JsonIndexInfo[]>` |
| `find<T>` | `(space, query, options?: { after?: string; limit?: number; signal? }) => Promise<JsonEntry<T>[]>` |
| `findByIndex<T>` | `(space, indexName, value, options?: { after?: string; limit?: number; signal? }) => Promise<JsonEntry<T>[]>` |
| `findStream<T>` | `(space, query, options?: ScanStreamOptions<string>) => AsyncIterableIterator<JsonEntry<T>>` |
| `findReadableStream<T>` | `(space, query, options?: ScanStreamOptions<string>) => ReadableStream<JsonEntry<T>>` |

`limit` defaults to `100` for scans and finds.

---

## Graph Engine (`db.graph`)

Directed graphs with typed nodes, typed edges, neighbor traversal, and reachability.

```ts
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
// ["user_1", ...all reachable node ids]
```

| Method | Signature |
| --- | --- |
| `setNode` | `(graph, id, kind, metadata?: JsonValue, options?: { signal? }) => Promise<MutationResult>` |
| `addEdge` | `(graph, source, target, kind, options?: { signal? }) => Promise<MutationResult>` |
| `neighbors` | `(graph, node, options?: { direction?; kind?; signal? }) => Promise<string[]>` |
| `reachability` | `(graph, startNodes, options?: { direction?; signal? }) => Promise<string[]>` |

Node and edge deletion is available through `db.batch()` with `graph_delete_node` and
`graph_remove_edge`.

---

## Full-Text Search (`db.fts`)

SQLite FTS5 with BM25 ranking and snippet generation.

```ts
await db.fts.index("articles", "art_1", "LiliaDB delivers unified SQLite multimodel storage.");

const hits = await db.fts.search("articles", "multimodel storage", { limit: 5 });
for (const hit of hits) {
  console.log(hit.docId, hit.rank, hit.snippet);
}
```

| Method | Signature |
| --- | --- |
| `index` | `(collection, docId, content, options?: { signal? }) => Promise<MutationResult>` |
| `search` | `(collection, query, options?: { limit?: number; signal? }) => Promise<FtsHit[]>` |

`FtsHit` is `{ collection, docId, snippet, rank }`. Remove indexed text with the
`fts_delete` batch operation.

---

## Vector Search (`db.vector`)

Native high-dimensional vector storage and approximate nearest neighbor (ANN) search
backed by HNSW.

```ts
// Float32 vectors (dimension inferred from the array length)
await db.vector.set("catalog", "item_1", [0.15, -0.42, 0.91, 0.05]);

// Explicit element type and dimension
await db.vector.set("catalog", "item_2", new Float32Array([0.1, 0.2, 0.3]), {
  dim: 3,
  elementType: "float32", // "float32" | "int8" | "bit"
});

const raw = await db.vector.get("catalog", "item_1");
// Uint8Array containing the encoded vector, or null when absent.

const hits = await db.vector.search("catalog", [0.12, -0.40, 0.88, 0.02], {
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
| `set` | `(collection, id, vector, options?: { dim?: number; elementType?: VectorElementType; signal? }) => Promise<MutationResult>` |
| `get` | `(collection, id, options?: { signal? }) => Promise<Uint8Array \| null>` |
| `delete` | `(collection, id, options?: { signal? }) => Promise<MutationResult>` |
| `search` | `(collection, query, options?: { dim?: number; elementType?: VectorElementType; metric?: VectorMetric; limit?: number; signal? }) => Promise<VectorHit[]>` |

`VectorHit` is `{ collection, id, distance }`. For `int8` and `bit` encodings, pass `dim`
explicitly when the input is a raw `Uint8Array`. `metric` defaults to `"cosine"` and
`limit` to `10`.

### Quantization Helpers (`quantization`)

```ts
import { quantization } from "@liliadb/node";

const floats = [0.15, -0.42, 0.91, 0.05];

// Int8 scalar quantization (returns { quantized: Int8Array, scale: number })
const { quantized, scale } = quantization.toInt8(floats);

// 1-bit binary packing
const packed = quantization.toBit(floats);

// Cosine distance, clamped to [0, 2]
const distance = quantization.cosineDistance(floats, [0.10, -0.40, 0.90, 0.0]);
```

---

## Hybrid Search (`db.hybrid`)

Combine BM25 full-text search and vector similarity with Reciprocal Rank Fusion (RRF).
Provide `query`, `vector`, or both.

```ts
const hits = await db.hybrid.search("catalog", {
  query: "wireless headphones",
  vector: [0.12, -0.40, 0.88, 0.02],
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

Loading an entire dataset with `.scan()` keeps every record in memory. Use the streaming
APIs for large or unbounded collections; they paginate transparently with keyset cursors.

### Async iteration

```ts
for await (const doc of db.json.scanStream("telemetry", { batchSize: 200 })) {
  console.log(doc.id, doc.value);
}

for await (const entry of db.kv.scanStream("analytics", {
  batchSize: 500,
  limit: 2500, // stop after 2,500 entries
})) {
  console.log(entry.key, entry.value);
}
```

`scanStream` and `stream` are aliases. Options are
`{ after?, limit?, batchSize?, signal? }`; `batchSize` defaults to `100`. Early `break`
stops fetching further batches.

### Resume from a cursor

```ts
for await (const doc of db.json.scanStream("events", { after: "event_0500", batchSize: 100 })) {
  console.log(doc.id);
}
```

### Node.js streams (`createReadStream`)

```ts
import { pipeline } from "node:stream/promises";
import { Transform } from "node:stream";

const readStream = db.json.createReadStream("records", { batchSize: 100 });

const toCsv = new Transform({
  objectMode: true,
  transform(doc, _encoding, callback) {
    callback(null, `${doc.id},${JSON.stringify(doc.value)}\n`);
  },
});

await pipeline(readStream, toCsv, process.stdout);
```

### WHATWG streams (`readableStream`)

```ts
const stream = db.json.readableStream("records", { batchSize: 100 });

for await (const doc of stream) {
  console.log(doc.id);
}
```

The same family is available on `db.kv` (`scanStream`, `stream`, `readableStream`,
`createReadStream`) and for filtered JSON queries (`findStream`, `findReadableStream`).

---

## Real-Time CDC (`db.watch`)

Subscribe to live mutation events for KV, JSON, Graph, FTS, and Vector writes.

```ts
const sub = await db.watch("orders", (event) => {
  switch (event.type) {
    case "kv_mutated":
      console.log(event.namespace, event.key, event.version, event.deleted);
      break;
    case "json_mutated":
      console.log(event.space, event.id, event.version, event.deleted);
      break;
  }
});

await db.json.put("orders", "ord_100", { total: 49.99 });

await sub.unsubscribe();
```

`db.kv.watch(namespace, callback, options?)` and `db.json.watch(space, callback, options?)`
are scoped aliases. `db.watch(callback, options?)` with no namespace observes all events.

Event variants:

- `kv_mutated`: `{ namespace, key, version?, deleted }`
- `json_mutated`: `{ space, id, version?, deleted }`
- `graph_node_mutated`: `{ graph, id, deleted }`
- `graph_edge_mutated`: `{ graph, source, target, kind, deleted }`
- `fts_mutated`: `{ collection, docId, deleted }`
- `vector_mutated`: `{ collection, id, deleted }`

A subscription is callable (`sub()`) and also exposes `unsubscribe()`,
`Symbol.dispose`, and `Symbol.asyncDispose`. Passing an `AbortSignal` unsubscribes
automatically when it aborts.

---

## Atomic Multi-Model Batches (`db.batch`)

The KV, JSON, Graph, FTS, and Vector write helpers are each a single-operation batch.
Combine multiple models into a single atomic commit:

```ts
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
    model: "kv_delete",
    namespace: "pending_verifications",
    key: new TextEncoder().encode("ada"),
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

If any operation fails, the whole batch rolls back and the call rejects.

---

## Synchronous Embedded API (`db.sync`)

For latency-critical embedded workloads, `db.sync` executes directly on the JavaScript
thread without Promises. It is available only in embedded mode; using it with a daemon
or after `close()` throws.

```ts
const encoder = new TextEncoder();

const write = db.sync.kv.set("cache", "k1", encoder.encode("v1"));
const entry = db.sync.kv.get("cache", "k1");
const entries = db.sync.kv.scan("cache", { limit: 100 });

db.sync.json.put("users", "u1", { name: "Alice" });
const doc = db.sync.json.get("users", "u1");

const results = db.sync.batch([
  { model: "kv_set", namespace: "cache", key: encoder.encode("k2"), value: encoder.encode("v2") },
  { model: "json_put", space: "users", id: "u2", value: { name: "Bob" } },
]);

const rows = db.sync.sql.query("SELECT * FROM lilia_json WHERE space = ?1", ["users"]);
const { rowsAffected } = db.sync.sql.execute(
  "UPDATE _lilia_json SET version = version + 1 WHERE space = ?1",
  ["users"],
);
```

`db.sync` exposes `kv` (`get`, `set`, `delete`, `scan`), `json` (`get`, `put`, `delete`,
`scan`), `batch`, and `sql` (`query`, `execute`, `raw`, plus tagged-template form).
It does not expose graph, FTS, vector, or watch operations.

---

## Declarative SQL (`db.sql`)

LiliaDB exposes SQLite views over its multimodel storage. Custom scalar functions
(`lilia_decode_kv`, `lilia_decode_json`) transparently decrypt values when an encryption
codec is enabled.

| View | Columns |
| --- | --- |
| `lilia_json` | `(space, id, value, version)` — `value` is decoded plaintext JSON |
| `lilia_kv` | `(namespace, key, value, version, expires_at_ms)` — `value` is decoded bytes |
| `lilia_graph_nodes` | `(graph, id, kind, metadata)` |
| `lilia_graph_edges` | `(graph, source, target, kind)` |

### Typed queries (`db.sql.query`)

```ts
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
   WHERE space = ?1 AND json_extract(value, '$.points') >= ?2
   ORDER BY points DESC`,
  ["developers", 900],
);
```

### Tagged templates

`db.sql` and `db.sync.sql` are also callable as tagged template literals, which bind
values as numbered parameters:

```ts
const role = "admin";
const rows = await db.sql<{ id: string; name: string }>`
  SELECT id, json_extract(value, '$.name') AS name
  FROM lilia_json
  WHERE space = 'users' AND json_extract(value, '$.role') = ${role}
`;
```

### Raw projections (`db.sql.raw`)

```ts
const raw = await db.sql.raw(
  "SELECT id, json_extract(value, '$.role') AS role FROM lilia_json WHERE space = ?1",
  ["users"],
);
console.log(raw.columns);   // ["id", "role"]
console.log(raw.rows);      // [["ada", "admin"], ...]
console.log(raw.rowsAffected);
```

### Mutations (`db.sql.execute`)

```ts
const { rowsAffected } = await db.sql.execute(
  "UPDATE _lilia_json SET version = version + 1 WHERE space = ?1",
  ["users"],
);
```

`query`, `raw`, and `execute` accept `(sql, params?, { signal? })`. Writes are routed
through the write queue.

---

## Versioning & Concurrency

- Versions are always `bigint`, from reads, scans, and mutations.
- `ifVersion: 0n` means "insert only if the entry does not exist".
- A mismatched `ifVersion` fails with a conflict error and changes nothing.
- Out-of-range versions throw `LiliaError` with code `INVALID_INPUT`.
- The stored version range is `0..=18446744073709551615` (SQLite `u64`).

`MutationResult` is `{ version?: bigint; deleted: boolean }`.

---

## Cancellation with `AbortSignal`

Every asynchronous method and stream accepts `options.signal`. A signal already aborted
rejects immediately; aborting later rejects the pending Promise or stops the iterator.

```ts
const controller = new AbortController();

const promise = db.json.scan("events", { signal: controller.signal });
controller.abort();

await promise; // rejects with the signal's reason
```

Pass `signal` to `Database.open()` to cancel opening, to `db.close()` to bound the drain
wait, and to `db.watch()` to unsubscribe on abort.

---

## Error Handling

All SDK operations throw structured `LiliaError` instances:

```ts
import { LiliaError } from "@liliadb/node";

try {
  await db.json.put("users", "ada", { status: "updated" }, { ifVersion: 99n });
} catch (error) {
  if (LiliaError.isLiliaError(error)) {
    console.error(`Code: ${error.code}`);          // e.g. "CONFLICT", "CLOSED", "TIMEOUT", "INVALID_INPUT"
    console.error(`Message: ${error.message}`);
    console.error(`Retryable: ${error.retryable}`); // true for transient / busy failures
    console.error(`RequestId: ${error.requestId}`);
    console.error(`Details: ${JSON.stringify(error.details)}`);
    console.error(`Cause: ${error.cause}`);
  }
}
```

`LiliaError` extends `Error` with `code`, `retryable`, `details`, `requestId`, and
`cause`. It also supports `instanceof LiliaError` and `LiliaError.isLiliaError(value)`.

---

## Lifecycle & Graceful Shutdown

`close()` drains operations already accepted by the SDK, then releases native locks and
worker threads. It is idempotent, and pending `close()` calls can be awaited again.

```ts
await db.close({ timeoutMs: 5_000, signal: undefined });
```

- `timeoutMs` defaults to `5_000` and must be an integer in `0..=2147483647`.
- A timeout rejects with code `TIMEOUT` (`retryable: true`) but does not cancel accepted
  work.
- New operations after `close()` reject with code `CLOSED`.

### Explicit resource management

```ts
await using db = await Database.open({ path: "app.lilia" });
await db.json.put("users", "ada", { name: "Ada" });
// db.close() is called automatically when the scope exits
```

Watch subscriptions also support `using`:

```ts
using sub = await db.watch("orders", (event) => console.log(event));
```

---

## Content Addressable Storage (CAS) BLOBs (`db.blob*`)

Large binaries (> 64 KB configurable threshold) are cryptographically hashed and offloaded to a deduplicated content-addressable storage directory outside SQLite's B-Tree and WAL:

```ts
// 1. Explicit BLOB storage
const desc = await db.blobStore(largePdfBytes, "application/pdf");
console.log(desc.$blob, desc.sizeBytes); // { "$blob": "<sha256>", "sizeBytes": 1048576 }

// 2. Direct retrieval
const bytes = await db.blobGet(desc.hash);

// 3. Inspect disk path or existence
console.log(db.blobPath(desc.hash), db.blobExists(desc.hash));

// 4. Synchronous access
const syncDesc = db.sync.blob.store(largePdfBytes, "application/pdf");

// 5. Atomic Garbage Collection (purges unreferenced blobs)
const stats = await db.blobGc();
console.log(`Purged ${stats.orphansPurged} orphans, reclaimed ${stats.bytesReclaimed} bytes`);
```

In addition, any `db.kv.set` whose payload exceeds `thresholdBytes` (default: 64 KB) is automatically and transparently offloaded to CAS. `db.kv.get` and `db.kv.scan` seamlessly reconstitute the original binary payload.

---

## Maintenance Operations

```ts
// SQLite integrity check
const ok: boolean = await db.integrityCheck({ signal });

// Atomic online backup to a destination file
await db.backup("./snapshots/app.lilia", { signal });
```

---

## Type Guards & Exported Types

```ts
import { isKvEntry, isJsonEntry, type KvEntry, type JsonEntry } from "@liliadb/node";

if (isKvEntry(value)) console.log(value.version);
if (isJsonEntry(value)) console.log(value.id);
```

The package exports all public types, including `OpenOptions`, `DatabaseMode`,
`Durability`, `KvEntry`, `JsonEntry`, `JsonIndexInfo`, `CreateIndexOptions`,
`JsonFindOptions`, `ScanStreamOptions`, `BatchOperation`, `DatabaseEvent`,
`WatchOptions`, `WatchSubscription`, `MutationResult`, `FtsHit`, `VectorElementType`,
`VectorMetric`, `VectorHit`, `HybridHit`, `SqlQueryResult`, `SqlFunction`,
`SyncSqlFunction`, `LiliaErrorShape`, `CloseOptions`, `GraphDirection`, and the
`quantization` helper object.

---

## License

Apache-2.0
