// Node.js surfaces of LiliaDB: Node-API embedded (async and sync calls), the daemon over IPC and
// over HTTP.

import { spawn } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { readFile } from "node:fs/promises";
import http from "node:http";
import { connect, createServer } from "node:net";
import { join, resolve } from "node:path";

import { platformSync } from "./harness.mjs";

const repo = resolve(import.meta.dirname, "../../..");
// `durability: "durable"`: the engine's journal is flushed past the drive cache on every commit.
const LILIA_SYNC = platformSync(process.platform, true);

function liliaVersion() {
  return JSON.parse(readFileSync(join(repo, "packages/node/package.json"), "utf8")).version;
}

async function liliaModule() {
  return import(join(repo, "packages/node/dist/index.js"));
}

function liliaOps(items) {
  return items.map(([key, value]) => ({ model: "kv_set", namespace: "bench", key, value }));
}

export async function liliaEmbedded(dir, sync) {
  const { Database } = await liliaModule();
  const db = await Database.open({ path: join(dir, "bench.lilia"), mode: "embedded", durability: "durable" });
  const id = sync ? "lilia-node-sync" : "lilia-node";
  const name = sync ? "LiliaDB Node-API (sync calls)" : "LiliaDB Node-API (async calls)";
  return {
    info: { id, name, version: liliaVersion(), sync: LILIA_SYNC },
    put: sync ? (items) => db.sync.batch(liliaOps(items)) : (items) => db.batch(liliaOps(items)),
    get: sync ? (k) => db.sync.kv.get("bench", k) !== undefined : async (k) => (await db.kv.get("bench", k)) !== undefined,
    close: () => db.close(),
  };
}

function daemonBinary() {
  const binary = join(repo, "target/release", process.platform === "win32" ? "lilia-daemon.exe" : "lilia-daemon");
  if (!existsSync(binary)) {
    throw new Error("target/release/lilia-daemon is missing: run `cargo build --release -p lilia-daemon`");
  }
  return binary;
}

async function freePort() {
  return new Promise((done, fail) => {
    const server = createServer();
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => done(port));
    });
    server.on("error", fail);
  });
}

async function until(ready, child, what) {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    if (child.exitCode != null) {
      throw new Error(`daemon exited with code ${child.exitCode} before ${what} was ready`);
    }
    if (await ready().catch(() => false)) {
      return;
    }
    await new Promise((r) => setTimeout(r, 25));
  }
  throw new Error(`timed out waiting for ${what}`);
}

/** Starts a daemon on `dir`; with `httpPort`, also its HTTP gateway. */
async function startDaemon(dir, httpPort) {
  const endpoint = process.platform === "win32" ? `\\\\.\\pipe\\lilia-bench-${process.pid}-${Date.now()}` : join(dir, "d.sock");
  const tokenFile = process.platform === "win32" ? join(dir, "ipc", "token") : join(dir, "d.token");
  const args = ["--database", join(dir, "d.lilia"), "--endpoint", endpoint, "--token-file", tokenFile];
  if (httpPort) {
    args.push("--http-port", String(httpPort));
  }
  const child = spawn(daemonBinary(), args, { stdio: "ignore" });
  // Never leave a daemon behind, whatever ends this process.
  process.on("exit", () => child.kill());
  await until(
    () =>
      new Promise((ok) => {
        if (!existsSync(tokenFile)) return ok(false);
        const socket = connect(endpoint);
        socket.once("connect", () => (socket.destroy(), ok(true)));
        socket.once("error", () => ok(false));
      }),
    child,
    "the IPC endpoint",
  );
  return { child, endpoint, tokenFile };
}

async function stopDaemon(child) {
  child.kill();
  await new Promise((r) => (child.exitCode != null ? r() : child.once("exit", r)));
}

export async function liliaDaemonIpc(dir) {
  const { Database } = await liliaModule();
  const { child, endpoint, tokenFile } = await startDaemon(dir);
  const db = await Database.open({ path: join(dir, "d.lilia"), mode: "daemon", endpoint, tokenFile });
  return {
    info: { id: "lilia-daemon-ipc", name: "LiliaDB daemon (IPC)", version: liliaVersion(), sync: LILIA_SYNC },
    put: (items) => db.batch(liliaOps(items)),
    get: async (k) => (await db.kv.get("bench", k)) !== undefined,
    close: async () => {
      await db.close();
      await stopDaemon(child);
    },
  };
}

function post(agent, port, token, path, body) {
  return new Promise((done, fail) => {
    const payload = JSON.stringify(body);
    const req = http.request(
      { host: "127.0.0.1", port, path, method: "POST", agent, headers: { "content-type": "application/json", "content-length": Buffer.byteLength(payload), authorization: `Bearer ${token}` } },
      (res) => {
        let text = "";
        res.on("data", (chunk) => (text += chunk));
        res.on("end", () => done({ status: res.statusCode, text }));
      },
    );
    req.on("error", fail);
    req.end(payload);
  });
}

export async function liliaDaemonHttp(dir) {
  const port = await freePort();
  const { child, tokenFile } = await startDaemon(dir, port);
  await until(async () => (await new Promise((ok) => http.get(`http://127.0.0.1:${port}/health`, (r) => (r.resume(), ok(r.statusCode === 200))).on("error", () => ok(false)))), child, "the HTTP gateway");
  const token = (await readFile(tokenFile, "utf8")).trim();
  const agent = new http.Agent({ keepAlive: true, maxSockets: 1 });
  const b64 = (bytes) => Buffer.from(bytes).toString("base64");
  return {
    info: { id: "lilia-daemon-http", name: "LiliaDB daemon (HTTP)", version: liliaVersion(), sync: LILIA_SYNC },
    async put(items) {
      const operations = items.map(([k, v]) => ({ type: "kv_set", namespace: "bench", key: b64(k), value: b64(v), encoding: "base64" }));
      const { status, text } = await post(agent, port, token, "/v1/batch", { operations });
      if (status !== 200) throw new Error(`HTTP batch failed (${status}): ${text}`);
    },
    async get(k) {
      const { status, text } = await post(agent, port, token, "/v1/kv/get", { namespace: "bench", key: b64(k), encoding: "base64" });
      if (status === 200) return JSON.parse(text).entry != null;
      throw new Error(`HTTP get failed (${status}): ${text}`);
    },
    close: async () => {
      agent.destroy();
      await stopDaemon(child);
    },
  };
}
