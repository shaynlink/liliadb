import assert from "node:assert/strict";
import { join } from "node:path";
import test from "node:test";
import { AppState, excludedFromBackup, root } from "../tests/react-native.js";
import {
  changedTopics,
  decodeUtf8,
  LiliaDatabase,
  LiliaError,
  resolveDatabasePath,
  topics,
  type Operation,
} from "./index.js";

const encoder = new TextEncoder();
let opened = 0;

/** A database of its own for each test. */
function open(options: Parameters<typeof LiliaDatabase.open>[0] = {}): Promise<LiliaDatabase> {
  return LiliaDatabase.open({ name: `test-${++opened}`, durability: "performance", ...options });
}

test("a database opens in Application Support, excluded from backups, and runs every call", async () => {
  const db = await open({ name: "models" });
  assert.equal(db.path, join(root, "applicationSupport", "models.lilia"));
  assert.ok(excludedFromBackup.includes(db.path));

  const written = await db.kv.set("settings", "theme", "dark");
  const entry = await db.kv.get("settings", "theme");
  assert.equal(decodeUtf8(entry!.value), "dark");
  assert.equal(entry!.version, written.version);
  assert.equal(
    decodeUtf8(db.sync.kv.get("settings", "theme")!.value),
    "dark",
    "the synchronous API reads the same",
  );

  db.sync.json.put("users", "u1", { name: "Ada", age: 36 });
  assert.deepEqual((await db.json.get("users", "u1"))?.value, { name: "Ada", age: 36 });
  assert.deepEqual(
    (await db.json.find("users", { age: 36 })).map((document) => document.id),
    ["u1"],
  );

  await db.graph.addEdge("g", "a", "b", "follows");
  assert.deepEqual(await db.graph.neighbors("g", "a"), ["b"]);
  await db.fts.index("notes", "n1", "local first storage");
  assert.equal((await db.fts.search("notes", "storage"))[0]?.docId, "n1");
  assert.deepEqual(await db.sql.query("SELECT id FROM lilia_json WHERE space = $1", ["users"]), [
    { id: "u1" },
  ]);
  await db.close();
});

test("writes notify the topics they change, reads none", async () => {
  const db = await open();
  const heard: string[] = [];
  const listen = (topic: string) => db.subscribe(topic, () => heard.push(topic));
  listen(topics.kv("settings", "theme"));
  listen(topics.kv("settings", "other"));
  listen(topics.json("users"));
  listen(topics.table("lilia_kv"));
  listen("*");

  await db.kv.set("settings", "theme", "light");
  assert.deepEqual(heard.toSorted(), ["*", "kv:settings:7468656d65", "table:lilia_kv"]);
  heard.length = 0;
  await db.kv.get("settings", "theme");
  await db.sql.query("SELECT 1");
  assert.equal(heard.length, 0, "reads notify nothing");

  const unsubscribe = db.subscribe(topics.json("users", "u1"), () => heard.push("u1"));
  db.sync.batch([
    { model: "json_put", space: "users", id: "u1", value: { name: "Ada" } },
    { model: "kv_delete", namespace: "settings", key: encoder.encode("other") },
  ]);
  assert.deepEqual(heard.toSorted(), [
    "*",
    "json:users",
    "kv:settings:6f74686572",
    "table:lilia_kv",
    "u1",
  ]);
  unsubscribe();
  heard.length = 0;
  await db.json.put("users", "u1", { name: "Grace" });
  assert.deepEqual(heard.toSorted(), ["*", "json:users"], "an unsubscribed listener hears nothing");
  await db.close();
});

