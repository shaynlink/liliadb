// Conversions between the public types of the package and the wire types of the protocol.

import { vectorToBytes } from "./math.js";
import type {
  BatchOperation,
  CrdtCounterState,
  CrdtOrSetState,
  CypherResult,
  DownsampleAggregation,
  FtsFuzzyHit,
  FtsHit,
  FtsPhoneticHit,
  FtsSuggestion,
  GeofenceMatch,
  GpsPoint,
  GraphDirection,
  GraphEdge,
  GraphNode,
  HnswConfig,
  HybridHit,
  ImpactAnalysis,
  JsonEntry,
  JsonIndexInfo,
  JsonRevision,
  JsonValue,
  KvEntry,
  LwwJsonDocumentState,
  MutationResult,
  NodeScore,
  PageRankConfig,
  SqlQueryResult,
  StorageStats,
  TimeSeriesLifecyclePolicy,
  TimeSeriesPoint,
  TimeSeriesRetentionStats,
  TimeSeriesStats,
  UndoStep,
  VectorHit,
  VectorIndexInfo,
  WeightedPath,
} from "./types.js";
import { checkedExpiry, checkedVersion, readVersion } from "./version.js";
import type {
  ResponseOf,
  WireAggregation,
  WireBatchOperation,
  WireDirection,
  WireFtsHit,
  WireGraphNode,
  WireHnswConfig,
  WireJsonEntry,
  WireKvEntry,
  WireLwwDocument,
  WirePageRankConfig,
  WirePolicy,
  WireUndoStep,
  WireWeightedPath,
} from "./wire.js";

const DIRECTIONS: Record<GraphDirection, WireDirection> = {
  outgoing: "outbound",
  incoming: "inbound",
  both: "both",
};

const AGGREGATIONS: Record<DownsampleAggregation, WireAggregation> = {
  avg: "Avg",
  min: "Min",
  max: "Max",
  sum: "Sum",
  first: "First",
  last: "Last",
  count: "Count",
};

const num = (value: number | bigint): number => Number(value);

export function toWireDirection(direction: GraphDirection | undefined): WireDirection {
  return DIRECTIONS[direction ?? "outgoing"];
}

export function toWirePageRankConfig(config: PageRankConfig = {}): WirePageRankConfig {
  return {
    damping_factor: config.dampingFactor,
    max_iterations: config.maxIterations,
    tolerance: config.tolerance,
    seeds: config.seeds,
    direction: config.direction === undefined ? undefined : toWireDirection(config.direction),
    edge_kind: config.edgeKind,
  };
}

export function toWireHnswConfig(config: HnswConfig): WireHnswConfig {
  return {
    m: config.m,
    ef_construction: config.efConstruction,
    ef_search: config.efSearch,
    metric: config.metric,
    element_type: config.elementType,
    dim: config.dim,
  };
}

export function toWirePolicy(policy: TimeSeriesLifecyclePolicy): WirePolicy {
  return {
    series_pattern: policy.seriesPattern,
    rules: policy.rules.map((rule) => ({
      resolution_ms: rule.resolutionMs,
      retention_ms: rule.retentionMs ?? null,
      aggregation: AGGREGATIONS[rule.aggregation],
    })),
  };
}

export function toWireCounterState(state: CrdtCounterState): CrdtCounterState {
  return { p: { ...state.p }, n: { ...state.n } };
}

export function toWireLwwDocument(state: LwwJsonDocumentState): WireLwwDocument {
  return {
    fields: Object.fromEntries(
      Object.entries(state.fields).map(([path, field]) => [
        path,
        {
          value: field.value,
          clock: {
            clock: {
              timestamp_ms: field.clock.clock.timestampMs,
              counter: field.clock.clock.counter,
            },
            actor_id: field.clock.actorId,
          },
          is_deleted: field.isDeleted,
        },
      ]),
    ),
  };
}

const encoder = new TextEncoder();

/** A merged CRDT document state as a batch carries it: its JSON. */
function lwwStateBytes(state: LwwJsonDocumentState): Uint8Array {
  return encoder.encode(JSON.stringify(toWireLwwDocument(state)));
}

