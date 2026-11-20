// bun:sqlite surface. Run by `sdk_suite.mjs`: `bun run bun_sqlite.ts --profile <p> --dir <dir>`.
import { Database } from "bun:sqlite";
import { join } from "node:path";

import { argument, emit, failure, PROFILES, runSurface, sqliteAdapter } from "./harness.mjs";

const args = process.argv.slice(2);
const settings = PROFILES[argument(args, "profile", "quick") as keyof typeof PROFILES];
const dir = argument(args, "dir", "");

try {
  const db = new Database(join(dir, "bench.sqlite"));
  await runSurface(sqliteAdapter("bun-sqlite", "bun:sqlite (Bun)", `Bun ${Bun.version}`, db, () => db.close()), settings);
} catch (error) {
  emit(failure("bun-sqlite", "sdk", error instanceof Error ? error.message : error));
}
