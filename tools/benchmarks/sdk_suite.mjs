#!/usr/bin/env node
// JavaScript surfaces of the benchmark suite. Called by `lilia-bench suite`; prints result lines
// (the `Line` schema of `tools/lilia-bench`) on standard output, progress on standard error.
// Every surface runs in its own process and its own fresh directory.
//
//   node tools/benchmarks/sdk_suite.mjs --profile quick|full

import { spawn } from "node:child_process";
import { mkdtempSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { argument, emit, failure, PROFILES } from "./sdk/harness.mjs";
import { NODE_SURFACES } from "./sdk/node_main.mjs";

const profile = argument(process.argv.slice(2), "profile", "quick");
if (!PROFILES[profile]) {
  console.error(`unknown profile ${profile}; expected one of ${Object.keys(PROFILES).join(", ")}`);
  process.exit(2);
}

const sdk = import.meta.dirname;
const surfaces = [
  ...Object.keys(NODE_SURFACES).map((id) => [id, process.execPath, [join(sdk, "sdk/node_main.mjs"), "--surface", id]]),
  ["bun-sqlite", "bun", ["run", join(sdk, "sdk/bun_sqlite.ts")]],
  ["deno-sqlite", "deno", ["run", "--allow-all", join(sdk, "sdk/deno_sqlite.ts")]],
];

for (const [id, runtime, args] of surfaces) {
  // Physical path: the daemon refuses IPC directories behind symlinks (macOS `/var` → `/private/var`).
  const dir = realpathSync(mkdtempSync(join(tmpdir(), "lilia-sdk-")));
  const started = performance.now();
  const outcome = await new Promise((done) => {
    const child = spawn(runtime, [...args, "--profile", profile, "--dir", dir], { stdio: ["ignore", "inherit", "pipe"] });
    let stderr = "";
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("error", (error) => done(`${runtime} could not start: ${error.message}`));
    child.on("close", (code) => done(code === 0 ? null : `${runtime} exited with ${code}: ${stderr.trim().split("\n").slice(-4).join(" ")}`));
  });
  if (outcome) {
    emit(failure(id, "sdk", outcome));
  }
  console.error(`  sdk        ${id.padEnd(18)} ${outcome ? "FAILED" : "finished"} in ${((performance.now() - started) / 1e3).toFixed(1)}s`);
  rmSync(dir, { recursive: true, force: true });
}
