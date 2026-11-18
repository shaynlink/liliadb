import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Database } from "./index.js";

async function withDb(fn: (db: Database, path: string) => Promise<void>) {
  const dir = await mkdtemp(join(tmpdir(), "lilia-cas-"));
  const dbPath = join(dir, "test.db");
  const db = await Database.open({
    path: dbPath,
    casOptions: {
      enabled: true,
      thresholdBytes: 65536,
    },
  });
  try {
    await fn(db, dbPath);
  } finally {
    await db.close();
    await rm(dir, { recursive: true, force: true });
  }
}

test("CAS Engine: blobStore, blobGet, blobExists, blobPath and blobGc lifecycle", async () => {
  await withDb(async (db, dbPath) => {
    const payload = new Uint8Array(80_000);
    payload.fill(0x5a);

    // 1. Store blob
    const desc = await db.blobStore(payload, "application/octet-stream");
    assert.ok(desc.hash && desc.hash.length === 64);
    assert.equal(desc.sizeBytes, 80_000);
    assert.equal(desc.mimeType, "application/octet-stream");
    assert.equal(desc.$blob, desc.hash);

    // 2. Exists & Path
    assert.equal(db.blobExists(desc.hash), true);
    const path = db.blobPath(desc.hash);
    assert.ok(existsSync(path), `File should exist on disk at ${path}`);

    // 3. Get blob
    const retrieved = await db.blobGet(desc.hash);
    assert.ok(retrieved);
    assert.equal(retrieved.length, 80_000);
    assert.deepEqual(retrieved, payload);

    // 4. Garbage Collect (orphan blob since no KV/JSON references point to it)
    const stats = await db.blobGc();
    assert.equal(stats.orphansPurged, 1);
    assert.equal(stats.bytesReclaimed, 80_000);

    // 5. Exists after GC
    assert.equal(db.blobExists(desc.hash), false);
    assert.equal(existsSync(path), false);
  });
});

test("CAS Engine: synchronous blob APIs on db.sync.blob", async () => {
  await withDb(async (db) => {
    const payload = new Uint8Array(70_000);
    payload.fill(0x7b);

    const desc = db.sync.blob.store(payload, "text/plain");
    assert.ok(desc.hash);
    assert.equal(desc.sizeBytes, 70_000);

    assert.equal(db.sync.blob.exists(desc.hash), true);
    const retrieved = db.sync.blob.get(desc.hash);
    assert.ok(retrieved);
    assert.deepEqual(retrieved, payload);

    const stats = db.sync.blob.gc();
    assert.equal(stats.orphansPurged, 1);
    assert.equal(stats.bytesReclaimed, 70_000);
    assert.equal(db.sync.blob.exists(desc.hash), false);
  });
});

test("CAS Engine: transparent KV offload and reassembly", async () => {
  await withDb(async (db) => {
    const largeValue = new Uint8Array(100_000);
    for (let i = 0; i < largeValue.length; i++) {
      largeValue[i] = i % 256;
    }

    // Set large KV entry (exceeds 64 KB threshold)
    await db.kv.set("assets", "report.pdf", largeValue);

    // Retrieve via kv.get -> transparently dereferenced from CAS
    const entry = await db.kv.get("assets", "report.pdf");
    assert.ok(entry);
    assert.equal(entry.value.length, 100_000);
    assert.deepEqual(new Uint8Array(entry.value), largeValue);
  });
});
