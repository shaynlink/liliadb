# LiliaDB Node.js SDK (`@liliadb/node`)

Official TypeScript and Node.js SDK for **LiliaDB**, the local-first multimodel embedded
and daemon database written in Rust.

---

## 1. Key Features

- **Embedded & Daemon Modes**: In-process execution through native Node-API bindings, or
  connection to a local `lilia-daemon` over Unix domain sockets / Windows named pipes.
- **Multimodel Engine**: Key/Value, JSON documents with dynamic secondary indexes, Graph
  traversal, Full-Text Search (FTS5), Vector search (HNSW), Hybrid RRF search, and
  declarative SQL queries.
- **Multi-Runtime Support**: Node.js `>=22`, Bun `>=1.0`, and Deno `>=2.0`.
- **Exact 64-bit Versioning**: Optimistic concurrency control (`ifVersion`) using native
  `bigint`, without IEEE-754 precision loss.
- **Streaming**: Keyset-paginated async iterators, Node.js `Readable` streams, and WHATWG
  `ReadableStream`s.
- **Real-Time CDC**: Live change subscriptions (`db.watch()`) with namespace/space
  filtering across all models.
- **Atomic Multi-Model Batches**: Apply KV, JSON, Graph, FTS, and Vector mutations in a
  single ACID commit (`db.batch()`).
- **Cancellation & Synchronous Access**: `AbortSignal` on all async operations, plus a
  zero-async-overhead embedded API via `db.sync`.

---

## 2. Installation

```bash
# pnpm
pnpm add @liliadb/node

# npm
npm install @liliadb/node

# Bun
bun add @liliadb/node

# Deno 2
deno add npm:@liliadb/node
```

> **Native addons**: Prebuilds are published for `darwin-arm64`, `darwin-x64`,
> `linux-arm64-gnu`, `linux-x64-gnu`, and `win32-x64-msvc`. Set `LILIA_NATIVE_PATH` to
> load a locally built addon instead.

---

## 3. Quick Start

```typescript
import { Database } from "@liliadb/node";

const db = await Database.open({
  path: "./app.lilia",
  durability: "durable", // "durable" | "balanced" | "performance"
});

// 1. Key/Value (binary safe, UTF-8 strings accepted for convenience)
const enc = new TextEncoder();
const dec = new TextDecoder();
await db.kv.set("session", "tok_abc", enc.encode("user_42"));
const entry = await db.kv.get("session", "tok_abc");
console.log("Session:", dec.decode(entry!.value), "version:", entry!.version);

// 2. JSON document store
await db.json.put("users", "usr_101", { name: "Alice", role: "admin" });
const user = await db.json.get("users", "usr_101");
console.log("User:", user?.value);

// 3. Embedded graph
await db.graph.setNode("social", "usr_101", "User", { name: "Alice" });
await db.graph.setNode("social", "usr_102", "User", { name: "Bob" });
await db.graph.addEdge("social", "usr_101", "usr_102", "FOLLOWS");
console.log("Friends:", await db.graph.neighbors("social", "usr_101", { direction: "outgoing" }));

// 4. Full-text search
await db.fts.index("articles", "art_1", "LiliaDB delivers local-first multimodel storage.");
console.log(await db.fts.search("articles", "multimodel storage"));

// 5. Vector search
await db.vector.set("catalog", "item_1", [0.15, -0.42, 0.91, 0.05]);
console.log(await db.vector.search("catalog", [0.12, -0.40, 0.88, 0.02], { limit: 5 }));

// 6. Declarative SQL
const admins = await db.sql.query("SELECT id, value FROM lilia_json WHERE space = 'users'");
console.log("Admins:", admins);

// 7. Clean shutdown
await db.close();
```

---

## 4. Opening a Database

```typescript
interface OpenOptions {
  path: string;
  mode?: "embedded" | "daemon";
  durability?: "durable" | "balanced" | "performance";
  plugins?: readonly ("kv" | "json")[];
  endpoint?: string;
  tokenFile?: string;
  busyTimeoutMs?: number;
  writerQueueCapacity?: number;
  readPoolSize?: number;
  checkpointPolicy?: { walBytes?: number; intervalMs?: number };
  encryptionKey?: string;
  signal?: AbortSignal;
}
```

