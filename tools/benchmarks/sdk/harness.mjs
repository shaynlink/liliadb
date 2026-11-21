// Workloads shared by every JavaScript surface (Node, Bun, Deno). Each surface module provides an
// adapter; the harness times it and prints result lines in the schema of `tools/lilia-bench`
// (`Line` in `src/record.rs`), one JSON object per line on standard output.

export const PROFILES = {
  quick: { records: 20_000, budgetMs: 1_000, valueSize: 256 },
  full: { records: 100_000, budgetMs: 5_000, valueSize: 256 },
};

const FRESH_KEYS = 1_000_000_000;
const SAMPLE_EVERY = 8;

/** 16-byte key: 8 bytes of hashed prefix, then the index (big-endian). */
export function key(i) {
  const k = new Uint8Array(16);
  const view = new DataView(k.buffer);
  let h = Math.imul(i ^ 0x5bd1e995, 0x9e3779b1) >>> 0;
  h = Math.imul(h ^ (h >>> 15), 0x85ebca6b) >>> 0;
  view.setUint32(0, h);
  view.setUint32(4, Math.imul(h ^ (h >>> 13), 0xc2b2ae35) >>> 0);
  view.setUint32(8, Math.floor(i / 2 ** 32));
  view.setUint32(12, i >>> 0);
  return k;
}

/** `size` bytes of compressible text derived from `i`. */
export function value(i, size) {
  const alphabet = "abcdefghijklmnopqrstuvwxyz0123456789 ,.:{}";
  const v = new Uint8Array(size);
  let x = (i * 2654435761) >>> 0;
  for (let j = 0; j < size; j++) {
    x = (Math.imul(x, 1103515245) + 12345) >>> 0;
    v[j] = alphabet.charCodeAt(x % alphabet.length);
  }
  return v;
}

function latency(nanos) {
  if (nanos.length === 0) {
    return { samples: 0, mean_us: 0, p50_us: 0, p95_us: 0, p99_us: 0, p999_us: 0, max_us: 0 };
  }
  nanos.sort((a, b) => a - b);
  const at = (q) => nanos[Math.min(nanos.length - 1, Math.round(q * (nanos.length - 1)))] / 1e3;
  const total = nanos.reduce((sum, n) => sum + n, 0);
  return {
    samples: nanos.length,
    mean_us: total / nanos.length / 1e3,
    p50_us: at(0.5),
    p95_us: at(0.95),
    p99_us: at(0.99),
    p999_us: at(0.999),
    max_us: nanos[nanos.length - 1] / 1e3,
  };
}

/** A `Line::Record` for the SDK suite. */
export function record(info, workload, params, ops, seconds, nanos, logicalBytes, extra = {}) {
  return {
    kind: "record",
    suite: "sdk",
    engine: info,
    workload,
    params,
    ops,
    seconds,
    ops_per_sec: seconds > 0 ? ops / seconds : 0,
    latency: latency(nanos),
    logical_bytes: logicalBytes,
    io: { device_bytes_written: 0, device_bytes_read: 0 },
    ...extra,
  };
}

/** A `Line::Failure` for the SDK suite. */
export function failure(engine, workload, reason) {
  return { kind: "failure", suite: "sdk", engine, workload, reason: String(reason) };
}

export function emit(line) {
  console.log(JSON.stringify(line));
}

/** `--name value` from the runtime's arguments. */
export function argument(args, name, fallback) {
  const at = args.indexOf(`--${name}`);
  return at >= 0 && at + 1 < args.length ? args[at + 1] : fallback;
}

/**
 * Runs the SDK workloads on `adapter`:
 * `{ info, put(items) -> void | Promise, get(key) -> boolean | Promise<boolean>, close() }`.
 * `put` receives `[key, value]` pairs and must commit them durably as one transaction.
 */
export async function runSurface(adapter, settings) {
  const { records, budgetMs, valueSize } = settings;
  const { info } = adapter;
  const payload = value(1, valueSize);
  try {
    await workloads(adapter, info, payload, records, budgetMs, valueSize);
  } finally {
    await adapter.close();
  }
}

