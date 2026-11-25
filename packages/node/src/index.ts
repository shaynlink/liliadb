// @liliadb/node: a LiliaDB database, embedded in the process through the native module or
// served by a daemon. The calls are those of `@liliadb/client`; this package adds the
// transports and Node's streams.

import { Readable } from "node:stream";
import {
  Database as ClientDatabase,
  type JsonEntry,
  type JsonValue,
  type KvEntry,
  type ScanStreamOptions,
} from "@liliadb/client";
import type { OpenOptions } from "./options.js";
import { DaemonTransport, NativeTransport, type Transport } from "./transport.js";

export * from "@liliadb/client";
export type { DatabaseMode, OpenOptions } from "./options.js";

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

/** The scans of `ClientDatabase` as web streams and Node `Readable`s. */
function kvStreams(kv: ClientDatabase["kv"]) {
  return {
    ...kv,
    readableStream: (
      namespace: string,
      options: ScanStreamOptions<Uint8Array | string> = {},
    ): ReadableStream<KvEntry> => webStream(kv.scanStream(namespace, options)),
    createReadStream: (
      namespace: string,
      options: ScanStreamOptions<Uint8Array | string> = {},
    ): Readable => Readable.from(kv.scanStream(namespace, options), { objectMode: true }),
  };
}

function jsonStreams(json: ClientDatabase["json"]) {
  return {
    ...json,
    readableStream: <T = JsonValue>(
      space: string,
      options: ScanStreamOptions<string> = {},
    ): ReadableStream<JsonEntry<T>> => webStream(json.scanStream<T>(space, options)),
    createReadStream: <T = JsonValue>(
      space: string,
      options: ScanStreamOptions<string> = {},
    ): Readable => Readable.from(json.scanStream<T>(space, options), { objectMode: true }),
  };
}

export class Database extends ClientDatabase {
  declare readonly kv: ReturnType<typeof kvStreams>;
  declare readonly json: ReturnType<typeof jsonStreams>;

  private constructor(transport: Transport) {
    super(transport);
    this.kv = kvStreams(this.kv);
    this.json = jsonStreams(this.json);
  }

  /** Opens the database of `options.path`, embedded, or connects to its daemon (`mode: "daemon"`). */
  static async open(options: OpenOptions): Promise<Database> {
    options.signal?.throwIfAborted();
    const transport =
      options.mode === "daemon"
        ? await DaemonTransport.open(
            options.endpoint ?? `${options.path}.sock`,
            options.tokenFile ?? `${options.path}.token`,
            options.signal,
          )
        : await NativeTransport.open(
            {
              path: options.path,
              durability: options.durability,
              profile: options.profile,
              ledger: options.ledger,
              historyRetentionMs: options.historyRetentionMs,
              undoDepth: options.undoDepth,
            },
            options.signal,
          );
    return new Database(transport);
  }
}
