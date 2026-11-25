// @db/sqlite (Deno FFI) surface. Run by `sdk_suite.mjs`:
// `deno run --allow-all deno_sqlite.ts --profile <p> --dir <dir> [--fullfsync]`.
import { Database } from "jsr:@db/sqlite@0.12.0";

import { argument, emit, failure, PROFILES, runSurface, sqliteAdapter } from "./harness.mjs";

const settings = PROFILES[argument(Deno.args, "profile", "quick") as keyof typeof PROFILES];
const dir = argument(Deno.args, "dir", "");
const fullfsync = Deno.args.includes("--fullfsync");
const surface = {
  id: "deno-sqlite",
  driver: "@db/sqlite 0.12.0",
  runtime: "Deno",
  version: `Deno ${Deno.version.deno}`,
  fullfsync,
  platform: Deno.build.os,
};

try {
  const db = new Database(`${dir}/bench.sqlite`);
  await runSurface(
    sqliteAdapter(surface, db, () => db.close()),
    settings,
  );
} catch (error) {
  emit(
    failure(
      fullfsync ? "deno-sqlite-fullfsync" : "deno-sqlite",
      "sdk",
      error instanceof Error ? error.message : error,
    ),
  );
}
