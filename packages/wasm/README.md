# `@liliadb/wasm`

> WebAssembly & OPFS (Origin Private File System) distribution for **LiliaDB**, the local-first multimodel embedded database.

Run LiliaDB everywhere JavaScript runs:
- **Browsers**: Client-side SQLite persisted with high performance in the Origin Private File System (OPFS) without locking the UI thread.
- **Edge Runtimes**: Cloudflare Workers, Deno Deploy, Vercel Edge with zero native binary dependencies.
- **Node.js**: Cross-platform fallback without native toolchain requirements.

---

## Features

- 🌸 **Identical Multimodel API**: 100% API parity with `@liliadb/node` (`db.kv`, `db.json`, `db.graph`, `db.fts`, `db.sql`, `db.batch`).
- 📁 **OPFS SAHPool Persistence**: Synchronous Access Handle Pool VFS for true zero-copy, non-volatile database files directly in modern browsers.
- 🧵 **Transparent Web Worker Bridge**: Browsers restrict sync access handles to Web Workers. `@liliadb/wasm` handles this transparently—call `Database.open()` from your main UI thread and operations are automatically proxied through a dedicated worker.
- ⚡ **Full-Text Search (FTS5)**: Embedded SQLite FTS5 engine compiled with BM25 ranking and query highlighting.
- 🔒 **AES-256-GCM Encryption**: Client-side transparent page and payload encryption using a single passphrase.
- 📦 **Zero Native Dependencies**: Ships pure `.wasm` and ESM TypeScript bindings.

---

## Installation

```bash
pnpm add @liliadb/wasm
# or
npm install @liliadb/wasm
```

---

## Quickstart

### 1. Browser with Persistent OPFS

```typescript
import { Database } from "@liliadb/wasm";

// Opens or creates a persistent SQLite database in the browser's OPFS
const db = await Database.open({
  path: "app.lilia",
  vfs: "opfs", // automatically uses opfs-sahpool
});

// JSON Document store
await db.json.put("users", "u-1", { name: "Alice", role: "admin" });
const user = await db.json.get("users", "u-1");
console.log(user?.value);

// FTS5 Full-Text Search
await db.fts.index("notes", "n-1", "LiliaDB supports high performance WebAssembly");
const hits = await db.fts.search("notes", "WebAssembly");
console.log(hits[0]?.snippet);

// Declarative SQL
const rows = await db.sql.query("SELECT * FROM _lilia_json WHERE space = ?", ["users"]);
console.log(rows);

// Clean shutdown
await db.close();
```

### 2. Edge Runtimes & In-Memory (Cloudflare Workers, Deno, Vercel)

```typescript
import { Database } from "@liliadb/wasm";

const db = await Database.open({
  path: ":memory:",
  vfs: "memory",
});

await db.kv.set("cache", new Uint8Array([1, 2]), new Uint8Array([99, 100]));
const cached = await db.kv.get("cache", new Uint8Array([1, 2]));
```

### 3. Transparent AES-256-GCM Encryption

```typescript
const db = await Database.open({
  path: "secure.lilia",
  vfs: "opfs",
  encryptionKey: "my-secure-master-password",
});
```

---

## Architecture

```
┌────────────────────────────────────────────────────────┐
│                   Main Browser Thread                  │
│                                                        │
│   db.json.put() ──►  Database  ──► WorkerClient (RPC)  │
└───────────────────────────┬────────────────────────────┘
                            │ postMessage (Transferables)
┌───────────────────────────▼────────────────────────────┐
│                  Dedicated Web Worker                  │
│                                                        │
│  worker.ts ──► DirectWasmClient ──► WasmDatabase (C)   │
│                                      │ (rusqlite)      │
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

---

## License

Apache-2.0