| Option | Description |
| --- | --- |
| `path` | **Required.** Database file path. |
| `mode` | `"embedded"` (default) or `"daemon"`. |
| `durability` | SQLite durability profile. |
| `plugins` | Enabled plugins. The v0.1 distribution requires both `kv` and `json` (default). |
| `endpoint` | Daemon IPC endpoint. Defaults to `${path}.sock`. |
| `tokenFile` | Daemon token file. Defaults to `${path}.token`. |
| `busyTimeoutMs` | SQLite busy timeout. |
| `writerQueueCapacity` | Native writer queue capacity. |
| `readPoolSize` | Number of native read connections. |
| `checkpointPolicy` | WAL checkpoint thresholds (`walBytes`, `intervalMs`), integers in `1..=4294967295`. |
| `encryptionKey` | Key used by the encryption codec at rest. |
| `signal` | Cancels opening. |

### Embedded Mode (Default)

Runs in-process. SQLite WAL work executes on background threads without blocking the
JavaScript event loop.

```typescript
const db = await Database.open({
  path: "app.lilia",
  durability: "durable",
  busyTimeoutMs: 5000,
  writerQueueCapacity: 64,
  readPoolSize: 4,
  checkpointPolicy: {
    walBytes: 4 * 1024 * 1024, // 4 MB
    intervalMs: 1000,
  },
});
```

### Daemon Mode

Connects to an isolated `lilia-daemon` process over length-prefixed MessagePack on local
IPC.

```typescript
const db = await Database.open({
  mode: "daemon",
  path: "app.lilia",
  endpoint: "app.lilia.sock", // or \\.\pipe\lilia on Windows
  tokenFile: "app.lilia.token",
});
```

---

## 5. Key/Value Model (`db.kv`)

Keys and values are binary `Uint8Array` slices. `string` inputs are UTF-8 encoded
automatically for keys, values, and `scan({ after })` cursors.

```typescript
const enc = new TextEncoder();
const dec = new TextDecoder();

// Put with optimistic concurrency and a millisecond TTL
const write = await db.kv.set(
  "sessions",
  enc.encode("sess_01"),
  enc.encode("active"),
  {
    ifVersion: 0n,                      // insert only when absent
    expiresAtMs: Date.now() + 3600_000, // 1 hour TTL
  },
);

// Get
const entry = await db.kv.get("sessions", enc.encode("sess_01"));
if (entry) {
  console.log(dec.decode(entry.value), entry.version, entry.expiresAtMs);
}

// Delete (optionally conditional)
await db.kv.delete("sessions", enc.encode("sess_01"), { ifVersion: write.version });

// Bounded in-memory scan
const entries = await db.kv.scan("sessions", { limit: 100 });
```

`KvEntry` is `{ namespace, key: Uint8Array, value: Uint8Array, version: bigint, expiresAtMs?: number }`.
`MutationResult` is `{ version?: bigint; deleted: boolean }`.

---

## 6. JSON Document Model (`db.json`)

```typescript
const put = await db.json.put("customers", "cust_100", {
  name: "Grace Hopper",
  plan: "enterprise",
  credits: 5000,
  tags: ["vip", "core"],
}, { ifVersion: 0n });

const doc = await db.json.get<{ name: string }>("customers", "cust_100");
console.log(doc?.value.name, doc?.version);

await db.json.delete("customers", "cust_100", { ifVersion: put.version });

const list = await db.json.scan("customers", { limit: 50 });
```

`JsonEntry<T>` is `{ space: string, id: string, value: T, version: bigint }`.

### Dynamic Secondary Indexes

`createIndex` backfills existing documents and indexes array elements (multikey).

