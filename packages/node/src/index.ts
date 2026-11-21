// @liliadb/node: a LiliaDB database, embedded in the process through the native module or
// served by a daemon. Every call is one operation of the LiliaDB protocol (see `api.ts`).

import { Readable } from "node:stream";
import { buildApi, type Api, type Runner } from "./api.js";
import { DaemonTransport, NativeTransport, type Transport } from "./transport.js";
import {
  LiliaError,
  type CloseOptions,
  type JsonEntry,
  type JsonValue,
  type KvEntry,
  type OpenOptions,
  type ScanStreamOptions,
} from "./types.js";
import { getUuid } from "./version.js";
import type { Operation, ResponseValue } from "./wire.js";

export * from "./types.js";
export { geo, quantization, timeseries } from "./math.js";
export type { Api } from "./api.js";

/** Largest timeout `close` accepts, in milliseconds. */
const MAX_CLOSE_TIMEOUT_MS = 0x7fff_ffff;

function lifecycleError(code: string, message: string, retryable: boolean): LiliaError {
  return new LiliaError({ code, message, retryable, request_id: getUuid() });
}

/** Waits for `promise` until `signal` aborts; the work itself runs on. */
async function abortable<T>(promise: Promise<T>, signal?: AbortSignal): Promise<T> {
  if (!signal) return promise;
  signal.throwIfAborted();
  const { promise: aborted, reject } = Promise.withResolvers<never>();
  const onAbort = () => reject(signal.reason ?? new DOMException("The operation was aborted", "AbortError"));
  signal.addEventListener("abort", onAbort, { once: true });
  try {
    return await Promise.race([promise, aborted]);
  } finally {
    signal.removeEventListener("abort", onAbort);
  }
}

/** `records` as a web stream, pulled one record at a time (`ReadableStream.from` is not everywhere). */
function webStream<T>(records: AsyncIterableIterator<T>): ReadableStream<T> {
  return new ReadableStream<T>({
    async pull(controller) {
      const next = await records.next();
      if (next.done) controller.close();
      else controller.enqueue(next.value);
    },
    async cancel(reason) {
      await records.return?.(reason);
    },
  });
}

/** Pages of `fetch` (each after the last key of the previous one) as one stream of records. */
async function* paginate<T, C>(
  fetch: (after: C | undefined, limit: number) => Promise<T[]>,
  cursor: (record: T) => C,
  options: ScanStreamOptions<C>
): AsyncIterableIterator<T> {
  const pageSize = Math.max(1, options.batchSize ?? 100);
  let remaining = options.limit === undefined ? Infinity : Math.max(0, options.limit);
  let after = options.after;
  while (remaining > 0) {
    options.signal?.throwIfAborted();
    const limit = Math.min(pageSize, remaining);
    const page = await fetch(after, limit);
    for (const record of page.slice(0, remaining)) {
      options.signal?.throwIfAborted();
      yield record;
      remaining--;
    }
    if (page.length < limit) return;
    after = cursor(page[page.length - 1]!);
  }
}

export class Database implements AsyncDisposable {
  private closed = false;
  private closing?: Promise<void>;
  private readonly pending = new Set<Promise<unknown>>();
  private syncApi?: Api<"sync">;
  private readonly api: Api<"async">;

  readonly kv;
  readonly json;
  readonly graph;
  readonly fts;
  readonly vector;
  readonly hybrid;
  readonly timeseries;
  readonly geo;
  readonly crdt;
  readonly sql;

  private constructor(private readonly transport: Transport) {
    const run: Runner<"async"> = (operation, map, signal) => this.run(operation, signal).then(map);
    this.api = buildApi<"async">(run);
    this.kv = {
      ...this.api.kv,
      /** The records of `namespace` in key order, a page at a time. */
      scanStream: (namespace: string, options: ScanStreamOptions<Uint8Array | string> = {}): AsyncIterableIterator<KvEntry> =>
        paginate(
          (after, limit) => this.api.kv.scan(namespace, { after, limit, signal: options.signal }),
          (entry): Uint8Array | string => entry.key,
          options
        ),
      readableStream: (namespace: string, options: ScanStreamOptions<Uint8Array | string> = {}): ReadableStream<KvEntry> =>
        webStream(this.kv.scanStream(namespace, options)),
      createReadStream: (namespace: string, options: ScanStreamOptions<Uint8Array | string> = {}): Readable =>
        Readable.from(this.kv.scanStream(namespace, options), { objectMode: true }),
    };
    this.json = {
      ...this.api.json,
      /** The documents of `space` in id order, a page at a time. */
      scanStream: <T = JsonValue>(space: string, options: ScanStreamOptions<string> = {}): AsyncIterableIterator<JsonEntry<T>> =>
        paginate((after, limit) => this.api.json.scan<T>(space, { after, limit, signal: options.signal }), (entry) => entry.id, options),
      /** The documents of `space` whose id starts with `prefix`, a page at a time. */
      scanPrefixStream: <T = JsonValue>(space: string, prefix: string, options: ScanStreamOptions<string> = {}): AsyncIterableIterator<JsonEntry<T>> =>
        paginate(
          (afterId, limit) => this.api.json.scanPrefix<T>(space, prefix, { afterId, limit, signal: options.signal }),
          (entry) => entry.id,
          options
        ),
      /** The documents `query` selects, a page at a time. */
      findStream: <T = JsonValue>(space: string, query: Record<string, JsonValue>, options: ScanStreamOptions<string> = {}): AsyncIterableIterator<JsonEntry<T>> =>
        paginate((after, limit) => this.api.json.find<T>(space, query, { after, limit, signal: options.signal }), (entry) => entry.id, options),
      readableStream: <T = JsonValue>(space: string, options: ScanStreamOptions<string> = {}): ReadableStream<JsonEntry<T>> =>
        webStream(this.json.scanStream<T>(space, options)),
      createReadStream: <T = JsonValue>(space: string, options: ScanStreamOptions<string> = {}): Readable =>
        Readable.from(this.json.scanStream<T>(space, options), { objectMode: true }),
    };
    this.graph = this.api.graph;
    this.fts = this.api.fts;
    this.vector = this.api.vector;
    this.hybrid = this.api.hybrid;
    this.timeseries = this.api.timeseries;
    this.geo = this.api.geo;
    this.crdt = this.api.crdt;
    this.sql = this.api.sql;
  }