export function toWireBatchOperation(operation: BatchOperation): WireBatchOperation {
  switch (operation.model) {
    case "kv_set":
      return {
        model: "kv_set",
        namespace: operation.namespace,
        key: operation.key,
        value: operation.value,
        if_version: checkedVersion(operation.ifVersion),
        expires_at_ms: checkedExpiry(operation.expiresAtMs),
      };
    case "kv_delete":
      return {
        model: "kv_delete",
        namespace: operation.namespace,
        key: operation.key,
        if_version: checkedVersion(operation.ifVersion),
      };
    case "json_put":
      return {
        model: "json_put",
        space: operation.space,
        id: operation.id,
        value: operation.value,
        if_version: checkedVersion(operation.ifVersion),
      };
    case "json_put_raw":
      return {
        model: "json_put_raw",
        space: operation.space,
        id: operation.id,
        data: operation.data,
        if_version: checkedVersion(operation.ifVersion),
      };
    case "json_delete":
      return {
        model: "json_delete",
        space: operation.space,
        id: operation.id,
        if_version: checkedVersion(operation.ifVersion),
      };
    case "graph_set_node":
      return {
        model: "graph_set_node",
        graph: operation.graph,
        id: operation.id,
        kind: operation.kind,
        metadata: operation.metadata ?? {},
        doc_space: operation.docSpace,
        doc_id: operation.docId,
      };
    case "graph_delete_node":
      return { model: "graph_delete_node", graph: operation.graph, id: operation.id };
    case "graph_add_edge":
      return {
        model: "graph_add_edge",
        graph: operation.graph,
        source: operation.source,
        target: operation.target,
        kind: operation.kind,
        weight: operation.weight ?? 1,
      };
    case "graph_remove_edge":
      return {
        model: "graph_remove_edge",
        graph: operation.graph,
        source: operation.source,
        target: operation.target,
        kind: operation.kind,
      };
    case "fts_index":
      return {
        model: "fts_index",
        collection: operation.collection,
        doc_id: operation.docId,
        content: operation.content,
      };
    case "fts_delete":
      return { model: "fts_delete", collection: operation.collection, doc_id: operation.docId };
    case "vector_set":
      return {
        model: "vector_set",
        collection: operation.collection,
        id: operation.id,
        dim: operation.dim,
        element_type: operation.elementType ?? "float32",
        vector: vectorToBytes(operation.vector, operation.elementType),
      };
    case "vector_delete":
      return { model: "vector_delete", collection: operation.collection, id: operation.id };
    case "crdt_pn_counter_increment":
    case "crdt_pn_counter_decrement":
      return {
        model: operation.model,
        space: operation.space,
        id: operation.id,
        actor_id: operation.actorId,
        amount: operation.amount,
      };
    case "crdt_or_set_add":
      return {
        model: "crdt_or_set_add",
        space: operation.space,
        id: operation.id,
        actor_id: operation.actorId,
        element: operation.element,
      };
    case "crdt_or_set_remove":
      return {
        model: "crdt_or_set_remove",
        space: operation.space,
        id: operation.id,
        element: operation.element,
        timestamp_ms: operation.timestampMs,
      };
    case "crdt_json_set_field":
      return {
        model: "crdt_json_set_field",
        space: operation.space,
        id: operation.id,
        actor_id: operation.actorId,
        path: operation.path,
        value: operation.value,
        timestamp_ms: operation.timestampMs,
      };
    case "crdt_json_delete_field":
      return {
        model: "crdt_json_delete_field",
        space: operation.space,
        id: operation.id,
        actor_id: operation.actorId,
        path: operation.path,
        timestamp_ms: operation.timestampMs,
      };
    case "crdt_json_merge":
      return {
        model: "crdt_json_merge",
        space: operation.space,
        id: operation.id,
        state: lwwStateBytes(operation.state),
      };
  }
}

export function mapKvEntry(entry: WireKvEntry): KvEntry {
  return {
    namespace: entry.namespace,
    key: entry.key,
    value: entry.value,
    version: readVersion(entry.version),
    ...(entry.expires_at_ms == null ? {} : { expiresAtMs: num(entry.expires_at_ms) }),
  };
}

export function mapJsonEntry<T = JsonValue>(entry: WireJsonEntry): JsonEntry<T> {
  return {
    space: entry.space,
    id: entry.id,
    value: entry.value as T,
    version: readVersion(entry.version),
  };
}

export function mapJsonRevisions<T = JsonValue>(
  revisions: ResponseOf<"json_revisions">,
): JsonRevision<T>[] {
  return revisions.map((revision) => ({
    version: readVersion(revision.version),
    ...(revision.doc ? { document: mapJsonEntry<T>(revision.doc) } : {}),
  }));
}

export function mapJsonIndexes(indexes: ResponseOf<"json_indexes">): JsonIndexInfo[] {
  return indexes.map((index) => ({
    space: index.space,
    indexName: index.index_name,
    jsonPath: index.json_path,
  }));
}

