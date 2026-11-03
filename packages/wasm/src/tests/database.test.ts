import assert from "node:assert/strict";
import test from "node:test";
import { Database, LiliaError } from "../index.js";

test("WASM in-memory database: lifecycle and integrity", async () => {
  const db = await Database.open({ path: ":memory:", vfs: "memory" });
  assert.equal(await db.integrityCheck(), true);
  await db.close();

  // Further operations after close should reject with CLOSED
  await assert.rejects(
    db.kv.get("test", new Uint8Array([1])),
    (err: unknown) => err instanceof LiliaError && err.code === "CLOSED"
  );
});

test("WASM KV store: CRUD, CAS, and scanning", async () => {
  const db = await Database.open({ path: ":memory:", vfs: "memory" });

  const k1 = new Uint8Array([1, 2, 3]);
  const v1 = new Uint8Array([10, 20, 30]);

  // Set & Get
  const setRes = await db.kv.set("users", k1, v1);
  assert.equal(setRes.deleted, false);
  assert.ok(setRes.version !== undefined && setRes.version > 0n);

  const getRes = await db.kv.get("users", k1);
  assert.ok(getRes);
  assert.equal(getRes.namespace, "users");
  assert.deepEqual([...getRes.key], [1, 2, 3]);
  assert.deepEqual([...getRes.value], [10, 20, 30]);
  assert.equal(getRes.version, setRes.version);

  // CAS update with ifVersion
  const v2 = new Uint8Array([40, 50, 60]);
  const updateRes = await db.kv.set("users", k1, v2, { ifVersion: setRes.version });
  assert.ok(updateRes.version && updateRes.version > setRes.version);

  // Stale CAS update should fail
  await assert.rejects(
    db.kv.set("users", k1, v1, { ifVersion: setRes.version }),
    (err: unknown) => err instanceof LiliaError && err.code === "CONFLICT"
  );

  // Scan
  const k2 = new Uint8Array([1, 2, 4]);
  await db.kv.set("users", k2, new Uint8Array([99]));
  const scan = await db.kv.scan("users", { limit: 10 });
  assert.equal(scan.length, 2);

  // Scan Stream
  const streamed: number[][] = [];
  for await (const entry of db.kv.scanStream("users", { batchSize: 1 })) {
    streamed.push([...entry.key]);
  }
  assert.equal(streamed.length, 2);
  assert.deepEqual(streamed[0], [1, 2, 3]);
  assert.deepEqual(streamed[1], [1, 2, 4]);

  // Delete
  const delRes = await db.kv.delete("users", k1);
  assert.equal(delRes.deleted, true);
  const afterDel = await db.kv.get("users", k1);
  assert.equal(afterDel, undefined);

  await db.close();
});

test("WASM JSON store: documents, indexing, and queries", async () => {
  const db = await Database.open({ path: ":memory:", vfs: "memory" });

  await db.json.put("products", "p1", { name: "Laptop", category: "electronics", price: 1200 });
  await db.json.put("products", "p2", { name: "Mouse", category: "electronics", price: 25 });
  await db.json.put("products", "p3", { name: "Desk", category: "furniture", price: 300 });

  // Get
  const p1 = await db.json.get("products", "p1");
  assert.ok(p1);
  assert.equal(p1.id, "p1");
  assert.deepEqual(p1.value, { name: "Laptop", category: "electronics", price: 1200 });

  // Secondary indexing
  await db.json.createIndex("products", "cat_idx", "$.category");
  const indexes = await db.json.listIndexes("products");
  assert.ok(indexes.some(i => i.indexName === "cat_idx"));

  const electronics = await db.json.findByIndex("products", "cat_idx", "electronics");
  assert.equal(electronics.length, 2);
  assert.ok(electronics.every(e => (e.value as any).category === "electronics"));

  // Find
  const found = await db.json.find("products", { name: "Desk" });
  assert.equal(found.length, 1);
  assert.equal(found[0]?.id, "p3");

  // Stream
  const streamedDocs: string[] = [];
  for await (const doc of db.json.scanStream("products", { batchSize: 2 })) {
    streamedDocs.push(doc.id);
  }
  assert.equal(streamedDocs.length, 3);

  // Drop index
  const dropped = await db.json.dropIndex("products", "cat_idx");
  assert.equal(dropped, true);

  await db.close();
});

test("WASM Graph engine: nodes, edges, and reachability", async () => {
  const db = await Database.open({ path: ":memory:", vfs: "memory" });

  await db.graph.setNode("dependency", "app", "service");
  await db.graph.setNode("dependency", "auth", "service");
  await db.graph.setNode("dependency", "db", "database");

  await db.graph.addEdge("dependency", "app", "auth", "depends_on");
  await db.graph.addEdge("dependency", "auth", "db", "depends_on");

  // Direct neighbors
  const appNeighbors = await db.graph.neighbors("dependency", "app", {
    direction: "outgoing",
    kind: "depends_on",
  });
  assert.deepEqual(appNeighbors, ["auth"]);

  // Transitive reachability
  const reachable = await db.graph.reachability("dependency", ["app"], {
    direction: "outgoing",
  });
  assert.deepEqual(reachable.sort(), ["app", "auth", "db"].sort());

  await db.close();
});

