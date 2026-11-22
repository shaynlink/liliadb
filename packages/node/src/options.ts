import type { EngineOptions } from "@liliadb/client";

export type DatabaseMode = "embedded" | "daemon";

export interface OpenOptions extends EngineOptions {
  /** Directory of the database (embedded); created if missing. */
  path: string;
  mode?: DatabaseMode;
  /** Socket (or named pipe) of the daemon; `<path>.sock` by default. */
  endpoint?: string;
  /** File holding the daemon's token; `<path>.token` by default. */
  tokenFile?: string;
  signal?: AbortSignal;
}
