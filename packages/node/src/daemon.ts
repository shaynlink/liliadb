import { readFile } from "node:fs/promises";
import { connect, type Socket } from "node:net";
import { decode, encode } from "@msgpack/msgpack";
import {
  LiliaError,
  type BatchOperation,
  type CentralityType,
  type CheckpointStats,
  type ComponentType,
  type ConflictRecord,
  type ConflictResolutionStrategy,
  type CrdtCounterState,
  type CrdtOrSetState,
  type CypherResult,
  type DatabaseEvent,
  type DictStats,
  type FtsHit,
  type GeofenceMatch,
  type GpsPoint,
  type AutoEmbedConfig,
  type EmbedStatus,
  type GraphDirection,
  type GraphEdge,
  type GraphNode,
  type HlcMutationOutcome,
  type HlcRecord,
  type HnswConfig,
  type HybridHit,
  type ImpactAnalysis,
  type JsonEntry,
  type JsonIndexInfo,
  type JsonRevisionInfo,
  type JsonValue,
  type KvEntry,
  type LiveQueryDiff,
  type LiveQuerySubscription,
  type LwwJsonDocumentState,
  type MaterializedViewInfo,
  type MutationResult,
  type NodeScore,
  type PageRankConfig,
  type RedoResult,
  type SqlQueryResult,
  type TableStorageStat,
  type TimeSeriesLifecyclePolicy,
  type TimeSeriesPoint,
  type TimeSeriesRetentionStats,
  type TimeSeriesStats,
  type UndoEntry,
  type UndoResult,
  type VectorElementType,
  type VectorHit,
  type VectorIndexInfo,
  type VectorMetric,
  type WatchSubscription,
  type WeightedPath,
} from "./types.js";
import { checkedVersion, jsonNumbers, readVersion, getUuid } from "./version.js";
import {
  batchOpToWire,
  formatHlcRequired,
  inferVectorDim,
  mapCheckpointStats,
  mapConflictRecord,
  mapCrdtCounterState,
  mapCrdtOrSetState,
  mapCypherResult,
  mapDatabaseEvent,
  mapDictStats,
  mapFtsHits,
  mapGeofenceMatches,
  mapGpsPoints,
  mapGraphEdge,
  mapGraphNode,
  mapHlcMutationOutcome,
  mapHlcRecord,
  mapHybridHits,
  mapImpactAnalysis,
  mapJsonEntry,
  mapJsonIndexList,
  mapJsonRevision,
  mapLiveQueryDiff,
  mapLwwJsonState,
  mapMaterializedViews,
  mapMutationResults,
  mapNodeScore,
  mapRedoResult,
  mapSqlQueryResult,
  mapStorageStats,
  mapTsPoints,
  mapTsPolicy,
  mapTsRetentionStats,
  mapTsStats,
  mapUndoEntry,
  mapUndoResult,
  mapVectorHits,
  mapVectorIndexInfo,
  mapWeightedPath,
  toWireHnswConfig,
  toWireLwwJsonState,
  toWirePageRankConfig,
  toWireTsPolicy,
  vectorToBytes,
} from "./mappers.js";
import { makeSubscription } from "./native.js";

interface WireResponse {
  id: string;
  result: {
    Ok?: unknown;
    Err?: { message: string; code: string; retryable: boolean; request_id: string | Uint8Array };
  };
}

type WireRecord = Record<string, unknown>;
const MSGPACK_ENCODE_OPTS = { useBigInt64: true, ignoreUndefined: true } as const;

export class DaemonClient {
  private activeSocket?: Socket;
  private socketBuffer = Buffer.alloc(0);
  private readonly pendingCalls = new Map<string, PromiseWithResolvers<WireResponse>>();
  private readonly streamSockets = new Set<Socket>();
  private connecting?: PromiseWithResolvers<Socket>;

  private constructor(private readonly endpoint: string, private readonly token: string) {}

  static async open(endpoint: string, tokenFile: string, options?: { signal?: AbortSignal }): Promise<DaemonClient> {
    options?.signal?.throwIfAborted();
    const token = (await readFile(tokenFile, "utf8")).trim();
    const client = new DaemonClient(endpoint, token);
    await client.call({ type: "handshake", major: 1, minor: 1 }, options?.signal);
    return client;
  }

  // KV
  async kvGet(namespace: string, key: Uint8Array, signal?: AbortSignal): Promise<KvEntry | undefined> {
    return mapKv(await this.call({ type: "kv_get", namespace, key }, signal));
  }
  async kvScan(namespace: string, after: Uint8Array | undefined, limit: number, signal?: AbortSignal): Promise<KvEntry[]> {
    return ((await this.call({ type: "kv_scan", namespace, after: after ? [...after] : undefined, limit }, signal)) as unknown[])
      .map((value) => mapKv(value) as KvEntry);
  }