```typescript
// Field name; JSON path defaults to $.field
await db.json.createIndex("customers", "plan");

// Explicit JSON path
await db.json.createIndex("customers", "email", "$.email");

// Named index
await db.json.createIndex("customers", "email", { name: "idx_custom_email", path: "$.email" });

// List and drop
const indexes = await db.json.listIndexes("customers");
// [{ space, indexName, jsonPath }]
await db.json.dropIndex("customers", "idx_custom_email");
```

### Querying

```typescript
// One or more fields (AND). Index-accelerated when possible, otherwise scans.
const enterprise = await db.json.find("customers", { plan: "enterprise" }, { limit: 10 });
const activeEnterprise = await db.json.find("customers", { plan: "enterprise", active: true });

// Multikey match: documents whose `tags` array contains "vip"
const vip = await db.json.find("customers", { tags: "vip" });

// Exact lookup by index name
const byEmail = await db.json.findByIndex("customers", "idx_custom_email", "grace@example.com");
```

`limit` defaults to `100` for scans and finds. `findByIndex` accepts
`string | number | boolean` values.

---

## 7. Graph Engine (`db.graph`)

```typescript
await db.graph.setNode("social", "user_1", "User", { name: "Alice" });
await db.graph.setNode("social", "user_2", "User", { name: "Bob" });
await db.graph.addEdge("social", "user_1", "user_2", "FOLLOWS");

const neighbors = await db.graph.neighbors("social", "user_1", {
  direction: "outgoing", // "outgoing" | "incoming" | "both"
  kind: "FOLLOWS",
});

const network = await db.graph.reachability("social", ["user_1"], {
  direction: "outgoing",
});
// includes the starting nodes and every reachable node
```

Node and edge deletion is performed through `db.batch()` using `graph_delete_node` and
`graph_remove_edge`.

---

## 8. Full-Text Search (`db.fts`)

SQLite FTS5 with BM25 scoring and snippet extraction.

```typescript
await db.fts.index("articles", "art_1", "LiliaDB delivers unified SQLite multimodel storage.");

const hits = await db.fts.search("articles", "multimodel storage", { limit: 5 });
for (const hit of hits) {
  console.log(hit.docId, hit.rank, hit.snippet);
}
```

`FtsHit` is `{ collection, docId, snippet, rank }`. Remove an indexed document with the
`fts_delete` batch operation.

---

## 9. Vector Search (`db.vector`)

Native HNSW-backed vector storage and approximate nearest neighbor search.

```typescript
// Dimension inferred from the array for Float32
await db.vector.set("catalog", "item_1", [0.15, -0.42, 0.91, 0.05]);

// Explicit dimension and element type
await db.vector.set("catalog", "item_2", new Float32Array([0.1, 0.2, 0.3]), {
  dim: 3,
  elementType: "float32", // "float32" | "int8" | "bit"
});

// Raw encoded bytes, or null when missing
const raw: Uint8Array | null = await db.vector.get("catalog", "item_1");

// ANN search
const hits = await db.vector.search("catalog", [0.12, -0.40, 0.88, 0.02], {
  metric: "cosine", // "cosine" | "l2" | "dot" | "hamming"
  limit: 5,
});
for (const hit of hits) {
  console.log(hit.id, hit.distance);
}

await db.vector.delete("catalog", "item_2");
```

`VectorHit` is `{ collection, id, distance }`. `metric` defaults to `"cosine"` and
`limit` to `10`. For `int8` and `bit` inputs supplied as a raw `Uint8Array`, pass `dim`
explicitly.

### Quantization Helpers

```typescript
import { quantization } from "@liliadb/node";

const floats = [0.15, -0.42, 0.91, 0.05];

// Int8 scalar quantization -> { quantized: Int8Array, scale: number }
const { quantized, scale } = quantization.toInt8(floats);

// 1-bit binary packing -> Uint8Array
const packed = quantization.toBit(floats);

// Cosine distance, clamped to [0, 2]
const distance = quantization.cosineDistance(floats, [0.10, -0.40, 0.90, 0.0]);
```

---

## 10. Hybrid Search (`db.hybrid`)

Reciprocal Rank Fusion (RRF) of BM25 text ranking and vector distance in one call.
Provide `query`, `vector`, or both.

