import { DirectWasmClient, ensureWasmInitialized } from "./client.js";
import { WorkerClient } from "./worker-bridge.js";
import { isBrowser, isDedicatedWorker, resolveVfs } from "./vfs.js";
import type {
  BatchOperation,
  CreateIndexOptions,
  FtsHit,
  GraphDirection,
  JsonEntry,
  JsonFindOptions,
  JsonIndexInfo,
  JsonValue,
  KvEntry,
  MutationResult,
  OpenOptions,
  ScanStreamOptions,
  SqlQueryResult,
  WasmOpenOptions,
} from "./types.js";
import { LiliaError } from "./types.js";

export * from "./types.js";
export { ensureWasmInitialized };

type Client = DirectWasmClient | WorkerClient;

export class Database {
  private closed = false;
  private readonly pending = new Set<Promise<unknown>>();
  private closing?: Promise<void>;

  private constructor(private readonly client: Client) {}

  static async open(options: WasmOpenOptions): Promise<Database> {
    const vfs = resolveVfs(options.path, options.vfs);

    // In a browser main UI thread with OPFS, sync access handles require a dedicated worker
    const shouldUseWorker =
      isBrowser() && !isDedicatedWorker() && (vfs === "opfs" || vfs === "opfs-sahpool");

    const client = shouldUseWorker
      ? await WorkerClient.open(options)
      : await DirectWasmClient.open(options);

    return new Database(client);
  }

  readonly kv = {
    get: (namespace: string, key: Uint8Array): Promise<KvEntry | undefined> =>
      this.run(client => client.kvGet(namespace, key)),

    set: async (
      namespace: string,
      key: Uint8Array,
      value: Uint8Array,
      options: { ifVersion?: bigint; expiresAtMs?: number } = {}
    ): Promise<MutationResult> =>
      (await this.batch([{ model: "kv_set", namespace, key, value, ...options }]))[0]!,

    delete: async (
      namespace: string,
      key: Uint8Array,
      options: { ifVersion?: bigint } = {}
    ): Promise<MutationResult> =>
      (await this.batch([{ model: "kv_delete", namespace, key, ...options }]))[0]!,

    scan: (
      namespace: string,
      options: { after?: Uint8Array; limit?: number } = {}
    ): Promise<KvEntry[]> =>
      this.run(client => client.kvScan(namespace, options.after, options.limit ?? 100)),

    scanStream: (
      namespace: string,
      options: ScanStreamOptions<Uint8Array> = {}
    ): AsyncIterableIterator<KvEntry> => this.kvScanStream(namespace, options),

    stream: (
      namespace: string,
      options: ScanStreamOptions<Uint8Array> = {}
    ): AsyncIterableIterator<KvEntry> => this.kvScanStream(namespace, options),
  };

  readonly json = {
    get: (space: string, id: string): Promise<JsonEntry | undefined> =>
      this.run(client => client.jsonGet(space, id)),

    put: async (
      space: string,
      id: string,
      value: JsonEntry["value"],
      options: { ifVersion?: bigint } = {}
    ): Promise<MutationResult> =>
      (await this.batch([{ model: "json_put", space, id, value, ...options }]))[0]!,

    delete: async (
      space: string,
      id: string,
      options: { ifVersion?: bigint } = {}
    ): Promise<MutationResult> =>
      (await this.batch([{ model: "json_delete", space, id, ...options }]))[0]!,

    scan: (
      space: string,
      options: { after?: string; limit?: number } = {}
    ): Promise<JsonEntry[]> =>
      this.run(client => client.jsonScan(space, options.after, options.limit ?? 100)),

    scanStream: (
      space: string,
      options: ScanStreamOptions<string> = {}
    ): AsyncIterableIterator<JsonEntry> => this.jsonScanStream(space, options),

    stream: (
      space: string,
      options: ScanStreamOptions<string> = {}
    ): AsyncIterableIterator<JsonEntry> => this.jsonScanStream(space, options),

    createIndex: (
      space: string,
      fieldOrPath: string,
      optionsOrPath?: string | CreateIndexOptions
    ): Promise<void> => {
      let name: string;
      let path: string;
      if (typeof optionsOrPath === "string") {
        name = fieldOrPath;
        path = optionsOrPath;
      } else if (typeof optionsOrPath === "object" && optionsOrPath !== null) {
        name = optionsOrPath.name ?? fieldOrPath;
        path =
          optionsOrPath.path ??
          (fieldOrPath.startsWith("$") || fieldOrPath.startsWith("/")
            ? fieldOrPath
            : `$.${fieldOrPath}`);
      } else {
        name = fieldOrPath;
        path =
          fieldOrPath.startsWith("$") || fieldOrPath.startsWith("/")
            ? fieldOrPath
            : `$.${fieldOrPath}`;
      }
      return this.run(client => client.jsonCreateIndex(space, name, path));
    },

    dropIndex: (space: string, indexName: string): Promise<boolean> =>
      this.run(client => client.jsonDropIndex(space, indexName)),

    listIndexes: (space: string): Promise<JsonIndexInfo[]> =>
      this.run(client => client.jsonListIndexes(space)),

    findByIndex: (
      space: string,
      indexName: string,
      value: string | number | boolean,
      options: JsonFindOptions = {}
    ): Promise<JsonEntry[]> =>
      this.run(client =>
        client.jsonScanByIndex(
          space,
          indexName,
          String(value),
          options.after,
          options.limit ?? 100
        )
      ),

    find: (
      space: string,
      query: Record<string, JsonValue>,
      options: JsonFindOptions = {}
    ): Promise<JsonEntry[]> =>
      this.run(client =>
        client.jsonFind(space, query, options.after, options.limit ?? 100)
      ),

    findStream: (
      space: string,
      query: Record<string, JsonValue>,
      options: ScanStreamOptions<string> = {}
    ): AsyncIterableIterator<JsonEntry> =>
      this.jsonFindStream(space, query, options),
  };

