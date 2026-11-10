export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
export type DatabaseMode = "embedded" | "daemon";
export type Durability = "durable" | "balanced" | "performance";

export interface OpenOptions {
  path: string;
  mode?: DatabaseMode;
  durability?: Durability;
  plugins?: readonly ("kv" | "json")[];
  endpoint?: string;
  tokenFile?: string;
  busyTimeoutMs?: number;
  writerQueueCapacity?: number;
  readPoolSize?: number;
  checkpointPolicy?: { walBytes?: number; intervalMs?: number };
  encryptionKey?: string;
  signal?: AbortSignal;
}

export interface KvEntry {
  namespace: string;
  key: Uint8Array;
  value: Uint8Array;
  version: bigint;
  expiresAtMs?: number;
}

export interface JsonEntry<T = JsonValue> {
  space: string;
  id: string;
  value: T;
  version: bigint;
}

export interface JsonIndexInfo {
  space: string;
  indexName: string;
  jsonPath: string;
}

export interface CreateIndexOptions {
  name?: string;
  path?: string;
}

export interface KvGetOptions {
  signal?: AbortSignal;
}

export interface KvSetOptions {
  ifVersion?: bigint;
  expiresAtMs?: number;
  signal?: AbortSignal;
}

export interface KvDeleteOptions {
  ifVersion?: bigint;
  signal?: AbortSignal;
}

export interface KvScanOptions {
  after?: Uint8Array | string;
  limit?: number;
  signal?: AbortSignal;
}

export interface JsonGetOptions {
  signal?: AbortSignal;
}

export interface JsonPutOptions {
  ifVersion?: bigint;
  signal?: AbortSignal;
}

export interface JsonDeleteOptions {
  ifVersion?: bigint;
  signal?: AbortSignal;
}

export interface JsonScanOptions {
  after?: string;
  limit?: number;
  signal?: AbortSignal;
}

export interface JsonFindOptions {
  after?: string;
  limit?: number;
  signal?: AbortSignal;
}

export interface ScanStreamOptions<T = string | Uint8Array> {
  after?: T;
  limit?: number;
  batchSize?: number;
  signal?: AbortSignal;
}

export interface WatchOptions {
  signal?: AbortSignal;
}

export interface CloseOptions {
  timeoutMs?: number;
  signal?: AbortSignal;
}

export type GraphDirection = "outgoing" | "incoming" | "both";

export interface FtsHit {
  collection: string;
  docId: string;
  snippet: string;
  rank: number;
}

export type VectorElementType = "float32" | "int8" | "bit";
export type VectorMetric = "cosine" | "l2" | "dot" | "hamming";

export interface VectorHit {
  collection: string;
  id: string;
  distance: number;
}

export interface HybridHit {
  id: string;
  rrfScore: number;
  ftsRank?: number;
  ftsSnippet?: string;
  vectorRank?: number;
  vectorDistance?: number;
}

export type BatchOperation =
  | { model: "kv_set"; namespace: string; key: Uint8Array; value: Uint8Array; ifVersion?: bigint; expiresAtMs?: number }
  | { model: "kv_delete"; namespace: string; key: Uint8Array; ifVersion?: bigint }
  | { model: "json_put"; space: string; id: string; value: JsonValue; ifVersion?: bigint }
  | { model: "json_delete"; space: string; id: string; ifVersion?: bigint }
  | { model: "graph_set_node"; graph: string; id: string; kind: string; metadata?: JsonValue }
  | { model: "graph_delete_node"; graph: string; id: string }
  | { model: "graph_add_edge"; graph: string; source: string; target: string; kind: string }
  | { model: "graph_remove_edge"; graph: string; source: string; target: string; kind: string }
  | { model: "fts_index"; collection: string; docId: string; content: string }
  | { model: "fts_delete"; collection: string; docId: string }
  | { model: "vector_set"; collection: string; id: string; dim: number; elementType?: VectorElementType; vector: Uint8Array | number[] | Float32Array }
  | { model: "vector_delete"; collection: string; id: string };

export type DatabaseEvent =
  | { type: "kv_mutated"; namespace: string; key: Uint8Array; version?: bigint; deleted: boolean }
  | { type: "json_mutated"; space: string; id: string; version?: bigint; deleted: boolean }
  | { type: "graph_node_mutated"; graph: string; id: string; deleted: boolean }
  | { type: "graph_edge_mutated"; graph: string; source: string; target: string; kind: string; deleted: boolean }
  | { type: "fts_mutated"; collection: string; docId: string; deleted: boolean }
  | { type: "vector_mutated"; collection: string; id: string; deleted: boolean };

export interface WatchSubscription extends Disposable, AsyncDisposable {
  unsubscribe(): void | Promise<void>;
  (): void | Promise<void>;
  [Symbol.dispose](): void;
  [Symbol.asyncDispose](): Promise<void>;
}

export interface MutationResult { version?: bigint; deleted: boolean }

export interface SqlQueryResult<T = Record<string, unknown>> {
  columns: string[];
  rows: JsonValue[][];
  rowsAffected: number;
  records: T[];
}

export interface SqlQueryOptions {
  signal?: AbortSignal;
}

export interface SqlFunction {
  <T = Record<string, unknown>>(strings: TemplateStringsArray, ...values: JsonValue[]): Promise<T[]>;
  query<T = Record<string, unknown>>(sql: string, params?: JsonValue[], options?: SqlQueryOptions): Promise<T[]>;
  execute(sql: string, params?: JsonValue[], options?: SqlQueryOptions): Promise<{ rowsAffected: number }>;
  raw<T = Record<string, unknown>>(sql: string, params?: JsonValue[], options?: SqlQueryOptions): Promise<SqlQueryResult<T>>;
}

export interface SyncSqlFunction {
  <T = Record<string, unknown>>(strings: TemplateStringsArray, ...values: JsonValue[]): T[];
  query<T = Record<string, unknown>>(sql: string, params?: JsonValue[]): T[];
  execute(sql: string, params?: JsonValue[]): { rowsAffected: number };
  raw<T = Record<string, unknown>>(sql: string, params?: JsonValue[]): SqlQueryResult<T>;
}

export interface LiliaErrorShape {
  code: string;
  message: string;
  retryable: boolean;
  details?: JsonValue;
  request_id: string;
}

export class LiliaError extends Error {
  readonly code: string;
  readonly retryable: boolean;
  readonly details?: JsonValue;
  readonly requestId?: string;

  constructor(shape: Partial<LiliaErrorShape> & { message: string; cause?: unknown }) {
    super(shape.message, { cause: shape.cause });
    this.name = "LiliaError";
    this.code = shape.code ?? "UNKNOWN";
    this.retryable = shape.retryable ?? false;
    this.details = shape.details;
    this.requestId = shape.request_id;
    if (Error.captureStackTrace) {
      Error.captureStackTrace(this, LiliaError);
    }
  }

  static isLiliaError(value: unknown): value is LiliaError {
    return value instanceof LiliaError;
  }
}

export function isKvEntry(value: unknown): value is KvEntry {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  return (
    typeof candidate.namespace === "string" &&
    candidate.key instanceof Uint8Array &&
    candidate.value instanceof Uint8Array &&
    typeof candidate.version === "bigint"
  );
}

export function isJsonEntry<T = JsonValue>(value: unknown): value is JsonEntry<T> {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  return (
    typeof candidate.space === "string" &&
    typeof candidate.id === "string" &&
    typeof candidate.version === "bigint" &&
    "value" in candidate
  );
}
