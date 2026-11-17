import { deflateRawSync } from "node:zlib";
import type {
  BatchOperation,
  CausalRelation,
  CheckpointStats,
  ConflictRecord,
  ConflictResolutionStrategy,
  CrdtCounterState,
  CrdtOrSetState,
  CypherResult,
  DatabaseEvent,
  DictStats,
  DownsampleAggregation,
  FtsHit,
  GeofenceMatch,
  GpsPoint,
  GraphEdge,
  GraphNode,
  HlcMutationOutcome,
  HlcRecord,
  HnswConfig,
  HybridHit,
  ImpactAnalysis,
  JsonEntry,
  JsonIndexInfo,
  JsonRevisionInfo,
  JsonValue,
  LiveQueryDiff,
  LwwJsonDocumentState,
  MaterializedViewInfo,
  MutationResult,
  NodeScore,
  PageRankConfig,
  RedoResult,
  SqlQueryResult,
  TableStorageStat,
  TimeSeriesLifecyclePolicy,
  TimeSeriesPoint,
  TimeSeriesRetentionStats,
  TimeSeriesStats,
  UndoEntry,
  UndoResult,
  VectorElementType,
  VectorHit,
  VectorIndexInfo,
  WeightedPath,
} from "./types.js";
import { checkedExpiry, checkedVersion, jsonNumbers, readVersion } from "./version.js";

export function formatHlc(raw: unknown): string | undefined {
  if (raw == null) return undefined;
  if (typeof raw === "string") return raw;
  if (typeof raw === "object") {
    const obj = raw as Record<string, unknown>;
    if (obj.millis != null && obj.counter != null) {
      const nodeId = typeof obj.node_id === "string" ? obj.node_id : typeof obj.nodeId === "string" ? obj.nodeId : "";
      const base = `${Number(obj.millis)}.${Number(obj.counter)}`;
      return nodeId.length > 0 ? `${base}@${nodeId}` : base;
    }
  }
  return String(raw);
}

export function formatHlcRequired(raw: unknown): string {
  return formatHlc(raw) ?? "";
}

function buildRecordRows<T>(columns: string[], rows: JsonValue[][]): T[] {
  return rows.map((row) => {
    const obj: Record<string, unknown> = {};
    for (let i = 0; i < columns.length; i++) {
      const col = columns[i];
      if (col !== undefined) obj[col] = row[i];
    }
    return obj as T;
  });
}

export function mapSqlQueryResult<T = Record<string, unknown>>(raw: Record<string, unknown>): SqlQueryResult<T> {
  const columns = (raw.columns as string[]) ?? [];
  const rows = jsonNumbers(raw.rows ?? []) as JsonValue[][];
  const rowsAffected = Number(raw.rows_affected ?? raw.rowsAffected ?? 0);
  let cachedRecords: T[] | undefined;
  return {
    columns,
    rows,
    rowsAffected,
    get records(): T[] {
      if (cachedRecords === undefined) {
        cachedRecords = buildRecordRows<T>(columns, rows);
      }
      return cachedRecords;
    },
  };
}

export function mapCypherResult<T = Record<string, JsonValue>>(raw: Record<string, unknown>): CypherResult<T> {
  const columns = (raw.columns as string[]) ?? [];
  const rows = jsonNumbers(raw.rows ?? []) as JsonValue[][];
  let cachedRecords: T[] | undefined;
  return {
    columns,
    rows,
    get records(): T[] {
      if (cachedRecords === undefined) {
        cachedRecords = buildRecordRows<T>(columns, rows);
      }
      return cachedRecords;
    },
  };
}

export function mapMutationResults(items: unknown[]): MutationResult[] {
  return items.map((entry) => {
    const item = entry as Record<string, unknown>;
    return {
      deleted: Boolean(item.deleted),
      version: item.version != null ? readVersion(item.version) : undefined,
      hlc: formatHlc(item.hlc),
      conflictDetected:
        item.conflict_detected != null
          ? Boolean(item.conflict_detected)
          : item.conflictDetected != null
          ? Boolean(item.conflictDetected)
          : undefined,
    };
  });
}