```typescript
const hits = await db.hybrid.search("catalog", {
  query: "wireless headphones",
  vector: [0.12, -0.40, 0.88, 0.02],
  metric: "cosine",
  rrfK: 60,
  ftsWeight: 1.0,
  vectorWeight: 1.0,
  limit: 10,
});

for (const hit of hits) {
  console.log(
    hit.id,
    hit.rrfScore,
    hit.ftsRank,
    hit.ftsSnippet,
    hit.vectorRank,
    hit.vectorDistance,
  );
}
```

`HybridHit` is `{ id, rrfScore, ftsRank?, ftsSnippet?, vectorRank?, vectorDistance? }`.
Defaults: `rrfK` `60`, `ftsWeight` `1.0`, `vectorWeight` `1.0`, `limit` `10`.

---

## 11. Synchronous Embedded API (`db.sync`)

Direct, Promise-free accessors for embedded workloads. Available only in embedded mode;
using it in daemon mode or after `close()` throws.

```typescript
const enc = new TextEncoder();

// KV
const write = db.sync.kv.set("cache", "k1", enc.encode("v1"));
const val = db.sync.kv.get("cache", "k1");
const all = db.sync.kv.scan("cache", { limit: 100 });
const deleted = db.sync.kv.delete("cache", "k1");

// JSON
db.sync.json.put("users", "u1", { name: "Rayane" });
const user = db.sync.json.get("users", "u1");
const docs = db.sync.json.scan("users");

// Atomic batch
const results = db.sync.batch([
  { model: "kv_set", namespace: "cache", key: enc.encode("k2"), value: enc.encode("v2") },
  { model: "json_put", space: "users", id: "u2", value: { name: "Bob" } },
]);

// SQL
const rows = db.sync.sql.query("SELECT * FROM lilia_json WHERE space = ?1", ["users"]);
const { rowsAffected } = db.sync.sql.execute(
  "UPDATE _lilia_json SET version = version + 1 WHERE space = ?1",
  ["users"],
);
const raw = db.sync.sql.raw("SELECT id FROM lilia_json WHERE space = ?1", ["users"]);

// Tagged template
const tagged = db.sync.sql<{ id: string }>`
  SELECT id FROM lilia_json WHERE json_extract(value, '$.role') = ${"admin"}
`;
```

`db.sync` exposes `kv` (`get`, `set`, `delete`, `scan`), `json` (`get`, `put`, `delete`,
`scan`), `batch`, and `sql` (`query`, `execute`, `raw`, tagged template). It does not
expose graph, FTS, vector, or watch operations.

---

## 12. Declarative SQL (`db.sql`)

SQLite views over multimodel storage. The scalar functions `lilia_decode_kv` and
`lilia_decode_json` transparently decrypt values when an encryption codec is enabled.

| View | Columns |
| --- | --- |
| `lilia_json` | `(space, id, value, version)` — `value` is decoded plaintext JSON |
| `lilia_kv` | `(namespace, key, value, version, expires_at_ms)` — `value` is decoded bytes |
| `lilia_graph_nodes` | `(graph, id, kind, metadata)` |
| `lilia_graph_edges` | `(graph, source, target, kind)` |

### Typed Queries (`db.sql.query`)

```typescript
interface UserRow {
  id: string;
  name: string;
  role: string;
}

const users = await db.sql.query<UserRow>(
  `SELECT id,
          json_extract(value, '$.name') AS name,
          json_extract(value, '$.role') AS role
   FROM lilia_json
   WHERE space = ?1`,
  ["users"],
);
```

### Tagged Templates

`db.sql` and `db.sync.sql` are callable as tagged template literals. Interpolated values
are bound as numbered parameters (`?1`, `?2`, ...):

```typescript
const role = "admin";
const rows = await db.sql<{ id: string; name: string }>`
  SELECT id, json_extract(value, '$.name') AS name
  FROM lilia_json
  WHERE space = 'users' AND json_extract(value, '$.role') = ${role}
`;
```

### Raw Projections (`db.sql.raw`)

