import initWasm, {
  init_opfs_vfs,
  WasmDatabase,
} from "../pkg/lilia_wasm.js";
import type {
  BatchOperation,
  FtsHit,
  GraphDirection,
  JsonEntry,
  JsonIndexInfo,
  JsonValue,
  KvEntry,
  MutationResult,
  SqlQueryResult,
  WasmOpenOptions,
} from "./types.js";
import { LiliaError } from "./types.js";
import { checkedExpiry, checkedVersion, readVersion } from "./version.js";
import { resolveVfs } from "./vfs.js";

let wasmInitPromise: Promise<unknown> | null = null;

export async function ensureWasmInitialized(
  customModule?: WebAssembly.Module | BufferSource | Response | Promise<Response>
): Promise<void> {
  if (!wasmInitPromise) {
    wasmInitPromise = (async () => {
      if (customModule) {
        await initWasm({ module_or_path: await customModule });
        return;
      }
      // If running under Node.js, load from filesystem
      if (
        typeof process !== "undefined" &&
        process.versions &&
        process.versions.node
      ) {
        try {
          const { readFile } = await import("node:fs/promises");
          const { fileURLToPath } = await import("node:url");
          const wasmPath = fileURLToPath(
            new URL("../pkg/lilia_wasm_bg.wasm", import.meta.url)
          );
          const bytes = await readFile(wasmPath);
          await initWasm({ module_or_path: bytes });
          return;
        } catch {
          // Fallback to default fetch init
        }
      }
      await initWasm();
    })();
  }
  await wasmInitPromise;
}

export class DirectWasmClient {
  private constructor(private readonly native: WasmDatabase) {}

  static async open(options: WasmOpenOptions): Promise<DirectWasmClient> {
    await ensureWasmInitialized(options.wasmModule);

    const vfs = resolveVfs(options.path, options.vfs);
    if (vfs === "opfs-sahpool") {
      await init_opfs_vfs();
    }

    const openOptions: Record<string, unknown> = {
      path: options.path,
      vfs,
    };
    if (options.durability) openOptions.durability = options.durability;
    if (options.busyTimeoutMs) openOptions.busyTimeoutMs = options.busyTimeoutMs;
    if (options.writerQueueCapacity)
      openOptions.writerQueueCapacity = options.writerQueueCapacity;
    if (options.encryptionKey) openOptions.encryptionKey = options.encryptionKey;

    try {
      const db = await WasmDatabase.open(openOptions);
      return new DirectWasmClient(db);
    } catch (error) {
      throw parseWasmError(error);
    }
  }

  async kvGet(namespace: string, key: Uint8Array): Promise<KvEntry | undefined> {
    try {
      const entry = this.native.kv_get(namespace, key) as
        | {
            namespace: string;
            key: number[] | Uint8Array;
            value: number[] | Uint8Array;
            version: string;
            expiresAtMs?: number;
          }
        | undefined;
      if (!entry) return undefined;
      return {
        namespace: entry.namespace,
        key: entry.key instanceof Uint8Array ? entry.key : new Uint8Array(entry.key),
        value: entry.value instanceof Uint8Array ? entry.value : new Uint8Array(entry.value),
        version: readVersion(entry.version),
        expiresAtMs: entry.expiresAtMs,
      };
    } catch (error) {
      throw parseWasmError(error);
    }
  }

  async kvScan(
    namespace: string,
    after: Uint8Array | undefined,
    limit: number
  ): Promise<KvEntry[]> {
    try {
      const entries = (this.native.kv_scan(namespace, after, limit) ?? []) as Array<{
        namespace: string;
        key: number[] | Uint8Array;
        value: number[] | Uint8Array;
        version: string;
        expiresAtMs?: number;
      }>;
      return entries.map(entry => ({
        namespace: entry.namespace,
        key: entry.key instanceof Uint8Array ? entry.key : new Uint8Array(entry.key),
        value: entry.value instanceof Uint8Array ? entry.value : new Uint8Array(entry.value),
        version: readVersion(entry.version),
        expiresAtMs: entry.expiresAtMs,
      }));
    } catch (error) {
      throw parseWasmError(error);
    }
  }

  async jsonGet(space: string, id: string): Promise<JsonEntry | undefined> {
    try {
      const raw = this.native.json_get(space, id);
      if (!raw) return undefined;
      return mapJson(JSON.parse(raw) as Record<string, unknown>);
    } catch (error) {
      throw parseWasmError(error);
    }
  }

  async jsonScan(space: string, after: string | undefined, limit: number): Promise<JsonEntry[]> {
    try {
      const raw = this.native.json_scan(space, after, limit);
      const list = JSON.parse(raw) as Array<Record<string, unknown>>;
      return list.map(mapJson);
    } catch (error) {
      throw parseWasmError(error);
    }
  }

  async jsonCreateIndex(space: string, name: string, path: string): Promise<void> {
    try {
      this.native.json_create_index(space, name, path);
    } catch (error) {
      throw parseWasmError(error);
    }
  }

  async jsonDropIndex(space: string, name: string): Promise<boolean> {
    try {
      return this.native.json_drop_index(space, name);
    } catch (error) {
      throw parseWasmError(error);
    }
  }

  async jsonListIndexes(space: string): Promise<JsonIndexInfo[]> {
    try {
      const raw = this.native.json_list_indexes(space);
      const list = JSON.parse(raw) as Array<{
        space: string;
        index_name: string;
        json_path: string;
      }>;
      return list.map(item => ({
        space: item.space,
        indexName: item.index_name,
        jsonPath: item.json_path,
      }));
    } catch (error) {
      throw parseWasmError(error);
    }
  }