export function mapJsonEntry<T = JsonValue>(raw: Record<string, unknown>): JsonEntry<T> {
  return {
    space: String(raw.space),
    id: String(raw.id),
    value: jsonNumbers(raw.value) as T,
    version: readVersion(raw.version),
  };
}

export function mapJsonIndexList(raw: unknown[]): JsonIndexInfo[] {
  return raw.map((entry) => {
    const item = entry as Record<string, unknown>;
    return {
      space: String(item.space),
      indexName: String(item.indexName ?? item.index_name ?? ""),
      jsonPath: String(item.jsonPath ?? item.json_path ?? ""),
    };
  });
}

export function mapJsonRevision(raw: Record<string, unknown>): JsonRevisionInfo {
  return {
    space: String(raw.space),
    id: String(raw.id),
    version: readVersion(raw.version),
    timestampMs: Number(raw.timestamp_ms ?? raw.timestampMs ?? 0),
    isDeletion: Boolean(raw.is_deletion ?? raw.isDeletion),
    isKeyframe: Boolean(raw.is_keyframe ?? raw.isKeyframe),
    patch: raw.patch != null ? jsonNumbers(raw.patch) : undefined,
  };
}

export function mapDictStats(raw: Record<string, unknown>): DictStats {
  return {
    space: String(raw.space),
    dictId: Number(raw.dict_id ?? raw.dictId ?? 0),
    sampleCount: Number(raw.sample_count ?? raw.sampleCount ?? 0),
    dictSize: Number(raw.dict_size ?? raw.dictSize ?? 0),
    createdAtMs: Number(raw.created_at_ms ?? raw.createdAtMs ?? 0),
  };
}

export function mapHlcRecord(raw: Record<string, unknown>): HlcRecord {
  return {
    model: String(raw.model),
    space: String(raw.space),
    id: String(raw.id),
    hlc: formatHlcRequired(raw.hlc),
    parentHlc: formatHlc(raw.parent_hlc ?? raw.parentHlc),
    nodeId: String(raw.node_id ?? raw.nodeId ?? ""),
    updatedAtMs: Number(raw.updated_at_ms ?? raw.updatedAtMs ?? 0),
  };
}

export function mapHlcMutationOutcome(raw: Record<string, unknown>): HlcMutationOutcome {
  return {
    hlc: formatHlcRequired(raw.hlc),
    version: readVersion(raw.version),
    causalRelation: String(raw.causal_relation ?? raw.causalRelation) as CausalRelation,
    conflictDetected: Boolean(raw.conflict_detected ?? raw.conflictDetected),
    conflictId:
      raw.conflict_id != null
        ? Number(raw.conflict_id)
        : raw.conflictId != null
        ? Number(raw.conflictId)
        : undefined,
    resolvedBy: (raw.resolved_by ?? raw.resolvedBy) as ConflictResolutionStrategy | undefined,
  };
}

export function mapConflictRecord(raw: Record<string, unknown>): ConflictRecord {
  return {
    id: raw.id != null ? Number(raw.id) : undefined,
    model: String(raw.model),
    space: String(raw.space),
    docId: String(raw.doc_id ?? raw.docId ?? ""),
    baseHlc: formatHlc(raw.base_hlc ?? raw.baseHlc),
    oursHlc: formatHlcRequired(raw.ours_hlc ?? raw.oursHlc),
    theirsHlc: formatHlcRequired(raw.theirs_hlc ?? raw.theirsHlc),
    oursValue: jsonNumbers(raw.ours_value ?? raw.oursValue),
    theirsValue: jsonNumbers(raw.theirs_value ?? raw.theirsValue),
    resolvedValue:
      (raw.resolved_value ?? raw.resolvedValue) != null
        ? jsonNumbers(raw.resolved_value ?? raw.resolvedValue)
        : undefined,
    resolutionStrategy: String(raw.resolution_strategy ?? raw.resolutionStrategy) as ConflictResolutionStrategy,
    status: String(raw.status),
    detectedAtMs: Number(raw.detected_at_ms ?? raw.detectedAtMs ?? 0),
  };
}