export function mapMutations(results: ResponseOf<"mutations">): MutationResult[] {
  return results.map((result) => ({
    ...(result.version == null ? {} : { version: readVersion(result.version) }),
    deleted: result.deleted,
  }));
}

function records<T>(columns: string[], rows: JsonValue[][]): T[] {
  return rows.map(
    (row) => Object.fromEntries(columns.map((column, i) => [column, row[i] ?? null])) as T,
  );
}

export function mapSqlResult<T = Record<string, unknown>>(
  result: ResponseOf<"sql">,
): SqlQueryResult<T> {
  const rows = result.rows;
  return {
    columns: result.columns,
    rows,
    rowsAffected: num(result.rows_affected),
    records: records<T>(result.columns, rows),
  };
}

export function mapCypherResult<T = Record<string, JsonValue>>(
  result: ResponseOf<"cypher_result">,
): CypherResult<T> {
  const rows = result.rows;
  return { columns: result.columns, rows, records: records<T>(result.columns, rows) };
}

export function mapGraphNode(node: WireGraphNode): GraphNode {
  return {
    graph: node.graph,
    id: node.id,
    kind: node.kind,
    metadata: node.metadata,
    ...(node.doc_space == null ? {} : { docSpace: node.doc_space }),
    ...(node.doc_id == null ? {} : { docId: node.doc_id }),
  };
}

export function mapGraphEdges(edges: ResponseOf<"graph_edges">): GraphEdge[] {
  return edges.map((edge) => ({ ...edge, weight: num(edge.weight) }));
}

export function mapWeightedPath(path: WireWeightedPath): WeightedPath {
  return {
    nodes: path.nodes,
    totalWeight: num(path.total_weight),
    edges: path.edges.map((edge) => ({ ...edge, weight: num(edge.weight) })),
  };
}

export function mapWeightedPaths(
  paths: ResponseOf<"weighted_path_map">,
): Record<string, WeightedPath> {
  return Object.fromEntries(
    Object.entries(paths).map(([node, path]) => [node, mapWeightedPath(path)]),
  );
}

export function mapNodeScores(scores: ResponseOf<"node_scores">): NodeScore[] {
  return scores.map((score) => ({ node: score.node, score: num(score.score) }));
}

export function mapImpact(impact: ResponseOf<"graph_impact">): ImpactAnalysis {
  return {
    direct: impact.direct,
    dependants: impact.dependants,
    totalAffected: num(impact.total_affected),
  };
}

function mapFtsHit(hit: WireFtsHit): FtsHit {
  return {
    collection: hit.collection,
    docId: hit.doc_id,
    snippet: hit.snippet,
    rank: num(hit.rank),
  };
}

export function mapFtsHits(hits: ResponseOf<"fts_hits">): FtsHit[] {
  return hits.map(mapFtsHit);
}

export function mapFtsSuggestions(suggestions: ResponseOf<"fts_suggestions">): FtsSuggestion[] {
  return suggestions.map((s) => ({
    term: s.term,
    distance: num(s.distance),
    docCount: num(s.doc_count),
    exactPrefix: s.exact_prefix,
  }));
}

export function mapFtsFuzzyHits(hits: ResponseOf<"fts_fuzzy_hits">): FtsFuzzyHit[] {
  return hits.map((hit) => ({
    ...mapFtsHit(hit),
    editDistance: num(hit.edit_distance),
    matchedTerms: hit.matched_terms,
  }));
}

export function mapFtsPhoneticHits(hits: ResponseOf<"fts_phonetic_hits">): FtsPhoneticHit[] {
  return hits.map((hit) => ({
    ...mapFtsHit(hit),
    matchedTerms: hit.matched_terms,
    phoneticCodes: hit.phonetic_codes,
  }));
}

export function mapVectorHits(hits: ResponseOf<"vector_hits">): VectorHit[] {
  return hits.map((hit) => ({
    collection: hit.collection,
    id: hit.id,
    distance: num(hit.distance),
  }));
}

export function mapVectorIndexInfo(
  info: NonNullable<ResponseOf<"vector_index_info">>,
): VectorIndexInfo {
  return {
    collection: info.collection,
    nodeCount: num(info.node_count),
    maxLevel: num(info.max_level),
    m: num(info.m),
    efConstruction: num(info.ef_construction),
    efSearch: num(info.ef_search),
    metric: info.metric,
    elementType: info.element_type,
    dim: num(info.dim),
  };
}