  // JSON + Time-Travel + Zstd + HLC
  async jsonGet<T = JsonValue>(space: string, id: string, signal?: AbortSignal): Promise<JsonEntry<T> | undefined> {
    const raw = await this.call({ type: "json_get", space, id }, signal);
    return raw ? mapJsonEntry<T>(raw as WireRecord) : undefined;
  }
  async jsonExtract(space: string, id: string, path: string, signal?: AbortSignal): Promise<JsonValue | undefined> {
    const raw = await this.call({ type: "json_extract", space, id, path }, signal);
    return raw == null ? undefined : jsonNumbers(raw);
  }
  async jsonScan<T = JsonValue>(space: string, after: string | undefined, limit: number, signal?: AbortSignal): Promise<JsonEntry<T>[]> {
    return ((await this.call({ type: "json_scan", space, after, limit }, signal)) as WireRecord[]).map((v) => mapJsonEntry<T>(v));
  }
  async jsonScanPrefix<T = JsonValue>(space: string, prefix: string, afterId: string | undefined, limit: number, signal?: AbortSignal): Promise<JsonEntry<T>[]> {
    return ((await this.call({ type: "json_scan_prefix", space, prefix, after_id: afterId, limit }, signal)) as WireRecord[]).map((v) =>
      mapJsonEntry<T>(v)
    );
  }
  async jsonCreateIndex(space: string, name: string, path: string, filter?: string, signal?: AbortSignal): Promise<void> {
    await this.call({ type: "json_create_index", space, name, path, filter }, signal);
  }
  async jsonDropIndex(space: string, name: string, signal?: AbortSignal): Promise<boolean> {
    return Boolean(await this.call({ type: "json_drop_index", space, name }, signal));
  }
  async jsonListIndexes(space: string, signal?: AbortSignal): Promise<JsonIndexInfo[]> {
    const list = (await this.call({ type: "json_list_indexes", space }, signal)) as unknown[];
    return mapJsonIndexList(list ?? []);
  }
  async jsonScanByIndex<T = JsonValue>(
    space: string,
    indexName: string,
    value: string,
    after: string | undefined,
    limit: number,
    signal?: AbortSignal
  ): Promise<JsonEntry<T>[]> {
    const list = (await this.call({ type: "json_scan_by_index", space, index_name: indexName, value, after, limit }, signal)) as WireRecord[];
    return (list ?? []).map((v) => mapJsonEntry<T>(v));
  }
  async jsonFind<T = JsonValue>(
    space: string,
    query: Record<string, JsonValue>,
    after: string | undefined,
    limit: number,
    signal?: AbortSignal
  ): Promise<JsonEntry<T>[]> {
    const list = (await this.call({ type: "json_find", space, query, after, limit }, signal)) as WireRecord[];
    return (list ?? []).map((v) => mapJsonEntry<T>(v));
  }
  async jsonGetAtVersion<T = JsonValue>(space: string, id: string, version: bigint, signal?: AbortSignal): Promise<JsonEntry<T> | undefined> {
    const raw = await this.call({ type: "json_get_at_version", space, id, version: checkedVersion(version) }, signal);
    return raw ? mapJsonEntry<T>(raw as WireRecord) : undefined;
  }
  async jsonGetAtTimestamp<T = JsonValue>(space: string, id: string, timestampMs: number, signal?: AbortSignal): Promise<JsonEntry<T> | undefined> {
    const raw = await this.call({ type: "json_get_at_timestamp", space, id, timestamp_ms: BigInt(Math.trunc(timestampMs)) }, signal);
    return raw ? mapJsonEntry<T>(raw as WireRecord) : undefined;
  }
  async jsonHistory(space: string, id: string, signal?: AbortSignal): Promise<JsonRevisionInfo[]> {
    return ((await this.call({ type: "json_history", space, id }, signal)) as WireRecord[]).map(mapJsonRevision);
  }
  async jsonDiff(space: string, id: string, fromVersion: bigint, toVersion: bigint, signal?: AbortSignal): Promise<JsonValue> {
    const raw = await this.call(
      {
        type: "json_diff",
        space,
        id,
        from_version: checkedVersion(fromVersion),
        to_version: checkedVersion(toVersion),
      },
      signal
    );
    return jsonNumbers(raw);
  }
  async jsonTrainDictionary(space: string, sampleCount?: number, dictSize?: number, signal?: AbortSignal): Promise<DictStats> {
    return mapDictStats(
      (await this.call(
        {
          type: "json_train_dictionary",
          space,
          sample_count: sampleCount ?? 100,
          dict_size: dictSize ?? 16_384,
        },
        signal
      )) as WireRecord
    );
  }
  async jsonSetAutoTrain(space: string, enabled: boolean, signal?: AbortSignal): Promise<void> {
    await this.call({ type: "json_set_auto_train", space, enabled }, signal);
  }
  async jsonIsAutoTrainEnabled(space: string, signal?: AbortSignal): Promise<boolean> {
    return Boolean(await this.call({ type: "json_is_auto_train_enabled", space }, signal));
  }
  async jsonPutHlc(
    space: string,
    id: string,
    value: JsonValue,
    hlc?: string,
    parentHlc?: string,
    strategy?: ConflictResolutionStrategy,
    signal?: AbortSignal
  ): Promise<HlcMutationOutcome> {
    return mapHlcMutationOutcome(
      (await this.call(
        {
          type: "json_put_hlc",
          space,
          id,
          value,
          hlc,
          parent_hlc: parentHlc,
          strategy: strategy ?? "last_write_wins",
        },
        signal
      )) as WireRecord
    );
  }
  async jsonGetHlc(space: string, id: string, signal?: AbortSignal): Promise<HlcRecord | undefined> {
    const raw = await this.call({ type: "json_get_hlc", space, id }, signal);
    return raw ? mapHlcRecord(raw as WireRecord) : undefined;
  }

