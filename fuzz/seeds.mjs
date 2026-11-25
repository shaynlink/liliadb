// Writes the seed corpus of the `request-msgpack` and `execute` fuzz targets: the operations
// `@liliadb/client` sends, captured from its API and encoded by its own encoder, so the fuzzer
// starts from what a real client sends. Build the client first:
//
//   pnpm --filter @liliadb/client build && node fuzz/seeds.mjs
//
// Seeds go to `fuzz/corpus/<target>/`, where `cargo fuzz run` reads its corpus.

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { Database, encodeWire } from "../packages/client/dist/index.js";

const corpus = join(dirname(fileURLToPath(import.meta.url)), "corpus");

/** A database whose transport records each operation instead of running it. */
class Recorder extends Database {
  operations = [];

  constructor() {
    const operations = [];
    super({
      execute: async (operation) => {
        operations.push(operation);
        throw new Error("recorded, not run");
      },
      close: async () => {},
    });
    this.operations = operations;
  }

  /** The operations `calls` send, in order. */
  async record(calls) {
    this.operations.length = 0;
    for (const call of calls) await call(this).catch(() => undefined);
    return [...this.operations];
  }
}

const recorder = new Recorder();
const polygon = JSON.stringify({
  type: "Polygon",
  coordinates: [
    [
      [2.29, 48.85],
      [2.3, 48.85],
      [2.3, 48.86],
      [2.29, 48.86],
      [2.29, 48.85],
    ],
  ],
});

/** Sequences of calls, each building state and reading it back. */
const sequences = {
  kv: [
    (db) => db.kv.set("settings", "theme", "dark"),
    (db) => db.kv.get("settings", "theme"),
    (db) => db.kv.scan("settings", { limit: 10 }),
    (db) => db.kv.delete("settings", "theme"),
  ],
  json: [
    (db) => db.json.put("users", "u1", { name: "Ada", role: "admin", tags: ["a", "b"] }),
    (db) => db.json.createIndex("users", "role"),
    (db) => db.json.findByIndex("users", "role", "admin"),
    (db) => db.json.find("users", { role: "admin" }),
    (db) => db.json.extract("users", "u1", "$.tags[0]"),
    (db) => db.json.history("users", "u1"),
  ],
  graph: [
    (db) => db.graph.setNode("social", "a", "user", { name: "a" }),
    (db) => db.graph.setNode("social", "b", "user"),
    (db) => db.graph.addEdge("social", "a", "b", "follows"),
    (db) => db.graph.neighbors("social", "a"),
    (db) => db.graph.shortestPath("social", "a", "b"),
    (db) => db.graph.pagerank("social"),
  ],
  fts: [
    (db) => db.fts.index("articles", "1", "LiliaDB stores documents and vectors"),
    (db) => db.fts.search("articles", "documents"),
    (db) => db.fts.fuzzySearch("articles", "documnts"),
  ],
  vector: [
    (db) => db.vector.createIndex("embeddings", { dim: 4, metric: "cosine" }),
    (db) => db.vector.set("embeddings", "v1", [0.1, 0.2, 0.3, 0.4]),
    (db) => db.vector.search("embeddings", [0.1, 0.2, 0.3, 0.4], { limit: 3 }),
  ],
  timeseries: [
    (db) =>
      db.timeseries.appendPoints("cpu", [
        { timestamp: 1_700_000_000_000, value: 0.5 },
        { timestamp: 1_700_000_001_000, value: 0.7 },
      ]),
    (db) => db.timeseries.queryRange("cpu", 1_700_000_000_000, 1_700_000_002_000),
  ],
  geo: [
    (db) => db.geo.addFence("paris", "Paris", "city", polygon),
    (db) => db.geo.findFencesForPoint(2.295, 48.855),
    (db) => db.geo.containsPoint(polygon, 2.295, 48.855),
  ],
  crdt: [
    (db) => db.crdt.counter.increment("likes", "post1", "actor-a", 3),
    (db) => db.crdt.set.add("tags", "post1", "actor-a", "rust"),
    (db) => db.crdt.json.setField("docs", "d1", "actor-a", "title", "Hello"),
    (db) => db.crdt.counter.get("likes", "post1"),
  ],
  sql: [
    (db) => db.kv.set("settings", "lang", "fr"),
    (db) => db.sql.query("SELECT * FROM lilia_kv WHERE namespace = $1", ["settings"]),
  ],
  maintenance: [
    (db) => db.kv.set("a", "b", "c"),
    (db) => db.checkpoint(),
    (db) => db.storageStats(),
    (db) => db.integrityCheck(),
  ],
};

function write(target, name, bytes) {
  const directory = join(corpus, target);
  mkdirSync(directory, { recursive: true });
  writeFileSync(join(directory, `seed-${name}`), bytes);
}

let count = 0;
for (const [name, calls] of Object.entries(sequences)) {
  const operations = await recorder.record(calls);
  write("execute", name, encodeWire(operations));
  for (const [index, operation] of operations.entries()) {
    const request = { id: `${name}-${index}`, token: "token", deadline_ms: 1000, operation };
    write("request-msgpack", `${name}-${index}`, encodeWire(request));
    count += 1;
  }
}
console.log(`${Object.keys(sequences).length} execute seeds, ${count} request seeds in ${corpus}`);
