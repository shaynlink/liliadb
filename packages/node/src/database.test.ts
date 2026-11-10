import assert from "node:assert/strict";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { connect } from "node:net";
import { spawn, type ChildProcess } from "node:child_process";
import test from "node:test";

import {
  Database,
  LiliaError,
  isKvEntry,
  isJsonEntry,
  type DatabaseMode,
  type JsonEntry,
  type KvEntry,
} from "./index.js";

import { existsSync } from "node:fs";

const candidateNative = [
  new URL("../../../target/release/liblilia_node_native.dylib", import.meta.url).pathname,
  new URL("../../../target/release/liblilia_node_native.so", import.meta.url).pathname,
  new URL("../../../target/release/lilia_node_native.dll", import.meta.url).pathname,
  new URL("../../../packages/node-darwin-arm64/lilia_node_native.node", import.meta.url).pathname,
  new URL("../../../target/debug/liblilia_node_native.dylib", import.meta.url).pathname,
  new URL("../../../target/debug/liblilia_node_native.so", import.meta.url).pathname,
  new URL("../../../target/debug/lilia_node_native.dll", import.meta.url).pathname,
].find(p => existsSync(p));

const candidateDaemon = [
  new URL("../../../target/debug/lilia-daemon", import.meta.url).pathname,
  new URL("../../../target/debug/lilia-daemon.exe", import.meta.url).pathname,
].find(p => existsSync(p));

const nativePath = process.env.LILIA_NATIVE_PATH ?? candidateNative;
const daemonBinary = process.env.LILIA_DAEMON_BIN ?? candidateDaemon;

test("close timeout drains accepted JS calls and can be awaited again", async () => {
  let release!: (value: []) => void;
  let closes = 0;
  const delayed = new Promise<[]>((resolve) => { release = resolve; });
  // Inject a deterministic delayed client through the runtime constructor. No
  // native timing assumptions are needed to exercise the SDK admission boundary.
  const database = Reflect.construct(Database, [{
    batch: () => delayed,
    close: async () => { closes += 1; },
  }]) as Database;
  await assert.rejects(database.close({ timeoutMs: -1 }), RangeError);
  const accepted = database.batch([]);
  await assert.rejects(database.close({ timeoutMs: 0 }), (error: unknown) =>
    error instanceof LiliaError && error.code === "TIMEOUT" && error.retryable && !!error.requestId);
  assert.equal(closes, 0);
  await assert.rejects(database.batch([]), (error: unknown) =>
    error instanceof LiliaError && error.code === "CLOSED" && !!error.requestId);
  release([]);
  await accepted;
  await Promise.all([database.close(), database.close()]);
  assert.equal(closes, 1);
});

test("embedded close drains submitted writes and releases storage", { skip: !nativePath }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "lilia-close-"));
  const path = join(directory, "db.lilia");
  try {
    const database = await Database.open({ path });
    const writes = Array.from({ length: 16 }, (_, index) =>
      database.json.put("drain", String(index), { index }));
    await database.close();
    await Promise.all(writes);
    const reopened = await Database.open({ path });
    try {
      assert.equal((await reopened.json.scan("drain")).length, 16);
      assert.equal(await reopened.integrityCheck(), true);
    } finally { await reopened.close(); }
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test("checkpoint policy rejects invalid values before opening native storage", async () => {
  for (const value of [0, -1, 0.5, Number.NaN, Number.POSITIVE_INFINITY, 2 ** 32]) {
    await assert.rejects(Database.open({ path: "unused.lilia", checkpointPolicy: { walBytes: value } }), RangeError);
    await assert.rejects(Database.open({ path: "unused.lilia", checkpointPolicy: { intervalMs: value } }), RangeError);
  }
});

test("embedded KV, JSON, and rollback conformance", { skip: !nativePath }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "lilia-node-"));
  try { await conformance("embedded", join(directory, "embedded.lilia")); }
  finally { await rm(directory, { recursive: true, force: true }); }
});

