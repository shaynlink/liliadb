// Node.js surfaces of the local databases LiliaDB is compared with: the SQLite drivers
// (better-sqlite3, node:sqlite, libsql, sqlite3), LMDB, LevelDB, PGlite, DuckDB and SurrealDB.
// Every adapter commits each `put` as one transaction in the database's most durable
// configuration, and declares the sync primitive that configuration issues (checked against the
// measured commit latency by the report).

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";

import {
  platformSync,
  SQLITE_GET,
  SQLITE_UPSERT,
  sqliteAdapter,
  sqliteInfo,
  sqliteSetup,
} from "./harness.mjs";

const require = createRequire(import.meta.url);

/** Version of an installed dependency of this package (read from disk: not every package exports its `package.json`). */
function packageVersion(name) {
  return JSON.parse(
    readFileSync(join(import.meta.dirname, "../node_modules", name, "package.json"), "utf8"),
  ).version;
}

/** The SQLite drivers: one surface per driver and configuration (`-fullfsync` for power-safe). */
export const SQLITE_DRIVERS = {
  "better-sqlite3": (dir, fullfsync) => {
    const Database = require("better-sqlite3");
    const db = new Database(join(dir, "bench.sqlite"));
    const surface = {
      id: "better-sqlite3",
      driver: "better-sqlite3",
      runtime: "Node",
      version: packageVersion("better-sqlite3"),
      fullfsync,
      platform: process.platform,
    };
    return sqliteAdapter(surface, db, () => db.close());
  },
  "node-sqlite": async (dir, fullfsync) => {
    const { DatabaseSync } = await import("node:sqlite");
    const db = new DatabaseSync(join(dir, "bench.sqlite"));
    const surface = {
      id: "node-sqlite",
      driver: "node:sqlite",
      runtime: "Node",
      version: `Node ${process.version}`,
      fullfsync,
      platform: process.platform,
    };
    return sqliteAdapter(surface, db, () => db.close());
  },
  libsql: (dir, fullfsync) => {
    const Database = require("libsql");
    const db = new Database(join(dir, "bench.sqlite"));
    // libsql 0.5.29 panics on a statement given a single `Uint8Array` argument; an array binds it.
    const surface = {
      id: "libsql",
      driver: "libsql",
      runtime: "Node",
      version: packageVersion("libsql"),
      fullfsync,
      platform: process.platform,
      arrayParams: true,
    };
    return sqliteAdapter(surface, db, () => db.close());
  },
  sqlite3: (dir, fullfsync) => sqlite3Driver(dir, fullfsync),
};

/** Promise over a node-sqlite3 call that reports through a trailing callback. */
function call(target, method, ...args) {
  return new Promise((done, fail) => {
    target[method](...args, function (error, row) {
      if (error) fail(error);
      else done(row === undefined ? this : row);
    });
  });
}

/** node-sqlite3 (`sqlite3` on npm): the asynchronous driver, every call on its worker thread. */
async function sqlite3Driver(dir, fullfsync) {
  const sqlite3 = require("sqlite3");
  const db = await new Promise((done, fail) => {
    const opened = new sqlite3.Database(join(dir, "bench.sqlite"), (error) =>
      error ? fail(error) : done(opened),
    );
  });
  for (const sql of sqliteSetup(fullfsync)) {
    await call(db, "exec", sql);
  }
  const upsert = await new Promise((done, fail) => {
    const statement = db.prepare(SQLITE_UPSERT, (error) => (error ? fail(error) : done(statement)));
  });
  const get = await new Promise((done, fail) => {
    const statement = db.prepare(SQLITE_GET, (error) => (error ? fail(error) : done(statement)));
  });
  return {
    info: sqliteInfo(
      "sqlite3",
      "sqlite3",
      "Node",
      packageVersion("sqlite3"),
      sqlite3.VERSION,
      fullfsync,
      process.platform,
    ),
    async put(items) {
      await call(db, "exec", "BEGIN IMMEDIATE");
      try {
        for (const [k, v] of items) {
          await call(upsert, "run", k, v);
        }
        await call(db, "exec", "COMMIT");
      } catch (error) {
        await call(db, "exec", "ROLLBACK");
        throw error;
      }
    },
    // A statement's `get` resolves to the statement itself when there is no row.
    get: async (k) => (await call(get, "get", k)) !== get,
    async close() {
      await call(upsert, "finalize");
      await call(get, "finalize");
      await call(db, "close");
    },
  };
}

/**
 * LMDB (lmdb-js). `overlappingSync: false`: by default lmdb-js acknowledges a commit before its
 * sync completes; off, each commit returns after `mdb_txn_commit` has flushed (`F_FULLFSYNC` on
 * Apple platforms, `MDB_FDATASYNC` in `mdb.c`).
 */
export async function lmdb(dir) {
  const { open } = require("lmdb");
  const db = open({
    path: join(dir, "lmdb"),
    keyEncoding: "binary",
    encoding: "binary",
    compression: false,
    overlappingSync: false,
  });
  return {
    info: {
      id: "lmdb-js",
      name: "lmdb-js (Node)",
      version: packageVersion("lmdb"),
      sync: platformSync(process.platform, true),
    },
    put(items) {
      db.transactionSync(() => {
        for (const [k, v] of items) {
          db.putSync(Buffer.from(k), Buffer.from(v));
        }
      });
    },
    get: (k) => db.getBinary(Buffer.from(k)) !== undefined,
    close: () => db.close(),
  };
}