export function mapGraphNode(raw: Record<string, unknown>): GraphNode {
  return {
    graph: String(raw.graph),
    id: String(raw.id),
    kind: String(raw.kind),
    metadata: jsonNumbers(raw.metadata ?? {}),
    docSpace: (raw.doc_space ?? raw.docSpace) != null ? String(raw.doc_space ?? raw.docSpace) : undefined,
    docId: (raw.doc_id ?? raw.docId) != null ? String(raw.doc_id ?? raw.docId) : undefined,
  };
}

export function mapGraphEdge(raw: Record<string, unknown>): GraphEdge {
  return {
    graph: String(raw.graph),
    source: String(raw.source),
    target: String(raw.target),
    kind: String(raw.kind),
    weight: Number(raw.weight ?? 1.0),
  };
}

export function mapWeightedPath(raw: Record<string, unknown>): WeightedPath {
  const edgesRaw = (raw.edges as Array<Record<string, unknown>>) ?? [];
  return {
    nodes: ((raw.nodes as unknown[]) ?? []).map(String),
    totalWeight: Number(raw.total_weight ?? raw.totalWeight ?? 0),
    edges: edgesRaw.map((e) => ({
      source: String(e.source),
      target: String(e.target),
      kind: String(e.kind),
      weight: Number(e.weight ?? 1.0),
    })),
  };
}

export function mapNodeScore(raw: Record<string, unknown>): NodeScore {
  return {
    node: String(raw.node),
    score: Number(raw.score ?? 0),
  };
}

export function mapImpactAnalysis(raw: Record<string, unknown>): ImpactAnalysis {
  return {
    direct: ((raw.direct as unknown[]) ?? []).map(String),
    dependants: ((raw.dependants as unknown[]) ?? []).map(String),
    totalAffected: Number(raw.total_affected ?? raw.totalAffected ?? 0),
  };
}

export function toWirePageRankConfig(config?: PageRankConfig): Record<string, unknown> {
  return {
    damping_factor: config?.dampingFactor ?? 0.85,
    max_iterations: config?.maxIterations ?? 50,
    tolerance: config?.tolerance ?? 1e-6,
    seeds: config?.seeds ?? undefined,
    direction: config?.direction ?? "outgoing",
    edge_kind: config?.edgeKind ?? undefined,
  };
}

export function mapFtsHits(hits: Array<Record<string, unknown>>): FtsHit[] {
  return hits.map((h) => ({
    collection: String(h.collection),
    docId: String(h.docId ?? h.doc_id ?? ""),
    snippet: String(h.snippet),
    rank: Number(h.rank),
  }));
}

export function mapVectorHits(hits: Array<Record<string, unknown>>): VectorHit[] {
  return hits.map((h) => ({
    collection: String(h.collection),
    id: String(h.id),
    distance: Number(h.distance),
  }));
}

export function mapHybridHits(hits: Array<Record<string, unknown>>): HybridHit[] {
  return hits.map((h) => ({
    id: String(h.id),
    rrfScore: Number(h.rrfScore ?? h.rrf_score ?? 0),
    ftsRank: h.ftsRank != null ? Number(h.ftsRank) : h.fts_rank != null ? Number(h.fts_rank) : undefined,
    ftsSnippet: h.ftsSnippet != null ? String(h.ftsSnippet) : h.fts_snippet != null ? String(h.fts_snippet) : undefined,
    vectorRank: h.vectorRank != null ? Number(h.vectorRank) : h.vector_rank != null ? Number(h.vector_rank) : undefined,
    vectorDistance:
      h.vectorDistance != null ? Number(h.vectorDistance) : h.vector_distance != null ? Number(h.vector_distance) : undefined,
  }));
}