async function workloads(adapter, info, payload, records, budgetMs, valueSize) {
  // write.commit: one key per transaction, until the budget runs out.
  {
    const nanos = [];
    const start = performance.now();
    let i = 0;
    while (performance.now() - start < budgetMs) {
      const t = performance.now();
      await adapter.put([[key(FRESH_KEYS + i), payload]]);
      nanos.push((performance.now() - t) * 1e6);
      i++;
    }
    const seconds = (performance.now() - start) / 1e3;
    emit(record(info, "write.commit", { threads: 1, value_size: valueSize }, i, seconds, nanos, i * (16 + valueSize)));
  }

  // write.batch: 100 keys per transaction.
  {
    const nanos = [];
    const start = performance.now();
    let written = 0;
    while (written < records && performance.now() - start < budgetMs) {
      const items = [];
      for (let j = 0; j < 100; j++) {
        items.push([key(2 * FRESH_KEYS + written + j), value(written + j, valueSize)]);
      }
      const t = performance.now();
      await adapter.put(items);
      nanos.push((performance.now() - t) * 1e6);
      written += 100;
    }
    const seconds = (performance.now() - start) / 1e3;
    emit(record(info, "write.batch", { batch: 100, value_size: valueSize }, written, seconds, nanos, written * (16 + valueSize)));
  }

  // load (not reported), then read.get of uniformly random loaded keys.
  for (let i = 0; i < records; i += 1_000) {
    const items = [];
    for (let j = i; j < Math.min(records, i + 1_000); j++) {
      items.push([key(j), value(j, valueSize)]);
    }
    await adapter.put(items);
  }
  const keys = Array.from({ length: records }, (_, i) => key(i));
  {
    const nanos = [];
    let reads = 0;
    let found = 0;
    let x = 0x9e3779b9;
    const start = performance.now();
    while (performance.now() - start < budgetMs) {
      x = (Math.imul(x, 1664525) + 1013904223) >>> 0;
      const k = keys[x % records];
      if (reads % SAMPLE_EVERY === 0) {
        const t = performance.now();
        found += (await adapter.get(k)) ? 1 : 0;
        nanos.push((performance.now() - t) * 1e6);
      } else {
        found += (await adapter.get(k)) ? 1 : 0;
      }
      reads++;
    }
    const seconds = (performance.now() - start) / 1e3;
    if (found !== reads) {
      throw new Error(`read.get found ${found} of ${reads} loaded keys`);
    }
    emit(record(info, "read.get", { threads: 1, state: "warm", records }, reads, seconds, nanos, reads * valueSize));
  }
}

/** SQLite table and pragmas identical to the Rust suite's `sqlite` engine. */
export const SQLITE_SETUP = [
  "PRAGMA journal_mode = WAL",
  "PRAGMA synchronous = FULL",
  "PRAGMA cache_size = -65536",
  "PRAGMA temp_store = MEMORY",
  "CREATE TABLE IF NOT EXISTS kv(k BLOB PRIMARY KEY, v BLOB NOT NULL) STRICT, WITHOUT ROWID",
];
export const SQLITE_UPSERT = "INSERT INTO kv(k, v) VALUES(?, ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v";
export const SQLITE_GET = "SELECT length(v) AS n FROM kv WHERE k = ?";

/** `fsync` stops at the drive cache on Apple platforms; elsewhere it is power-safe. */
export const SQLITE_SYNC = "fsync";

/**
 * Adapter over a synchronous SQLite driver (better-sqlite3, node:sqlite, bun:sqlite, @db/sqlite):
 * all expose `exec`, `prepare(sql).run/get`.
 */
export function sqliteAdapter(id, name, version, db, close) {
  for (const sql of SQLITE_SETUP) {
    db.exec(sql);
  }
  const upsert = db.prepare(SQLITE_UPSERT);
  const get = db.prepare(SQLITE_GET);
  return {
    info: { id, name, version: `${version} (SQLite ${db.prepare("SELECT sqlite_version() AS v").get().v})`, sync: SQLITE_SYNC },
    put(items) {
      db.exec("BEGIN IMMEDIATE");
      try {
        for (const [k, v] of items) {
          upsert.run(k, v);
        }
        db.exec("COMMIT");
      } catch (error) {
        db.exec("ROLLBACK");
        throw error;
      }
    },
    // Drivers return `undefined` (Node, Deno) or `null` (Bun) for no row.
    get: (k) => get.get(k) != null,
    close,
  };
}