  // Batch & SQL
  async batch(operations: readonly BatchOperation[], signal?: AbortSignal): Promise<MutationResult[]> {
    const result = (await this.call(
      { type: "batch", operations: operations.map((op) => batchOpToWire(op, "msgpack-daemon")) },
      signal
    )) as unknown[];
    return mapMutationResults(result ?? []);
  }
  async sqlQuery<T = Record<string, unknown>>(sql: string, params: unknown[] = [], signal?: AbortSignal): Promise<SqlQueryResult<T>> {
    const raw = (await this.call({ type: "sql_query", sql, params }, signal)) as WireRecord;
    return mapSqlQueryResult<T>(raw);
  }

  // Graph
  async graphNeighbors(graph: string, node: string, direction?: GraphDirection, kind?: string, signal?: AbortSignal): Promise<string[]> {
    return ((await this.call({ type: "graph_neighbors", graph, node, direction: direction ?? "outgoing", kind }, signal)) as string[]) ?? [];
  }
  async graphReachability(graph: string, startNodes: string[], direction?: GraphDirection, signal?: AbortSignal): Promise<string[]> {
    return ((await this.call({ type: "graph_reachability", graph, start_nodes: startNodes, direction: direction ?? "outgoing" }, signal)) as string[]) ?? [];
  }
  async graphGetNode(graph: string, id: string, signal?: AbortSignal): Promise<GraphNode | undefined> {
    const raw = await this.call({ type: "graph_get_node", graph, id }, signal);
    return raw ? mapGraphNode(raw as WireRecord) : undefined;
  }
  async graphGetNodeDocument<T = JsonValue>(graph: string, id: string, signal?: AbortSignal): Promise<JsonEntry<T> | undefined> {
    const raw = await this.call({ type: "graph_get_node_document", graph, id }, signal);
    return raw ? mapJsonEntry<T>(raw as WireRecord) : undefined;
  }
  async graphDeleteNode(graph: string, id: string, signal?: AbortSignal): Promise<boolean> {
    return Boolean(await this.call({ type: "graph_delete_node", graph, id }, signal));
  }
  async graphListNodes(graph: string, kind?: string, limit?: number, signal?: AbortSignal): Promise<GraphNode[]> {
    return ((await this.call({ type: "graph_list_nodes", graph, kind, limit: limit ?? 100 }, signal)) as WireRecord[]).map(mapGraphNode);
  }
  async graphRemoveEdge(graph: string, source: string, target: string, kind: string, signal?: AbortSignal): Promise<boolean> {
    return Boolean(await this.call({ type: "graph_remove_edge", graph, source, target, kind }, signal));
  }
  async graphListEdges(graph: string, source?: string, target?: string, kind?: string, limit?: number, signal?: AbortSignal): Promise<GraphEdge[]> {
    return ((await this.call({ type: "graph_list_edges", graph, source, target, kind, limit: limit ?? 100 }, signal)) as WireRecord[]).map(
      mapGraphEdge
    );
  }
  async graphImpactAnalysis(graph: string, changedNodes: string[], signal?: AbortSignal): Promise<ImpactAnalysis> {
    return mapImpactAnalysis((await this.call({ type: "graph_impact_analysis", graph, changed_nodes: changedNodes }, signal)) as WireRecord);
  }
  async graphShortestPath(graph: string, start: string, target: string, direction?: GraphDirection, kindFilter?: string, signal?: AbortSignal): Promise<WeightedPath | undefined> {
    const raw = await this.call(
      { type: "graph_shortest_path", graph, start, target, direction: direction ?? "outgoing", kind_filter: kindFilter },
      signal
    );
    return raw ? mapWeightedPath(raw as WireRecord) : undefined;
  }
  async graphDijkstra(graph: string, start: string, target?: string, direction?: GraphDirection, kindFilter?: string, signal?: AbortSignal): Promise<WeightedPath | undefined> {
    const raw = await this.call(
      { type: "graph_dijkstra", graph, start, target, direction: direction ?? "outgoing", kind_filter: kindFilter },
      signal
    );
    return raw ? mapWeightedPath(raw as WireRecord) : undefined;
  }
  async graphAllShortestPaths(graph: string, start: string, direction?: GraphDirection, kindFilter?: string, signal?: AbortSignal): Promise<Record<string, WeightedPath>> {
    const raw = (await this.call(
      { type: "graph_all_shortest_paths", graph, start, direction: direction ?? "outgoing", kind_filter: kindFilter },
      signal
    )) as Record<string, WireRecord>;
    return Object.fromEntries(Object.entries(raw ?? {}).map(([k, v]) => [k, mapWeightedPath(v)]));
  }
  async graphAstarSpatial(graph: string, start: string, target: string, direction?: GraphDirection, kindFilter?: string, signal?: AbortSignal): Promise<WeightedPath | undefined> {
    const raw = await this.call(
      { type: "graph_astar_spatial", graph, start, target, direction: direction ?? "outgoing", kind_filter: kindFilter },
      signal
    );
    return raw ? mapWeightedPath(raw as WireRecord) : undefined;
  }
  async graphPagerank(graph: string, config?: PageRankConfig, signal?: AbortSignal): Promise<NodeScore[]> {
    return ((await this.call({ type: "graph_page_rank", graph, config: toWirePageRankConfig(config) }, signal)) as WireRecord[]).map(
      mapNodeScore
    );
  }
  async graphCentrality(graph: string, centrality: CentralityType, direction?: GraphDirection, kindFilter?: string, normalized?: boolean, signal?: AbortSignal): Promise<NodeScore[]> {
    return ((await this.call(
      {
        type: "graph_centrality",
        graph,
        centrality,
        direction: direction ?? "outgoing",
        kind_filter: kindFilter,
        normalized: normalized ?? false,
      },
      signal
    )) as WireRecord[]).map(mapNodeScore);
  }
  async graphHasCycle(graph: string, direction?: GraphDirection, kindFilter?: string, signal?: AbortSignal): Promise<boolean> {
    return Boolean(await this.call({ type: "graph_has_cycle", graph, direction: direction ?? "outgoing", kind_filter: kindFilter }, signal));
  }
  async graphFindCycles(graph: string, maxCycles?: number, kindFilter?: string, signal?: AbortSignal): Promise<string[][]> {
    return ((await this.call({ type: "graph_find_cycles", graph, max_cycles: maxCycles ?? 100, kind_filter: kindFilter }, signal)) as string[][]) ?? [];
  }
  async graphConnectedComponents(graph: string, componentType: ComponentType, kindFilter?: string, signal?: AbortSignal): Promise<string[][]> {
    return ((await this.call({ type: "graph_connected_components", graph, component_type: componentType, kind_filter: kindFilter }, signal)) as string[][]) ?? [];
  }
  async graphCypher<T = Record<string, JsonValue>>(graph: string, query: string, signal?: AbortSignal): Promise<CypherResult<T>> {
    return mapCypherResult<T>((await this.call({ type: "graph_cypher", graph, query }, signal)) as WireRecord);
  }