/** LevelDB through classic-level, `sync: true` writes. Its LevelDB 1.20 syncs with `fsync`/`fdatasync`. */
export async function classicLevel(dir) {
  const { ClassicLevel } = require("classic-level");
  const db = new ClassicLevel(join(dir, "leveldb"), { keyEncoding: "view", valueEncoding: "view" });
  await db.open();
  return {
    info: {
      id: "classic-level",
      name: "LevelDB (classic-level, Node)",
      version: packageVersion("classic-level"),
      sync: platformSync(process.platform, false),
    },
    put: (items) =>
      db.batch(
        items.map(([key, value]) => ({ type: "put", key, value })),
        { sync: true },
      ),
    get: async (k) => (await db.get(k)) !== undefined,
    close: () => db.close(),
  };
}

/**
 * PGlite: Postgres compiled to WebAssembly, on the Node file system. Its Emscripten `NODEFS` has no
 * `fsync`, so Postgres's syncs return without reaching the disk: no configuration is durable.
 */
export async function pglite(dir) {
  const { PGlite } = await import("@electric-sql/pglite");
  const db = await PGlite.create(join(dir, "pglite"));
  await db.exec("CREATE TABLE kv(k bytea PRIMARY KEY, v bytea NOT NULL)");
  const upsert = "INSERT INTO kv(k, v) VALUES($1, $2) ON CONFLICT(k) DO UPDATE SET v = excluded.v";
  const postgres = (await db.query("SHOW server_version")).rows[0].server_version;
  return {
    info: {
      id: "pglite",
      name: "PGlite (Node)",
      version: `${packageVersion("@electric-sql/pglite")} (Postgres ${postgres})`,
      sync: "none",
    },
    put: (items) =>
      db.transaction(async (tx) => {
        for (const [k, v] of items) {
          await tx.query(upsert, [k, v]);
        }
      }),
    get: async (k) =>
      (await db.query("SELECT length(v) AS n FROM kv WHERE k = $1", [k])).rows.length === 1,
    close: () => db.close(),
  };
}

/** DuckDB, `fsync_mode = FULL` (`F_FULLFSYNC` on Apple platforms; the default stops at `fsync`). */
export async function duckdb(dir) {
  const { DuckDBInstance } = await import("@duckdb/node-api");
  const instance = await DuckDBInstance.create(join(dir, "bench.duckdb"), { fsync_mode: "FULL" });
  const connection = await instance.connect();
  await connection.run("CREATE TABLE kv(k BLOB PRIMARY KEY, v BLOB NOT NULL)");
  const upsert = await connection.prepare("INSERT OR REPLACE INTO kv VALUES ($1, $2)");
  const get = await connection.prepare("SELECT octet_length(v) AS n FROM kv WHERE k = $1");
  const version = (await connection.runAndReadAll("SELECT version() AS v")).getRowObjects()[0].v;
  return {
    info: {
      id: "duckdb",
      name: "DuckDB (Node)",
      version: `${packageVersion("@duckdb/node-api")} (DuckDB ${version})`,
      sync: platformSync(process.platform, true),
    },
    async put(items) {
      await connection.run("BEGIN TRANSACTION");
      try {
        for (const [k, v] of items) {
          upsert.bindBlob(1, k);
          upsert.bindBlob(2, v);
          await upsert.run();
        }
        await connection.run("COMMIT");
      } catch (error) {
        await connection.run("ROLLBACK");
        throw error;
      }
    },
    async get(k) {
      get.bindBlob(1, k);
      return (await get.runAndReadAll()).currentRowCount === 1;
    },
    close() {
      connection.closeSync();
      instance.closeSync();
    },
  };
}

const hex = (bytes) => Buffer.from(bytes).toString("hex");
/**
 * SurrealDB embedded on SurrealKV, `sync=every`: each commit syncs its log before returning
 * (`File::sync_all`, so `F_FULLFSYNC` on Apple platforms). One `FOR` statement is one transaction.
 */
export async function surrealdb(dir) {
  const { Surreal, RecordId } = await import("surrealdb");
  const { createNodeEngines, engineVersion } = await import("@surrealdb/node");
  const db = new Surreal({ engines: createNodeEngines() });
  await db.connect(`surrealkv://${join(dir, "surrealkv")}?sync=every`);
  await db.use({ namespace: "bench", database: "bench" });
  return {
    info: {
      id: "surrealdb",
      name: "SurrealDB (SurrealKV, Node)",
      version: `${packageVersion("@surrealdb/node")} (engine ${engineVersion()})`,
      sync: platformSync(process.platform, true),
    },
    async put(items) {
      const rows = items.map(([k, v]) => ({ k: hex(k), v }));
      await db.query("FOR $r IN $rows { UPSERT type::record('kv', $r.k) SET v = $r.v; }", { rows });
    },
    get: async (k) => (await db.select(new RecordId("kv", hex(k)))) != null,
    close: () => db.close(),
  };
}
