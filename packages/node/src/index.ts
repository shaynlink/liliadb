import { DaemonClient } from "./daemon.js";
import { EmbeddedClient } from "./native.js";
import { Readable } from "node:stream";
import { LiliaError } from "./types.js";
import { getUuid } from "./version.js";
import type {
  BatchOperation,
  CloseOptions,
  CreateIndexOptions,
  DatabaseEvent,
  FtsHit,
  GraphDirection,
  JsonDeleteOptions,
  JsonEntry,
  JsonFindOptions,
  JsonGetOptions,
  JsonIndexInfo,
  JsonPutOptions,
  JsonScanOptions,
  JsonValue,
  KvDeleteOptions,
  KvEntry,
  KvGetOptions,
  KvScanOptions,
  KvSetOptions,
  MutationResult,
  OpenOptions,
  ScanStreamOptions,
  SqlFunction,
  SqlQueryOptions,
  SqlQueryResult,
  SyncSqlFunction,
  VectorElementType,
  VectorHit,
  VectorMetric,
  HybridHit,
  WatchOptions,
  WatchSubscription,
} from "./types.js";

export * from "./types.js";

type Client = EmbeddedClient | DaemonClient;

const textEncoder = new TextEncoder();

function toUint8Array(value: Uint8Array | string): Uint8Array {
  return typeof value === "string" ? textEncoder.encode(value) : value;
}

function toReadableStream<T>(iterable: AsyncIterable<T>): ReadableStream<T> {
  if (typeof (ReadableStream as unknown as { from?: (it: AsyncIterable<T>) => ReadableStream<T> }).from === "function") {
    return (ReadableStream as unknown as { from: (it: AsyncIterable<T>) => ReadableStream<T> }).from(iterable);
  }
  const iterator = iterable[Symbol.asyncIterator]();
  return new ReadableStream<T>({
    async pull(controller) {
      try {
        const { value, done } = await iterator.next();
        if (done) {
          controller.close();
        } else {
          controller.enqueue(value);
        }
      } catch (err) {
        controller.error(err);
      }
    },
    async cancel(reason) {
      await iterator.return?.(reason);
    },
  });
}

function buildTaggedQuery(strings: TemplateStringsArray, values: JsonValue[]): { sql: string; params: JsonValue[] } {
  let sql = "";
  for (let i = 0; i < strings.length; i++) {
    sql += strings[i];
    if (i < values.length) {
      sql += `?${i + 1}`;
    }
  }
  return { sql, params: values };
}

export class Database implements AsyncDisposable {
  private closed = false;
  private readonly pending = new Set<Promise<unknown>>();
  private closing?: Promise<void>;
  private constructor(private readonly client: Client) {}

  static async open(options: OpenOptions): Promise<Database> {
    options.signal?.throwIfAborted();
    for (const value of [options.checkpointPolicy?.walBytes, options.checkpointPolicy?.intervalMs]) {
      if (value !== undefined && (!Number.isInteger(value) || value < 1 || value > 0xffff_ffff)) {
        throw new RangeError("checkpoint policy values must be integers in 1..=4294967295");
      }
    }
    const plugins = options.plugins ?? ["kv", "json"];
    if (!plugins.includes("kv") || !plugins.includes("json")) {
      throw new Error("the v0.1 distribution requires kv and json plugins");
    }
    const client = options.mode === "daemon"
      ? await DaemonClient.open(
          options.endpoint ?? `${options.path}.sock`,
          options.tokenFile ?? `${options.path}.token`,
          { signal: options.signal }
        )
      : await EmbeddedClient.open(
          {
            path: options.path,
            durability: options.durability,
            busyTimeoutMs: options.busyTimeoutMs,
            writerQueueCapacity: options.writerQueueCapacity,
            readPoolSize: options.readPoolSize,
            checkpointPolicy: options.checkpointPolicy,
            encryptionKey: options.encryptionKey,
          },
          options.signal
        );
    return new Database(client);
  }

  async [Symbol.asyncDispose](): Promise<void> {
    await this.close();
  }