  // FTS
  async ftsDelete(collection: string, docId: string, signal?: AbortSignal): Promise<boolean> {
    return Boolean(await this.call({ type: "fts_delete", collection, doc_id: docId }, signal));
  }
  async ftsSearch(collection: string, query: string, limit?: number, signal?: AbortSignal): Promise<FtsHit[]> {
    return mapFtsHits(((await this.call({ type: "fts_search", collection, query, limit: limit ?? 10 }, signal)) as WireRecord[]) ?? []);
  }

  // Vector & Hybrid
  async vectorGet(collection: string, id: string, signal?: AbortSignal): Promise<Uint8Array | null> {
    const raw = (await this.call({ type: "vector_get", collection, id }, signal)) as [number, string, Uint8Array] | null;
    return raw ? new Uint8Array(raw[2]) : null;
  }
  async vectorSearchText(
    collection: string,
    text: string,
    limit?: number,
    signal?: AbortSignal
  ): Promise<VectorHit[]> {
    const hits = (await this.call(
      {
        type: "vector_search_text",
        collection,
        text,
        limit: limit ?? 10,
      },
      signal
    )) as WireRecord[];
    return mapVectorHits(hits ?? []);
  }
  async waitForEmbeddings(timeoutMs?: number, signal?: AbortSignal): Promise<void> {
    await this.call({ type: "wait_for_embeddings", timeout_ms: timeoutMs ?? 60_000 }, signal);
  }
  jsonSetAutoEmbedSync(collection: string, config?: AutoEmbedConfig): void {
    throw new Error("jsonSetAutoEmbedSync is only supported in embedded mode");
  }
  jsonEmbedStatusSync(collection: string): EmbedStatus {
    throw new Error("jsonEmbedStatusSync is only supported in embedded mode");
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
    const d = inferVectorDim(query, elementType, dim);
    const hits = (await this.call(
      {
        type: "vector_search",
        collection,
        vector: raw,
        dim: d,
        element_type: elementType ?? "float32",
        metric: metric ?? "cosine",
        limit: limit ?? 10,
      },
      signal
    )) as WireRecord[];
    return mapVectorHits(hits ?? []);
  }
  async vectorCreateIndex(collection: string, config: HnswConfig, signal?: AbortSignal): Promise<void> {
    await this.call({ type: "vector_create_index", collection, config: toWireHnswConfig(config) }, signal);
  }
  async vectorGetIndexInfo(collection: string, signal?: AbortSignal): Promise<VectorIndexInfo | undefined> {
    const raw = await this.call({ type: "vector_get_index_info", collection }, signal);
    return raw ? mapVectorIndexInfo(raw as WireRecord) : undefined;
  }
  async vectorDropIndex(collection: string, signal?: AbortSignal): Promise<boolean> {
    return Boolean(await this.call({ type: "vector_drop_index", collection }, signal));
  }
  async vectorSearchFiltered(
    collection: string,
    query: Float32Array | number[] | Uint8Array,
    candidateIds: string[],
    dim?: number,
    elementType?: VectorElementType,
    metric?: VectorMetric,
    limit?: number,
    signal?: AbortSignal
  ): Promise<VectorHit[]> {
    const raw = vectorToBytes(query, elementType);
    const d = inferVectorDim(query, elementType, dim);
    const hits = (await this.call(
      {
        type: "vector_search_filtered",
        collection,
        query_vector: raw,
        dim: d,
        element_type: elementType ?? "float32",
        metric: metric ?? "cosine",
        candidate_ids: candidateIds,
        limit: limit ?? 10,
      },
      signal
    )) as WireRecord[];
    return mapVectorHits(hits ?? []);
  }
  async vectorSearchInGraphNeighbors(
    collection: string,
    graph: string,
    startNode: string,
    direction: GraphDirection | undefined,
    query: Float32Array | number[] | Uint8Array,
    dim?: number,
    elementType?: VectorElementType,
    metric?: VectorMetric,
    limit?: number,
    signal?: AbortSignal
  ): Promise<VectorHit[]> {
    const raw = vectorToBytes(query, elementType);
    const d = inferVectorDim(query, elementType, dim);
    const hits = (await this.call(
      {
        type: "vector_search_in_graph_neighbors",
        collection,
        graph,
        start_node: startNode,
        direction: direction ?? "outgoing",
        query_vector: raw,
        dim: d,
        element_type: elementType ?? "float32",
        metric: metric ?? "cosine",
        limit: limit ?? 10,
      },
      signal
    )) as WireRecord[];
    return mapVectorHits(hits ?? []);
  }
  async vectorSearchByJsonIndex(
    collection: string,
    space: string,
    indexName: string,
    indexValue: string,
    query: Float32Array | number[] | Uint8Array,
    dim?: number,
    elementType?: VectorElementType,
    metric?: VectorMetric,
    limit?: number,
    signal?: AbortSignal
  ): Promise<VectorHit[]> {
    const raw = vectorToBytes(query, elementType);
    const d = inferVectorDim(query, elementType, dim);
    const hits = (await this.call(
      {
        type: "vector_search_by_json_index",
        collection,
        space,
        index_name: indexName,
        index_value: indexValue,
        query_vector: raw,
        dim: d,
        element_type: elementType ?? "float32",
        metric: metric ?? "cosine",
        limit: limit ?? 10,
      },
      signal
    )) as WireRecord[];
    return mapVectorHits(hits ?? []);
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
    const d = options.vector ? inferVectorDim(options.vector, options.elementType, options.dim) : undefined;
    const hits = (await this.call(
      {
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
      },
      options.signal
    )) as WireRecord[];
    return mapHybridHits(hits ?? []);
  }

