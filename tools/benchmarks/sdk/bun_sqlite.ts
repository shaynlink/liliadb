// bun:sqlite surface. Run by `sdk_suite.mjs`:
// `bun run bun_sqlite.ts --profile <p> --dir <dir> [--fullfsync]`.
import { Database } from "bun:sqlite";
import { join } from "node:path";

import { argument, emit, failure, PROFILES, runSurface, sqliteAdapter } from "./harness.mjs";

const args = process.argv.slice(2);
const settings = PROFILES[argument(args, "profile", "quick") as keyof typeof PROFILES];
const dir = argument(args, "dir", "");
const fullfsync = args.includes("--fullfsync");
const surface = {
  id: "bun-sqlite",
  driver: "bun:sqlite",
  runtime: "Bun",
  version: `Bun ${Bun.version}`,
  fullfsync,
  platform: process.platform,
};

try {
  const db = new Database(join(dir, "bench.sqlite"));
  await runSurface(
    sqliteAdapter(surface, db, () => db.close()),
    settings,
  );
} catch (error) {
  emit(
    failure(
      fullfsync ? "bun-sqlite-fullfsync" : "bun-sqlite",
      "sdk",
      error instanceof Error ? error.message : error,
    ),
  );
}