```typescript
const raw = await db.sql.raw(
  "SELECT id, json_extract(value, '$.role') AS role FROM lilia_json WHERE space = ?1",
  ["users"],
);
raw.columns;      // ["id", "role"]
raw.rows;         // [["ada", "admin"], ...]
raw.rowsAffected; // 0 for SELECT
```

### Mutations (`db.sql.execute`)

```typescript
const { rowsAffected } = await db.sql.execute(
  "UPDATE _lilia_json SET version = version + 1 WHERE space = ?1",
  ["users"],
);
```

`query`, `raw`, and `execute` accept `(sql, params?, { signal? })`. Mutations are routed
through the write queue; write to the underlying `_lilia_*` tables (for example
`_lilia_json`).

---

## 13. Streaming & Node.js Streams

All streaming APIs use transparent keyset pagination and are memory-bounded.

### Async Iteration (`for await`)

```typescript
// JSON documents with automatic chunking
for await (const doc of db.json.scanStream("events", { batchSize: 200, limit: 1000 })) {
  console.log(doc.id, doc.value);
}

// Key/Value pairs
for await (const entry of db.kv.scanStream("analytics", { batchSize: 500 })) {
  console.log(entry.key, entry.value);
}

// Filtered JSON queries
for await (const doc of db.json.findStream("users", { role: "admin" }, { batchSize: 50 })) {
  console.log(doc.id);
}
```

`scanStream` and `stream` are aliases. Options are
`{ after?, limit?, batchSize?, signal? }`; `batchSize` defaults to `100`. Breaking out of
the loop stops further fetching.

### Resume from a Cursor (`after`)

```typescript
for await (const doc of db.json.scanStream("events", { after: "event_0500", batchSize: 100 })) {
  console.log(doc.id);
}
```

### Node.js Streams (`createReadStream`)

```typescript
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

### WHATWG Streams (`readableStream`)

```typescript
const stream = db.json.readableStream("records", { batchSize: 100 });
for await (const doc of stream) {
  console.log(doc.id);
}
```

Streaming surface summary:

| Namespace | Async iterator | WHATWG stream | Node.js stream |
| --- | --- | --- | --- |
| `db.kv` | `scanStream`, `stream` | `readableStream` | `createReadStream` |
| `db.json` | `scanStream`, `stream` | `readableStream` | `createReadStream` |
| `db.json` (filtered) | `findStream` | `findReadableStream` | — |

---

## 14. Real-Time CDC (`db.watch`)

Subscribe to live mutations across KV, JSON, Graph, FTS, and Vector collections.

```typescript
const sub = await db.watch("orders", (event) => {
  console.log(`[CDC] ${event.type}`, event);
});

// Mutate data -> triggers the callback
await db.json.put("orders", "ord_100", { total: 49.99 });

// Cleanup
await sub.unsubscribe();
```

- `db.watch(namespace?, callback, options?)` observes everything when no namespace is
  given.
- `db.kv.watch(namespace, callback, options?)` and
  `db.json.watch(space, callback, options?)` are scoped aliases.
- Pass `{ signal }` to unsubscribe automatically on abort.

Event variants:

| `type` | Fields |
| --- | --- |
| `kv_mutated` | `namespace`, `key`, `version?`, `deleted` |
| `json_mutated` | `space`, `id`, `version?`, `deleted` |
| `graph_node_mutated` | `graph`, `id`, `deleted` |
| `graph_edge_mutated` | `graph`, `source`, `target`, `kind`, `deleted` |
| `fts_mutated` | `collection`, `docId`, `deleted` |
| `vector_mutated` | `collection`, `id`, `deleted` |

A `WatchSubscription` is a callable unsubscribe function and also provides
`unsubscribe()`, `Symbol.dispose`, and `Symbol.asyncDispose`.

---

## 15. Atomic Multi-Model Transactions (`db.batch`)

Combine operations across models into a single ACID commit:

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

Supported models:

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

## 16. Version Precision & OCC

- Versions returned by reads, scans, and mutations are always `bigint`.
- `ifVersion` must be a `bigint` in `0..=18446744073709551615`; `0n` means
  *the entry does not exist*.
- Out-of-bounds versions throw `LiliaError` with code `INVALID_INPUT`.
- A stale `ifVersion` fails with a conflict and changes nothing.

---

## 17. Cancellation (`AbortSignal`)

All asynchronous methods and streams accept `{ signal }`. An already-aborted signal
rejects immediately; aborting later rejects the pending Promise or ends the iterator.

```typescript
const controller = new AbortController();
const pending = db.json.scan("events", { signal: controller.signal });
controller.abort();
await pending; // rejects with the signal reason
```

Signals are also accepted by `Database.open()`, `db.close()`, and `db.watch()`.

---

## 18. Error Handling

Every SDK operation throws a structured `LiliaError` on failure:

```typescript
import { LiliaError } from "@liliadb/node";