  // TimeSeries
  async tsAppendPoints(seriesId: string, points: TimeSeriesPoint[], resolutionMs?: number, signal?: AbortSignal): Promise<void> {
    await this.call({ type: "ts_append_points", series_id: seriesId, points, resolution_ms: resolutionMs }, signal);
  }
  async tsQueryRange(seriesId: string, startTime: number, endTime: number, resolutionMs?: number, signal?: AbortSignal): Promise<TimeSeriesPoint[]> {
    return mapTsPoints(
      ((await this.call(
        {
          type: "ts_query_range",
          series_id: seriesId,
          start_time: BigInt(Math.trunc(startTime)),
          end_time: BigInt(Math.trunc(endTime)),
          resolution_ms: resolutionMs != null ? BigInt(Math.trunc(resolutionMs)) : undefined,
        },
        signal
      )) as WireRecord[]) ?? []
    );
  }
  async tsQueryAuto(seriesId: string, startTime: number, endTime: number, maxPoints: number, signal?: AbortSignal): Promise<TimeSeriesPoint[]> {
    return mapTsPoints(
      ((await this.call(
        {
          type: "ts_query_auto",
          series_id: seriesId,
          start_time: BigInt(Math.trunc(startTime)),
          end_time: BigInt(Math.trunc(endTime)),
          max_points: maxPoints,
        },
        signal
      )) as WireRecord[]) ?? []
    );
  }
  async tsQueryStats(seriesId: string, startTime: number, endTime: number, resolutionMs?: number, signal?: AbortSignal): Promise<TimeSeriesStats | undefined> {
    const raw = await this.call(
      {
        type: "ts_query_stats",
        series_id: seriesId,
        start_time: BigInt(Math.trunc(startTime)),
        end_time: BigInt(Math.trunc(endTime)),
        resolution_ms: resolutionMs != null ? BigInt(Math.trunc(resolutionMs)) : undefined,
      },
      signal
    );
    return raw ? mapTsStats(raw as WireRecord) : undefined;
  }
  async tsSetPolicy(policy: TimeSeriesLifecyclePolicy, signal?: AbortSignal): Promise<void> {
    await this.call({ type: "ts_set_policy", policy: toWireTsPolicy(policy) }, signal);
  }
  async tsGetPolicy(seriesPattern: string, signal?: AbortSignal): Promise<TimeSeriesLifecyclePolicy | undefined> {
    const raw = await this.call({ type: "ts_get_policy", series_pattern: seriesPattern }, signal);
    return raw ? mapTsPolicy(raw as WireRecord) : undefined;
  }
  async tsListPolicies(signal?: AbortSignal): Promise<TimeSeriesLifecyclePolicy[]> {
    return (((await this.call({ type: "ts_list_policies" }, signal)) as WireRecord[]) ?? []).map(mapTsPolicy);
  }
  async tsDeletePolicy(seriesPattern: string, signal?: AbortSignal): Promise<boolean> {
    return Boolean(await this.call({ type: "ts_delete_policy", series_pattern: seriesPattern }, signal));
  }
  async tsApplyRetention(seriesId?: string, policy?: TimeSeriesLifecyclePolicy, nowMs?: number, signal?: AbortSignal): Promise<TimeSeriesRetentionStats> {
    return mapTsRetentionStats(
      (await this.call(
        {
          type: "ts_apply_retention",
          series_id: seriesId,
          policy: policy ? toWireTsPolicy(policy) : undefined,
          now_ms: nowMs != null ? BigInt(Math.trunc(nowMs)) : undefined,
        },
        signal
      )) as WireRecord
    );
  }