test("embedded synchronous db.sync KV, JSON, SQL and batch conformance", { skip: !nativePath }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "lilia-sync-"));
  const path = join(directory, "sync.lilia");
  try {
    const db = await Database.open({ path });
    const enc = new TextEncoder();
    const dec = new TextDecoder();

    // 1. Sync KV
    const setRes = db.sync.kv.set("cfg", enc.encode("k1"), enc.encode("v1"));
    assert.equal(setRes.version, 1n);
    assert.equal(setRes.deleted, false);

    const getRes = db.sync.kv.get("cfg", enc.encode("k1"));
    assert.ok(getRes !== undefined);
    assert.equal(dec.decode(getRes.value), "v1");
    assert.equal(getRes.version, 1n);

    const scanRes = db.sync.kv.scan("cfg");
    assert.equal(scanRes.length, 1);
    assert.equal(dec.decode(scanRes[0]!.value), "v1");

    const delRes = db.sync.kv.delete("cfg", enc.encode("k1"));
    assert.equal(delRes.deleted, true);
    assert.equal(db.sync.kv.get("cfg", enc.encode("k1")), undefined);

    // 2. Sync JSON
    const putRes = db.sync.json.put("docs", "d1", { title: "Fast", count: 42 });
    assert.equal(putRes.version, 1n);

    const doc = db.sync.json.get("docs", "d1");
    assert.ok(doc !== undefined);
    assert.deepEqual(doc.value, { title: "Fast", count: 42 });

    const scanJson = db.sync.json.scan("docs");
    assert.equal(scanJson.length, 1);
    assert.equal(scanJson[0]!.id, "d1");

    // 3. Sync SQL
    const sqlRows = db.sync.sql.query<{ id: string; version: number }>(
      "SELECT id, version FROM lilia_json WHERE space = ?1",
      ["docs"]
    );
    assert.equal(sqlRows.length, 1);
    assert.equal(sqlRows[0]?.id, "d1");

    const sqlExec = db.sync.sql.execute(
      "UPDATE _lilia_json SET version = version + 1 WHERE space = ?1 AND id = ?2",
      ["docs", "d1"]
    );
    assert.equal(sqlExec.rowsAffected, 1);

    const updatedDoc = db.sync.json.get("docs", "d1");
    assert.equal(updatedDoc?.version, 2n);

    // 4. Sync Batch
    const batchRes = db.sync.batch([
      { model: "kv_set", namespace: "b", key: enc.encode("k_a"), value: enc.encode("v_a") },
      { model: "json_put", space: "b", id: "j_a", value: { ok: true } },
    ]);
    assert.equal(batchRes.length, 2);
    assert.equal(batchRes[0]?.deleted, false);
    assert.equal(batchRes[1]?.deleted, false);

    await db.close();
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("daemon KV and JSON conformance", { skip: !daemonBinary }, async () => {
  const directory = await realpath(await mkdtemp(join(tmpdir(), "lilia-daemon-")));
  const path = join(directory, "daemon.lilia");
  const endpoint = process.platform === "win32" ? `\\\\.\\pipe\\liliadb-test-${process.pid}` : `${path}.sock`;
  const tokenFile = process.platform === "win32" ? join(directory, "ipc", "token") : `${path}.token`;
  let daemon: ChildProcess | undefined;
  try {
    daemon = spawn(daemonBinary!, ["--database", path, "--endpoint", endpoint, "--token-file", tokenFile], {
      stdio: ["ignore", "inherit", "inherit"],
    });
    await waitUntilReady(tokenFile, endpoint, daemon);
    await conformance("daemon", path, endpoint, tokenFile);
  } finally {
    daemon?.kill();
    await rm(directory, { recursive: true, force: true });
  }
});

test("embedded db.watch receives live CDC events filtered by namespace", { skip: !nativePath }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "lilia-watch-emb-"));
  const path = join(directory, "watch_emb.lilia");
  try {
    const db = await Database.open({ path });
    const events: any[] = [];
    const sub = await db.watch("orders", (event) => {
      events.push(event);
    });

    const enc = new TextEncoder();
    // Non-matching mutation
    await db.kv.set("inventory", enc.encode("item_1"), enc.encode("in_stock"));

    // Matching mutations
    await db.kv.set("orders", enc.encode("ord_1"), enc.encode("paid"));
    await db.json.put("orders", "doc_1", { status: "pending" });

    // Wait a brief moment for events to dispatch
    for (let i = 0; i < 20; i++) {
      if (events.length >= 2) break;
      await new Promise(r => setTimeout(r, 25));
    }

    assert.equal(events.length, 2);
    assert.equal(events[0].type, "kv_mutated");
    assert.equal(events[0].namespace, "orders");
    assert.equal(new TextDecoder().decode(events[0].key), "ord_1");
    assert.equal(events[1].type, "json_mutated");
    assert.equal(events[1].space, "orders");
    assert.equal(events[1].id, "doc_1");

    await sub.unsubscribe();
    await db.close();
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("daemon db.watch receives live CDC events over binary IPC", { skip: !daemonBinary }, async () => {
  const directory = await realpath(await mkdtemp(join(tmpdir(), "l-w-")));
  const path = join(directory, "d.lilia");
  const endpoint = process.platform === "win32" ? `\\\\.\\pipe\\liliadb-watch-${process.pid}` : `${path}.sock`;
  const tokenFile = process.platform === "win32" ? join(directory, "ipc", "token") : `${path}.token`;
  let daemon: ChildProcess | undefined;
  try {
    daemon = spawn(daemonBinary!, ["--database", path, "--endpoint", endpoint, "--token-file", tokenFile], {
      stdio: ["ignore", "inherit", "inherit"],
    });
    await waitUntilReady(tokenFile, endpoint, daemon);

    const db = await Database.open({ path, mode: "daemon", endpoint, tokenFile });
    const events: any[] = [];
    const sub = await db.watch("telemetry", (event) => {
      events.push(event);
    });

    const enc = new TextEncoder();
    // Non-matching write
    await db.kv.set("logs", enc.encode("log_1"), enc.encode("info"));

    // Matching writes
    await db.kv.set("telemetry", enc.encode("sensor_1"), enc.encode("24.5"));
    await db.json.put("telemetry", "metric_1", { cpu: 85 });

    for (let i = 0; i < 20; i++) {
      if (events.length >= 2) break;
      await new Promise(r => setTimeout(r, 25));
    }

    assert.equal(events.length, 2);
    assert.equal(events[0].type, "kv_mutated");
    assert.equal(events[0].namespace, "telemetry");
    assert.equal(new TextDecoder().decode(events[0].key), "sensor_1");
    assert.equal(events[1].type, "json_mutated");
    assert.equal(events[1].space, "telemetry");
    assert.equal(events[1].id, "metric_1");

    await sub.unsubscribe();
    await db.close();
  } finally {
    daemon?.kill();
    await rm(directory, { recursive: true, force: true });
  }
});

async function conformance(mode: DatabaseMode, path: string, endpoint?: string, tokenFile?: string): Promise<void> {
  const database = await Database.open({ path, mode, endpoint, tokenFile,
    checkpointPolicy: mode === "embedded" ? { walBytes: 4096, intervalMs: 25 } : undefined });
  const bytes = new TextEncoder();
  try {
    const write = await database.kv.set("cache", bytes.encode("answer"), bytes.encode("42"), { ifVersion: 0n });
    assert.equal(write.version, 1n);
    assert.equal(new TextDecoder().decode((await database.kv.get("cache", bytes.encode("answer")))?.value), "42");
    const document = await database.json.put("people", "ada", { name: "Ada" }, { ifVersion: 0n });
    assert.equal(document.version, 1n);
    assert.deepEqual((await database.json.get("people", "ada"))?.value, { name: "Ada" });
    await assert.rejects(
      database.batch([
        { model: "json_put", space: "people", id: "grace", value: { name: "Grace" } },
        { model: "json_delete", space: "people", id: "missing", ifVersion: 1n },
      ]),
      (error: unknown) => error instanceof LiliaError || error instanceof Error,
    );
    assert.equal(await database.json.get("people", "grace"), undefined);
    // Verify streaming with real engine
    const streamItems: JsonEntry[] = [];
    for await (const entry of database.json.scanStream("people", { batchSize: 1 })) {
      streamItems.push(entry);
    }
    assert.equal(streamItems.length, 1);
    assert.equal(streamItems[0]?.id, "ada");

    const streamKv: KvEntry[] = [];
    for await (const entry of database.kv.scanStream("cache", { batchSize: 1 })) {
      streamKv.push(entry);
    }
    assert.equal(streamKv.length, 1);
    assert.equal(new TextDecoder().decode(streamKv[0]?.value), "42");

    // Verify SQL query, raw, and execute
    interface PersonRow {
      id: string;
      name: string;
      version: number;
    }
    const sqlRows = await database.sql.query<PersonRow>(
      "SELECT id, json_extract(value, '$.name') AS name, version FROM lilia_json WHERE space = ?1",
      ["people"],
    );
    assert.equal(sqlRows.length, 1);
    assert.equal(sqlRows[0]?.id, "ada");
    assert.equal(sqlRows[0]?.name, "Ada");
    assert.equal(sqlRows[0]?.version, 1);

    const raw = await database.sql.raw(
      "SELECT id, json_extract(value, '$.name') AS name FROM lilia_json WHERE space = ?1",
      ["people"],
    );
    assert.deepEqual(raw.columns, ["id", "name"]);
    assert.deepEqual(raw.rows, [["ada", "Ada"]]);
    assert.equal(raw.rowsAffected, 0);

    const affected = await database.sql.execute(
      "UPDATE _lilia_json SET version = version + 1 WHERE space = ?1 AND id = ?2",
      ["people", "ada"],
    );
    assert.equal(affected.rowsAffected, 1);
    const updatedDoc = await database.json.get("people", "ada");
    assert.equal(updatedDoc?.version, 2n);

    await assert.rejects(
      database.sql.query("SELECT * FROM non_existent_table"),
      (error: unknown) => error instanceof LiliaError || error instanceof Error,
    );

    // Verify Graph
    await database.graph.setNode("workspace", "nodeA", "service", { env: "prod" });
    await database.graph.setNode("workspace", "nodeB", "database");
    await database.graph.addEdge("workspace", "nodeA", "nodeB", "depends_on");
    const neighbors = await database.graph.neighbors("workspace", "nodeA", { direction: "outgoing" });
    assert.deepEqual(neighbors, ["nodeB"]);
    const reachable = await database.graph.reachability("workspace", ["nodeA"], { direction: "outgoing" });
    assert.deepEqual(reachable, ["nodeA", "nodeB"]);

    // Verify FTS
    await database.fts.index("articles", "art1", "hello world search in liliadb");
    const hits = await database.fts.search("articles", "world");
    assert.equal(hits.length, 1);
    assert.equal(hits[0]?.docId, "art1");

    // Verify Vector & Hybrid Search
    await database.vector.set("docs", "vec1", [1.0, 0.0, 0.0]);
    await database.vector.set("docs", "vec2", [0.0, 1.0, 0.0]);
    const vecHits = await database.vector.search("docs", [0.95, 0.05, 0.0], { limit: 1 });
    assert.equal(vecHits.length, 1);
    assert.equal(vecHits[0]?.id, "vec1");

    await database.vector.set("articles", "art1", [1.0, 0.0, 0.0]);
    const hybridHits = await database.hybrid.search("articles", {
      query: "hello",
      vector: [1.0, 0.0, 0.0],
      limit: 5,
    });
    assert.equal(hybridHits.length, 1);
    assert.equal(hybridHits[0]?.id, "art1");
    assert.ok(hybridHits[0]?.rrfScore > 0);

    // Verify JSON Indexing & find
    await database.json.put("users", "u1", { name: "Alice", role: "admin", level: 10, active: true });
    await database.json.put("users", "u2", { name: "Bob", role: "member", level: 5, active: true });
    await database.json.put("users", "u3", { name: "Charlie", role: "admin", level: 8, active: false });

    // 1. find fallback without index
    const adminsFallback = await database.json.find("users", { role: "admin" });
    assert.equal(adminsFallback.length, 2);
    assert.equal(adminsFallback[0]?.id, "u1");
    assert.equal(adminsFallback[1]?.id, "u3");

    // 2. createIndex
    await database.json.createIndex("users", "role");
    const indexes = await database.json.listIndexes("users");
    assert.equal(indexes.length, 1);
    assert.equal(indexes[0]?.indexName, "role");

    // 3. find with index acceleration
    const adminsIndexed = await database.json.find("users", { role: "admin" });
    assert.equal(adminsIndexed.length, 2);
    assert.equal(adminsIndexed[0]?.id, "u1");
    assert.equal(adminsIndexed[1]?.id, "u3");

    // 4. findByIndex
    const members = await database.json.findByIndex("users", "role", "member");
    assert.equal(members.length, 1);
    assert.equal(members[0]?.id, "u2");

    // 5. multi-condition find
    const activeAdmins = await database.json.find("users", { role: "admin", active: true });
    assert.equal(activeAdmins.length, 1);
    assert.equal(activeAdmins[0]?.id, "u1");

    // 6. findStream
    const streamed: JsonEntry[] = [];
    for await (const doc of database.json.findStream("users", { role: "admin" })) {
      streamed.push(doc);
    }
    assert.equal(streamed.length, 2);
    assert.equal(streamed[0]?.id, "u1");
    assert.equal(streamed[1]?.id, "u3");

    // 7. dropIndex
    const dropped = await database.json.dropIndex("users", "role");
    assert.equal(dropped, true);
    const indexesAfter = await database.json.listIndexes("users");
    assert.equal(indexesAfter.length, 0);

    assert.equal(await database.integrityCheck(), true);
  } finally {
    await database.close();
  }
  await assert.rejects(database.integrityCheck(), (error: unknown) =>
    error instanceof LiliaError && error.code === "CLOSED");
}

test("scanStream and createReadStream paginate across batches and respect limits", async () => {
  const entries: KvEntry[] = Array.from({ length: 25 }, (_, i) => ({
    namespace: "test",
    key: new Uint8Array([i]),
    value: new Uint8Array([i * 2]),
    version: BigInt(i + 1),
  }));

  const mockClient = {
    kvScan: async (_ns: string, after: Uint8Array | undefined, limit: number) => {
      const startIndex = after !== undefined ? after[0]! + 1 : 0;
      return entries.slice(startIndex, startIndex + limit);
    },
    jsonScan: async (_sp: string, after: string | undefined, limit: number) => {
      const startIndex = after !== undefined ? Number(after) + 1 : 0;
      return entries.slice(startIndex, startIndex + limit).map(e => ({
        space: "items",
        id: String(e.key[0]),
        value: { v: e.key[0] },
        version: e.version,
      }));
    },
    close: async () => {},
  };

  const db = Reflect.construct(Database, [mockClient]) as Database;

  // 1. Full stream with batchSize 5
  const full: KvEntry[] = [];
  for await (const entry of db.kv.scanStream("test", { batchSize: 5 })) {
    full.push(entry);
  }
  assert.equal(full.length, 25);
  assert.equal(full[0]!.key[0], 0);
  assert.equal(full[24]!.key[0], 24);

  // 2. Global limit 12 with batchSize 5 (3 batches: 5, 5, 2)
  const limited: KvEntry[] = [];
  for await (const entry of db.kv.stream("test", { batchSize: 5, limit: 12 })) {
    limited.push(entry);
  }
  assert.equal(limited.length, 12);
  assert.equal(limited[11]!.key[0], 11);

  // 3. Early break in for await
  const earlyBreak: KvEntry[] = [];
  for await (const entry of db.kv.scanStream("test", { batchSize: 5 })) {
    earlyBreak.push(entry);
    if (earlyBreak.length === 7) break;
  }
  assert.equal(earlyBreak.length, 7);

  // 4. JSON scanStream
  const jsonItems: JsonEntry[] = [];
  for await (const item of db.json.scanStream("items", { batchSize: 10, limit: 15 })) {
    jsonItems.push(item);
  }
  assert.equal(jsonItems.length, 15);
  assert.equal(jsonItems[0]!.id, "0");
  assert.equal(jsonItems[14]!.id, "14");

  // 5. createReadStream Node.js stream
  const chunks: JsonEntry[] = [];
  const stream = db.json.createReadStream("items", { batchSize: 10, limit: 8 });
  for await (const chunk of stream) {
    chunks.push(chunk as JsonEntry);
  }
  assert.equal(chunks.length, 8);
  assert.equal(chunks[7]!.id, "7");

  // 6. Empty scanStream
  const empty: KvEntry[] = [];
  for await (const entry of db.kv.scanStream("empty", { limit: 0 })) {
    empty.push(entry);
  }
  assert.equal(empty.length, 0);
});

test("explicit resource management with await using closes database automatically", { skip: !nativePath }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "lilia-using-"));
  const path = join(directory, "using.lilia");
  try {
    let dbRef!: Database;
    {
      await using db = await Database.open({ path });
      dbRef = db;
      await db.json.put("test", "id1", { val: 123 });
      const doc = await db.json.get<{ val: number }>("test", "id1");
      assert.equal(doc?.value?.val, 123);
    }
    // db should now be closed automatically upon exiting the scope!
    await assert.rejects(dbRef.json.get("test", "id1"), (err: unknown) =>
      err instanceof LiliaError && err.code === "CLOSED"
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("explicit resource management with using auto-unsubscribes watch", { skip: !nativePath }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "lilia-sub-"));
  const path = join(directory, "sub.lilia");
  try {
    await using db = await Database.open({ path });
    const events: any[] = [];
    {
      using sub = await db.watch("orders", (event) => events.push(event));
      assert.ok(typeof sub[Symbol.dispose] === "function");
      await db.json.put("orders", "o1", { amount: 100 });
      for (let i = 0; i < 20 && events.length === 0; i++) {
        await new Promise(r => setTimeout(r, 20));
      }
      assert.equal(events.length, 1);
    }
    // Out of scope: sub was synchronously disposed via using!
    await db.json.put("orders", "o2", { amount: 200 });
    await new Promise(r => setTimeout(r, 50));
    assert.equal(events.length, 1);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("SQL tagged template literals parameterize queries safely", { skip: !nativePath }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "lilia-sql-tag-"));
  const path = join(directory, "tag.lilia");
  try {
    await using db = await Database.open({ path });
    await db.json.put("users", "u1", { name: "Alice", role: "admin" });
    await db.json.put("users", "u2", { name: "Bob", role: "guest" });

    interface UserRow {
      id: string;
      name: string;
    }

    // Async tagged template
    const role = "admin";
    const rows = await db.sql<UserRow>`SELECT id, json_extract(value, '$.name') AS name FROM lilia_json WHERE space = 'users' AND json_extract(value, '$.role') = ${role}`;
    assert.equal(rows.length, 1);
    assert.equal(rows[0]?.name, "Alice");

    // Sync tagged template
    const syncRows = db.sync.sql<UserRow>`SELECT id, json_extract(value, '$.name') AS name FROM lilia_json WHERE space = 'users' AND json_extract(value, '$.role') = ${"guest"}`;
    assert.equal(syncRows.length, 1);
    assert.equal(syncRows[0]?.name, "Bob");
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("AbortSignal cancels operations proactively", { skip: !nativePath }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "lilia-abort-"));
  const path = join(directory, "abort.lilia");
  try {
    await using db = await Database.open({ path });

    // 1. Pre-aborted signal throws immediately
    const preAborted = AbortSignal.abort(new Error("pre-aborted"));
    await assert.rejects(
      db.json.get("test", "id", { signal: preAborted }),
      /pre-aborted/
    );

    // 2. AbortSignal in watch auto-unsubscribes
    const controller = new AbortController();
    const events: any[] = [];
    await db.watch("auto", (e) => events.push(e), { signal: controller.signal });
    await db.json.put("auto", "1", { ok: 1 });
    for (let i = 0; i < 20 && events.length === 0; i++) {
      await new Promise(r => setTimeout(r, 20));
    }
    assert.equal(events.length, 1);

    // Abort controller
    controller.abort();
    await db.json.put("auto", "2", { ok: 2 });
    await new Promise(r => setTimeout(r, 50));
    assert.equal(events.length, 1);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("WHATWG ReadableStream streams entries according to web standards", { skip: !nativePath }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "lilia-webstream-"));
  const path = join(directory, "ws.lilia");
  try {
    await using db = await Database.open({ path });
    for (let i = 0; i < 10; i++) {
      await db.json.put("items", `id_${i}`, { index: i });
    }

    // 1. JSON readableStream
    const jsonStream = db.json.readableStream<{ index: number }>("items", { batchSize: 3 });
    assert.ok(jsonStream instanceof ReadableStream);
    const jsonItems: { index: number }[] = [];
    for await (const doc of jsonStream) {
      jsonItems.push(doc.value);
    }
    assert.equal(jsonItems.length, 10);
    assert.equal(jsonItems[0]?.index, 0);
    assert.equal(jsonItems[9]?.index, 9);

    // 2. KV readableStream
    await db.kv.set("cache", "k1", "v1");
    await db.kv.set("cache", "k2", "v2");
    const kvStream = db.kv.readableStream("cache", { batchSize: 1 });
    assert.ok(kvStream instanceof ReadableStream);
    const kvEntries: KvEntry[] = [];
    for await (const entry of kvStream) {
      kvEntries.push(entry);
    }
    assert.equal(kvEntries.length, 2);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("KV string ergonomics and type guards conform to standards", { skip: !nativePath }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "lilia-ergonomics-"));
  const path = join(directory, "ergo.lilia");
  try {
    await using db = await Database.open({ path });

    // Async KV string key & value
    await db.kv.set("users", "user:alice", "developer");
    const entry = await db.kv.get("users", "user:alice");
    assert.ok(entry !== undefined);
    assert.ok(isKvEntry(entry));
    assert.equal(new TextDecoder().decode(entry.value), "developer");

    // Sync KV string key & value
    db.sync.kv.set("users", "user:bob", "designer");
    const syncEntry = db.sync.kv.get("users", "user:bob");
    assert.ok(syncEntry !== undefined);
    assert.ok(isKvEntry(syncEntry));
    assert.equal(new TextDecoder().decode(syncEntry.value), "designer");

    // Generic JSON entry and isJsonEntry
    interface Profile { name: string; age: number }
    await db.json.put<Profile>("profiles", "p1", { name: "Alice", age: 30 });
    const profileDoc = await db.json.get<Profile>("profiles", "p1");
    assert.ok(profileDoc !== undefined);
    assert.ok(isJsonEntry(profileDoc));
    assert.equal(profileDoc.value.name, "Alice");
    assert.equal(profileDoc.value.age, 30);

    // LiliaError cause support
    const err = new LiliaError({ message: "top error", cause: new Error("underlying") });
    assert.ok(LiliaError.isLiliaError(err));
    assert.equal(err.name, "LiliaError");
    assert.ok(err.cause instanceof Error);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

async function waitUntilReady(tokenFile: string, endpoint: string, daemon: ChildProcess): Promise<void> {
  const deadline = Date.now() + 5_000;
  while (Date.now() < deadline) {
    if (daemon.exitCode !== null) throw new Error(`daemon exited with ${daemon.exitCode}`);
    try {
      const { access } = await import("node:fs/promises");
      await access(tokenFile);
      // The token is created before SQLite is opened and the IPC listener is bound.
      await new Promise<void>((resolve, reject) => {
        const socket = connect(endpoint);
        socket.once("connect", () => { socket.destroy(); resolve(); });
        socket.once("error", error => { socket.destroy(); reject(error); });
      });
      return;
    } catch {
      await new Promise(resolve => setTimeout(resolve, 25));
    }
  }
  throw new Error("daemon startup timed out");
}
