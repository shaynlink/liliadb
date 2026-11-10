import { readFile } from "node:fs/promises";
import { connect, type Socket } from "node:net";
import { decode, encode } from "@msgpack/msgpack";
import {
  LiliaError,
  type BatchOperation,
  type DatabaseEvent,
  type FtsHit,
  type GraphDirection,
  type HybridHit,
  type JsonEntry,
  type JsonIndexInfo,
  type JsonValue,
  type KvEntry,
  type MutationResult,
  type SqlQueryResult,
  type VectorElementType,
  type VectorHit,
  type VectorMetric,
  type WatchSubscription,
} from "./types.js";
import { checkedVersion, checkedExpiry, readVersion, jsonNumbers, getUuid } from "./version.js";
import { mapDatabaseEvent, mapSqlQueryResult, vectorToBytes } from "./native.js";

interface WireResponse { id: string; result: { Ok?: unknown; Err?: { message: string; code: string; retryable: boolean; request_id: string | Uint8Array } } }

export class DaemonClient {
  private activeSocket?: Socket;
  private socketBuffer = Buffer.alloc(0);
  private readonly pendingCalls = new Map<string, PromiseWithResolvers<WireResponse>>();
  private connecting?: PromiseWithResolvers<Socket>;

  private constructor(private readonly endpoint: string, private readonly token: string) {}

  static async open(endpoint: string, tokenFile: string, options?: { signal?: AbortSignal }): Promise<DaemonClient> {
    options?.signal?.throwIfAborted();
    const token = (await readFile(tokenFile, "utf8")).trim();
    const client = new DaemonClient(endpoint, token);
    await client.call({ type: "handshake", major: 1, minor: 0 }, options?.signal);
    return client;
  }