export function toWireHnswConfig(config: HnswConfig): Record<string, unknown> {
  return {
    m: config.m ?? 16,
    ef_construction: config.efConstruction ?? 64,
    ef_search: config.efSearch ?? 32,
    metric: config.metric ?? "cosine",
    element_type: config.elementType ?? "float32",
    dim: config.dim,
  };
}

export function mapVectorIndexInfo(raw: Record<string, unknown>): VectorIndexInfo {
  return {
    collection: String(raw.collection),
    nodeCount: Number(raw.node_count ?? raw.nodeCount ?? 0),
    maxLevel: Number(raw.max_level ?? raw.maxLevel ?? 0),
    m: Number(raw.m ?? 16),
    efConstruction: Number(raw.ef_construction ?? raw.efConstruction ?? 64),
    efSearch: Number(raw.ef_search ?? raw.efSearch ?? 32),
    metric: String(raw.metric) as VectorIndexInfo["metric"],
    elementType: String(raw.element_type ?? raw.elementType) as VectorElementType,
    dim: Number(raw.dim ?? 0),
  };
}

function toWireAggregation(agg: DownsampleAggregation | string): string {
  switch (agg.toLowerCase()) {
    case "min":
      return "Min";
    case "max":
      return "Max";
    case "sum":
      return "Sum";
    case "first":
      return "First";
    case "last":
      return "Last";
    case "count":
      return "Count";
    case "avg":
    default:
      return "Avg";
  }
}

export function toWireTsPolicy(policy: TimeSeriesLifecyclePolicy): Record<string, unknown> {
  return {
    series_pattern: policy.seriesPattern,
    rules: policy.rules.map((r) => ({
      resolution_ms: r.resolutionMs,
      retention_ms: r.retentionMs ?? null,
      aggregation: toWireAggregation(r.aggregation),
    })),
  };
}

export function mapTsPolicy(raw: Record<string, unknown>): TimeSeriesLifecyclePolicy {
  const rules = (raw.rules as Array<Record<string, unknown>>) ?? [];
  return {
    seriesPattern: String(raw.series_pattern ?? raw.seriesPattern ?? ""),
    rules: rules.map((r) => ({
      resolutionMs: Number(r.resolution_ms ?? r.resolutionMs ?? 0),
      retentionMs:
        r.retention_ms === null || r.retentionMs === null
          ? null
          : r.retention_ms != null
          ? Number(r.retention_ms)
          : r.retentionMs != null
          ? Number(r.retentionMs)
          : undefined,
      aggregation: String(r.aggregation ?? "avg").toLowerCase() as DownsampleAggregation,
    })),
  };
}

export function mapTsPoints(points: Array<Record<string, unknown>>): TimeSeriesPoint[] {
  return points.map((p) => ({
    timestamp: Number(p.timestamp),
    value: Number(p.value),
  }));
}

export function mapTsStats(raw: Record<string, unknown>): TimeSeriesStats {
  return {
    min: Number(raw.min),
    max: Number(raw.max),
    avg: Number(raw.avg),
    sum: Number(raw.sum),
    count: Number(raw.count),
  };
}

export function mapTsRetentionStats(raw: Record<string, unknown>): TimeSeriesRetentionStats {
  return {
    seriesProcessed: Number(raw.series_processed ?? raw.seriesProcessed ?? 0),
    downsampledPointsCreated: Number(raw.downsampled_points_created ?? raw.downsampledPointsCreated ?? 0),
    chunksPruned: Number(raw.chunks_pruned ?? raw.chunksPruned ?? 0),
    bytesReclaimedApprox: Number(raw.bytes_reclaimed_approx ?? raw.bytesReclaimedApprox ?? 0),
  };
}