test("WASM FTS5 engine: full-text indexing and search", async () => {
  const db = await Database.open({ path: ":memory:", vfs: "memory" });

  await db.fts.index(
    "knowledge",
    "article-1",
    "WebAssembly provides sandboxed binary code execution on the web and edge"
  );
  await db.fts.index(
    "knowledge",
    "article-2",
    "Origin Private File System OPFS provides fast synchronous persistent file access"
  );

  const wasmHits = await db.fts.search("knowledge", "WebAssembly");
  assert.equal(wasmHits.length, 1);
  assert.equal(wasmHits[0]?.docId, "article-1");
  assert.ok(wasmHits[0]?.snippet.includes("<b>WebAssembly</b>"));

  const opfsHits = await db.fts.search("knowledge", "OPFS");
  assert.equal(opfsHits.length, 1);
  assert.equal(opfsHits[0]?.docId, "article-2");
  assert.ok(opfsHits[0]?.snippet.includes("<b>OPFS</b>"));

  await db.close();
});

test("WASM SQL engine: relational queries and JSON extracts", async () => {
  const db = await Database.open({ path: ":memory:", vfs: "memory" });

  await db.json.put("users", "u1", { name: "Alice", active: true });
  await db.json.put("users", "u2", { name: "Bob", active: false });

  // Query records
  const users = await db.sql.query<{ id: string; name: string }>(
    "SELECT id, json_extract(value, '$.name') as name FROM _lilia_json WHERE space = ? ORDER BY id",
    ["users"]
  );
  assert.equal(users.length, 2);
  assert.equal(users[0]?.id, "u1");
  assert.equal(users[0]?.name, "Alice");
  assert.equal(users[1]?.id, "u2");
  assert.equal(users[1]?.name, "Bob");

  // Raw query
  const raw = await db.sql.raw(
    "SELECT count(*) as count FROM _lilia_json WHERE space = ?",
    ["users"]
  );
  assert.deepEqual(raw.columns, ["count"]);
  assert.equal(raw.rows[0]?.[0], 2);

  await db.close();
});

test("WASM Atomic multi-model batch and rollback", async () => {
  const db = await Database.open({ path: ":memory:", vfs: "memory" });

  // Atomic batch across KV, JSON, Graph, FTS
  const batchRes = await db.batch([
    {
      model: "kv_set",
      namespace: "config",
      key: new Uint8Array([1]),
      value: new Uint8Array([2]),
    },
    {
      model: "json_put",
      space: "orders",
      id: "ord-1",
      value: { amount: 100 },
    },
    {
      model: "graph_set_node",
      graph: "g",
      id: "node-1",
      kind: "entity",
    },
    {
      model: "fts_index",
      collection: "notes",
      docId: "note-1",
      content: "Important batch note",
    },
  ]);
  assert.equal(batchRes.length, 4);
  assert.ok(batchRes.every(r => !r.deleted));

  // Conflicting batch must rollback everything atomically
  await assert.rejects(
    db.batch([
      {
        model: "kv_set",
        namespace: "config",
        key: new Uint8Array([1]),
        value: new Uint8Array([99]),
        ifVersion: 999999n, // Non-existent version triggers conflict
      },
      {
        model: "json_put",
        space: "orders",
        id: "ord-2",
        value: { amount: 200 },
      },
    ]),
    (err: unknown) => err instanceof LiliaError && err.code === "CONFLICT"
  );

  // ord-2 must NOT have been written
  const ord2 = await db.json.get("orders", "ord-2");
  assert.equal(ord2, undefined);

  await db.close();
});

test("WASM AES-256-GCM transparent encryption", async () => {
  const db = await Database.open({
    path: ":memory:",
    vfs: "memory",
    encryptionKey: "my-secure-master-passphrase",
  });

  await db.kv.set(
    "vault",
    new Uint8Array([42]),
    new Uint8Array([1, 2, 3, 4, 5])
  );
  await db.json.put("secure_docs", "d1", { secret: "top-secret-data" });

  const kvVal = await db.kv.get("vault", new Uint8Array([42]));
  assert.ok(kvVal);
  assert.deepEqual([...kvVal.value], [1, 2, 3, 4, 5]);

  const docVal = await db.json.get("secure_docs", "d1");
  assert.ok(docVal);
  assert.deepEqual(docVal.value, { secret: "top-secret-data" });

  await db.close();
});
