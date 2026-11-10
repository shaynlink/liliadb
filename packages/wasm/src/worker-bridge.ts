import type {
  BatchOperation,
  FtsHit,
  GraphDirection,
  HybridHit,
  JsonEntry,
  JsonIndexInfo,
  JsonValue,
  KvEntry,
  MutationResult,
  SqlQueryResult,
  VectorElementType,
  VectorHit,
  VectorMetric,
  WasmOpenOptions,
} from "./types.js";
import { LiliaError } from "./types.js";
import { readVersion } from "./version.js";

export class WorkerClient {
  private nextId = 1;
  private readonly pending = new Map<
    number,
    { resolve: (val: any) => void; reject: (err: any) => void }
  >();
  private readonly worker: Worker;

  private constructor(worker: Worker) {
    this.worker = worker;
    this.worker.onmessage = (event: MessageEvent) => {
      const { id, success, result, error } = event.data as {
        id: number;
        success: boolean;
        result?: any;
        error?: { name: string; message: string; code?: string; retryable?: boolean };
      };

      const handler = this.pending.get(id);
      if (!handler) return;
      this.pending.delete(id);

      if (success) {
        handler.resolve(result);
      } else {
        if (error?.code) {
          handler.reject(
            new LiliaError({
              code: error.code,
              message: error.message,
              retryable: error.retryable ?? false,
            })
          );
        } else {
          const err = new Error(error?.message ?? "Worker error");
          err.name = error?.name ?? "Error";
          handler.reject(err);
        }
      }
    };

    this.worker.onerror = (err: ErrorEvent) => {
      for (const handler of this.pending.values()) {
        handler.reject(new Error(err.message ?? "Web Worker error"));
      }
      this.pending.clear();
    };
  }

  static async open(options: WasmOpenOptions): Promise<WorkerClient> {
    let worker: Worker;
    if (options.worker instanceof Worker) {
      worker = options.worker;
    } else if (typeof options.worker === "string" || options.worker instanceof URL) {
      worker = new Worker(options.worker, { type: "module" });
    } else {
      // Default: load worker relative to current module
      const workerUrl = new URL("./worker.js", import.meta.url);
      worker = new Worker(workerUrl, { type: "module" });
    }

    const client = new WorkerClient(worker);
    await client.request("open", options);
    return client;
  }

  private request<T>(type: string, payload?: any): Promise<T> {
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.worker.postMessage({ id, type, payload });
    });
  }

  async kvGet(namespace: string, key: Uint8Array): Promise<KvEntry | undefined> {
    const entry = await this.request<any>("kvGet", { namespace, key });
    if (!entry) return undefined;
    return {
      namespace: entry.namespace,
      key: entry.key instanceof Uint8Array ? entry.key : new Uint8Array(entry.key),
      value: entry.value instanceof Uint8Array ? entry.value : new Uint8Array(entry.value),
      version: readVersion(entry.version),
      expiresAtMs: entry.expiresAtMs,
    };
  }

  async kvScan(
    namespace: string,
    after: Uint8Array | undefined,
    limit: number
  ): Promise<KvEntry[]> {
    const entries = (await this.request<any[]>("kvScan", { namespace, after, limit })) ?? [];
    return entries.map(entry => ({
      namespace: entry.namespace,
      key: entry.key instanceof Uint8Array ? entry.key : new Uint8Array(entry.key),
      value: entry.value instanceof Uint8Array ? entry.value : new Uint8Array(entry.value),
      version: readVersion(entry.version),
      expiresAtMs: entry.expiresAtMs,
    }));
  }

  async jsonGet(space: string, id: string): Promise<JsonEntry | undefined> {
    const entry = await this.request<any>("jsonGet", { space, id });
    if (!entry) return undefined;
    return {
      space: entry.space,
      id: entry.id,
      value: entry.value,
      version: readVersion(entry.version),
    };
  }

  async jsonScan(space: string, after: string | undefined, limit: number): Promise<JsonEntry[]> {
    const list = (await this.request<any[]>("jsonScan", { space, after, limit })) ?? [];
    return list.map(entry => ({
      space: entry.space,
      id: entry.id,
      value: entry.value,
      version: readVersion(entry.version),
    }));
  }

  jsonCreateIndex(space: string, name: string, path: string): Promise<void> {
    return this.request("jsonCreateIndex", { space, name, path });
  }

  jsonDropIndex(space: string, name: string): Promise<boolean> {
    return this.request("jsonDropIndex", { space, name });
  }

  jsonListIndexes(space: string): Promise<JsonIndexInfo[]> {
    return this.request("jsonListIndexes", { space });
  }

  async jsonScanByIndex(
    space: string,
    indexName: string,
    value: string,
    after: string | undefined,
    limit: number
  ): Promise<JsonEntry[]> {
    const list =
      (await this.request<any[]>("jsonScanByIndex", {
        space,
        indexName,
        value,
        after,
        limit,
      })) ?? [];
    return list.map(entry => ({
      space: entry.space,
      id: entry.id,
      value: entry.value,
      version: readVersion(entry.version),
    }));
  }

  async jsonFind(
    space: string,
    query: Record<string, JsonValue>,
    after: string | undefined,
    limit: number
  ): Promise<JsonEntry[]> {
    const list =
      (await this.request<any[]>("jsonFind", {
        space,
        query,
        after,
        limit,
      })) ?? [];
    return list.map(entry => ({
      space: entry.space,
      id: entry.id,
      value: entry.value,
      version: readVersion(entry.version),
    }));
  }

  async batch(operations: readonly BatchOperation[]): Promise<MutationResult[]> {
    const raw = await this.request<any[]>("batch", { operations });
    return raw.map(item => ({
      deleted: item.deleted,
      version: item.version == null ? undefined : readVersion(item.version),
    }));
  }

  graphNeighbors(
    graph: string,
    node: string,
    direction?: GraphDirection,
    kind?: string
  ): Promise<string[]> {
    return this.request("graphNeighbors", { graph, node, direction, kind });
  }

  graphReachability(
    graph: string,
    startNodes: string[],
    direction?: GraphDirection
  ): Promise<string[]> {
    return this.request("graphReachability", { graph, startNodes, direction });
  }

  ftsSearch(collection: string, query: string, limit?: number): Promise<FtsHit[]> {
    return this.request("ftsSearch", { collection, query, limit });
  }

  vectorGet(collection: string, id: string): Promise<Uint8Array | null> {
    return this.request("vectorGet", { collection, id });
  }

  vectorSearch(
    collection: string,
    query: Float32Array | number[] | Uint8Array,
    dim?: number,
    elementType?: VectorElementType,
    metric?: VectorMetric,
    limit?: number
  ): Promise<VectorHit[]> {
    return this.request("vectorSearch", { collection, query, dim, elementType, metric, limit });
  }

  hybridSearch(
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
    } = {}
  ): Promise<HybridHit[]> {
    return this.request("hybridSearch", { collection, options });
  }

  sqlQuery(sql: string, params: unknown[] = []): Promise<SqlQueryResult> {
    return this.request("sqlQuery", { sql, params });
  }

  integrityCheck(): Promise<boolean> {
    return this.request("integrityCheck");
  }

  async close(timeoutMs?: number): Promise<void> {
    try {
      await this.request("close", { timeoutMs });
    } finally {
      this.worker.terminate();
    }
  }
}
