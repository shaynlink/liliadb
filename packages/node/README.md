# @liliadb/node

Official TypeScript and Node.js SDK for **LiliaDB**, the local-first multimodel embedded
and daemon database written in Rust.

---

## 1. Key Features

- **Embedded & Daemon Modes**: In-process execution through native Node-API bindings, or
  connection to a local `lilia-daemon` over Unix domain sockets / Windows named pipes.
- **Multimodel Engine**: Key/Value, JSON documents with secondary indexes, graphs and their
  algorithms (paths, PageRank, centralities, cycles, components, Cypher), full-text search,
  vector search (HNSW), hybrid RRF search, time series, geofences and GPS tracks, CRDTs, and
  read-only SQL (DataFusion) over every model.
- **Multi-Runtime Support**: Node.js `>=22`, Bun `>=1.0`, and Deno `>=2.0`.
- **Exact 64-bit Versioning**: Optimistic concurrency control (`ifVersion`) using native
  `bigint`, without IEEE-754 precision loss.
- **Streaming**: Keyset-paginated async iterators, Node.js `Readable` streams, and WHATWG
  `ReadableStream`s.
- **History**: reads at a past version or time, document revisions and diffs, undo and redo.
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
  path: "./app-data", // a directory, created if missing
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

// 6. Read-only SQL over the models
const admins = await db.sql.query("SELECT id, value->>'name' AS name FROM users WHERE json_get_str(value, 'role') = $1", ["admin"]);
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
  profile?: "server" | "mobile" | "constrained";
  ledger?: boolean;
  historyRetentionMs?: number;
  undoDepth?: number;
  endpoint?: string;
  tokenFile?: string;
  signal?: AbortSignal;
}
```

| Option | Description |
| --- | --- |
| `path` | **Required.** Directory of the database, created if missing. |
| `mode` | `"embedded"` (default) or `"daemon"`. |
| `durability` | `"durable"` (default): every acknowledged commit survives power loss; `"balanced"`: the journal stays consistent, the latest commits may be lost; `"performance"`: no sync on commit. |
| `profile` | Buffers sized for the device: `"server"` (default) from the whole machine, `"mobile"` from a quarter of its memory, `"constrained"` from a sixteenth. |
| `ledger` | Keep a tamper-evident ledger of every record write. A database that keeps one must always be opened with it. |
| `historyRetentionMs` | How long old versions stay readable (`getAtVersion`, `getAtTimestamp`, `history`); a week by default, `0` keeps none. |
| `undoDepth` | Commits `db.undo()` can undo; `0` (default) makes commits not undoable. |
| `endpoint` | Daemon IPC endpoint. Defaults to `${path}.sock`. |
| `tokenFile` | Daemon token file. Defaults to `${path}.token`. |
| `signal` | Cancels opening. |

### Embedded Mode (Default)

Runs in-process through the native addon. Each call runs on a worker thread, off the
JavaScript event loop; `db.sync` runs the same calls on the caller's thread.

```typescript
const db = await Database.open({
  path: "app-data",
  durability: "balanced",
  profile: "mobile",
  undoDepth: 32,
});
```

### Daemon Mode

Connects to an isolated `lilia-daemon` process over local IPC: the connection opens with the
JS preface (`LJS1`), then sends MessagePack requests and receives JS frames (one JSON text for
`JSON.parse`, then raw bytes), the same results the native addon returns. Embedded and
daemon databases run the same operations; only the synchronous `db.sync` API needs an embedded
database.

```typescript
const db = await Database.open({
  mode: "daemon",
  path: "app-data",
  endpoint: "app-data.sock", // or \\.\pipe\lilia on Windows
  tokenFile: "app-data.token",
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
// Terms combined with AND; a key is a field path (`plan`, `$.user.city`) or an index name.
// An index on a term serves the query; without one, the space is scanned.
const enterprise = await db.json.find("customers", { plan: "enterprise" }, { limit: 10 });
const activeEnterprise = await db.json.find("customers", { plan: "enterprise", active: true });

// Multikey match: documents whose `tags` array contains "vip" (index "tags" on $.tags)
const vip = await db.json.find("customers", { tags: "vip" });

// Exact lookup by index name
const byEmail = await db.json.findByIndex("customers", "idx_custom_email", "grace@example.com");
```

`limit` defaults to `100` for scans and finds; `0` reads everything. `findByIndex` accepts
`string | number | boolean` values. Create an index on the fields you query often.

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

Nodes link to documents (`setNode(..., { docSpace, docId })`, read with `getNodeDocument`).
Algorithms run over the whole graph: `shortestPath`, `dijkstra`, `allShortestPaths`,
`astarSpatial`, `pagerank`, `centrality` (`"betweenness" | "closeness" | "degree"`),
`hasCycle`, `findCycles`, `connectedComponents`, `impactAnalysis`, and `cypher` for a subset
of Cypher. `deleteNode`, `removeEdge`, `listNodes` and `listEdges` manage the graph.

---

## 8. Full-Text Search (`db.fts`)

BM25 ranking with snippets, typo-tolerant search and suggestions.

```typescript
await db.fts.index("articles", "art_1", "LiliaDB delivers local-first multimodel storage.");

const hits = await db.fts.search("articles", "multimodel storage", { limit: 5 });
for (const hit of hits) {
  console.log(hit.docId, hit.rank, hit.snippet);
}
```

`FtsHit` is `{ collection, docId, snippet, rank }`. `db.fts.delete` removes a document;
`suggest(collection, prefix)`, `fuzzySearch(collection, query, { maxDistance })` and
`phoneticSearch(collection, query)` tolerate typos; `loadDictionary(collection, words)` adds
words the suggestions draw from.

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
const rows = db.sync.sql.query("SELECT id FROM lilia_json WHERE space = $1", ["users"]);
const raw = db.sync.sql.raw("SELECT id FROM users");

// Tagged template
const tagged = db.sync.sql<{ id: string }>`
  SELECT id FROM users WHERE json_get_str(value, 'role') = ${"admin"}
`;
```

`db.sync` has every call of the asynchronous API (models, `batch`, `undo`, checks, `sql`),
without streams. Options take no `signal`: the call runs to completion on the caller's
thread.

---

## 12. Declarative SQL (`db.sql`)

Read-only SQL (the DataFusion dialect) over tables of every model, through the caller's
policies. Parameters are numbered: `$1`, `$2`, ...

| Table | Columns |
| --- | --- |
| `lilia_json` | `space`, `id`, `value` (JSON text), `version` |
| `lilia_kv` | `namespace`, `key`, `value` (bytes), `version`, `expires_at_ms` |
| `lilia_graph_nodes` | `graph`, `id`, `kind`, `metadata` (JSON text) |
| `lilia_graph_edges` | `graph`, `source`, `target`, `kind`, `weight` |
| `lilia_timeseries` | `series`, `timestamp`, `value` |
| `<space>` | one table per JSON space: `id`, `value`, `version` |

JSON is read with `value->>'field'`, `json_get_str(value, 'field')`, `json_get_int`,
`json_get_float` and `json_get_bool`.

### Typed Queries (`db.sql.query`)

```typescript
interface UserRow {
  id: string;
  name: string;
}

const users = await db.sql.query<UserRow>(
  "SELECT id, value->>'name' AS name FROM lilia_json WHERE space = $1",
  ["users"],
);
```

### Tagged Templates

`db.sql` and `db.sync.sql` are callable as tagged template literals. Interpolated values
are bound as parameters, never spliced into the text:

```typescript
const role = "admin";
const rows = await db.sql<{ id: string; name: string }>`
  SELECT id, value->>'name' AS name FROM users WHERE json_get_str(value, 'role') = ${role}
`;
```

### Raw Projections (`db.sql.raw`)

```typescript
const raw = await db.sql.raw("SELECT id, value->>'role' AS role FROM users");
raw.columns; // ["id", "role"]
raw.rows;    // [["ada", "admin"], ...]
```

`query` and `raw` accept `(sql, params?, { signal? })`. Writes go through the models and
`db.batch`.

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

## 14. Atomic Multi-Model Transactions (`db.batch`)

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
| `graph_set_node` | `graph`, `id`, `kind`, `metadata?`, `docSpace?`, `docId?` |
| `graph_delete_node` | `graph`, `id` |
| `graph_add_edge` | `graph`, `source`, `target`, `kind`, `weight?` |
| `graph_remove_edge` | `graph`, `source`, `target`, `kind` |
| `fts_index` | `collection`, `docId`, `content` |
| `fts_delete` | `collection`, `docId` |
| `vector_set` | `collection`, `id`, `dim`, `elementType?`, `vector` |
| `vector_delete` | `collection`, `id` |
| `crdt_pn_counter_increment`, `crdt_pn_counter_decrement` | `space`, `id`, `actorId`, `amount` |
| `crdt_or_set_add` | `space`, `id`, `actorId`, `element` |
| `crdt_or_set_remove` | `space`, `id`, `element`, `timestampMs?` |
| `crdt_json_set_field` | `space`, `id`, `actorId`, `path`, `value`, `timestampMs?` |
| `crdt_json_delete_field` | `space`, `id`, `actorId`, `path`, `timestampMs?` |
| `crdt_json_merge` | `space`, `id`, `state` (as `crdt.json.state` returns it) |

If any operation fails, the whole batch rolls back and the call rejects.

---

## 15. Version Precision & OCC

- Versions returned by reads, scans, and mutations are always `bigint`. A version is the
  engine sequence of the write: it grows with every write of the database, not per record.
- `ifVersion` must be a `bigint` in `0..=18446744073709551615`; `0n` means
  *the entry does not exist*.
- Out-of-bounds versions throw `LiliaError` with code `INVALID_INPUT`.
- A stale `ifVersion` fails with a conflict and changes nothing.

---

## 16. Cancellation (`AbortSignal`)

All asynchronous methods and streams accept `{ signal }`. An already-aborted signal
rejects immediately; aborting later rejects the pending Promise or ends the iterator.

```typescript
const controller = new AbortController();
const pending = db.json.scan("events", { signal: controller.signal });
controller.abort();
await pending; // rejects with the signal reason
```

Signals are also accepted by `Database.open()` and `db.close()`. Aborting stops the wait,
not the operation: a write that started still commits.

---

## 17. Error Handling

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

## 18. Lifecycle & Graceful Shutdown

```typescript
await db.close({ timeoutMs: 5_000 });

// Reads every stored entry (block checksums) and verifies the ledger, if any
const ok: boolean = await db.integrityCheck();
// Verifies the ledger only, fast
const quick: boolean = await db.quickCheck();
// Writes the memtable to a sorted run; describes the storage
await db.checkpoint();
const stats = await db.storageStats(); // { levels, memtableBytes, immutableMemtables, version, oldestVersion }
```

- `close()` drains accepted in-flight operations, then releases locks and workers.
- `timeoutMs` defaults to `5_000` and must be an integer in `0..=2147483647`.
- A timeout rejects with `TIMEOUT` (`retryable: true`) without cancelling accepted work.
- Operations started after `close()` reject with `CLOSED`.
- `close()` is idempotent and can be awaited again.

### Explicit resource management

```typescript
await using db = await Database.open({ path: "app-data" });
await db.json.put("users", "ada", { name: "Ada" });
// db.close() runs automatically when the scope exits
```

---

## 19. Time Travel & Undo / Redo (`db.undo`, `db.redo`)

Reads in the past, within the history retention (`historyRetentionMs`):

```typescript
const first = await db.json.put("docs", "d", { n: 1 });
await db.json.put("docs", "d", { n: 2 });

await db.json.getAtVersion("docs", "d", first.version!); // { n: 1 }
await db.json.getAtTimestamp("docs", "d", Date.now() - 60_000);
const revisions = await db.json.history("docs", "d"); // [{ version, document? }, ...]
const diff = await db.json.diff("docs", "d", first.version!, revisions.at(-1)!.version);
```

With `undoDepth > 0`, commits can be undone and redone, newest first:

```typescript
const undone = await db.undo(); // { version, keys } of the commit, or undefined
const redone = await db.redo();
const steps = await db.undoHistory();
```

---

## 20. Conflict-Free Replicated Data Types (CRDTs)

Replicated types that merge deterministically; each write names the actor that made it.

```typescript
// PN-Counter
await db.crdt.counter.increment("counters", "visitors", "node-1", 2);
await db.crdt.counter.decrement("counters", "visitors", "node-2");
const total = await db.crdt.counter.get("counters", "visitors"); // 1
const merged = await db.crdt.counter.merge("counters", "visitors", remoteState);

// Observed-Remove Set
await db.crdt.set.add("tags", "doc-1", "node-1", "database");
await db.crdt.set.remove("tags", "doc-1", "draft");
const tags = await db.crdt.set.read("tags", "doc-1");

// Last-Write-Wins JSON document
await db.crdt.json.setField("profiles", "user-1", "node-1", "email", "ada@example.com");
const profile = await db.crdt.json.get("profiles", "user-1");
const merged = await db.crdt.json.merge("profiles", "user-1", remoteDocument); // no version if nothing changed
```

`state(...)` returns the replicated state of each type, to send to another node and merge
there.

---

## 21. GPS Tracking & Geofencing (`db.geo`)

Polygon geofences and GPS tracks; polygons are JSON `[lon, lat]` rings.

```typescript
import { geo } from "@liliadb/node";

await db.geo.addFence("warehouse", "Warehouse", "site", geo.circleToGeopoly(2.3522, 48.8566, 500));
const inside = await db.geo.findFencesForPoint(2.353, 48.857); // [{ id, name, category }]

// Recording a track reports the fences its points enter
const entered = await db.geo.recordGpsPoints("device-1", [
  { timestamp: 1_700_000_000_000, latitude: 48.856, longitude: 2.352 },
  { timestamp: 1_700_000_060_000, latitude: 48.86, longitude: 2.36 },
]);
const points = await db.geo.getTrackPoints("device-1", { startTime: 1_700_000_000_000 });
const meters = await db.geo.trackDistance("device-1");
```

---

## 22. Time-Series Telemetry & Downsampling (`db.timeseries`)

```typescript
import { timeseries } from "@liliadb/node";

await db.timeseries.appendPoints("sensors", [
  { timestamp: Date.now(), value: 24.5 },
  { timestamp: Date.now() + 1000, value: 24.8 },
]);
const points = await db.timeseries.queryRange("sensors", Date.now() - 3_600_000, Date.now());
const stats = await db.timeseries.queryStats("sensors", Date.now() - 3_600_000, Date.now());
const chart = await db.timeseries.queryAuto("sensors", 0, Date.now(), 500);

// Lifecycle policies downsample closed buckets and drop expired points
await db.timeseries.setPolicy({
  seriesPattern: "sensors*",
  rules: [
    { resolutionMs: 0, retentionMs: 86_400_000, aggregation: "avg" },
    { resolutionMs: 60_000, retentionMs: null, aggregation: "avg" },
  ],
});
await db.timeseries.applyRetention();

// Client-side downsampling
const rollups = timeseries.downsample(points, 300_000, "avg");
```

---

## 23. Type Guards & Public Types

```typescript
import { isKvEntry, isJsonEntry, LiliaError } from "@liliadb/node";

if (isKvEntry(value)) console.log(value.version);
if (isJsonEntry(value)) console.log(value.id);
if (LiliaError.isLiliaError(error)) console.log(error.code);
```

Exported types include: `OpenOptions`, `DatabaseMode`, `Durability`, `KvEntry`,
`JsonEntry`, `JsonIndexInfo`, `CreateIndexOptions`, `JsonGetOptions`, `JsonPutOptions`,
`JsonDeleteOptions`, `JsonScanOptions`, `JsonFindOptions`, `KvGetOptions`,
`KvSetOptions`, `KvDeleteOptions`, `KvScanOptions`, `ScanStreamOptions`, `CloseOptions`,
`GraphDirection`, `FtsHit`, `FtsSuggestion`, `FtsFuzzyHit`, `FtsPhoneticHit`,
`VectorElementType`, `VectorMetric`, `VectorHit`, `HybridHit`, `BatchOperation`,
`MutationResult`, `JsonRevision`, `UndoStep`, `StorageStats`, `SqlQueryResult`,
`SqlQueryOptions`, `SqlFunction`, `SyncSqlFunction`, `LiliaErrorShape`, and the
`quantization`, `timeseries` and `geo` helper objects.

---

## 24. License

Apache-2.0