  get sync() {
    if (!(this.client instanceof EmbeddedClient)) {
      throw new Error("Synchronous operations are only supported in embedded mode");
    }
    const client = this.client;
    if (this.closed) throw lifecycleError("CLOSED", "database is closing or closed", false);

    const syncSql = ((
      stringsOrQuery: TemplateStringsArray | string,
      ...valuesOrParams: unknown[]
    ): unknown => {
      if (Array.isArray(stringsOrQuery) && "raw" in stringsOrQuery) {
        const { sql, params } = buildTaggedQuery(stringsOrQuery as TemplateStringsArray, valuesOrParams as JsonValue[]);
        return client.sqlQuerySync(sql, params).records;
      }
      return client.sqlQuerySync(stringsOrQuery as string, (valuesOrParams[0] as JsonValue[]) ?? []).records;
    }) as unknown as SyncSqlFunction;

    syncSql.query = <T = Record<string, unknown>>(sql: string, params: JsonValue[] = []): T[] =>
      client.sqlQuerySync(sql, params).records as T[];

    syncSql.execute = (sql: string, params: JsonValue[] = []): { rowsAffected: number } =>
      ({ rowsAffected: client.sqlQuerySync(sql, params).rowsAffected });

    syncSql.raw = <T = Record<string, unknown>>(sql: string, params: JsonValue[] = []): SqlQueryResult<T> =>
      client.sqlQuerySync(sql, params) as SqlQueryResult<T>;

    return {
      kv: {
        get: (namespace: string, key: Uint8Array | string): KvEntry | undefined =>
          client.kvGetSync(namespace, toUint8Array(key)),
        set: (
          namespace: string,
          key: Uint8Array | string,
          value: Uint8Array | string,
          options: { ifVersion?: bigint; expiresAtMs?: number } = {}
        ): MutationResult =>
          client.batchSync([{ model: "kv_set", namespace, key: toUint8Array(key), value: toUint8Array(value), ...options }])[0]!,
        delete: (
          namespace: string,
          key: Uint8Array | string,
          options: { ifVersion?: bigint } = {}
        ): MutationResult =>
          client.batchSync([{ model: "kv_delete", namespace, key: toUint8Array(key), ...options }])[0]!,
        scan: (
          namespace: string,
          options: { after?: Uint8Array | string; limit?: number } = {}
        ): KvEntry[] =>
          client.kvScanSync(namespace, options.after ? toUint8Array(options.after) : undefined, options.limit ?? 100),
      },
      json: {
        get: <T = JsonValue>(space: string, id: string): JsonEntry<T> | undefined =>
          client.jsonGetSync<T>(space, id),
        put: <T = JsonValue>(
          space: string,
          id: string,
          value: T,
          options: { ifVersion?: bigint } = {}
        ): MutationResult =>
          client.batchSync([{ model: "json_put", space, id, value: value as unknown as JsonValue, ...options }])[0]!,
        delete: (
          space: string,
          id: string,
          options: { ifVersion?: bigint } = {}
        ): MutationResult =>
          client.batchSync([{ model: "json_delete", space, id, ...options }])[0]!,
        scan: <T = JsonValue>(
          space: string,
          options: { after?: string; limit?: number } = {}
        ): JsonEntry<T>[] =>
          client.jsonScanSync<T>(space, options.after, options.limit ?? 100),
      },
      batch: (operations: readonly BatchOperation[]): MutationResult[] => client.batchSync(operations),
      sql: syncSql,
    };
  }