  async jsonScanByIndex(
    space: string,
    indexName: string,
    value: string,
    after: string | undefined,
    limit: number
  ): Promise<JsonEntry[]> {
    try {
      const raw = this.native.json_scan_by_index(space, indexName, value, after, limit);
      const list = JSON.parse(raw) as Array<Record<string, unknown>>;
      return list.map(mapJson);
    } catch (error) {
      throw parseWasmError(error);
    }
  }

  async jsonFind(
    space: string,
    query: Record<string, JsonValue>,
    after: string | undefined,
    limit: number
  ): Promise<JsonEntry[]> {
    try {
      const raw = this.native.json_find(space, JSON.stringify(query), after, limit);
      const list = JSON.parse(raw) as Array<Record<string, unknown>>;
      return list.map(mapJson);
    } catch (error) {
      throw parseWasmError(error);
    }
  }

  async batch(operations: readonly BatchOperation[]): Promise<MutationResult[]> {
    try {
      const wire = operations.map(toWireOperation);
      const raw = this.native.batch(JSON.stringify(wire));
      const result = JSON.parse(raw) as Array<{
        version?: string | null;
        deleted: boolean;
      }>;
      return result.map(item => ({
        deleted: item.deleted,
        version: item.version == null ? undefined : readVersion(item.version),
      }));
    } catch (error) {
      throw parseWasmError(error);
    }
  }

  async graphNeighbors(
    graph: string,
    node: string,
    direction?: GraphDirection,
    kind?: string
  ): Promise<string[]> {
    try {
      return (this.native.graph_neighbors(
        graph,
        node,
        direction,
        kind
      ) ?? []) as string[];
    } catch (error) {
      throw parseWasmError(error);
    }
  }

  async graphReachability(
    graph: string,
    startNodes: string[],
    direction?: GraphDirection
  ): Promise<string[]> {
    try {
      return (this.native.graph_reachability(
        graph,
        startNodes,
        direction
      ) ?? []) as string[];
    } catch (error) {
      throw parseWasmError(error);
    }
  }

  async ftsSearch(
    collection: string,
    query: string,
    limit?: number
  ): Promise<FtsHit[]> {
    try {
      const hits = (this.native.fts_search(
        collection,
        query,
        limit
      ) ?? []) as Array<{
        collection: string;
        docId?: string;
        doc_id?: string;
        snippet: string;
        rank: number;
      }>;
      return hits.map(h => ({
        collection: h.collection,
        docId: h.docId ?? h.doc_id ?? "",
        snippet: h.snippet,
        rank: h.rank,
      }));
    } catch (error) {
      throw parseWasmError(error);
    }
  }

  async sqlQuery(
    sql: string,
    params: unknown[] = []
  ): Promise<SqlQueryResult> {
    try {
      const raw = this.native.sql_query(sql, JSON.stringify(params));
      return mapSqlQueryResult(JSON.parse(raw) as Record<string, unknown>);
    } catch (error) {
      throw parseWasmError(error);
    }
  }

  async integrityCheck(): Promise<boolean> {
    try {
      return this.native.integrity_check();
    } catch (error) {
      throw parseWasmError(error);
    }
  }

  async close(timeoutMs?: number): Promise<void> {
    try {
      this.native.close(timeoutMs);
    } catch (error) {
      throw parseWasmError(error);
    }
  }
}

function mapJson(entry: Record<string, unknown>): JsonEntry {
  return {
    space: String(entry.space),
    id: String(entry.id),
    value: entry.value as JsonEntry["value"],
    version: readVersion(entry.version),
  };
}

function toWireOperation(operation: BatchOperation): Record<string, unknown> {
  const result = { ...operation } as Record<string, unknown>;
  if ("ifVersion" in operation) {
    result.if_version = checkedVersion(operation.ifVersion)?.toString();
    delete result.ifVersion;
  }
  if ("expiresAtMs" in operation) {
    result.expires_at_ms = checkedExpiry(operation.expiresAtMs);
    delete result.expiresAtMs;
  }
  if (operation.model === "kv_set" || operation.model === "kv_delete") {
    result.key = [...operation.key];
    if (operation.model === "kv_set") result.value = [...operation.value];
  }
  if (operation.model === "graph_set_node") {
    result.metadata = operation.metadata ?? {};
  }
  if (operation.model === "fts_index" || operation.model === "fts_delete") {
    result.doc_id = operation.docId;
    delete result.docId;
  }
  return result;
}

export function mapSqlQueryResult<T = Record<string, unknown>>(
  raw: Record<string, unknown>
): SqlQueryResult<T> {
  const columns = (raw.columns as string[]) ?? [];
  const rows = (raw.rows as JsonValue[][]) ?? [];
  const rowsAffected = Number(raw.rows_affected ?? 0);
  const records: T[] = rows.map(row => {
    const obj: Record<string, unknown> = {};
    for (let i = 0; i < columns.length; i++) {
      const col = columns[i];
      if (col !== undefined) {
        obj[col] = row[i];
      }
    }
    return obj as T;
  });
  return { columns, rows, rowsAffected, records };
}

function parseWasmError(error: unknown): Error {
  if (typeof error === "string") {
    try {
      const shape = JSON.parse(error) as { code: string; message: string; retryable?: boolean };
      if (shape && typeof shape.code === "string" && typeof shape.message === "string") {
        return new LiliaError({
          code: shape.code,
          message: shape.message,
          retryable: shape.retryable ?? false,
        });
      }
    } catch {}
    return new Error(error);
  }
  if (error instanceof Error) {
    try {
      const shape = JSON.parse(error.message) as { code: string; message: string; retryable?: boolean };
      if (shape && typeof shape.code === "string" && typeof shape.message === "string") {
        return new LiliaError({
          code: shape.code,
          message: shape.message,
          retryable: shape.retryable ?? false,
        });
      }
    } catch {}
    return error;
  }
  return new Error(String(error));
}
