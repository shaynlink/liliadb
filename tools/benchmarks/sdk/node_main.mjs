// One Node.js surface per process, so a crashing binding cannot take the others down.
// Run by `sdk_suite.mjs`: `node node_main.mjs --surface <id> --profile <p> --dir <dir>`.

import { argument, emit, failure, PROFILES, runSurface } from "./harness.mjs";
import { classicLevel, duckdb, lmdb, pglite, SQLITE_DRIVERS, surrealdb } from "./node_competitors.mjs";
import { liliaDaemonHttp, liliaDaemonIpc, liliaEmbedded } from "./node_surfaces.mjs";

/** Every Node.js surface by id; each SQLite driver twice, with and without `fullfsync`. */
export const NODE_SURFACES = {
  ...Object.fromEntries(
    Object.entries(SQLITE_DRIVERS).flatMap(([id, open]) => [
      [id, (dir) => open(dir, false)],
      [`${id}-fullfsync`, (dir) => open(dir, true)],
    ]),
  ),
  "lmdb-js": (dir) => lmdb(dir),
  "classic-level": (dir) => classicLevel(dir),
  pglite: (dir) => pglite(dir),
  duckdb: (dir) => duckdb(dir),
  surrealdb: (dir) => surrealdb(dir),
  "lilia-node": (dir) => liliaEmbedded(dir, false),
  "lilia-node-sync": (dir) => liliaEmbedded(dir, true),
  "lilia-daemon-ipc": (dir) => liliaDaemonIpc(dir),
  "lilia-daemon-http": (dir) => liliaDaemonHttp(dir),
};

if (import.meta.main) {
  const args = process.argv.slice(2);
  const id = argument(args, "surface", "");
  const settings = PROFILES[argument(args, "profile", "quick")];
  const open = NODE_SURFACES[id];
  // Exit once standard output is flushed, even if a failed binding left handles open.
  const finish = () => process.stdout.write("", () => process.exit(0));
  // A binding may reject a promise nobody awaits (a dropped connection); that ends this surface.
  process.on("unhandledRejection", (error) => {
    emit(failure(id, "sdk", error instanceof Error ? error.message : error));
    finish();
  });
  try {
    if (!open) throw new Error(`unknown Node surface ${id}`);
    await runSurface(await open(argument(args, "dir", "")), settings);
  } catch (error) {
    emit(failure(id, "sdk", error instanceof Error ? error.message : error));
  }
  finish();
}