  readonly kv = {
    get: (namespace: string, key: Uint8Array | string, options: KvGetOptions = {}): Promise<KvEntry | undefined> =>
      this.run(client => client.kvGet(namespace, toUint8Array(key), options.signal), options.signal),
    set: async (
      namespace: string,
      key: Uint8Array | string,
      value: Uint8Array | string,
      options: KvSetOptions = {}
    ): Promise<MutationResult> => {
      const { signal, ...rest } = options;
      return (await this.batch([{ model: "kv_set", namespace, key: toUint8Array(key), value: toUint8Array(value), ...rest }], { signal }))[0]!;
    },
    delete: async (
      namespace: string,
      key: Uint8Array | string,
      options: KvDeleteOptions = {}
    ): Promise<MutationResult> => {
      const { signal, ...rest } = options;
      return (await this.batch([{ model: "kv_delete", namespace, key: toUint8Array(key), ...rest }], { signal }))[0]!;
    },
    scan: (namespace: string, options: KvScanOptions = {}): Promise<KvEntry[]> =>
      this.run(
        client => client.kvScan(namespace, options.after ? toUint8Array(options.after) : undefined, options.limit ?? 100, options.signal),
        options.signal
      ),
    scanStream: (namespace: string, options: ScanStreamOptions<Uint8Array | string> = {}): AsyncIterableIterator<KvEntry> =>
      this.kvScanStream(namespace, options),
    stream: (namespace: string, options: ScanStreamOptions<Uint8Array | string> = {}): AsyncIterableIterator<KvEntry> =>
      this.kvScanStream(namespace, options),
    readableStream: (namespace: string, options: ScanStreamOptions<Uint8Array | string> = {}): ReadableStream<KvEntry> =>
      toReadableStream(this.kvScanStream(namespace, options)),
    createReadStream: (namespace: string, options: ScanStreamOptions<Uint8Array | string> = {}): Readable =>
      Readable.from(this.kvScanStream(namespace, options), { objectMode: true }),
    watch: (
      namespace: string,
      callback: (event: DatabaseEvent) => void,
      options: WatchOptions = {}
    ): Promise<WatchSubscription> =>
      this.watch(namespace, callback, options),
  };

  readonly json = {
    get: <T = JsonValue>(space: string, id: string, options: JsonGetOptions = {}): Promise<JsonEntry<T> | undefined> =>
      this.run(client => client.jsonGet<T>(space, id, options.signal), options.signal),
    put: async <T = JsonValue>(
      space: string,
      id: string,
      value: T,
      options: JsonPutOptions = {}
    ): Promise<MutationResult> => {
      const { signal, ...rest } = options;
      return (await this.batch([{ model: "json_put", space, id, value: value as unknown as JsonValue, ...rest }], { signal }))[0]!;
    },
    delete: async (
      space: string,
      id: string,
      options: JsonDeleteOptions = {}
    ): Promise<MutationResult> => {
      const { signal, ...rest } = options;
      return (await this.batch([{ model: "json_delete", space, id, ...rest }], { signal }))[0]!;
    },
    scan: <T = JsonValue>(space: string, options: JsonScanOptions = {}): Promise<JsonEntry<T>[]> =>
      this.run(client => client.jsonScan<T>(space, options.after, options.limit ?? 100, options.signal), options.signal),
    scanStream: <T = JsonValue>(space: string, options: ScanStreamOptions<string> = {}): AsyncIterableIterator<JsonEntry<T>> =>
      this.jsonScanStream<T>(space, options),
    stream: <T = JsonValue>(space: string, options: ScanStreamOptions<string> = {}): AsyncIterableIterator<JsonEntry<T>> =>
      this.jsonScanStream<T>(space, options),
    readableStream: <T = JsonValue>(space: string, options: ScanStreamOptions<string> = {}): ReadableStream<JsonEntry<T>> =>
      toReadableStream(this.jsonScanStream<T>(space, options)),
    createReadStream: <T = JsonValue>(space: string, options: ScanStreamOptions<string> = {}): Readable =>
      Readable.from(this.jsonScanStream<T>(space, options), { objectMode: true }),
    createIndex: (
      space: string,
      fieldOrPath: string,
      optionsOrPath?: string | CreateIndexOptions,
      options: { signal?: AbortSignal } = {}
    ): Promise<void> => {
      let name: string;
      let path: string;
      if (typeof optionsOrPath === "string") {
        name = fieldOrPath;
        path = optionsOrPath;
      } else if (typeof optionsOrPath === "object" && optionsOrPath !== null) {
        name = optionsOrPath.name ?? fieldOrPath;
        path = optionsOrPath.path ?? (fieldOrPath.startsWith("$") || fieldOrPath.startsWith("/") ? fieldOrPath : `$.${fieldOrPath}`);
      } else {
        name = fieldOrPath;
        path = fieldOrPath.startsWith("$") || fieldOrPath.startsWith("/") ? fieldOrPath : `$.${fieldOrPath}`;
      }
      return this.run(client => client.jsonCreateIndex(space, name, path, options.signal), options.signal);
    },
    dropIndex: (space: string, indexName: string, options: { signal?: AbortSignal } = {}): Promise<boolean> =>
      this.run(client => client.jsonDropIndex(space, indexName, options.signal), options.signal),
    listIndexes: (space: string, options: { signal?: AbortSignal } = {}): Promise<JsonIndexInfo[]> =>
      this.run(client => client.jsonListIndexes(space, options.signal), options.signal),
    findByIndex: <T = JsonValue>(
      space: string,
      indexName: string,
      value: string | number | boolean,
      options: JsonFindOptions = {}
    ): Promise<JsonEntry<T>[]> =>
      this.run(client => client.jsonScanByIndex<T>(space, indexName, String(value), options.after, options.limit ?? 100, options.signal), options.signal),
    find: <T = JsonValue>(
      space: string,
      query: Record<string, JsonValue>,
      options: JsonFindOptions = {}
    ): Promise<JsonEntry<T>[]> =>
      this.run(client => client.jsonFind<T>(space, query, options.after, options.limit ?? 100, options.signal), options.signal),
    findStream: <T = JsonValue>(
      space: string,
      query: Record<string, JsonValue>,
      options: ScanStreamOptions<string> = {}
    ): AsyncIterableIterator<JsonEntry<T>> =>
      this.jsonFindStream<T>(space, query, options),
    findReadableStream: <T = JsonValue>(
      space: string,
      query: Record<string, JsonValue>,
      options: ScanStreamOptions<string> = {}
    ): ReadableStream<JsonEntry<T>> =>
      toReadableStream(this.jsonFindStream<T>(space, query, options)),
    watch: (space: string, callback: (event: DatabaseEvent) => void, options: WatchOptions = {}): Promise<WatchSubscription> =>
      this.watch(space, callback, options),
  };

