import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  Database,
  LiliaError,
  geo,
  isJsonEntry,
  isKvEntry,
  quantization,
  timeseries,
  type DatabaseMode,
  type JsonEntry,
  type KvEntry,
} from "./index.js";
import { tryLoadNative, type Transport } from "./transport.js";
import type { Operation, ResponseValue } from "./wire.js";

const native = tryLoadNative();
const daemonBinary =
  process.env.LILIA_DAEMON_BIN ??
  ["lilia-daemon", "lilia-daemon.exe"]
    .map((name) => new URL(`../../../target/debug/${name}`, import.meta.url).pathname)
    .find((path) => existsSync(path));

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/** A database over `transport`, as `Database.open` builds one. */
function over(transport: Transport): Database {
  return Reflect.construct(Database, [transport]) as Database;
}

/** A transport answering every operation with `answer`. */
function fake(answer: (operation: Operation) => ResponseValue | Promise<ResponseValue>, onClose: () => void = () => {}): Transport {
  return {
    execute: async (operation) => answer(operation),
    executeSync: () => {
      throw new Error("not synchronous");
    },
    close: async () => onClose(),
  };
}

async function withDirectory(prefix: string, body: (directory: string) => Promise<void>): Promise<void> {
  const directory = await realpath(await mkdtemp(join(tmpdir(), prefix)));
  try {
    await body(directory);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}

test("close waits for the calls already made and refuses later ones", async () => {
  const { promise: delayed, resolve: release } = Promise.withResolvers<ResponseValue>();
  let closes = 0;
  const database = over(fake(() => delayed, () => { closes += 1; }));
  await assert.rejects(database.close({ timeoutMs: -1 }), RangeError);
  const accepted = database.batch([]);
  await assert.rejects(database.close({ timeoutMs: 0 }), (error: unknown) =>
    error instanceof LiliaError && error.code === "TIMEOUT" && error.retryable && !!error.requestId);
  assert.equal(closes, 0);
  await assert.rejects(database.batch([]), (error: unknown) =>
    error instanceof LiliaError && error.code === "CLOSED" && !!error.requestId);
  release({ type: "mutations", value: [] });
  assert.deepEqual(await accepted, []);
  await Promise.all([database.close(), database.close()]);
  assert.equal(closes, 1);
});

test("protocol errors and unexpected responses surface as LiliaErrors", async () => {
  const database = over(fake((operation) =>
    operation.type === "kv_get" ? { type: "bool", value: true } : { type: "ack" }));
  await assert.rejects(database.kv.get("ns", "k"), (error: unknown) =>
    error instanceof LiliaError && error.code === "PROTOCOL");
  await database.close();
});

test("streams page through scans and respect limits", async () => {
  const keys = Array.from({ length: 25 }, (_, i) => i);
  const database = over(fake((operation) => {
    if (operation.type === "kv_scan") {
      const start = operation.after === undefined ? 0 : operation.after[0]! + 1;
      return {
        type: "kv_page",
        value: keys.slice(start, start + operation.limit).map((i) => ({
          namespace: operation.namespace,
          key: new Uint8Array([i]),
          value: new Uint8Array([i * 2]),
          version: i + 1,
          expires_at_ms: null,
        })),
      };
    }
    if (operation.type === "json_scan") {
      const start = operation.after === undefined ? 0 : Number(operation.after) + 1;
      return {
        type: "json_page",
        value: keys.slice(start, start + operation.limit).map((i) => ({ space: operation.space, id: String(i), value: { v: i }, version: i + 1 })),
      };
    }
    return { type: "ack" };
  }));

  const all: KvEntry[] = [];
  for await (const entry of database.kv.scanStream("test", { batchSize: 5 })) all.push(entry);
  assert.equal(all.length, 25);
  assert.equal(all[24]!.key[0], 24);
  assert.equal(all[24]!.version, 25n);

  const limited: KvEntry[] = [];
  for await (const entry of database.kv.scanStream("test", { batchSize: 5, limit: 12 })) limited.push(entry);
  assert.equal(limited.length, 12);

  const documents: JsonEntry[] = [];
  for await (const chunk of database.json.createReadStream("items", { batchSize: 10, limit: 8 })) documents.push(chunk as JsonEntry);
  assert.deepEqual(documents.map((d) => d.id), ["0", "1", "2", "3", "4", "5", "6", "7"]);

  const none: KvEntry[] = [];
  for await (const entry of database.kv.scanStream("empty", { limit: 0 })) none.push(entry);
  assert.equal(none.length, 0);
  await database.close();
});

test("helpers quantize vectors, downsample series and measure positions", () => {
  const { quantized, scale } = quantization.toInt8([1, -0.5, 0]);
  assert.deepEqual([...quantized], [127, -64, 0]);
  assert.ok(Math.abs(quantization.fromInt8(quantized, scale)[0]! - 1) < 1e-6);
  assert.deepEqual([...quantization.toBit([1, -1, 1, 0, 0, 0, 0, 0, 1])], [0b1010_0000, 0b1000_0000]);
  assert.deepEqual(
    timeseries.downsample([{ timestamp: -1, value: 1 }, { timestamp: 0, value: 2 }, { timestamp: 9, value: 4 }], 10, "max"),
    [{ timestamp: -10, value: 1 }, { timestamp: 0, value: 4 }]
  );
  const eiffelToArc = geo.haversineDistance(48.8584, 2.2945, 48.8738, 2.295);
  assert.ok(eiffelToArc > 1_600 && eiffelToArc < 1_800, String(eiffelToArc));
  const circle = JSON.parse(geo.circleToGeopoly(2.35, 48.85, 100, 8)) as [number, number][];
  assert.equal(circle.length, 9);
  assert.deepEqual(circle[0], circle[8]);
});

async function conformance(mode: DatabaseMode, path: string, endpoint?: string, tokenFile?: string): Promise<void> {
  const database = await Database.open({ path, mode, endpoint, tokenFile, durability: "performance" });
  try {
    const write = await database.kv.set("cache", "answer", "42", { ifVersion: 0n });
    assert.ok(write.version! > 0n);
    const entry = await database.kv.get("cache", "answer");
    assert.equal(decoder.decode(entry?.value), "42");
    assert.equal(entry?.version, write.version);

    const put = await database.json.put("people", "ada", { name: "Ada" }, { ifVersion: 0n });
    assert.ok(put.version! > write.version!, "versions are engine sequences");
    assert.deepEqual((await database.json.get("people", "ada"))?.value, { name: "Ada" });
    await assert.rejects(
      database.batch([
        { model: "json_put", space: "people", id: "grace", value: { name: "Grace" } },
        { model: "json_delete", space: "people", id: "missing", ifVersion: 1n },
      ]),
      (error: unknown) => error instanceof LiliaError && error.code === "CONFLICT" && error.retryable
    );
    assert.equal(await database.json.get("people", "grace"), undefined, "a failed batch writes nothing");

    const streamed: JsonEntry[] = [];
    for await (const document of database.json.scanStream("people", { batchSize: 1 })) streamed.push(document);
    assert.deepEqual(streamed.map((d) => d.id), ["ada"]);

    const rows = await database.sql.query<{ id: string; name: string }>(
      "SELECT id, value->>'name' AS name FROM lilia_json WHERE space = $1",
      ["people"]
    );
    assert.deepEqual(rows, [{ id: "ada", name: "Ada" }]);
    const raw = await database.sql.raw("SELECT id FROM people");
    assert.deepEqual(raw.columns, ["id"]);
    assert.deepEqual(raw.rows, [["ada"]]);
    await assert.rejects(database.sql.query("SELECT * FROM missing_space.nope"), LiliaError);

    await database.graph.setNode("workspace", "a", "service", { env: "prod" });
    await database.graph.setNode("workspace", "b", "database");
    await database.graph.addEdge("workspace", "a", "b", "depends_on");
    assert.deepEqual(await database.graph.neighbors("workspace", "a", { direction: "outgoing" }), ["b"]);
    assert.deepEqual(await database.graph.reachability("workspace", ["a"]), ["a", "b"]);

    await database.fts.index("articles", "art1", "hello world search in liliadb");
    const hits = await database.fts.search("articles", "world");
    assert.deepEqual(hits.map((h) => h.docId), ["art1"]);

    await database.vector.set("docs", "v1", [1, 0, 0]);
    await database.vector.set("docs", "v2", [0, 1, 0]);
    const near = await database.vector.search("docs", [0.95, 0.05, 0], { limit: 1 });
    assert.deepEqual(near.map((h) => h.id), ["v1"]);
    await database.vector.set("articles", "art1", [1, 0, 0]);
    const hybrid = await database.hybrid.search("articles", { query: "hello", vector: [1, 0, 0], limit: 5 });
    assert.equal(hybrid[0]?.id, "art1");
    assert.ok(hybrid[0]!.rrfScore > 0 && hybrid[0]!.ftsRank === 1 && hybrid[0]!.vectorRank === 1);

    await database.json.createIndex("users", "role");
    await database.json.createIndex("users", "active");
    for (const [id, role, active] of [["u1", "admin", true], ["u2", "member", true], ["u3", "admin", false]] as const) {
      await database.json.put("users", id, { role, active });
    }
    assert.deepEqual((await database.json.listIndexes("users")).map((i) => i.indexName).sort(), ["active", "role"]);
    assert.deepEqual((await database.json.find("users", { role: "admin" })).map((d) => d.id), ["u1", "u3"]);
    assert.deepEqual((await database.json.find("users", { role: "admin", active: true })).map((d) => d.id), ["u1"]);
    assert.deepEqual((await database.json.findByIndex("users", "role", "member")).map((d) => d.id), ["u2"]);
    const admins: JsonEntry[] = [];
    for await (const document of database.json.findStream("users", { role: "admin" }, { batchSize: 1 })) admins.push(document);
    assert.deepEqual(admins.map((d) => d.id), ["u1", "u3"]);
    assert.equal(await database.json.dropIndex("users", "role"), true);

    assert.equal(await database.integrityCheck(), true);
  } finally {
    await database.close();
  }
  await assert.rejects(database.integrityCheck(), (error: unknown) => error instanceof LiliaError && error.code === "CLOSED");
}

test("embedded conformance", { skip: !native }, async () => {
  await withDirectory("lilia-node-", (directory) => conformance("embedded", join(directory, "embedded")));
});

test("daemon conformance over MessagePack", { skip: !daemonBinary }, async () => {
  await withDirectory("lilia-daemon-", async (directory) => {
    const path = join(directory, "db");
    const endpoint = process.platform === "win32" ? `\\\\.\\pipe\\liliadb-test-${process.pid}` : join(directory, "d.sock");
    const tokenFile = join(directory, "token");
    const daemon = spawn(daemonBinary!, ["--database", path, "--endpoint", endpoint, "--token-file", tokenFile], {
      stdio: ["ignore", "inherit", "inherit"],
    });
    try {
      await waitUntilServing(path, endpoint, tokenFile, daemon);
      await conformance("daemon", path, endpoint, tokenFile);
      const database = await Database.open({ path, mode: "daemon", endpoint, tokenFile });
      assert.throws(() => database.sync, (error: unknown) => error instanceof LiliaError && error.code === "UNSUPPORTED");
      await database.close();
    } finally {
      daemon.kill();
    }
  });
});

test("the synchronous API runs the same calls in place", { skip: !native }, async () => {
  await withDirectory("lilia-sync-", async (directory) => {
    const db = await Database.open({ path: join(directory, "db"), durability: "performance" });
    const set = db.sync.kv.set("cfg", "k1", "v1");
    assert.equal(set.deleted, false);
    assert.equal(db.sync.kv.get("cfg", "k1")?.version, set.version);
    assert.equal(db.sync.kv.scan("cfg").length, 1);
    assert.equal(db.sync.kv.delete("cfg", "k1").deleted, true);
    assert.equal(db.sync.kv.get("cfg", "k1"), undefined);

    db.sync.json.put("docs", "d1", { title: "Fast", count: 42 });
    assert.deepEqual(db.sync.json.get("docs", "d1")?.value, { title: "Fast", count: 42 });
    const rows = db.sync.sql.query<{ id: string }>("SELECT id FROM lilia_json WHERE space = $1", ["docs"]);
    assert.deepEqual(rows, [{ id: "d1" }]);
    const results = db.sync.batch([
      { model: "kv_set", namespace: "b", key: encoder.encode("k"), value: encoder.encode("v") },
      { model: "json_put", space: "b", id: "j", value: { ok: true } },
    ]);
    assert.equal(results.length, 2);
    await db.close();
    assert.throws(() => db.sync, (error: unknown) => error instanceof LiliaError && error.code === "CLOSED");
  });
});

test("every model answers through the embedded transport", { skip: !native }, async () => {
  await withDirectory("lilia-models-", async (directory) => {
    await using db = await Database.open({ path: join(directory, "db"), durability: "performance", undoDepth: 8 });

    await db.json.put("steps", "s1", { title: "start" });
    for (const id of ["s1", "s2", "s3"]) await db.graph.setNode("flow", id, "step", {}, { docSpace: "steps" });
    for (const [source, target] of [["s1", "s2"], ["s2", "s3"], ["s3", "s1"]] as const) {
      await db.graph.addEdge("flow", source, target, "next", { weight: 2 });
    }
    assert.deepEqual(await db.graph.getNodeDocument("flow", "s1"), { title: "start" });
    assert.equal((await db.graph.getNode("flow", "s2"))?.docSpace, "steps");
    assert.deepEqual((await db.graph.shortestPath("flow", "s1", "s3"))?.nodes, ["s1", "s2", "s3"]);
    assert.equal((await db.graph.pagerank("flow")).length, 3);
    assert.equal((await db.graph.centrality("flow", "degree", { direction: "both" })).length, 3);
    assert.equal(await db.graph.hasCycle("flow"), true);
    assert.equal((await db.graph.connectedComponents("flow", "strongly_connected")).length, 1);
    const cypher = await db.graph.cypher<{ id: string }>("flow", "MATCH (s:step) RETURN s.id AS id");
    assert.deepEqual(cypher.records.map((r) => r.id).sort(), ["s1", "s2", "s3"]);
    assert.equal((await db.graph.listEdges("flow")).length, 3);

    await db.fts.index("notes", "n1", "persistent storage engine");
    assert.ok((await db.fts.suggest("notes", "stor")).some((s) => s.term === "storage"));
    assert.deepEqual((await db.fts.fuzzySearch("notes", "storaje")).map((h) => h.docId), ["n1"]);

    await db.vector.createIndex("emb", { dim: 2, metric: "l2" });
    assert.equal((await db.vector.getIndexInfo("emb"))?.dim, 2);
    await db.vector.set("emb", "e1", [1, 2]);
    assert.deepEqual([...new Float32Array((await db.vector.get("emb", "e1"))!.buffer)], [1, 2]);
    assert.equal(await db.vector.dropIndex("emb"), true);

    await db.timeseries.appendPoints("cpu", Array.from({ length: 10 }, (_, i) => ({ timestamp: i * 1_000, value: i })));
    assert.equal((await db.timeseries.queryStats("cpu", 0, 10_000))?.count, 10);
    await db.timeseries.setPolicy({ seriesPattern: "cpu*", rules: [{ resolutionMs: 0, retentionMs: 86_400_000, aggregation: "avg" }] });
    assert.deepEqual(await db.timeseries.listPolicies(), [
      { seriesPattern: "cpu*", rules: [{ resolutionMs: 0, retentionMs: 86_400_000, aggregation: "avg" }] },
    ]);
    assert.equal((await db.timeseries.applyRetention()).seriesProcessed >= 0, true);

    const square = "[[2.0,48.0],[3.0,48.0],[3.0,49.0],[2.0,49.0],[2.0,48.0]]";
    await db.geo.addFence("zone", "Zone", "test", square);
    assert.deepEqual((await db.geo.findFencesForPoint(2.5, 48.5)).map((f) => f.id), ["zone"]);
    const crossings = await db.geo.recordGpsPoints("t", [
      { timestamp: 1_000, latitude: 48.5, longitude: 2.5 },
      { timestamp: 2_000, latitude: 48.5, longitude: 2.6 },
    ]);
    assert.ok(crossings.length >= 1);
    assert.equal((await db.geo.getTrackPoints("t")).length, 2);
    assert.ok((await db.geo.trackDistance("t")) > 5_000);

    await db.crdt.counter.increment("c", "hits", "n1", 3);
    assert.equal(await db.crdt.counter.get("c", "hits"), 3);
    assert.equal(await db.crdt.counter.merge("c", "hits", await db.crdt.counter.state("c", "hits")), 3);
    await db.crdt.set.add("c", "tags", "n1", "x");
    assert.deepEqual(await db.crdt.set.read("c", "tags"), ["x"]);
    await db.crdt.json.setField("c", "doc", "n1", "title", "draft");
    assert.deepEqual(await db.crdt.json.get("c", "doc"), { title: "draft" });
    const state = await db.crdt.json.state("c", "doc");
    assert.equal((await db.crdt.json.merge("c", "doc", state!)).version, undefined, "merging its own state changes nothing");
    const remote = structuredClone(state!);
    remote.fields["/title"] = { value: "final", clock: { clock: { timestampMs: Date.now() + 60_000, counter: 0 }, actorId: "n2" }, isDeleted: false };
    assert.ok((await db.crdt.json.merge("c", "doc", remote)).version! > 0n);
    assert.deepEqual(await db.crdt.json.get("c", "doc"), { title: "final" });

    const first = await db.json.put("docs", "d", { n: 1 });
    const second = await db.json.put("docs", "d", { n: 2 });
    assert.deepEqual((await db.json.getAtVersion("docs", "d", first.version!))?.value, { n: 1 });
    assert.deepEqual((await db.json.history("docs", "d")).map((r) => r.document?.value), [{ n: 1 }, { n: 2 }]);
    assert.notEqual(await db.json.diff("docs", "d", first.version!, second.version!), null);
    const undone = await db.undo();
    assert.ok(undone && undone.version > first.version! && undone.version <= second.version!, "the commit of the second write");
    assert.deepEqual((await db.json.get("docs", "d"))?.value, { n: 1 });
    assert.ok((await db.redo()) !== undefined);
    assert.deepEqual((await db.json.get("docs", "d"))?.value, { n: 2 });
    assert.ok((await db.undoHistory()).length > 0);

    await db.checkpoint();
    const stats = await db.storageStats();
    assert.ok(stats.levels.some((level) => level.runs > 0) && stats.version >= second.version!);
    assert.equal(await db.quickCheck(), true);
  });
});

test("close drains submitted writes and reopening finds them", { skip: !native }, async () => {
  await withDirectory("lilia-close-", async (directory) => {
    const path = join(directory, "db");
    const database = await Database.open({ path });
    const writes = Array.from({ length: 16 }, (_, index) => database.json.put("drain", String(index), { index }));
    await database.close();
    await Promise.all(writes);
    await using reopened = await Database.open({ path });
    assert.equal((await reopened.json.scan("drain")).length, 16);
    assert.equal(await reopened.integrityCheck(), true);
  });
});

test("await using closes the database at the end of its scope", { skip: !native }, async () => {
  await withDirectory("lilia-using-", async (directory) => {
    let escaped!: Database;
    {
      await using db = await Database.open({ path: join(directory, "db") });
      escaped = db;
      await db.json.put("test", "id1", { val: 123 });
      assert.equal((await db.json.get<{ val: number }>("test", "id1"))?.value.val, 123);
    }
    await assert.rejects(escaped.json.get("test", "id1"), (error: unknown) => error instanceof LiliaError && error.code === "CLOSED");
  });
});

test("tagged SQL templates bind their values as parameters", { skip: !native }, async () => {
  await withDirectory("lilia-sql-", async (directory) => {
    await using db = await Database.open({ path: join(directory, "db") });
    await db.json.put("users", "u1", { name: "Alice", role: "admin" });
    await db.json.put("users", "u2", { name: "Bob", role: "guest" });
    const role = "admin' OR '1'='1";
    assert.deepEqual(await db.sql<{ id: string }>`SELECT id FROM users WHERE json_get_str(value, 'role') = ${role}`, []);
    const rows = await db.sql<{ name: string }>`SELECT value->>'name' AS name FROM users WHERE json_get_str(value, 'role') = ${"admin"}`;
    assert.deepEqual(rows, [{ name: "Alice" }]);
    const syncRows = db.sync.sql<{ name: string }>`SELECT value->>'name' AS name FROM users WHERE json_get_str(value, 'role') = ${"guest"}`;
    assert.deepEqual(syncRows, [{ name: "Bob" }]);
  });
});

test("an aborted signal rejects before the call starts", { skip: !native }, async () => {
  await withDirectory("lilia-abort-", async (directory) => {
    await using db = await Database.open({ path: join(directory, "db") });
    await assert.rejects(db.json.get("test", "id", { signal: AbortSignal.abort(new Error("pre-aborted")) }), /pre-aborted/);
  });
});

test("web streams and string keys follow the platform's conventions", { skip: !native }, async () => {
  await withDirectory("lilia-web-", async (directory) => {
    await using db = await Database.open({ path: join(directory, "db") });
    for (let i = 0; i < 10; i++) await db.json.put("items", `id_${i}`, { index: i });
    const documents = db.json.readableStream<{ index: number }>("items", { batchSize: 3 });
    assert.ok(documents instanceof ReadableStream);
    const indexes: number[] = [];
    for await (const document of documents) indexes.push(document.value.index);
    assert.deepEqual(indexes, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);

    await db.kv.set("users", "user:alice", "developer");
    const entry = await db.kv.get("users", "user:alice");
    assert.ok(isKvEntry(entry));
    assert.equal(decoder.decode(entry.value), "developer");
    await db.json.put("profiles", "p1", { name: "Alice", age: 30 });
    assert.ok(isJsonEntry(await db.json.get("profiles", "p1")));
    const error = new LiliaError({ message: "top", cause: new Error("under") });
    assert.ok(LiliaError.isLiliaError(error) && error.cause instanceof Error);
  });
});

/** Waits until the daemon of `path` accepts a client. */
async function waitUntilServing(path: string, endpoint: string, tokenFile: string, daemon: ChildProcess): Promise<void> {
  const deadline = Date.now() + 10_000;
  for (;;) {
    if (daemon.exitCode !== null) throw new Error(`daemon exited with ${daemon.exitCode}`);
    try {
      const database = await Database.open({ path, mode: "daemon", endpoint, tokenFile });
      await database.close();
      return;
    } catch (error) {
      if (Date.now() > deadline) throw error;
      await new Promise((resolve) => setTimeout(resolve, 25));
    }
  }
}