try {
  await db.json.put("users", "ada", { status: "updated" }, { ifVersion: 99n });
} catch (error) {
  if (LiliaError.isLiliaError(error)) {
    console.error(`Code: ${error.code}`);          // "CONFLICT", "CLOSED", "TIMEOUT", "INVALID_INPUT", ...
    console.error(`Message: ${error.message}`);
    console.error(`Retryable: ${error.retryable}`); // true for transient failures
    console.error(`RequestId: ${error.requestId}`);
    console.error(`Details: ${JSON.stringify(error.details)}`);
    console.error(`Cause: ${error.cause}`);
  }
}
```

`LiliaError` extends `Error` with `code`, `retryable`, `details`, `requestId`, and
`cause`. Use `instanceof LiliaError` or the static `LiliaError.isLiliaError(value)`.

---

## 19. Lifecycle & Graceful Shutdown

```typescript
await db.close({ timeoutMs: 5_000 });

// SQLite integrity check
const ok: boolean = await db.integrityCheck();

// Atomic online backup
await db.backup("./snapshots/app.lilia");
```

- `close()` drains accepted in-flight operations, then releases locks and workers.
- `timeoutMs` defaults to `5_000` and must be an integer in `0..=2147483647`.
- A timeout rejects with `TIMEOUT` (`retryable: true`) without cancelling accepted work.
- Operations started after `close()` reject with `CLOSED`.
- `close()` is idempotent and can be awaited again.

### Explicit resource management

```typescript
await using db = await Database.open({ path: "app.lilia" });
await db.json.put("users", "ada", { name: "Ada" });
// db.close() runs automatically when the scope exits

using sub = await db.watch("orders", (event) => console.log(event));
// sub.unsubscribe() runs automatically when the scope exits
```

---

## 20. Live Differential Queries (`db.liveQuery`)

Subscribe to SQL query results with granular, row-level diffs powered by the SQLite Session API:

```typescript
const sub = await db.liveQuery(
  "SELECT id, json_extract(value, '$.status') AS status FROM lilia_json WHERE space = 'orders'",
  (diff) => {
    console.log("Initial load:", diff.isInitial);
    console.log("Added rows:", diff.added);
    console.log("Updated rows:", diff.updated);
    console.log("Removed rows:", diff.removed);
  }
);

// Unsubscribe when done
await sub.unsubscribe();
```

---

## 21. Materialized Views & IVM (`db.createMaterializedView`)

Incremental View Maintenance computes and maintains aggregates in $O(1)$ time without rescanning full source tables:

```typescript
// Create an incrementally updated materialized view
await db.createMaterializedView(
  "order_stats",
  "SELECT json_extract(value, '$.status') AS status, COUNT(*) AS total, SUM(json_extract(value, '$.total')) AS revenue FROM lilia_json WHERE space = 'orders' GROUP BY status"
);

// List views
const views = await db.listMaterializedViews();

// Query the view directly via SQL
const stats = await db.sql.query("SELECT * FROM _lilia_ivm_order_stats");