  readonly graph = {
    setNode: async (
      graph: string,
      id: string,
      kind: string,
      metadata?: JsonValue,
      options: { signal?: AbortSignal } = {}
    ): Promise<MutationResult> =>
      (await this.batch([{ model: "graph_set_node", graph, id, kind, metadata }], { signal: options.signal }))[0]!,
    addEdge: async (
      graph: string,
      source: string,
      target: string,
      kind: string,
      options: { signal?: AbortSignal } = {}
    ): Promise<MutationResult> =>
      (await this.batch([{ model: "graph_add_edge", graph, source, target, kind }], { signal: options.signal }))[0]!,
    neighbors: (
      graph: string,
      node: string,
      options: { direction?: GraphDirection; kind?: string; signal?: AbortSignal } = {}
    ): Promise<string[]> =>
      this.run(client => client.graphNeighbors(graph, node, options.direction, options.kind, options.signal), options.signal),
    reachability: (
      graph: string,
      startNodes: string[],
      options: { direction?: GraphDirection; signal?: AbortSignal } = {}
    ): Promise<string[]> =>
      this.run(client => client.graphReachability(graph, startNodes, options.direction, options.signal), options.signal),
  };

  readonly fts = {
    index: async (
      collection: string,
      docId: string,
      content: string,
      options: { signal?: AbortSignal } = {}
    ): Promise<MutationResult> =>
      (await this.batch([{ model: "fts_index", collection, docId, content }], { signal: options.signal }))[0]!,
    search: (
      collection: string,
      query: string,
      options: { limit?: number; signal?: AbortSignal } = {}
    ): Promise<FtsHit[]> =>
      this.run(client => client.ftsSearch(collection, query, options.limit, options.signal), options.signal),
  };