  // Geo & GPS
  async geoAddFence(id: string, name: string, category: string, polygonJson: string, metadata?: string, signal?: AbortSignal): Promise<void> {
    await this.call({ type: "geo_add_fence", id, name, category, polygon_json: polygonJson, metadata }, signal);
  }
  async geoRemoveFence(id: string, signal?: AbortSignal): Promise<boolean> {
    return Boolean(await this.call({ type: "geo_remove_fence", id }, signal));
  }
  async geoContainsPoint(polygonJson: string, lon: number, lat: number, signal?: AbortSignal): Promise<boolean> {
    return Boolean(await this.call({ type: "geo_contains_point", polygon_json: polygonJson, lon, lat }, signal));
  }
  async geoFindFencesForPoint(lon: number, lat: number, signal?: AbortSignal): Promise<GeofenceMatch[]> {
    return mapGeofenceMatches(((await this.call({ type: "geo_find_fences_for_point", lon, lat }, signal)) as WireRecord[]) ?? []);
  }
  async geoRecordGpsPoints(trackId: string, points: GpsPoint[], signal?: AbortSignal): Promise<GeofenceMatch[]> {
    return mapGeofenceMatches(((await this.call({ type: "geo_record_gps_points", track_id: trackId, points }, signal)) as WireRecord[]) ?? []);
  }
  async geoGetTrackPoints(trackId: string, startTime?: number, endTime?: number, signal?: AbortSignal): Promise<GpsPoint[]> {
    return mapGpsPoints(
      ((await this.call(
        {
          type: "geo_get_track_points",
          track_id: trackId,
          start_time: startTime != null ? BigInt(Math.trunc(startTime)) : undefined,
          end_time: endTime != null ? BigInt(Math.trunc(endTime)) : undefined,
        },
        signal
      )) as WireRecord[]) ?? []
    );
  }
  async geoTrackDistance(trackId: string, signal?: AbortSignal): Promise<number> {
    return Number(await this.call({ type: "geo_track_distance", track_id: trackId }, signal));
  }

  // CRDT
  async crdtCounterGet(space: string, id: string, signal?: AbortSignal): Promise<number> {
    return Number(await this.call({ type: "crdt_counter_get", space, id }, signal));
  }
  async crdtCounterState(space: string, id: string, signal?: AbortSignal): Promise<CrdtCounterState> {
    return mapCrdtCounterState((await this.call({ type: "crdt_counter_state", space, id }, signal)) as WireRecord);
  }
  async crdtCounterMerge(space: string, id: string, remote: CrdtCounterState, signal?: AbortSignal): Promise<number> {
    return Number(await this.call({ type: "crdt_counter_merge", space, id, remote }, signal));
  }
  async crdtSetRead(space: string, id: string, signal?: AbortSignal): Promise<string[]> {
    return ((await this.call({ type: "crdt_set_get", space, id }, signal)) as string[]) ?? [];
  }
  async crdtSetContains(space: string, id: string, element: string, signal?: AbortSignal): Promise<boolean> {
    return Boolean(await this.call({ type: "crdt_set_contains", space, id, element }, signal));
  }
  async crdtSetState(space: string, id: string, signal?: AbortSignal): Promise<CrdtOrSetState> {
    return mapCrdtOrSetState((await this.call({ type: "crdt_set_state", space, id }, signal)) as WireRecord);
  }
  async crdtSetPrune(space: string, id: string, cutoffMs: number, signal?: AbortSignal): Promise<number> {
    return Number(await this.call({ type: "crdt_set_prune", space, id, cutoff_ms: BigInt(Math.trunc(cutoffMs)) }, signal));
  }
  async crdtSetMerge(space: string, id: string, remote: CrdtOrSetState, signal?: AbortSignal): Promise<void> {
    await this.call({ type: "crdt_set_merge", space, id, remote }, signal);
  }
  async crdtJsonGet<T = JsonValue>(space: string, id: string, signal?: AbortSignal): Promise<T | undefined> {
    const raw = await this.call({ type: "crdt_json_get", space, id }, signal);
    return raw == null ? undefined : (jsonNumbers(raw) as T);
  }
  async crdtJsonState(space: string, id: string, signal?: AbortSignal): Promise<LwwJsonDocumentState | undefined> {
    const raw = await this.call({ type: "crdt_json_state", space, id }, signal);
    return raw ? mapLwwJsonState(raw as WireRecord) : undefined;
  }
  async crdtJsonMerge(space: string, id: string, remote: LwwJsonDocumentState, signal?: AbortSignal): Promise<bigint> {
    return readVersion(await this.call({ type: "crdt_json_merge", space, id, remote: toWireLwwJsonState(remote) }, signal));
  }

