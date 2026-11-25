// @liliadb/wasm: a LiliaDB database in WebAssembly, in the browser's origin private file system
// or in memory. The calls are those of `@liliadb/client`.

import {
  Database as ClientDatabase,
  nativeError,
  type EngineOptions,
  type Transport,
} from "@liliadb/client";
import type { Endpoint } from "./messages.js";
import { loadModule, ModuleDatabase, type ModuleSource } from "./module.js";
import { ModuleTransport, WorkerTransport } from "./transport.js";

export * from "@liliadb/client";
export type { ModuleSource } from "./module.js";
export type { Endpoint } from "./messages.js";
export { serve } from "./serve.js";

export interface OpenOptions extends EngineOptions {
  /** The database's directory in the origin private file system; required there. */
  path?: string;
  /**
   * Where the database keeps its files: `opfs`, the origin private file system (persistent,
   * one tab at a time), or `memory`. `opfs` where the browser has it, else `memory`.
   */
  storage?: "opfs" | "memory";
  /** Slot files the OPFS pool starts with (64); it grows as the database does. */
  slots?: number;
  /**
   * Where the database runs. By default, in this thread when it is a dedicated worker or the
   * database is in memory, else in a worker of this package (OPFS needs one). `true` asks for
   * that worker, `false` for this thread; a `Worker`, or the URL of a script that calls
   * `serve(self)`, runs it there.
   */
  worker?: boolean | Worker | URL | string;
  /** The WebAssembly module (`pkg/lilia_wasm_bg.wasm` of this package by default). */
  wasm?: ModuleSource;
}

/** Whether this thread is a dedicated worker, where OPFS files open synchronously. */
function inDedicatedWorker(): boolean {
  const scope = (globalThis as { DedicatedWorkerGlobalScope?: abstract new () => unknown })
    .DedicatedWorkerGlobalScope;
  return scope !== undefined && globalThis instanceof scope;
}

function hasOpfs(): boolean {
  return typeof globalThis.navigator?.storage?.getDirectory === "function";
}

/** The worker `option` names, and how to end it once the database closes. */
function workerOf(option: Exclude<OpenOptions["worker"], false | undefined>): {
  endpoint: Endpoint;
  release: () => void;
} {
  if (typeof Worker === "undefined") {
    throw nativeError(
      new Error(
        '{"code":"UNSUPPORTED","message":"this runtime has no Web Workers: open with worker: false","retryable":false}',
      ),
    );
  }
  if (option instanceof Worker) return { endpoint: option, release: () => undefined };
  const worker = new Worker(option === true ? new URL("./worker.js", import.meta.url) : option, {
    type: "module",
  });
  return { endpoint: worker, release: () => worker.terminate() };
}

export class Database extends ClientDatabase {
  private constructor(transport: Transport) {
    super(transport);
  }

  /**
   * Opens (or creates) the database `options` describe. In this thread its calls are also
   * available synchronously (`db.sync`); in a worker they are not.
   */
  static async open(options: OpenOptions = {}): Promise<Database> {
    const storage = options.storage ?? (hasOpfs() ? "opfs" : "memory");
    const json = JSON.stringify({
      path: options.path,
      storage,
      slots: options.slots,
      durability: options.durability,
      profile: options.profile ?? "mobile",
      ledger: options.ledger,
      history_retention_ms: options.historyRetentionMs,
      undo_depth: options.undoDepth,
    });
    const worker = options.worker ?? !(storage === "memory" || inDedicatedWorker());
    if (worker !== false) {
      const { endpoint, release } = workerOf(worker);
      return new Database(await WorkerTransport.open(endpoint, json, options.wasm, release));
    }
    await loadModule(options.wasm);
    try {
      return new Database(new ModuleTransport(await ModuleDatabase.open(json)));
    } catch (error) {
      throw nativeError(error);
    }
  }
}