  readonly vector = {
    set: async (
      collection: string,
      id: string,
      vector: Float32Array | number[] | Uint8Array,
      options: { dim?: number; elementType?: VectorElementType; signal?: AbortSignal } = {}
    ): Promise<MutationResult> => {
      const dim = options.dim ?? (vector instanceof Uint8Array ? vector.byteLength / 4 : vector.length);
      return (await this.batch([{ model: "vector_set", collection, id, dim, elementType: options.elementType, vector }], { signal: options.signal }))[0]!;
    },
    get: (
      collection: string,
      id: string,
      options: { signal?: AbortSignal } = {}
    ): Promise<Uint8Array | null> =>
      this.run(client => client.vectorGet(collection, id, options.signal), options.signal),
    delete: async (
      collection: string,
      id: string,
      options: { signal?: AbortSignal } = {}
    ): Promise<MutationResult> =>
      (await this.batch([{ model: "vector_delete", collection, id }], { signal: options.signal }))[0]!,
    search: (
      collection: string,
      query: Float32Array | number[] | Uint8Array,
      options: { dim?: number; elementType?: VectorElementType; metric?: VectorMetric; limit?: number; signal?: AbortSignal } = {}
    ): Promise<VectorHit[]> =>
      this.run(client => client.vectorSearch(collection, query, options.dim, options.elementType, options.metric, options.limit, options.signal), options.signal),
  };

  readonly hybrid = {
    search: (
      collection: string,
      options: {
        query?: string;
        vector?: Float32Array | number[] | Uint8Array;
        dim?: number;
        elementType?: VectorElementType;
        metric?: VectorMetric;
        rrfK?: number;
        ftsWeight?: number;
        vectorWeight?: number;
        limit?: number;
        signal?: AbortSignal;
      } = {}
    ): Promise<HybridHit[]> =>
      this.run(client => client.hybridSearch(collection, options), options.signal),
  };

  readonly sql: SqlFunction = (() => {
    const fn = (async <T = Record<string, unknown>>(
      stringsOrQuery: TemplateStringsArray | string,
      ...valuesOrParams: unknown[]
    ): Promise<T[]> => {
      if (Array.isArray(stringsOrQuery) && "raw" in stringsOrQuery) {
        const { sql, params } = buildTaggedQuery(stringsOrQuery as TemplateStringsArray, valuesOrParams as JsonValue[]);
        return (await this.run(client => client.sqlQuery<T>(sql, params))).records;
      }
      return (await this.run(client => client.sqlQuery<T>(stringsOrQuery as string, (valuesOrParams[0] as JsonValue[]) ?? []))).records;
    }) as unknown as SqlFunction;

    fn.query = <T = Record<string, unknown>>(sql: string, params: JsonValue[] = [], options: SqlQueryOptions = {}): Promise<T[]> =>
      this.run(async client => (await client.sqlQuery<T>(sql, params, options.signal)).records, options.signal);

    fn.execute = (sql: string, params: JsonValue[] = [], options: SqlQueryOptions = {}): Promise<{ rowsAffected: number }> =>
      this.run(async client => ({ rowsAffected: (await client.sqlQuery(sql, params, options.signal)).rowsAffected }), options.signal);

    fn.raw = <T = Record<string, unknown>>(sql: string, params: JsonValue[] = [], options: SqlQueryOptions = {}): Promise<SqlQueryResult<T>> =>
      this.run(client => client.sqlQuery<T>(sql, params, options.signal), options.signal);

    return fn;
  })();

  watch(
    namespaceOrCallback?: string | ((event: DatabaseEvent) => void),
    callbackOrOptions?: ((event: DatabaseEvent) => void) | WatchOptions,
    maybeOptions?: WatchOptions
  ): Promise<WatchSubscription> {
    const namespace = typeof namespaceOrCallback === "string" ? namespaceOrCallback : undefined;
    const cb = typeof namespaceOrCallback === "function" ? namespaceOrCallback : (typeof callbackOrOptions === "function" ? callbackOrOptions : undefined);
    const options = typeof callbackOrOptions === "object" ? callbackOrOptions : maybeOptions;

    if (!cb) {
      throw new TypeError("watch callback must be a function");
    }
    return this.run(client => client.watch(namespace, cb, options), options?.signal);
  }