test("changedTopics names each model's topics", () => {
  const key = encoder.encode("k");
  const batch: Operation = {
    type: "batch",
    operations: [
      { model: "kv_set", namespace: "ns", key, value: key },
      { model: "graph_add_edge", graph: "g", source: "a", target: "b", kind: "k", weight: 1 },
      { model: "crdt_or_set_add", space: "s", id: "set", actor_id: "me", element: "x" },
    ],
  };
  assert.deepEqual(changedTopics(batch), [
    "kv:ns",
    "kv:ns:6b",
    "table:lilia_kv",
    "graph:g",
    "table:lilia_graph_edges",
    "crdt:s",
    "crdt:s:set",
  ]);
  assert.deepEqual(changedTopics({ type: "ts_append_points", series_id: "cpu", points: [] }), [
    "timeseries",
    "table:lilia_timeseries",
    "timeseries:cpu",
  ]);
  assert.equal(changedTopics({ type: "undo" }), undefined, "an undo may change anything");
  assert.deepEqual(changedTopics({ type: "kv_get", namespace: "ns", key }), []);
});

test("close refuses later calls, clears the listeners, and checkpoints happen in the background", async () => {
  const db = await open();
  await db.kv.set("a", "b", "c");
  AppState.emit("background");
  let heard = 0;
  db.subscribe("*", () => heard++);
  await db.close();
  assert.ok(db.isClosed);
  await assert.rejects(
    db.kv.get("a", "b"),
    (error: unknown) => error instanceof LiliaError && error.code === "CLOSED",
  );

  const reopened = await LiliaDatabase.open({ path: db.path });
  assert.equal(
    decodeUtf8((await reopened.kv.get("a", "b"))!.value),
    "c",
    "the write survived the close",
  );
  await reopened.close();
  assert.equal(heard, 0);
});

test("in development, open returns the database already open at the path", async () => {
  const development = globalThis as { __DEV__?: boolean };
  development.__DEV__ = true;
  try {
    const first = await open({ name: "shared" });
    assert.equal(await LiliaDatabase.open({ name: "shared" }), first);
    await first.close();
    const second = await LiliaDatabase.open({ name: "shared" });
    assert.notEqual(second, first, "a closed database is not reused");
    await second.close();
  } finally {
    delete development.__DEV__;
  }
});

test("paths follow the platform options", () => {
  assert.equal(resolveDatabasePath({ name: "app" }), join(root, "applicationSupport", "app.lilia"));
  assert.equal(
    resolveDatabasePath({ name: "app.lilia", ios: { directory: "caches" } }),
    join(root, "caches", "app.lilia"),
  );
  assert.equal(
    resolveDatabasePath({ name: "w", ios: { groupContainerIdentifier: "group.test" } }),
    join(root, "documents", "w.lilia"),
  );
  assert.equal(resolveDatabasePath({ path: "/explicit" }), "/explicit");
  assert.throws(
    () => resolveDatabasePath({ ios: { groupContainerIdentifier: "group.other" } }),
    LiliaError,
  );
  assert.throws(() => resolveDatabasePath({ path: " " }), LiliaError);
});

test("open reports the engine's errors as LiliaErrors", async () => {
  await assert.rejects(
    LiliaDatabase.open({ name: "bad", durability: "fast" as never }),
    (error: unknown) => error instanceof LiliaError && error.code === "INVALID_INPUT",
  );
});

test("decodeUtf8 reads what TextEncoder writes, and each malformed byte as U+FFFD", () => {
  for (const text of ["", "plain", "é à ç", "日本語", "emoji 🎉 and 𝄞", "\u0000￿"]) {
    assert.equal(decodeUtf8(encoder.encode(text)), text);
  }
  const long = "x🎉".repeat(5_000);
  assert.equal(decodeUtf8(encoder.encode(long)), long, "past the chunk size");
  assert.equal(decodeUtf8(new Uint8Array([0x61, 0xff, 0x62])), "a�b");
  assert.equal(decodeUtf8(new Uint8Array([0xe2, 0x82])), "��", "a cut sequence");
  assert.equal(decodeUtf8(new Uint8Array([0xc0, 0xaf])), "��", "an overlong encoding");
  assert.equal(decodeUtf8(new Uint8Array([0xed, 0xa0, 0x80])), "���", "a surrogate");
});