export function mapGpsPoints(points: Array<Record<string, unknown>>): GpsPoint[] {
  return points.map((p) => ({
    timestamp: Number(p.timestamp),
    latitude: Number(p.latitude),
    longitude: Number(p.longitude),
    altitude: p.altitude != null ? Number(p.altitude) : undefined,
    speed: p.speed != null ? Number(p.speed) : undefined,
    accuracy: p.accuracy != null ? Number(p.accuracy) : undefined,
  }));
}

export function mapGeofenceMatches(matches: Array<Record<string, unknown>>): GeofenceMatch[] {
  return matches.map((m) => ({
    id: String(m.id),
    name: String(m.name),
    category: String(m.category),
    metadata: m.metadata != null ? String(m.metadata) : undefined,
  }));
}

export function mapCrdtCounterState(raw: Record<string, unknown>): CrdtCounterState {
  const mapRecord = (r: unknown): Record<string, number> => {
    if (!r || typeof r !== "object") return {};
    return Object.fromEntries(Object.entries(r as Record<string, unknown>).map(([k, v]) => [k, Number(v)]));
  };
  return {
    p: mapRecord(raw.p),
    n: mapRecord(raw.n),
  };
}

export function mapCrdtOrSetState(raw: Record<string, unknown>): CrdtOrSetState {
  const entriesRaw = (raw.entries ?? {}) as Record<string, unknown>;
  const entries: Record<string, string[]> = {};
  for (const [k, v] of Object.entries(entriesRaw)) {
    entries[k] = Array.isArray(v) ? v.map(String) : [];
  }
  const tombRaw = (raw.tombstones ?? {}) as Record<string, unknown>;
  const tombstones: Record<string, number> = {};
  for (const [k, v] of Object.entries(tombRaw)) {
    tombstones[k] = Number(v);
  }
  return { entries, tombstones };
}

export function toWireLwwJsonState(state: LwwJsonDocumentState | Record<string, unknown>): Record<string, unknown> {
  const fieldsIn = (state as LwwJsonDocumentState).fields ?? {};
  const fieldsOut: Record<string, unknown> = {};
  for (const [path, rawField] of Object.entries(fieldsIn)) {
    const field = rawField as unknown as Record<string, unknown>;
    const rawLwwClock = (field.clock ?? {}) as Record<string, unknown>;
    const rawLogical = (rawLwwClock.clock ?? {}) as Record<string, unknown>;
    fieldsOut[path] = {
      value: field.value ?? null,
      clock: {
        clock: {
          timestamp_ms: Number(rawLogical.timestampMs ?? rawLogical.timestamp_ms ?? 0),
          counter: Number(rawLogical.counter ?? 0),
        },
        actor_id: String(rawLwwClock.actorId ?? rawLwwClock.actor_id ?? ""),
      },
      is_deleted: Boolean(field.isDeleted ?? field.is_deleted ?? false),
    };
  }
  return { fields: fieldsOut };
}

export function mapLwwJsonState(raw: Record<string, unknown>): LwwJsonDocumentState {
  const fieldsIn = (raw.fields ?? {}) as Record<string, Record<string, unknown>>;
  const fieldsOut: LwwJsonDocumentState["fields"] = {};
  for (const [path, field] of Object.entries(fieldsIn)) {
    const lwwClock = (field.clock ?? {}) as Record<string, unknown>;
    const logical = (lwwClock.clock ?? {}) as Record<string, unknown>;
    fieldsOut[path] = {
      value: jsonNumbers(field.value ?? null),
      clock: {
        clock: {
          timestampMs: Number(logical.timestamp_ms ?? logical.timestampMs ?? 0),
          counter: Number(logical.counter ?? 0),
        },
        actorId: String(lwwClock.actor_id ?? lwwClock.actorId ?? ""),
      },
      isDeleted: Boolean(field.is_deleted ?? field.isDeleted ?? false),
    };
  }
  return { fields: fieldsOut };
}

export function mapUndoResult(raw: Record<string, unknown>): UndoResult {
  return {
    id: Number(raw.id),
    summary: String(raw.summary),
    timestampMs: Number(raw.timestamp_ms ?? raw.timestampMs ?? 0),
  };
}

