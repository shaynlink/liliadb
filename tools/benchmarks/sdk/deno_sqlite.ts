// @db/sqlite (Deno FFI) surface. Run by `sdk_suite.mjs`:
// `deno run --allow-all deno_sqlite.ts --profile <p> --dir <dir>`.
import { Database } from "jsr:@db/sqlite@0.12.0";

import { argument, emit, failure, PROFILES, runSurface, sqliteAdapter } from "./harness.mjs";

const settings = PROFILES[argument(Deno.args, "profile", "quick") as keyof typeof PROFILES];
const dir = argument(Deno.args, "dir", "");

try {
  const db = new Database(`${dir}/bench.sqlite`);
  await runSurface(sqliteAdapter("deno-sqlite", "@db/sqlite 0.12.0 (Deno)", `Deno ${Deno.version.deno}`, db, () => db.close()), settings);
} catch (error) {
  emit(failure("deno-sqlite", "sdk", error instanceof Error ? error.message : error));
}