  batch(operations: readonly BatchOperation[], options: { signal?: AbortSignal } = {}): Promise<MutationResult[]> {
    return this.run(client => client.batch(operations, options.signal), options.signal);
  }

  integrityCheck(options: { signal?: AbortSignal } = {}): Promise<boolean> {
    return this.run(client => client.integrityCheck(options.signal), options.signal);
  }

  backup(destination: string, options: { signal?: AbortSignal } = {}): Promise<void> {
    return this.run(client => client.backup(destination, options.signal), options.signal);
  }

  async close(options: CloseOptions = {}): Promise<void> {
    options.signal?.throwIfAborted();
    const timeout = options.timeoutMs ?? 5_000;
    if (!Number.isInteger(timeout) || timeout < 0 || timeout > 0x7fff_ffff) {
      throw new RangeError("close timeoutMs must be an integer in 0..=2147483647");
    }
    if (!this.closing) {
      this.closed = true;
      // Include calls accepted by JS but not yet admitted by a native worker.
      // Caller timeout does not cancel these promises or the eventual cleanup.
      this.closing = Promise.allSettled([...this.pending]).then(() => this.client.close(0xffff_ffff));
    }

    const resolvers = Promise.withResolvers<void>();
    const timer = setTimeout(() => {
      resolvers.reject(lifecycleError("TIMEOUT", "close is still draining; operations are not cancelled", true));
    }, timeout);

    let onAbort: (() => void) | undefined;
    if (options.signal) {
      onAbort = () => {
        resolvers.reject(options.signal!.reason ?? new DOMException("The operation was aborted", "AbortError"));
      };
      options.signal.addEventListener("abort", onAbort, { once: true });
    }

    this.closing
      .then(() => resolvers.resolve(), (err) => resolvers.reject(err))
      .finally(() => {
        clearTimeout(timer);
        if (options.signal && onAbort) {
          options.signal.removeEventListener("abort", onAbort);
        }
      });

    return resolvers.promise;
  }

  private async run<T>(action: (client: Client) => Promise<T>, signal?: AbortSignal): Promise<T> {
    signal?.throwIfAborted();
    if (this.closed) throw lifecycleError("CLOSED", "database is closing or closed", false);

    const operation = Promise.resolve().then(() => action(this.client));
    this.pending.add(operation);

    const cleanup = () => this.pending.delete(operation);
    void operation.then(cleanup, cleanup);

    if (!signal) return await operation;

    const resolvers = Promise.withResolvers<T>();
    const onAbort = () => {
      resolvers.reject(signal.reason ?? new DOMException("The operation was aborted", "AbortError"));
    };
    signal.addEventListener("abort", onAbort, { once: true });

    operation
      .then(resolvers.resolve, resolvers.reject)
      .finally(() => signal.removeEventListener("abort", onAbort));

    return await resolvers.promise;
  }

  private async *kvScanStream(namespace: string, options: ScanStreamOptions<Uint8Array | string>): AsyncIterableIterator<KvEntry> {
    options.signal?.throwIfAborted();
    const batchSize = Math.max(1, options.batchSize ?? 100);
    let remaining = options.limit !== undefined ? Math.max(0, options.limit) : Infinity;
    let cursor = options.after ? toUint8Array(options.after) : undefined;

    while (remaining > 0) {
      options.signal?.throwIfAborted();
      const fetchLimit = Math.min(batchSize, remaining);
      const batch = await this.run(client => client.kvScan(namespace, cursor, fetchLimit, options.signal), options.signal);
      if (batch.length === 0) {
        break;
      }
      for (const entry of batch) {
        options.signal?.throwIfAborted();
        yield entry;
        remaining--;
        if (remaining <= 0) {
          break;
        }
      }
      if (batch.length < fetchLimit) {
        break;
      }
      cursor = batch[batch.length - 1]!.key;
    }
  }

