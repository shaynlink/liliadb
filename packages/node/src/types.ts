export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
export type DatabaseMode = "embedded" | "daemon";
export type Durability = "durable" | "balanced" | "performance";
export type ResourceProfile = "server" | "mobile" | "constrained";

export interface OpenOptions {
  /** Directory of the database (embedded); created if missing. */
  path: string;
  mode?: DatabaseMode;
  durability?: Durability;
  profile?: ResourceProfile;
  /** Keep a tamper-evident ledger of every record write; a database that keeps one must always be opened with it. */
  ledger?: boolean;
  /** How long old versions stay readable, in milliseconds; a week by default, 0 keeps none. */
  historyRetentionMs?: number;
  /** Commits `undo` can undo; none by default. Needs a history retention. */
  undoDepth?: number;
  /** Socket (or named pipe) of the daemon; `<path>.sock` by default. */
  endpoint?: string;
  /** File holding the daemon's token; `<path>.token` by default. */
  tokenFile?: string;
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
  filter?: string;
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

export interface JsonScanPrefixOptions {
  afterId?: string;
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

export interface CloseOptions {
  timeoutMs?: number;
  signal?: AbortSignal;
}

export type GraphDirection = "outgoing" | "incoming" | "both";
export type CentralityType = "betweenness" | "closeness" | "degree";
export type ComponentType = "strongly_connected" | "weakly_connected";

export interface GraphNode {
  graph: string;
  id: string;
  kind: string;
  metadata: JsonValue;
  docSpace?: string;
  docId?: string;
}

export interface GraphEdge {
  graph: string;
  source: string;
  target: string;
  kind: string;
  weight: number;
}

export interface PathEdge {
  source: string;
  target: string;
  kind: string;
  weight: number;
}

export interface WeightedPath {
  nodes: string[];
  totalWeight: number;
  edges: PathEdge[];
}

export interface NodeScore {
  node: string;
  score: number;
}

export interface ImpactAnalysis {
  direct: string[];
  dependants: string[];
  totalAffected: number;
}

export interface PageRankConfig {
  dampingFactor?: number;
  maxIterations?: number;
  tolerance?: number;
  seeds?: string[];
  direction?: GraphDirection;
  edgeKind?: string;
}

export interface CypherResult<T = Record<string, JsonValue>> {
  columns: string[];
  rows: JsonValue[][];
  records: T[];
}

export interface GraphSetNodeOptions {
  docSpace?: string;
  docId?: string;
  signal?: AbortSignal;
}

export interface GraphAddEdgeOptions {
  weight?: number;
  signal?: AbortSignal;
}

export interface GraphTraversalOptions {
  direction?: GraphDirection;
  kindFilter?: string;
  signal?: AbortSignal;
}

export interface GraphListNodesOptions {
  kind?: string;
  limit?: number;
  signal?: AbortSignal;
}

export interface GraphListEdgesOptions {
  source?: string;
  target?: string;
  kind?: string;
  limit?: number;
  signal?: AbortSignal;
}

export interface GraphCentralityOptions extends GraphTraversalOptions {
  normalized?: boolean;
}

export interface GraphFindCyclesOptions {
  maxCycles?: number;
  kindFilter?: string;
  signal?: AbortSignal;
}

export interface FtsSuggestion {
  term: string;
  distance: number;
  docCount: number;
  exactPrefix: boolean;
}

export interface FtsFuzzyHit extends FtsHit {
  editDistance: number;
  matchedTerms: string[];
}

export interface FtsPhoneticHit extends FtsHit {
  matchedTerms: string[];
  phoneticCodes: string[];
}

export interface FtsSearchOptions {
  limit?: number;
  /** Most edits between a query term and a matching term; 1 by default. */
  maxDistance?: number;
  signal?: AbortSignal;
}

export interface FtsHit {
  collection: string;
  docId: string;
  snippet: string;
  rank: number;
}

export type VectorElementType = "float32" | "int8" | "bit";
export type VectorMetric = "cosine" | "l2" | "dot" | "hamming";

export interface HnswConfig {
  m?: number;
  efConstruction?: number;
  efSearch?: number;
  metric?: VectorMetric;
  elementType?: VectorElementType;
  dim: number;
}

export interface VectorIndexInfo {
  collection: string;
  nodeCount: number;
  maxLevel: number;
  m: number;
  efConstruction: number;
  efSearch: number;
  metric: VectorMetric;
  elementType: VectorElementType;
  dim: number;
}

export interface VectorSearchOptions {
  dim?: number;
  elementType?: VectorElementType;
  metric?: VectorMetric;
  limit?: number;
  signal?: AbortSignal;
}

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

export interface TimeSeriesPoint {
  timestamp: number;
  value: number;
}

export interface TimeSeriesStats {
  min: number;
  max: number;
  avg: number;
  sum: number;
  count: number;
}

export type DownsampleAggregation = "avg" | "min" | "max" | "sum" | "first" | "last" | "count";

export interface TimeSeriesRetentionRule {
  resolutionMs: number;
  retentionMs?: number | null;
  aggregation: DownsampleAggregation;
}

export interface TimeSeriesLifecyclePolicy {
  seriesPattern: string;
  rules: TimeSeriesRetentionRule[];
}

export interface TimeSeriesRetentionStats {
  seriesProcessed: number;
  downsampledPointsCreated: number;
  chunksPruned: number;
  bytesReclaimedApprox: number;
}

export interface GpsPoint {
  timestamp: number;
  latitude: number;
  longitude: number;
  altitude?: number;
  speed?: number;
  accuracy?: number;
}

export interface GeofenceMatch {
  id: string;
  name: string;
  category: string;
  metadata?: string;
}

export interface GeoAddFenceOptions {
  metadata?: string;
  signal?: AbortSignal;
}

export interface GeoTrackQueryOptions {
  startTime?: number;
  endTime?: number;
  signal?: AbortSignal;
}

export interface CrdtCounterState {
  p: Record<string, number>;
  n: Record<string, number>;
}

export interface CrdtOrSetState {
  entries: Record<string, string[]>;
  tombstones: Record<string, number>;
}

export interface CrdtLogicalClock {
  timestampMs: number;
  counter: number;
}

export interface CrdtLwwClock {
  clock: CrdtLogicalClock;
  actorId: string;
}

export interface CrdtLwwField {
  value: JsonValue;
  clock: CrdtLwwClock;
  isDeleted: boolean;
}

export interface LwwJsonDocumentState {
  fields: Record<string, CrdtLwwField>;
}

export type BatchOperation =
  | { model: "kv_set"; namespace: string; key: Uint8Array; value: Uint8Array; ifVersion?: bigint; expiresAtMs?: number }
  | { model: "kv_delete"; namespace: string; key: Uint8Array; ifVersion?: bigint }
  | { model: "json_put"; space: string; id: string; value: JsonValue; ifVersion?: bigint }
  | { model: "json_put_raw"; space: string; id: string; data: Uint8Array; ifVersion?: bigint }
  | { model: "json_delete"; space: string; id: string; ifVersion?: bigint }
  | { model: "graph_set_node"; graph: string; id: string; kind: string; metadata?: JsonValue; docSpace?: string; docId?: string }
  | { model: "graph_delete_node"; graph: string; id: string }
  | { model: "graph_add_edge"; graph: string; source: string; target: string; kind: string; weight?: number }
  | { model: "graph_remove_edge"; graph: string; source: string; target: string; kind: string }
  | { model: "fts_index"; collection: string; docId: string; content: string }
  | { model: "fts_delete"; collection: string; docId: string }
  | { model: "vector_set"; collection: string; id: string; dim: number; elementType?: VectorElementType; vector: Uint8Array | number[] | Float32Array }
  | { model: "vector_delete"; collection: string; id: string }
  | { model: "crdt_pn_counter_increment"; space: string; id: string; actorId: string; amount: number | bigint }
  | { model: "crdt_pn_counter_decrement"; space: string; id: string; actorId: string; amount: number | bigint }
  | { model: "crdt_or_set_add"; space: string; id: string; actorId: string; element: string }
  | { model: "crdt_or_set_remove"; space: string; id: string; element: string; timestampMs?: number }
  | { model: "crdt_json_set_field"; space: string; id: string; actorId: string; path: string; value: JsonValue; timestampMs?: number }
  | { model: "crdt_json_delete_field"; space: string; id: string; actorId: string; path: string; timestampMs?: number }
  | { model: "crdt_json_merge"; space: string; id: string; state: Uint8Array | LwwJsonDocumentState };

/** One version of a document; `document` is absent where it was deleted. */
export interface JsonRevision<T = JsonValue> {
  version: bigint;
  document?: JsonEntry<T>;
}

/** A commit `undo` reverted or `redo` restored. */
export interface UndoStep {
  /** The version of the commit's first write. */
  version: bigint;
  /** The keys it wrote: records and the index entries derived from them. */
  keys: number;
}

/** The sorted runs of one level of the storage. */
export interface LevelStats {
  runs: number;
  bytes: number;
}

/** The shape of the storage. */
export interface StorageStats {
  /** Level 0 first. */
  levels: LevelStats[];
  memtableBytes: number;
  immutableMemtables: number;
  /** The newest version written. */
  version: bigint;
  /** The oldest version still readable. */
  oldestVersion: bigint;
}

export interface MutationResult {
  /** The engine sequence of the write; a later write of the record has a greater one. */
  version?: bigint;
  deleted: boolean;
}

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
  raw<T = Record<string, unknown>>(sql: string, params?: JsonValue[], options?: SqlQueryOptions): Promise<SqlQueryResult<T>>;
}

export interface SyncSqlFunction {
  <T = Record<string, unknown>>(strings: TemplateStringsArray, ...values: JsonValue[]): T[];
  query<T = Record<string, unknown>>(sql: string, params?: JsonValue[]): T[];
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