export function mapHybridHits(hits: ResponseOf<"hybrid_hits">): HybridHit[] {
  return hits.map((hit) => ({
    id: hit.id,
    rrfScore: num(hit.rrf_score),
    ...(hit.fts_rank == null ? {} : { ftsRank: num(hit.fts_rank) }),
    ...(hit.fts_snippet == null ? {} : { ftsSnippet: hit.fts_snippet }),
    ...(hit.vector_rank == null ? {} : { vectorRank: num(hit.vector_rank) }),
    ...(hit.vector_distance == null ? {} : { vectorDistance: num(hit.vector_distance) }),
  }));
}

export function mapPoints(points: ResponseOf<"ts_points">): TimeSeriesPoint[] {
  return points.map((point) => ({ timestamp: num(point.timestamp), value: num(point.value) }));
}

export function mapSeriesStats(stats: NonNullable<ResponseOf<"ts_stats">>): TimeSeriesStats {
  return {
    min: num(stats.min),
    max: num(stats.max),
    avg: num(stats.avg),
    sum: num(stats.sum),
    count: num(stats.count),
  };
}

export function mapPolicy(policy: WirePolicy): TimeSeriesLifecyclePolicy {
  const names = Object.fromEntries(
    Object.entries(AGGREGATIONS).map(([name, wire]) => [wire, name]),
  ) as Record<WireAggregation, DownsampleAggregation>;
  return {
    seriesPattern: policy.series_pattern,
    rules: policy.rules.map((rule) => ({
      resolutionMs: num(rule.resolution_ms),
      retentionMs: rule.retention_ms == null ? null : num(rule.retention_ms),
      aggregation: names[rule.aggregation],
    })),
  };
}

export function mapRetentionStats(
  stats: ResponseOf<"ts_retention_stats">,
): TimeSeriesRetentionStats {
  return {
    seriesProcessed: num(stats.series_processed),
    downsampledPointsCreated: num(stats.downsampled_points_created),
    chunksPruned: num(stats.chunks_pruned),
    bytesReclaimedApprox: num(stats.bytes_reclaimed_approx),
  };
}

export function mapGpsPoints(points: ResponseOf<"gps_points">): GpsPoint[] {
  return points.map((point) => ({
    timestamp: num(point.timestamp),
    latitude: num(point.latitude),
    longitude: num(point.longitude),
    ...(point.altitude == null ? {} : { altitude: num(point.altitude) }),
    ...(point.speed == null ? {} : { speed: num(point.speed) }),
    ...(point.accuracy == null ? {} : { accuracy: num(point.accuracy) }),
  }));
}

export function mapGeofenceMatches(matches: ResponseOf<"geofence_matches">): GeofenceMatch[] {
  return matches.map((match) => ({
    id: match.id,
    name: match.name,
    category: match.category,
    ...(match.metadata == null ? {} : { metadata: match.metadata }),
  }));
}

const numbers = (record: Record<string, number | bigint>): Record<string, number> =>
  Object.fromEntries(Object.entries(record).map(([key, value]) => [key, num(value)]));

export function mapCounterState(state: ResponseOf<"crdt_counter_state">): CrdtCounterState {
  return { p: numbers(state.p), n: numbers(state.n) };
}

export function mapSetState(state: ResponseOf<"crdt_set_state">): CrdtOrSetState {
  return { entries: state.entries, tombstones: numbers(state.tombstones) };
}

export function mapLwwDocument(state: WireLwwDocument): LwwJsonDocumentState {
  return {
    fields: Object.fromEntries(
      Object.entries(state.fields).map(([path, field]) => [
        path,
        {
          value: field.value,
          clock: {
            clock: {
              timestampMs: num(field.clock.clock.timestamp_ms),
              counter: num(field.clock.clock.counter),
            },
            actorId: field.clock.actor_id,
          },
          isDeleted: field.is_deleted,
        },
      ]),
    ),
  };
}

export function mapUndoStep(step: WireUndoStep): UndoStep {
  return { version: readVersion(step.version), keys: num(step.keys) };
}

export function mapStorageStats(stats: ResponseOf<"storage_stats">): StorageStats {
  return {
    levels: stats.levels.map((level) => ({ runs: num(level.runs), bytes: num(level.bytes) })),
    memtableBytes: num(stats.memtable_bytes),
    immutableMemtables: num(stats.immutable_memtables),
    version: readVersion(stats.version),
    oldestVersion: readVersion(stats.oldest_version),
  };
}