  private async *jsonScanStream<T = JsonValue>(space: string, options: ScanStreamOptions<string>): AsyncIterableIterator<JsonEntry<T>> {
    options.signal?.throwIfAborted();
    const batchSize = Math.max(1, options.batchSize ?? 100);
    let remaining = options.limit !== undefined ? Math.max(0, options.limit) : Infinity;
    let cursor = options.after;

    while (remaining > 0) {
      options.signal?.throwIfAborted();
      const fetchLimit = Math.min(batchSize, remaining);
      const batch = await this.run(client => client.jsonScan<T>(space, cursor, fetchLimit, options.signal), options.signal);
      if (batch.length === 0) {
        break;
      }
      for (const entry of batch) {
        options.signal?.throwIfAborted();
        yield entry;
        remaining--;
        if (remaining <= 0) {
          break;
        }
      }
      if (batch.length < fetchLimit) {
        break;
      }
      cursor = batch[batch.length - 1]!.id;
    }
  }

  private async *jsonFindStream<T = JsonValue>(space: string, query: Record<string, JsonValue>, options: ScanStreamOptions<string>): AsyncIterableIterator<JsonEntry<T>> {
    options.signal?.throwIfAborted();
    const batchSize = Math.max(1, options.batchSize ?? 100);
    let remaining = options.limit !== undefined ? Math.max(0, options.limit) : Infinity;
    let cursor = options.after;

    while (remaining > 0) {
      options.signal?.throwIfAborted();
      const fetchLimit = Math.min(batchSize, remaining);
      const batch = await this.run(client => client.jsonFind<T>(space, query, cursor, fetchLimit, options.signal), options.signal);
      if (batch.length === 0) {
        break;
      }
      for (const entry of batch) {
        options.signal?.throwIfAborted();
        yield entry;
        remaining--;
        if (remaining <= 0) {
          break;
        }
      }
      if (batch.length < fetchLimit) {
        break;
      }
      cursor = batch[batch.length - 1]!.id;
    }
  }
}

function lifecycleError(code: string, message: string, retryable: boolean): LiliaError {
  return new LiliaError({ code, message, retryable, request_id: getUuid() });
}

export const quantization = {
  toInt8(floats: Float32Array | number[]): { quantized: Int8Array; scale: number } {
    let maxAbs = 0;
    for (let i = 0; i < floats.length; i++) {
      const abs = Math.abs(floats[i]!);
      if (abs > maxAbs) maxAbs = abs;
    }
    if (maxAbs === 0) {
      return { quantized: new Int8Array(floats.length), scale: 1.0 };
    }
    const scale = maxAbs / 127.0;
    const invScale = 1.0 / scale;
    const quantized = new Int8Array(floats.length);
    for (let i = 0; i < floats.length; i++) {
      quantized[i] = Math.max(-128, Math.min(127, Math.round(floats[i]! * invScale)));
    }
    return { quantized, scale };
  },

  toBit(floats: Float32Array | number[]): Uint8Array {
    const numBytes = Math.ceil(floats.length / 8);
    const packed = new Uint8Array(numBytes);
    for (let i = 0; i < floats.length; i++) {
      if (floats[i]! > 0) {
        const byteIdx = Math.floor(i / 8);
        const bitIdx = 7 - (i % 8);
        packed[byteIdx]! |= 1 << bitIdx;
      }
    }
    return packed;
  },

  cosineDistance(a: Float32Array | number[], b: Float32Array | number[]): number {
    const len = Math.min(a.length, b.length);
    if (len === 0) return 1.0;
    let dot = 0,
      normA = 0,
      normB = 0;
    for (let i = 0; i < len; i++) {
      const va = a[i]!,
        vb = b[i]!;
      dot += va * vb;
      normA += va * va;
      normB += vb * vb;
    }
    if (normA <= 0 || normB <= 0) return 1.0;
    const sim = dot / (Math.sqrt(normA) * Math.sqrt(normB));
    return Math.max(0, Math.min(2, 1.0 - sim));
  },
};

export type {
  VectorElementType,
  VectorMetric,
  VectorHit,
  HybridHit,
};