export function mapRedoResult(raw: Record<string, unknown>): RedoResult {
  return mapUndoResult(raw);
}

export function mapUndoEntry(raw: Record<string, unknown>): UndoEntry {
  return {
    id: Number(raw.id),
    timestampMs: Number(raw.timestamp_ms ?? raw.timestampMs ?? 0),
    summary: String(raw.summary),
    undone: Boolean(raw.undone),
  };
}

export function mapMaterializedViews(rawList: Array<Record<string, unknown>>): MaterializedViewInfo[] {
  return rawList.map((item) => ({
    name: String(item.name),
    sourceTable: String(item.source_table ?? item.sourceTable ?? ""),
    query: String(item.query),
    storageTable: String(item.storage_table ?? item.storageTable ?? ""),
    groupBy:
      (item.group_by ?? item.groupBy) != null ? String(item.group_by ?? item.groupBy) : undefined,
    createdAtMs: Number(item.created_at_ms ?? item.createdAtMs ?? 0),
  }));
}

export function mapCheckpointStats(raw: Record<string, unknown>): CheckpointStats {
  return {
    attempts: Number(raw.attempts ?? 0),
    incomplete: Number(raw.incomplete ?? 0),
    logFrames: Number(raw.log_frames ?? raw.logFrames ?? 0),
    checkpointedFrames: Number(raw.checkpointed_frames ?? raw.checkpointedFrames ?? 0),
    lastError: (raw.last_error ?? raw.lastError) != null ? String(raw.last_error ?? raw.lastError) : undefined,
  };
}

export function mapStorageStats(rawList: Array<Record<string, unknown>>): TableStorageStat[] {
  return rawList.map((item) => ({
    name: String(item.name),
    pageCount: Number(item.page_count ?? item.pageCount ?? 0),
    payloadBytes: Number(item.payload_bytes ?? item.payloadBytes ?? 0),
    unusedBytes: Number(item.unused_bytes ?? item.unusedBytes ?? 0),
  }));
}

export function mapLiveQueryDiff<T = Record<string, unknown>>(raw: Record<string, unknown>): LiveQueryDiff<T> {
  return {
    subscriptionId: Number(raw.subscription_id ?? raw.subscriptionId ?? 0),
    added: (jsonNumbers(raw.added ?? []) as T[]) ?? [],
    updated: (jsonNumbers(raw.updated ?? []) as T[]) ?? [],
    removed: (jsonNumbers(raw.removed ?? []) as T[]) ?? [],
    isInitial: Boolean(raw.is_initial ?? raw.isInitial),
  };
}