  async kvGet(namespace: string, key: Uint8Array, signal?: AbortSignal): Promise<KvEntry | undefined> {
    return mapKv(await this.call({ type: "kv_get", namespace, key }, signal)) as KvEntry | undefined;
  }
  async kvScan(namespace: string, after: Uint8Array | undefined, limit: number, signal?: AbortSignal): Promise<KvEntry[]> {
    return ((await this.call({ type: "kv_scan", namespace, after: after ? [...after] : undefined, limit }, signal)) as unknown[]).map(value => mapKv(value) as KvEntry);
  }
  async jsonGet<T = JsonValue>(space: string, id: string, signal?: AbortSignal): Promise<JsonEntry<T> | undefined> {
    return mapJson<T>(await this.call({ type: "json_get", space, id }, signal));
  }
  async jsonScan<T = JsonValue>(space: string, after: string | undefined, limit: number, signal?: AbortSignal): Promise<JsonEntry<T>[]> {
    return ((await this.call({ type: "json_scan", space, after, limit }, signal)) as unknown[]).map(value => mapJson<T>(value)!);
  }
  async jsonCreateIndex(space: string, name: string, path: string, signal?: AbortSignal): Promise<void> {
    await this.call({ type: "json_create_index", space, name, path }, signal);
  }
  async jsonDropIndex(space: string, name: string, signal?: AbortSignal): Promise<boolean> {
    return Boolean(await this.call({ type: "json_drop_index", space, name }, signal));
  }
  async jsonListIndexes(space: string, signal?: AbortSignal): Promise<JsonIndexInfo[]> {
    const list = (await this.call({ type: "json_list_indexes", space }, signal)) as Array<{ space: string; index_name?: string; indexName?: string; json_path?: string; jsonPath?: string }>;
    return (list ?? []).map(item => ({
      space: item.space,
      indexName: item.indexName ?? item.index_name ?? "",
      jsonPath: item.jsonPath ?? item.json_path ?? "",
    }));
  }
  async jsonScanByIndex<T = JsonValue>(space: string, indexName: string, value: string, after: string | undefined, limit: number, signal?: AbortSignal): Promise<JsonEntry<T>[]> {
    const list = (await this.call({ type: "json_scan_by_index", space, index_name: indexName, value, after, limit }, signal)) as unknown[];
    return (list ?? []).map(val => mapJson<T>(val)!);
  }
  async jsonFind<T = JsonValue>(space: string, query: Record<string, JsonValue>, after: string | undefined, limit: number, signal?: AbortSignal): Promise<JsonEntry<T>[]> {
    const list = (await this.call({ type: "json_find", space, query, after, limit }, signal)) as unknown[];
    return (list ?? []).map(val => mapJson<T>(val)!);
  }
  async batch(operations: readonly BatchOperation[], signal?: AbortSignal): Promise<MutationResult[]> {
    const result = await this.call({ type: "batch", operations: operations.map(toWire) }, signal) as Array<{ version?: bigint | number | null; deleted: boolean }>;
    return result.map(item => ({ ...item, version: item.version == null ? undefined : readVersion(item.version) }));
  }
  async integrityCheck(signal?: AbortSignal): Promise<boolean> { return Boolean(await this.call({ type: "integrity_check" }, signal)); }
  async backup(destination: string, signal?: AbortSignal): Promise<void> { await this.call({ type: "backup", destination }, signal); }
  async sqlQuery<T = Record<string, unknown>>(sql: string, params: unknown[] = [], signal?: AbortSignal): Promise<SqlQueryResult<T>> {
    const raw = (await this.call({ type: "sql_query", sql, params }, signal)) as Record<string, unknown>;
    return mapSqlQueryResult<T>(raw);
  }
  async graphNeighbors(graph: string, node: string, direction?: GraphDirection, kind?: string, signal?: AbortSignal): Promise<string[]> {
    return ((await this.call({ type: "graph_neighbors", graph, node, direction: direction ?? "outgoing", kind }, signal)) as string[]) ?? [];
  }
  async graphReachability(graph: string, startNodes: string[], direction?: GraphDirection, signal?: AbortSignal): Promise<string[]> {
    return ((await this.call({ type: "graph_reachability", graph, start_nodes: startNodes, direction: direction ?? "outgoing" }, signal)) as string[]) ?? [];
  }
  async ftsSearch(collection: string, query: string, limit?: number, signal?: AbortSignal): Promise<FtsHit[]> {
    const hits = (await this.call({ type: "fts_search", collection, query, limit: limit ?? 10 }, signal)) as Array<Record<string, unknown>>;
    return (hits ?? []).map(h => ({
      collection: String(h.collection),
      docId: String(h.doc_id),
      snippet: String(h.snippet),
      rank: Number(h.rank),
    }));
  }
  async vectorGet(collection: string, id: string, signal?: AbortSignal): Promise<Uint8Array | null> {
    const raw = (await this.call({ type: "vector_get", collection, id }, signal)) as [number, string, Uint8Array] | null;
    return raw ? new Uint8Array(raw[2]) : null;
  }
  async vectorSearch(
    collection: string,
    query: Float32Array | number[] | Uint8Array,
    dim?: number,
    elementType?: VectorElementType,
    metric?: VectorMetric,
    limit?: number,
    signal?: AbortSignal
  ): Promise<VectorHit[]> {
    const raw = vectorToBytes(query, elementType);
    const d = dim ?? (query instanceof Uint8Array ? query.byteLength / 4 : query.length);
    const hits = (await this.call({
      type: "vector_search",
      collection,
      vector: raw,
      dim: d,
      element_type: elementType ?? "float32",
      metric: metric ?? "cosine",
      limit: limit ?? 10,
    }, signal)) as Array<Record<string, unknown>>;
    return (hits ?? []).map(h => ({
      collection: String(h.collection),
      id: String(h.id),
      distance: Number(h.distance),
    }));
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
      signal?: AbortSignal;
    } = {}
  ): Promise<HybridHit[]> {
    const raw = options.vector ? vectorToBytes(options.vector, options.elementType) : undefined;
    const d = options.dim ?? (options.vector ? (options.vector instanceof Uint8Array ? options.vector.byteLength / 4 : options.vector.length) : undefined);
    const hits = (await this.call({
      type: "hybrid_search",
      collection,
      fts_query: options.query,
      vector: raw,
      dim: d,
      element_type: options.elementType ?? (options.vector ? "float32" : undefined),
      metric: options.metric ?? (options.vector ? "cosine" : undefined),
      rrf_k: options.rrfK ?? 60,
      fts_weight: options.ftsWeight ?? 1.0,
      vector_weight: options.vectorWeight ?? 1.0,
      limit: options.limit ?? 10,
    }, options.signal)) as Array<Record<string, unknown>>;
    return (hits ?? []).map(h => ({
      id: String(h.id),
      rrfScore: Number(h.rrf_score ?? h.rrfScore ?? 0),
      ftsRank: h.fts_rank != null ? Number(h.fts_rank) : (h.ftsRank != null ? Number(h.ftsRank) : undefined),
      ftsSnippet: h.fts_snippet != null ? String(h.fts_snippet) : (h.ftsSnippet != null ? String(h.ftsSnippet) : undefined),
      vectorRank: h.vector_rank != null ? Number(h.vector_rank) : (h.vectorRank != null ? Number(h.vectorRank) : undefined),
      vectorDistance: h.vector_distance != null ? Number(h.vector_distance) : (h.vectorDistance != null ? Number(h.vectorDistance) : undefined),
    }));
  }
  async watch(
    namespace: string | undefined,
    callback: (event: DatabaseEvent) => void,
    options?: { signal?: AbortSignal }
  ): Promise<WatchSubscription> {
    options?.signal?.throwIfAborted();
    const socket = connect(this.endpoint);
    const connectResolvers = Promise.withResolvers<void>();
    socket.once("connect", () => connectResolvers.resolve());
    socket.once("error", (err) => connectResolvers.reject(err));
    await connectResolvers.promise;

    const payload = encode(
      { id: getUuid(), token: this.token, operation: { type: "subscribe", namespace } },
      { useBigInt64: true }
    );
    const header = Buffer.allocUnsafe(4);
    header.writeUInt32BE(payload.byteLength);
    socket.write(Buffer.concat([header, Buffer.from(payload)]));

    let buffer = Buffer.alloc(0);

    socket.on("data", (chunk: Buffer) => {
      buffer = Buffer.concat([buffer, chunk]);
      while (buffer.length >= 4) {
        const length = buffer.readUInt32BE(0);
        if (buffer.length < 4 + length) {
          break;
        }
        const frameData = buffer.subarray(4, 4 + length);
        buffer = buffer.subarray(4 + length);
        try {
          const wire = decode(frameData, { useBigInt64: true }) as WireResponse;
          if (wire.result.Err) {
            break;
          }
          if (wire.result.Ok) {
            const rawVal = wire.result.Ok as Record<string, unknown>;
            if (rawVal && typeof rawVal === "object" && rawVal.type === "event" && rawVal.value) {
              const event = mapDatabaseEvent(rawVal.value);
              if (event) {
                callback(event);
              }
            } else if (rawVal && typeof rawVal === "object" && rawVal.type !== "ack") {
              const event = mapDatabaseEvent(rawVal);
              if (event) {
                callback(event);
              }
            }
          }
        } catch {}
      }
    });

    const sub = (() => {
      socket.destroy();
    }) as WatchSubscription;
    sub.unsubscribe = () => {
      socket.destroy();
    };
    sub[Symbol.dispose] = () => {
      socket.destroy();
    };
    sub[Symbol.asyncDispose] = async () => {
      socket.destroy();
    };

    if (options?.signal) {
      if (options.signal.aborted) {
        sub.unsubscribe();
      } else {
        options.signal.addEventListener("abort", () => sub.unsubscribe(), { once: true });
      }
    }
    return sub;
  }

  close(_timeoutMs?: number): Promise<void> {
    if (this.activeSocket) {
      this.activeSocket.destroy();
      this.activeSocket = undefined;
    }
    for (const pending of this.pendingCalls.values()) {
      pending.reject(
        new LiliaError({
          code: "CLOSED",
          message: "daemon client connection closed",
          retryable: false,
          request_id: getUuid(),
        })
      );
    }
    this.pendingCalls.clear();
    return Promise.resolve();
  }

  private async getSocket(): Promise<Socket> {
    if (this.activeSocket && !this.activeSocket.destroyed) {
      return this.activeSocket;
    }
    if (this.connecting) return this.connecting.promise;

    const resolvers = Promise.withResolvers<Socket>();
    this.connecting = resolvers;

    const socket = connect(this.endpoint);
    socket.once("connect", () => {
      this.activeSocket = socket;
      this.socketBuffer = Buffer.alloc(0);
      this.connecting = undefined;
      resolvers.resolve(socket);
    });
    socket.once("error", (err) => {
      this.connecting = undefined;
      resolvers.reject(err);
    });
    socket.on("data", (chunk: Buffer) => {
      this.socketBuffer = Buffer.concat([this.socketBuffer, chunk]);
      while (this.socketBuffer.length >= 4) {
        const length = this.socketBuffer.readUInt32BE(0);
        if (this.socketBuffer.length < 4 + length) {
          break;
        }
        const frameData = this.socketBuffer.subarray(4, 4 + length);
        this.socketBuffer = this.socketBuffer.subarray(4 + length);
        try {
          const wire = decode(frameData, { useBigInt64: true }) as WireResponse;
          const handler = this.pendingCalls.get(wire.id);
          if (handler) {
            this.pendingCalls.delete(wire.id);
            handler.resolve(wire);
          }
        } catch {
          // Drop unparseable frame
        }
      }
    });
    socket.on("close", () => {
      if (this.activeSocket === socket) {
        this.activeSocket = undefined;
      }
      for (const [id, pending] of this.pendingCalls.entries()) {
        this.pendingCalls.delete(id);
        pending.reject(new Error("socket closed unexpectedly"));
      }
    });

    return resolvers.promise;
  }

  private async call(operation: Record<string, unknown>, signal?: AbortSignal): Promise<unknown> {
    signal?.throwIfAborted();
    const socket = await this.getSocket();
    signal?.throwIfAborted();

    const id = getUuid();
    const payload = encode(
      { id, token: this.token, deadline_ms: 5_000, operation },
      { useBigInt64: true }
    );
    const header = Buffer.allocUnsafe(4);
    header.writeUInt32BE(payload.byteLength);

    const resolvers = Promise.withResolvers<WireResponse>();
    this.pendingCalls.set(id, resolvers);

    let onAbort: (() => void) | undefined;
    if (signal) {
      onAbort = () => {
        this.pendingCalls.delete(id);
        resolvers.reject(signal.reason ?? new DOMException("The operation was aborted", "AbortError"));
      };
      signal.addEventListener("abort", onAbort, { once: true });
    }

    socket.write(Buffer.concat([header, Buffer.from(payload)]), (err) => {
      if (err) {
        this.pendingCalls.delete(id);
        if (signal && onAbort) signal.removeEventListener("abort", onAbort);
        resolvers.reject(err);
      }
    });

    let response: WireResponse;
    try {
      response = await resolvers.promise;
    } finally {
      if (signal && onAbort) signal.removeEventListener("abort", onAbort);
    }

    if (response.result.Err) {
      const error = response.result.Err;
      throw new LiliaError({ ...error, request_id: uuidString(error.request_id) });
    }
    return unwrapValue(response.result.Ok);
  }
}