  // HLC & Conflicts
  async hlcNext(signal?: AbortSignal): Promise<string> {
    return formatHlcRequired(await this.call({ type: "hlc_next" }, signal));
  }
  async hlcObserve(remote: string, signal?: AbortSignal): Promise<string> {
    return formatHlcRequired(await this.call({ type: "hlc_observe", remote }, signal));
  }
  async listConflicts(space?: string, signal?: AbortSignal): Promise<ConflictRecord[]> {
    return (((await this.call({ type: "list_conflicts", space }, signal)) as WireRecord[]) ?? []).map(mapConflictRecord);
  }
  async resolveConflict(conflictId: number, resolvedValue: JsonValue, signal?: AbortSignal): Promise<void> {
    await this.call({ type: "resolve_conflict", conflict_id: conflictId, resolved_value: resolvedValue }, signal);
  }

  // Undo / Redo
  async undo(signal?: AbortSignal): Promise<UndoResult | undefined> {
    const raw = await this.call({ type: "undo" }, signal);
    return raw ? mapUndoResult(raw as WireRecord) : undefined;
  }
  async redo(signal?: AbortSignal): Promise<RedoResult | undefined> {
    const raw = await this.call({ type: "redo" }, signal);
    return raw ? mapRedoResult(raw as WireRecord) : undefined;
  }
  async undoHistory(signal?: AbortSignal): Promise<UndoEntry[]> {
    return (((await this.call({ type: "undo_history" }, signal)) as WireRecord[]) ?? []).map(mapUndoEntry);
  }

  // Streams & IVM
  async watch(
    namespace: string | undefined,
    callback: (event: DatabaseEvent) => void,
    options?: { signal?: AbortSignal }
  ): Promise<WatchSubscription> {
    options?.signal?.throwIfAborted();
    const socket = await this.openStreamSocket({ type: "subscribe", namespace }, options?.signal);
    let buffer = Buffer.alloc(0);

    socket.on("data", (chunk: Buffer) => {
      buffer = Buffer.concat([buffer, chunk]);
      while (buffer.length >= 4) {
        const length = buffer.readUInt32BE(0);
        if (buffer.length < 4 + length) break;
        const frameData = buffer.subarray(4, 4 + length);
        buffer = buffer.subarray(4 + length);
        try {
          const wire = decode(frameData, { useBigInt64: true }) as WireResponse;
          if (wire.result.Err) break;
          const rawVal = wire.result.Ok as WireRecord | undefined;
          if (rawVal && typeof rawVal === "object") {
            const payload = rawVal.type === "event" && rawVal.value ? rawVal.value : rawVal.type !== "ack" ? rawVal : undefined;
            if (payload) {
              const event = mapDatabaseEvent(payload);
              if (event) callback(event);
            }
          }
        } catch {}
      }
    });

    return makeSubscription<WatchSubscription>(() => {
      this.streamSockets.delete(socket);
      socket.destroy();
    }, options?.signal);
  }