  readonly graph = {
    setNode: async (
      graph: string,
      id: string,
      kind: string,
      metadata?: JsonValue
    ): Promise<MutationResult> =>
      (await this.batch([{ model: "graph_set_node", graph, id, kind, metadata }]))[0]!,

    addEdge: async (
      graph: string,
      source: string,
      target: string,
      kind: string
    ): Promise<MutationResult> =>
      (await this.batch([{ model: "graph_add_edge", graph, source, target, kind }]))[0]!,

    neighbors: (
      graph: string,
      node: string,
      options: { direction?: GraphDirection; kind?: string } = {}
    ): Promise<string[]> =>
      this.run(client =>
        client.graphNeighbors(graph, node, options.direction, options.kind)
      ),

    reachability: (
      graph: string,
      startNodes: string[],
      options: { direction?: GraphDirection } = {}
    ): Promise<string[]> =>
      this.run(client =>
        client.graphReachability(graph, startNodes, options.direction)
      ),
  };

  readonly fts = {
    index: async (
      collection: string,
      docId: string,
      content: string
    ): Promise<MutationResult> =>
      (await this.batch([{ model: "fts_index", collection, docId, content }]))[0]!,

    search: (
      collection: string,
      query: string,
      options: { limit?: number } = {}
    ): Promise<FtsHit[]> =>
      this.run(client => client.ftsSearch(collection, query, options.limit)),
  };

  readonly sql = {
    query: <T = Record<string, unknown>>(
      sql: string,
      params: JsonValue[] = []
    ): Promise<T[]> =>
      this.run(async client => (await client.sqlQuery(sql, params)).records as T[]),

    execute: (
      sql: string,
      params: JsonValue[] = []
    ): Promise<{ rowsAffected: number }> =>
      this.run(async client => ({
        rowsAffected: (await client.sqlQuery(sql, params)).rowsAffected,
      })),

    raw: <T = Record<string, unknown>>(
      sql: string,
      params: JsonValue[] = []
    ): Promise<SqlQueryResult<T>> =>
      this.run(client => client.sqlQuery(sql, params) as Promise<SqlQueryResult<T>>),
  };

  batch(operations: readonly BatchOperation[]): Promise<MutationResult[]> {
    return this.run(client => client.batch(operations));
  }

  integrityCheck(): Promise<boolean> {
    return this.run(client => client.integrityCheck());
  }

  async close(options: { timeoutMs?: number } = {}): Promise<void> {
    const timeout = options.timeoutMs ?? 5_000;
    if (!Number.isInteger(timeout) || timeout < 0 || timeout > 0x7fff_ffff) {
      throw new RangeError("close timeoutMs must be an integer in 0..=2147483647");
    }
    if (!this.closing) {
      this.closed = true;
      this.closing = Promise.allSettled([...this.pending]).then(() =>
        this.client.close(0xffff_ffff)
      );
    }
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([
        this.closing,
        new Promise<never>((_, reject) => {
          timer = setTimeout(
            () =>
              reject(
                new LiliaError({
                  code: "TIMEOUT",
                  message: "close is still draining; operations are not cancelled",
                  retryable: true,
                })
              ),
            timeout
          );
        }),
      ]);
    } finally {
      clearTimeout(timer);
    }
  }

  private run<T>(action: (client: Client) => Promise<T>): Promise<T> {
    if (this.closed) {
      return Promise.reject(
        new LiliaError({
          code: "CLOSED",
          message: "database is closing or closed",
          retryable: false,
        })
      );
    }
    const operation = Promise.resolve().then(() => action(this.client));
    this.pending.add(operation);
    void operation.then(
      () => this.pending.delete(operation),
      () => this.pending.delete(operation)
    );
    return operation;
  }

  private async *kvScanStream(
    namespace: string,
    options: ScanStreamOptions<Uint8Array>
  ): AsyncIterableIterator<KvEntry> {
    const batchSize = Math.max(1, options.batchSize ?? 100);
    let remaining = options.limit !== undefined ? Math.max(0, options.limit) : Infinity;
    let cursor = options.after;

    while (remaining > 0) {
      const fetchLimit = Math.min(batchSize, remaining);
      const batch = await this.run(client =>
        client.kvScan(namespace, cursor, fetchLimit)
      );
      if (batch.length === 0) {
        break;
      }
      for (const entry of batch) {
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

  private async *jsonScanStream(
    space: string,
    options: ScanStreamOptions<string>
  ): AsyncIterableIterator<JsonEntry> {
    const batchSize = Math.max(1, options.batchSize ?? 100);
    let remaining = options.limit !== undefined ? Math.max(0, options.limit) : Infinity;
    let cursor = options.after;

    while (remaining > 0) {
      const fetchLimit = Math.min(batchSize, remaining);
      const batch = await this.run(client =>
        client.jsonScan(space, cursor, fetchLimit)
      );
      if (batch.length === 0) {
        break;
      }
      for (const entry of batch) {
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

  private async *jsonFindStream(
    space: string,
    query: Record<string, JsonValue>,
    options: ScanStreamOptions<string>
  ): AsyncIterableIterator<JsonEntry> {
    const batchSize = Math.max(1, options.batchSize ?? 100);
    let remaining = options.limit !== undefined ? Math.max(0, options.limit) : Infinity;
    let cursor = options.after;

    while (remaining > 0) {
      const fetchLimit = Math.min(batchSize, remaining);
      const batch = await this.run(client =>
        client.jsonFind(space, query, cursor, fetchLimit)
      );
      if (batch.length === 0) {
        break;
      }
      for (const entry of batch) {
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