// Drop view
await db.dropMaterializedView("order_stats");
```

---

## 22. Time Travel & Undo / Redo (`db.undo`, `db.redo`)

Track transactional change history and revert or replay mutations:

```typescript
// Undo the last mutation batch
const undone = await db.undo();
console.log("Reverted transaction:", undone?.batchId);

// Redo the reverted mutation
const redone = await db.redo();

// Inspect undo history
const history = await db.undoHistory();
for (const entry of history) {
  console.log(`[${entry.timestampMs}] ${entry.model} op on ${entry.id}`);
}
```

---

## 23. Conflict-Free Replicated Data Types (CRDTs)

Distributed primitives that merge deterministically across edge nodes:

```typescript
// Distributed PN-Counter
await db.crdt.pnCounter("counters", "visitors").increment(1n, "node-1");
await db.crdt.pnCounter("counters", "visitors").decrement(1n, "node-2");
const total = await db.crdt.pnCounter("counters", "visitors").read();

// Observed-Remove Set (OR-Set)
await db.crdt.orSet("tags", "doc-1").add("database", "node-1");
await db.crdt.orSet("tags", "doc-1").remove("draft", "node-2");
const tags = await db.crdt.orSet("tags", "doc-1").read();

// Last-Write-Wins (LWW) JSON Document
await db.crdt.doc("profiles", "user-1").setField("email", "ada@example.com", "node-1");
const profile = await db.crdt.doc("profiles", "user-1").read();
```

---

## 24. GPS Tracking & Geofencing (`db.geo`)

High-precision spatial boundaries and GPS trajectory analysis:

```typescript
// Register a circular geofence (lat, lon, radius in meters)
await db.geo.addFence("warehouse", {
  kind: "circle",
  center: [48.8566, 2.3522],
  radiusMeters: 500,
});

// Test a point
const matches = await db.geo.testPoint([48.8570, 2.3530]);
console.log("Inside fences:", matches);

// Evaluate GPS track breach events
const track = [
  { lat: 48.8560, lon: 2.3520, timestampMs: 1700000000000 },
  { lat: 48.8600, lon: 2.3600, timestampMs: 1700000060000 },
];
const breaches = await db.geo.evaluateGpsTrack("device-1", track);
```

---

## 25. Time-Series Telemetry & Downsampling (`db.ts`)

Telemetry streaming with Gorilla compression and aggregation:

```typescript
// Append points to a time-series
await db.ts.append("sensors", [
  { timestampMs: Date.now(), value: 24.5 },
  { timestampMs: Date.now() + 1000, value: 24.8 },
]);

// Query range with Gorilla decompression
const points = await db.ts.queryRange("sensors", {
  fromMs: Date.now() - 3600_000,
  toMs: Date.now(),
});

// Downsample into 5-minute buckets
const rollups = await db.ts.downsample("sensors", {
  windowMs: 300_000,
  aggregation: "avg",
});
```

---

## 26. Type Guards & Public Types

```typescript
import { isKvEntry, isJsonEntry, LiliaError } from "@liliadb/node";

if (isKvEntry(value)) console.log(value.version);
if (isJsonEntry(value)) console.log(value.id);
if (LiliaError.isLiliaError(error)) console.log(error.code);
```

Exported types include: `OpenOptions`, `DatabaseMode`, `Durability`, `KvEntry`,
`JsonEntry`, `JsonIndexInfo`, `CreateIndexOptions`, `JsonGetOptions`, `JsonPutOptions`,
`JsonDeleteOptions`, `JsonScanOptions`, `JsonFindOptions`, `KvGetOptions`,
`KvSetOptions`, `KvDeleteOptions`, `KvScanOptions`, `ScanStreamOptions`, `WatchOptions`,
`WatchSubscription`, `CloseOptions`, `GraphDirection`, `FtsHit`, `VectorElementType`,
`VectorMetric`, `VectorHit`, `HybridHit`, `BatchOperation`, `DatabaseEvent`,
`MutationResult`, `SqlQueryResult`, `SqlQueryOptions`, `SqlFunction`,
`SyncSqlFunction`, `LiliaErrorShape`, and the `quantization` helper object.

---

## 27. License

Apache-2.0
