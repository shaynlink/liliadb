import assert from "node:assert/strict";
import { mkdtemp, realpath, rm, access } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { connect } from "node:net";
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { test } from "bun:test";

import { Database } from "../src/index.js";

const candidateDaemon = [
  new URL("../../../target/debug/lilia-daemon", import.meta.url).pathname,
  new URL("../../../target/release/lilia-daemon", import.meta.url).pathname,
].find((p) => existsSync(p));

const daemonBinary = process.env.LILIA_DAEMON_BIN ?? candidateDaemon;

async function waitUntilReady(tokenFile: string, endpoint: string, daemon: ChildProcess): Promise<void> {
  const deadline = Date.now() + 5_000;
  while (Date.now() < deadline) {
    if (daemon.exitCode !== null) throw new Error(`daemon exited with ${daemon.exitCode}`);
    try {
      await access(tokenFile);
      await new Promise<void>((resolve, reject) => {
        const socket = connect(endpoint);
        socket.once("connect", () => { socket.destroy(); resolve(); });
        socket.once("error", (error) => { socket.destroy(); reject(error); });
      });
      return;
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 25));
    }
  }
  throw new Error("daemon startup timed out");
}

test("Bun: embedded client executes KV, JSON and SQL mutations", async () => {
  const dir = await realpath(await mkdtemp(join(tmpdir(), "bun-emb-")));
  const dbPath = join(dir, "embedded.lilia");

  try {
    const db = await Database.open({ path: dbPath });

    // KV operations
    const key = new TextEncoder().encode("user:1");
    const val = new TextEncoder().encode("Alice");
    const setRes = await db.kv.set("accounts", key, val);
    assert.ok(setRes.version !== undefined);

    const getRes = await db.kv.get("accounts", key);
    assert.ok(getRes !== undefined);
    assert.equal(new TextDecoder().decode(getRes.value), "Alice");

    // JSON operations
    const putRes = await db.json.put("profiles", "p1", { name: "Alice", active: true });
    assert.ok(putRes.version !== undefined);

    const jsonDoc = await db.json.get("profiles", "p1");
    assert.ok(jsonDoc !== undefined);
    assert.equal((jsonDoc.value as Record<string, unknown>).name, "Alice");

    // SQL analytics
    const sqlRes = await db.sql.query<{ id: string; name: string }>(
      "SELECT id, json_extract(value, '$.name') AS name FROM lilia_json WHERE space = 'profiles'"
    );
    assert.equal(sqlRes.length, 1);
    assert.equal(sqlRes[0]?.name, "Alice");

    await db.close();
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("Bun: embedded synchronous db.sync client executes KV, JSON and SQL mutations", async () => {
  const dir = await realpath(await mkdtemp(join(tmpdir(), "bun-sync-")));
  const dbPath = join(dir, "sync.lilia");

  try {
    const db = await Database.open({ path: dbPath });

    // Sync KV
    const key = new TextEncoder().encode("user:sync");
    const val = new TextEncoder().encode("Bob");
    const setRes = db.sync.kv.set("users", key, val);
    assert.equal(setRes.version, 1n);

    const getRes = db.sync.kv.get("users", key);
    assert.ok(getRes !== undefined);
    assert.equal(new TextDecoder().decode(getRes.value), "Bob");

    // Sync JSON
    const putRes = db.sync.json.put("items", "i1", { name: "Gadget", price: 99 });
    assert.equal(putRes.version, 1n);

    const doc = db.sync.json.get("items", "i1");
    assert.ok(doc !== undefined);
    assert.equal((doc.value as Record<string, unknown>).name, "Gadget");

    // Sync SQL
    const sqlRes = db.sync.sql.query<{ id: string; name: string }>(
      "SELECT id, json_extract(value, '$.name') AS name FROM lilia_json WHERE space = 'items'"
    );
    assert.equal(sqlRes.length, 1);
    assert.equal(sqlRes[0]?.name, "Gadget");

    await db.close();
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("Bun: daemon client connects over Unix domain socket IPC", async () => {
  if (!daemonBinary) {
    console.warn("Skipping Bun daemon test: lilia-daemon binary not found");
    return;
  }

  const dir = await realpath(await mkdtemp(join(tmpdir(), "bun-dae-")));
  const dbPath = join(dir, "d.lilia");
  const endpoint = `${dbPath}.sock`;
  const tokenFile = `${dbPath}.token`;

  const daemonProc = spawn(
    daemonBinary,
    ["--database", dbPath, "--endpoint", endpoint, "--token-file", tokenFile],
    { stdio: "ignore" }
  );

  try {
    await waitUntilReady(tokenFile, endpoint, daemonProc);

    const db = await Database.open({
      path: dbPath,
      mode: "daemon",
      endpoint,
      tokenFile,
    });

    const key = new TextEncoder().encode("config:bun");
    const val = new TextEncoder().encode("fast");
    await db.kv.set("system", key, val);

    const got = await db.kv.get("system", key);
    assert.ok(got !== undefined);
    assert.equal(new TextDecoder().decode(got.value), "fast");

    await db.close();
  } finally {
    daemonProc.kill("SIGTERM");
    await rm(dir, { recursive: true, force: true });
  }
});
