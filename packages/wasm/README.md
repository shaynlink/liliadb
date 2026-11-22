# `@liliadb/wasm`

> Local-first multimodel database in WebAssembly: key/value, JSON documents, graph,
> full-text search, vectors, time series, geo, CRDTs and read-only SQL, on the native LiliaDB
> engine compiled to `wasm32`, persisted in the browser's origin private file system (OPFS)
> or kept in memory.

The calls are those of every LiliaDB JavaScript client (`@liliadb/client`): the same API as
`@liliadb/node` and `@liliadb/react-native`.

---

## How it works

- `crates/lilia-wasm` compiles the engine to WebAssembly. It runs without background
  threads: the writer that fills a memtable flushes it.
- On **OPFS**, a database is a pool of slot files in one directory (`lilia_io::OpfsPool`).
  Their synchronous access handles exist only in dedicated workers, and an open handle locks
  its file: one tab at a time opens a database. The pool grows before an operation when a
  quarter of its slots or fewer are free.
- In **memory**, the database lives as long as its module.
- Each call is one MessagePack operation; the module answers with a JS frame, one JSON text
  and the raw bytes it refers to, the exchange of the native bindings.
- From a page, the database runs in the package's worker: requests and frames cross by
  transfer, without copies. In a dedicated worker, or in memory, it runs in the calling
  thread, where `db.sync` runs the same calls synchronously.

---

## Installation

```sh
pnpm add @liliadb/wasm
# or
npm install @liliadb/wasm
```

The package ships the module in `pkg/` (`lilia_wasm_bg.wasm`, about 36 MB, 10 MB gzipped:
DataFusion's SQL is most of it). Serve it compressed.

---

## Quick start

### A page, with OPFS

```ts
import { Database } from "@liliadb/wasm";

const db = await Database.open({ path: "app" });   // runs in the package's worker

await db.kv.set("settings", "theme", "dark");
await db.json.put("users", "u1", { name: "Ada", age: 36 });
const adults = await db.json.find("users", { age: 36 });
const rows = await db.sql.query("SELECT id, value->>'name' AS name FROM users");

await db.close();
```

### A dedicated worker of the app's own

```ts
// worker.ts: the database runs here, with the synchronous API.
import { Database } from "@liliadb/wasm";

const db = await Database.open({ path: "app" });
const theme = db.sync.kv.get("settings", "theme");
```

### In memory

```ts
const db = await Database.open({ storage: "memory" });
```

Outside the browser (Node, Deno, Bun, edge runtimes), give the module's bytes, which `fetch`
does not read from `file:` URLs:

```ts
import { readFile } from "node:fs/promises";

const wasm = await readFile(new URL(import.meta.resolve("@liliadb/wasm/pkg/lilia_wasm_bg.wasm")));
const db = await Database.open({ storage: "memory", wasm });
```

On Node, `@liliadb/node` runs the engine natively, on the file system, with more threads.

---

## Opening a database

| Option | Description |
| --- | --- |
| `path` | The database's directory in the origin private file system; required there. |
| `storage` | `opfs` (persistent) or `memory`. `opfs` where the runtime has it, else `memory`. |
| `slots` | Slot files the OPFS pool starts with (64); it grows as the database does. |
| `worker` | Where the database runs. By default in the calling thread when it is a dedicated worker or the database is in memory, else in the package's worker. `true` asks for that worker, `false` for the calling thread; a `Worker`, or the URL of a script that calls `serve(self)`, runs it there. |
| `wasm` | The module: a URL, a `Response`, its bytes or a compiled `WebAssembly.Module`; `pkg/lilia_wasm_bg.wasm` next to the package by default. |
| `durability` | `durable` (default): every acknowledged commit survives power loss; `balanced`; `performance`: no sync on commit. |
| `profile` | Memory and cache budget: `mobile` (default here), `constrained`, `server`. |
| `ledger` | Keeps a tamper-evident ledger of every record write. |
| `historyRetentionMs` | How long old versions stay readable; a week by default, 0 keeps none. |
| `undoDepth` | Commits `undo` can undo; needs a history retention. |

### Workers and bundlers

The package's worker is `@liliadb/wasm/worker`, loaded as a module worker from
`new URL("./worker.js", import.meta.url)`. When a bundler or a Content Security Policy needs
the worker created elsewhere, pass it:

```ts
const worker = new Worker(new URL("@liliadb/wasm/worker", import.meta.url), { type: "module" });
const db = await Database.open({ path: "app", worker });
```

A worker of the app's own can serve a database to the page too:

```ts
// db-worker.ts
import { serve } from "@liliadb/wasm";
serve(self);
```

```ts
const db = await Database.open({ path: "app", worker: new URL("./db-worker.ts", import.meta.url) });
```

---

## Database API

The API of `@liliadb/node`, documented in its README, without the Node streams:

```ts
await db.kv.set("ns", "k", "v", { expiresAtMs: Date.now() + 60_000 });
for await (const document of db.json.scanStream("users")) { /* a page at a time */ }
await db.json.createIndex("users", "age");
await db.graph.addEdge("social", "u1", "u2", "follows");
await db.fts.index("notes", "n1", "local first storage");
const hits = await db.vector.search("embeddings", new Float32Array([0.1, 0.2, 0.3]), { limit: 5 });
await db.batch([
  { model: "kv_set", namespace: "ns", key: new TextEncoder().encode("a"), value: new Uint8Array([1]) },
  { model: "json_put", space: "orders", id: "o1", value: { total: 12 } },
]);
```

Also `hybrid`, `timeseries`, `geo`, `crdt`, `undo`/`redo`, `quickCheck`, `integrityCheck`,
`checkpoint` and `storageStats`. SQL is read-only DataFusion: parameters are `$1`, `$2`...,
JSON fields are read with `value->>'field'` and `json_get_*`, and every JSON space is also a
table.

`db.sync` exists when the database runs in the calling thread; through a worker it throws
`UNSUPPORTED`.

### Errors

Every failure is a `LiliaError` with a stable `code` (`CONFLICT`, `CLOSED`, `INVALID_INPUT`,
`UNSUPPORTED`, `DISK_FULL`...), `message` and `retryable`:

```ts
import { LiliaError } from "@liliadb/wasm";

try {
  await db.kv.set("ns", "k", "v", { ifVersion: 41n });
} catch (error) {
  if (error instanceof LiliaError && error.code === "CONFLICT") { /* read again, retry */ }
}
```

### Closing

`close()` waits for the calls already made, makes every acknowledged write durable, releases
the OPFS files (another tab may then open the database) and ends the package's worker; later
calls fail with `CLOSED`. `await using db = await Database.open(...)` closes at the end of the
scope.

---

## Building the module

```sh
tools/wasm/build.sh          # the `wasm` profile, optimized for size, into packages/wasm/pkg
tools/wasm/build.sh --debug  # faster to build
```

It needs the `wasm32-unknown-unknown` target, the `wasm-bindgen` CLI of the crate's version,
and a C compiler that targets wasm32 (Homebrew's LLVM on macOS, or
`CC_wasm32_unknown_unknown` and `AR_wasm32_unknown_unknown`). `pnpm --filter @liliadb/wasm
test` then runs the package's tests on an in-memory database, and through its worker
protocol over a `MessageChannel`.

---

## License

Apache-2.0