export function mapDatabaseEvent(raw: unknown): DatabaseEvent | undefined {
  if (!raw || typeof raw !== "object") return undefined;
  const item = raw as Record<string, unknown>;
  const type = String(item.type);
  switch (type) {
    case "kv_mutated": {
      const rawKey = item.key;
      const key =
        rawKey instanceof Uint8Array
          ? rawKey
          : Array.isArray(rawKey)
          ? new Uint8Array(rawKey)
          : typeof rawKey === "string"
          ? Buffer.from(rawKey, "hex")
          : new Uint8Array();
      return {
        type: "kv_mutated",
        namespace: String(item.namespace),
        key,
        version: item.version == null ? undefined : readVersion(item.version),
        deleted: Boolean(item.deleted),
      };
    }
    case "json_mutated":
      return {
        type: "json_mutated",
        space: String(item.space),
        id: String(item.id),
        version: item.version == null ? undefined : readVersion(item.version),
        deleted: Boolean(item.deleted),
      };
    case "graph_node_mutated":
      return {
        type: "graph_node_mutated",
        graph: String(item.graph),
        id: String(item.id),
        deleted: Boolean(item.deleted),
      };
    case "graph_edge_mutated":
      return {
        type: "graph_edge_mutated",
        graph: String(item.graph),
        source: String(item.source),
        target: String(item.target),
        kind: String(item.kind),
        deleted: Boolean(item.deleted),
      };
    case "fts_mutated":
      return {
        type: "fts_mutated",
        collection: String(item.collection),
        docId: String(item.doc_id ?? item.docId ?? ""),
        deleted: Boolean(item.deleted),
      };
    case "vector_mutated":
      return {
        type: "vector_mutated",
        collection: String(item.collection),
        id: String(item.id),
        deleted: Boolean(item.deleted),
      };
    case "crdt_counter_mutated":
      return {
        type: "crdt_counter_mutated",
        space: String(item.space),
        id: String(item.id),
        value: Number(item.value ?? 0),
      };
    case "crdt_set_mutated":
      return {
        type: "crdt_set_mutated",
        space: String(item.space),
        id: String(item.id),
        element: String(item.element),
        deleted: Boolean(item.deleted),
      };
    case "crdt_json_mutated":
      return {
        type: "crdt_json_mutated",
        space: String(item.space),
        id: String(item.id),
        version: item.version == null ? undefined : readVersion(item.version),
      };
    case "conflict_detected":
      return {
        type: "conflict_detected",
        model: String(item.model),
        space: String(item.space),
        id: String(item.id),
        baseHlc: formatHlc(item.base_hlc ?? item.baseHlc),
        oursHlc: formatHlcRequired(item.ours_hlc ?? item.oursHlc),
        theirsHlc: formatHlcRequired(item.theirs_hlc ?? item.theirsHlc),
        resolutionStrategy: String(
          item.resolution_strategy ?? item.resolutionStrategy
        ) as ConflictResolutionStrategy,
      };
    default:
      return undefined;
  }
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
  }
  if (elementType === "bit") {
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
  }
  const f32 = v instanceof Float32Array ? v : new Float32Array(v);
  return new Uint8Array(f32.buffer, f32.byteOffset, f32.byteLength);
}

export function inferVectorDim(
  v: Float32Array | number[] | Uint8Array,
  elementType?: VectorElementType,
  explicitDim?: number
): number {
  if (explicitDim !== undefined) return explicitDim;
  if (v instanceof Uint8Array) {
    if (elementType === "int8") return v.byteLength;
    if (elementType === "bit") return v.byteLength * 8;
    return v.byteLength / 4;
  }
  return v.length;
}

function encodeCrdtJsonStateBytes(state: Uint8Array | LwwJsonDocumentState): Uint8Array {
  if (state instanceof Uint8Array) return state;
  const wireJson = JSON.stringify(toWireLwwJsonState(state));
  return new Uint8Array(deflateRawSync(Buffer.from(wireJson, "utf8")));
}