function uuidString(value: string | Uint8Array): string {
  if (typeof value === "string") return value;
  if (!(value instanceof Uint8Array) || value.length !== 16) throw new Error("invalid daemon error request ID");
  const hex = Array.from(value, b => b.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

function unwrapValue(value: unknown): unknown {
  if (!value || typeof value !== "object") return value;
  const record = value as Record<string, unknown>;
  return "value" in record ? record.value : value;
}

function toWire(operation: BatchOperation): Record<string, unknown> {
  const result = { ...operation } as Record<string, unknown>;
  if ("ifVersion" in operation) { result.if_version = checkedVersion(operation.ifVersion); delete result.ifVersion; }
  if ("expiresAtMs" in operation) {
    const expiry = checkedExpiry(operation.expiresAtMs);
    result.expires_at_ms = expiry === undefined ? undefined : BigInt(expiry);
    delete result.expiresAtMs;
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
    result.vector = vectorToBytes(operation.vector, operation.elementType);
  }
  return result;
}

function mapKv(value: unknown): KvEntry | undefined {
  if (!value) return undefined;
  const item = value as Record<string, unknown>;
  return { namespace: String(item.namespace), key: item.key as Uint8Array, value: item.value as Uint8Array, version: readVersion(item.version), expiresAtMs: item.expires_at_ms == null ? undefined : Number(item.expires_at_ms) };
}

function mapJson<T = JsonValue>(value: unknown): JsonEntry<T> | undefined {
  if (!value) return undefined;
  const item = value as Record<string, unknown>;
  return { space: String(item.space), id: String(item.id), value: jsonNumbers(item.value) as T, version: readVersion(item.version) };
}
