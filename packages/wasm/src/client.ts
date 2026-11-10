import initWasm, {
  init_opfs_vfs,
  WasmDatabase,
} from "../pkg/lilia_wasm.js";
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

  async vectorGet(collection: string, id: string): Promise<Uint8Array | null> {
    try {
      const res = this.native.vector_get(collection, id);
      return res ? new Uint8Array(res) : null;
    } catch (error) {
      throw parseWasmError(error);
    }
  }

  async vectorSearch(
    collection: string,
    query: Float32Array | number[] | Uint8Array,
    dim?: number,
    elementType?: VectorElementType,
    metric?: VectorMetric,
    limit?: number
  ): Promise<VectorHit[]> {
    try {
      const raw = vectorToBytes(query, elementType);
      const d = dim ?? (query instanceof Uint8Array ? query.byteLength / 4 : query.length);
      const hits = (this.native.vector_search(
        collection,
        raw,
        d,
        elementType,
        metric,
        limit
      ) ?? []) as VectorHit[];
      return hits;
    } catch (error) {
      throw parseWasmError(error);
    }
  }

  async hybridSearch(
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
    try {
      const raw = options.vector ? vectorToBytes(options.vector, options.elementType) : undefined;
      const d = options.dim ?? (options.vector ? (options.vector instanceof Uint8Array ? options.vector.byteLength / 4 : options.vector.length) : undefined);
      const hits = (this.native.hybrid_search(
        collection,
        options.query,
        raw,
        d,
        options.elementType,
        options.metric,
        options.rrfK,
        options.ftsWeight,
        options.vectorWeight,
        options.limit
      ) ?? []) as HybridHit[];
      return hits;
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
  if (operation.model === "vector_set") {
    result.element_type = operation.elementType ?? "float32";
    result.vector = [...vectorToBytes(operation.vector, operation.elementType)];
  }
  return result;
}

export function vectorToBytes(
  v: Float32Array | number[] | Uint8Array,
  elementType?: VectorElementType
): Uint8Array {
  if (v instanceof Uint8Array) return v;
  if (elementType === "int8") {
    let maxAbs = 0;
    for (let i = 0; i < v.length; i++) {
      const abs = Math.abs(v[i]!);
      if (abs > maxAbs) maxAbs = abs;
    }
    const scale = maxAbs === 0 ? 1.0 : maxAbs / 127.0;
    const invScale = 1.0 / scale;
    const quantized = new Int8Array(v.length);
    for (let i = 0; i < v.length; i++) {
      quantized[i] = Math.max(-128, Math.min(127, Math.round(v[i]! * invScale)));
    }
    return new Uint8Array(quantized.buffer, quantized.byteOffset, quantized.byteLength);
  } else if (elementType === "bit") {
    const numBytes = Math.ceil(v.length / 8);
    const packed = new Uint8Array(numBytes);
    for (let i = 0; i < v.length; i++) {
      if (v[i]! > 0) {
        const byteIdx = Math.floor(i / 8);
        const bitIdx = 7 - (i % 8);
        packed[byteIdx]! |= 1 << bitIdx;
      }
    }
    return packed;
  } else {
    const f32 = v instanceof Float32Array ? v : new Float32Array(v);
    return new Uint8Array(f32.buffer, f32.byteOffset, f32.byteLength);
  }
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