export function batchOpToWire(
  op: BatchOperation,
  mode: "msgpack-daemon" | "msgpack-native" | "json-native"
): Record<string, unknown> {
  const isJson = mode === "json-native";
  const encodeVersion = (v: bigint | undefined) => {
    const checked = checkedVersion(v);
    if (checked === undefined) return undefined;
    return mode === "msgpack-daemon" ? checked : checked.toString();
  };
  const encodeExpiry = (ms: number | undefined) => {
    const checked = checkedExpiry(ms);
    if (checked === undefined) return undefined;
    return mode === "msgpack-daemon" ? BigInt(checked) : checked;
  };
  const encodeBytes = (bytes: Uint8Array) => (isJson ? [...bytes] : bytes);

  switch (op.model) {
    case "kv_set":
      return {
        model: "kv_set",
        namespace: op.namespace,
        key: encodeBytes(op.key),
        value: encodeBytes(op.value),
        if_version: encodeVersion(op.ifVersion),
        expires_at_ms: encodeExpiry(op.expiresAtMs),
      };
    case "kv_delete":
      return {
        model: "kv_delete",
        namespace: op.namespace,
        key: encodeBytes(op.key),
        if_version: encodeVersion(op.ifVersion),
      };
    case "json_put":
      return {
        model: "json_put",
        space: op.space,
        id: op.id,
        value: op.value,
        if_version: encodeVersion(op.ifVersion),
      };
    case "json_put_raw":
      return {
        model: "json_put_raw",
        space: op.space,
        id: op.id,
        data: encodeBytes(op.data),
        if_version: encodeVersion(op.ifVersion),
      };
    case "json_put_hlc":
      return {
        model: "json_put_hlc",
        space: op.space,
        id: op.id,
        value: op.value,
        hlc: op.hlc,
        parent_hlc: op.parentHlc,
        strategy: op.strategy ?? "last_write_wins",
      };
    case "json_delete":
      return {
        model: "json_delete",
        space: op.space,
        id: op.id,
        if_version: encodeVersion(op.ifVersion),
      };
    case "graph_set_node":
      return {
        model: "graph_set_node",
        graph: op.graph,
        id: op.id,
        kind: op.kind,
        metadata: op.metadata ?? {},
        doc_space: op.docSpace,
        doc_id: op.docId,
      };
    case "graph_delete_node":
      return {
        model: "graph_delete_node",
        graph: op.graph,
        id: op.id,
      };
    case "graph_add_edge":
      return {
        model: "graph_add_edge",
        graph: op.graph,
        source: op.source,
        target: op.target,
        kind: op.kind,
        weight: op.weight ?? 1.0,
      };
    case "graph_remove_edge":
      return {
        model: "graph_remove_edge",
        graph: op.graph,
        source: op.source,
        target: op.target,
        kind: op.kind,
      };
    case "fts_index":
      return {
        model: "fts_index",
        collection: op.collection,
        doc_id: op.docId,
        content: op.content,
      };
    case "fts_delete":
      return {
        model: "fts_delete",
        collection: op.collection,
        doc_id: op.docId,
      };
    case "vector_set":
      return {
        model: "vector_set",
        collection: op.collection,
        id: op.id,
        dim: op.dim,
        element_type: op.elementType ?? "float32",
        vector: encodeBytes(vectorToBytes(op.vector, op.elementType)),
      };
    case "vector_delete":
      return {
        model: "vector_delete",
        collection: op.collection,
        id: op.id,
      };
    case "crdt_pn_counter_increment":
      return {
        model: "crdt_pn_counter_increment",
        space: op.space,
        id: op.id,
        actor_id: op.actorId,
        amount: isJson ? Number(op.amount) : typeof op.amount === "bigint" ? op.amount : Number(op.amount),
      };
    case "crdt_pn_counter_decrement":
      return {
        model: "crdt_pn_counter_decrement",
        space: op.space,
        id: op.id,
        actor_id: op.actorId,
        amount: isJson ? Number(op.amount) : typeof op.amount === "bigint" ? op.amount : Number(op.amount),
      };
    case "crdt_or_set_add":
      return {
        model: "crdt_or_set_add",
        space: op.space,
        id: op.id,
        actor_id: op.actorId,
        element: op.element,
      };
    case "crdt_or_set_remove":
      return {
        model: "crdt_or_set_remove",
        space: op.space,
        id: op.id,
        element: op.element,
        timestamp_ms: op.timestampMs,
      };
    case "crdt_json_set_field":
      return {
        model: "crdt_json_set_field",
        space: op.space,
        id: op.id,
        actor_id: op.actorId,
        path: op.path,
        value: op.value,
        timestamp_ms: op.timestampMs,
      };
    case "crdt_json_delete_field":
      return {
        model: "crdt_json_delete_field",
        space: op.space,
        id: op.id,
        actor_id: op.actorId,
        path: op.path,
        timestamp_ms: op.timestampMs,
      };
    case "crdt_json_merge":
      return {
        model: "crdt_json_merge",
        space: op.space,
        id: op.id,
        state: encodeBytes(encodeCrdtJsonStateBytes(op.state)),
      };
  }
}