  /** Opens the database of `options.path`, embedded, or connects to its daemon (`mode: "daemon"`). */
  static async open(options: OpenOptions): Promise<Database> {
    options.signal?.throwIfAborted();
    const transport = options.mode === "daemon"
      ? await DaemonTransport.open(options.endpoint ?? `${options.path}.sock`, options.tokenFile ?? `${options.path}.token`, options.signal)
      : await NativeTransport.open(
          {
            path: options.path,
            durability: options.durability,
            profile: options.profile,
            ledger: options.ledger,
            historyRetentionMs: options.historyRetentionMs,
            undoDepth: options.undoDepth,
          },
          options.signal
        );
    return new Database(transport);
  }

  batch: Api<"async">["batch"] = (operations, options) => this.api.batch(operations, options);
  undo: Api<"async">["undo"] = (options) => this.api.undo(options);
  redo: Api<"async">["redo"] = (options) => this.api.redo(options);
  undoHistory: Api<"async">["undoHistory"] = (options) => this.api.undoHistory(options);
  quickCheck: Api<"async">["quickCheck"] = (options) => this.api.quickCheck(options);
  integrityCheck: Api<"async">["integrityCheck"] = (options) => this.api.integrityCheck(options);
  checkpoint: Api<"async">["checkpoint"] = (options) => this.api.checkpoint(options);
  storageStats: Api<"async">["storageStats"] = (options) => this.api.storageStats(options);

  /** The same calls, run on the caller's thread; embedded databases only. */
  get sync(): Api<"sync"> {
    this.ensureOpen();
    if (!(this.transport instanceof NativeTransport)) {
      throw lifecycleError("UNSUPPORTED", "synchronous operations need an embedded database", false);
    }
    const transport = this.transport;
    this.syncApi ??= buildApi<"sync">((operation, map) => {
      this.ensureOpen();
      return map(transport.executeSync(operation));
    });
    return this.syncApi;
  }

  async [Symbol.asyncDispose](): Promise<void> {
    await this.close();
  }

  /**
   * Closes the database once the calls already made return; later calls fail with `CLOSED`.
   * A `timeoutMs` (5 seconds by default) that passes rejects with `TIMEOUT` but cancels
   * nothing: the close completes when the calls do.
   */
  async close(options: CloseOptions = {}): Promise<void> {
    options.signal?.throwIfAborted();
    const timeout = options.timeoutMs ?? 5_000;
    if (!Number.isInteger(timeout) || timeout < 0 || timeout > MAX_CLOSE_TIMEOUT_MS) {
      throw new RangeError(`close timeoutMs must be an integer in 0..=${MAX_CLOSE_TIMEOUT_MS}`);
    }
    if (!this.closing) {
      this.closed = true;
      this.closing = Promise.allSettled([...this.pending]).then(() => this.transport.close(MAX_CLOSE_TIMEOUT_MS));
    }
    const { promise: timedOut, reject } = Promise.withResolvers<never>();
    const timer = setTimeout(() => reject(lifecycleError("TIMEOUT", "close is still draining; operations are not cancelled", true)), timeout);
    try {
      await abortable(Promise.race([this.closing, timedOut]), options.signal);
    } finally {
      clearTimeout(timer);
    }
  }

  private ensureOpen(): void {
    if (this.closed) throw lifecycleError("CLOSED", "database is closing or closed", false);
  }

  /** Runs `operation`, counted among the calls `close` waits for. */
  private async run(operation: Operation, signal?: AbortSignal): Promise<ResponseValue> {
    signal?.throwIfAborted();
    this.ensureOpen();
    const running = this.transport.execute(operation);
    this.pending.add(running);
    const settle = () => this.pending.delete(running);
    running.then(settle, settle);
    return await abortable(running, signal);
  }
}