  async liveQuery<T = Record<string, unknown>>(
    sql: string,
    params: JsonValue[] = [],
    callback: (diff: LiveQueryDiff<T>) => void,
    options?: { signal?: AbortSignal }
  ): Promise<LiveQuerySubscription> {
    options?.signal?.throwIfAborted();
    const socket = await this.openStreamSocket({ type: "live_query_subscribe", sql, params }, options?.signal);
    const ready = Promise.withResolvers<void>();
    let settled = false;
    let acked = false;
    let initialReceived = false;
    let buffer = Buffer.alloc(0);

    const fail = (err: unknown) => {
      if (!settled) {
        settled = true;
        this.streamSockets.delete(socket);
        socket.destroy();
        ready.reject(err);
      }
    };

    const onAbort = () => fail(options?.signal?.reason ?? new DOMException("The operation was aborted", "AbortError"));
    options?.signal?.addEventListener("abort", onAbort, { once: true });

    socket.once("error", (err) => fail(err));
    socket.once("close", () => fail(new Error("liveQuery stream closed before initialization")));

    socket.on("data", (chunk: Buffer) => {
      buffer = Buffer.concat([buffer, chunk]);
      while (buffer.length >= 4) {
        const length = buffer.readUInt32BE(0);
        if (buffer.length < 4 + length) break;
        const frameData = buffer.subarray(4, 4 + length);
        buffer = buffer.subarray(4 + length);
        try {
          const wire = decode(frameData, { useBigInt64: true }) as WireResponse;
          if (wire.result.Err) {
            const error = wire.result.Err;
            fail(new LiliaError({ ...error, request_id: uuidString(error.request_id) }));
            break;
          }
          const rawVal = wire.result.Ok as WireRecord | undefined;
          if (!rawVal || typeof rawVal !== "object") continue;
          if (rawVal.type === "ack") {
            acked = true;
          } else if (rawVal.type === "live_query_diff" && rawVal.value && typeof rawVal.value === "object") {
            const diff = mapLiveQueryDiff<T>(rawVal.value as WireRecord);
            callback(diff);
            if (diff.isInitial) initialReceived = true;
          }
          if (!settled && acked && initialReceived) {
            settled = true;
            ready.resolve();
          }
        } catch (err) {
          fail(err);
          break;
        }
      }
    });

    try {
      await ready.promise;
    } finally {
      options?.signal?.removeEventListener("abort", onAbort);
    }

    return makeSubscription<LiveQuerySubscription>(() => {
      this.streamSockets.delete(socket);
      socket.destroy();
    }, options?.signal);
  }

  async createMaterializedView(name: string, query: string, signal?: AbortSignal): Promise<void> {
    await this.call({ type: "create_materialized_view", name, query }, signal);
  }
  async dropMaterializedView(name: string, signal?: AbortSignal): Promise<void> {
    await this.call({ type: "drop_materialized_view", name }, signal);
  }
  async refreshMaterializedView(name: string, signal?: AbortSignal): Promise<void> {
    await this.call({ type: "refresh_materialized_view", name }, signal);
  }
  async listMaterializedViews(signal?: AbortSignal): Promise<MaterializedViewInfo[]> {
    return mapMaterializedViews(((await this.call({ type: "list_materialized_views" }, signal)) as WireRecord[]) ?? []);
  }

  // Diagnostics & Lifecycle
  async quickCheck(signal?: AbortSignal): Promise<boolean> {
    return Boolean(await this.call({ type: "quick_check" }, signal));
  }
  async integrityCheck(signal?: AbortSignal): Promise<boolean> {
    return Boolean(await this.call({ type: "integrity_check" }, signal));
  }
  async checkpoint(signal?: AbortSignal): Promise<void> {
    await this.call({ type: "checkpoint" }, signal);
  }
  async checkpointStats(signal?: AbortSignal): Promise<CheckpointStats> {
    return mapCheckpointStats((await this.call({ type: "checkpoint_stats" }, signal)) as WireRecord);
  }
  async storageStats(signal?: AbortSignal): Promise<TableStorageStat[]> {
    return mapStorageStats(((await this.call({ type: "storage_stats" }, signal)) as WireRecord[]) ?? []);
  }
  async backup(destination: string, signal?: AbortSignal): Promise<void> {
    await this.call({ type: "backup", destination }, signal);
  }

  close(_timeoutMs?: number): Promise<void> {
    for (const sock of this.streamSockets) {
      sock.destroy();
    }
    this.streamSockets.clear();
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

  private async openStreamSocket(operation: WireRecord, signal?: AbortSignal): Promise<Socket> {
    signal?.throwIfAborted();
    const socket = connect(this.endpoint);
    const connectResolvers = Promise.withResolvers<void>();
    socket.once("connect", () => connectResolvers.resolve());
    socket.once("error", (err) => connectResolvers.reject(err));
    await connectResolvers.promise;
    if (signal?.aborted) {
      socket.destroy();
      signal.throwIfAborted();
    }
    this.streamSockets.add(socket);
    socket.once("close", () => this.streamSockets.delete(socket));

    const payload = encode({ id: getUuid(), token: this.token, operation }, MSGPACK_ENCODE_OPTS);
    const header = Buffer.allocUnsafe(4);
    header.writeUInt32BE(payload.byteLength);
    socket.write(Buffer.concat([header, Buffer.from(payload)]));
    return socket;
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
        } catch {}
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

  private async call(operation: WireRecord, signal?: AbortSignal): Promise<unknown> {
    signal?.throwIfAborted();
    const socket = await this.getSocket();
    signal?.throwIfAborted();

    const id = getUuid();
    const payload = encode({ id, token: this.token, deadline_ms: 5_000, operation }, MSGPACK_ENCODE_OPTS);
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
  const hex = Array.from(value, (b) => b.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

function unwrapValue(value: unknown): unknown {
  if (!value || typeof value !== "object") return value;
  const record = value as WireRecord;
  return "value" in record ? record.value : value;
}

function mapKv(value: unknown): KvEntry | undefined {
  if (!value) return undefined;
  const item = value as WireRecord;
  return {
    namespace: String(item.namespace),
    key: item.key as Uint8Array,
    value: item.value as Uint8Array,
    version: readVersion(item.version),
    expiresAtMs: item.expires_at_ms == null ? undefined : Number(item.expires_at_ms),
  };
}
