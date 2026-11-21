// The calls of a database, each defined once over a runner: the asynchronous API runs them on
// the transport's workers and returns promises, the synchronous one runs them in place.

import {
  mapCounterState,
  mapCypherResult,
  mapFtsFuzzyHits,
  mapFtsHits,
  mapFtsPhoneticHits,
  mapFtsSuggestions,
  mapGeofenceMatches,
  mapGpsPoints,
  mapGraphEdges,
  mapGraphNode,
  mapHybridHits,
  mapImpact,
  mapJsonEntry,
  mapJsonIndexes,
  mapJsonRevisions,
  mapKvEntry,
  mapLwwDocument,
  mapMutations,
  mapNodeScores,
  mapPoints,
  mapPolicy,
  mapRetentionStats,
  mapSeriesStats,
  mapSetState,
  mapSqlResult,
  mapStorageStats,
  mapUndoStep,
  mapVectorHits,
  mapVectorIndexInfo,
  mapWeightedPath,
  mapWeightedPaths,
  toWireBatchOperation,
  toWireCounterState,
  toWireDirection,
  toWireHnswConfig,
  toWireLwwDocument,
  toWirePageRankConfig,
  toWirePolicy,
} from "./mappers.js";
import { vectorDim, vectorToBytes } from "./math.js";
import type {
  BatchOperation,
  CentralityType,
  ComponentType,
  CreateIndexOptions,
  CrdtCounterState,
  CrdtOrSetState,
  FtsSearchOptions,
  GeoAddFenceOptions,
  GeoTrackQueryOptions,
  GpsPoint,
  GraphAddEdgeOptions,
  GraphCentralityOptions,
  GraphDirection,
  GraphFindCyclesOptions,
  GraphListEdgesOptions,
  GraphListNodesOptions,
  GraphSetNodeOptions,
  GraphTraversalOptions,
  HnswConfig,
  JsonDeleteOptions,
  JsonFindOptions,
  JsonGetOptions,
  JsonPutOptions,
  JsonScanOptions,
  JsonScanPrefixOptions,
  JsonValue,
  KvDeleteOptions,
  KvGetOptions,
  KvScanOptions,
  KvSetOptions,
  LwwJsonDocumentState,
  PageRankConfig,
  SqlFunction,
  SqlQueryOptions,
  SqlQueryResult,
  SyncSqlFunction,
  TimeSeriesLifecyclePolicy,
  TimeSeriesPoint,
  VectorElementType,
  VectorMetric,
  VectorSearchOptions,
} from "./types.js";
import { expect, type Operation, type ResponseOf, type ResponseType, type ResponseValue, type WireElementType } from "./wire.js";

export type Mode = "async" | "sync";
/** A result in mode `M`: a promise of it, or itself. */
export type Out<M extends Mode, T> = M extends "async" ? Promise<T> : T;
/** Runs `operation` and maps its response; `signal` stops the wait of an asynchronous run. */
export type Runner<M extends Mode> = <T>(
  operation: Operation,
  map: (response: ResponseValue) => T,
  signal?: AbortSignal
) => Out<M, T>;

type Signal = { signal?: AbortSignal };
type VectorInput = Float32Array | number[] | Uint8Array;

/** Results count of a search when the caller gives none. */
const SEARCH_LIMIT = 10;
/** Records of a scan when the caller gives no limit. */
const SCAN_LIMIT = 100;

const encoder = new TextEncoder();
const bytes = (value: Uint8Array | string): Uint8Array => (typeof value === "string" ? encoder.encode(value) : value);

/** `fieldOrPath` as an index name and path: a field `a.b` indexes `$.a.b`. */
function indexDefinition(fieldOrPath: string, optionsOrPath?: string | CreateIndexOptions): { name: string; path: string; filter?: string } {
  const isPath = (value: string) => value.startsWith("$") || value.startsWith("/") || value.includes("(");
  const defaultPath = isPath(fieldOrPath) ? fieldOrPath : `$.${fieldOrPath}`;
  if (typeof optionsOrPath === "string") return { name: fieldOrPath, path: optionsOrPath };
  return {
    name: optionsOrPath?.name ?? fieldOrPath,
    path: optionsOrPath?.path ?? defaultPath,
    filter: optionsOrPath?.filter,
  };
}

/** A query vector: its bytes, dimension and element type. */
function queryVector(vector: VectorInput, elementType?: VectorElementType, dim?: number): { data: Uint8Array; dim: number; element_type: WireElementType } {
  return { data: vectorToBytes(vector, elementType), dim: vectorDim(vector, elementType, dim), element_type: elementType ?? "float32" };
}

/** A tagged SQL template as a query with `$n` parameters. */
function taggedQuery(strings: TemplateStringsArray, values: JsonValue[]): { sql: string; params: JsonValue[] } {
  const sql = strings.reduce((query, part, i) => query + part + (i < values.length ? `$${i + 1}` : ""), "");
  return { sql, params: values };
}

/** The calls of a database over `run`. */
// The object is the API; splitting it would only scatter one table of calls.
export function buildApi<M extends Mode>(run: Runner<M>) {
  const call = <K extends ResponseType, T>(operation: Operation, type: K, map: (value: ResponseOf<K>) => T, signal?: AbortSignal): Out<M, T> =>
    run(operation, (response) => map(expect(response, type)), signal);
  const ack = (operation: Operation, signal?: AbortSignal): Out<M, void> => call(operation, "ack", () => undefined, signal);
  const batch = (operations: readonly BatchOperation[], signal?: AbortSignal) =>
    call({ type: "batch", operations: operations.map(toWireBatchOperation) }, "mutations", mapMutations, signal);
  const write = (operation: BatchOperation, signal?: AbortSignal) =>
    call({ type: "batch", operations: [toWireBatchOperation(operation)] }, "mutations", (results) => mapMutations(results)[0]!, signal);
  const document = <T>(operation: Operation, signal?: AbortSignal) =>
    call(operation, "json", (entry) => (entry ? mapJsonEntry<T>(entry) : undefined), signal);
  const documents = <T>(operation: Operation, signal?: AbortSignal) =>
    call(operation, "json_page", (entries) => entries.map((entry) => mapJsonEntry<T>(entry)), signal);
  const path = (operation: Operation, signal?: AbortSignal) =>
    call(operation, "weighted_path", (found) => (found ? mapWeightedPath(found) : undefined), signal);
  const vectorHits = (operation: Operation, signal?: AbortSignal) => call(operation, "vector_hits", mapVectorHits, signal);
  const flag = (operation: Operation, signal?: AbortSignal) => call(operation, "bool", (value) => value, signal);

  const sqlRaw = <T>(sql: string, params: JsonValue[] = [], options: SqlQueryOptions = {}) =>
    call({ type: "sql_query", sql, params }, "sql", (result) => mapSqlResult<T>(result), options.signal);
  const sqlRecords = <T>(sql: string, params: JsonValue[] = [], options: SqlQueryOptions = {}) =>
    call({ type: "sql_query", sql, params }, "sql", (result) => mapSqlResult<T>(result).records, options.signal);
  const sql = Object.assign(
    <T = Record<string, unknown>>(strings: TemplateStringsArray, ...values: JsonValue[]) => {
      const query = taggedQuery(strings, values);
      return sqlRecords<T>(query.sql, query.params);
    },
    {
      query: <T = Record<string, unknown>>(query: string, params?: JsonValue[], options?: SqlQueryOptions) => sqlRecords<T>(query, params, options),
      raw: <T = Record<string, unknown>>(query: string, params?: JsonValue[], options?: SqlQueryOptions): Out<M, SqlQueryResult<T>> =>
        sqlRaw<T>(query, params, options),
    }
  ) as unknown as M extends "async" ? SqlFunction : SyncSqlFunction;

  return {
    kv: {
      get: (namespace: string, key: Uint8Array | string, options: KvGetOptions = {}) =>
        call({ type: "kv_get", namespace, key: bytes(key) }, "kv", (entry) => (entry ? mapKvEntry(entry) : undefined), options.signal),
      set: (namespace: string, key: Uint8Array | string, value: Uint8Array | string, options: KvSetOptions = {}) =>
        write(
          { model: "kv_set", namespace, key: bytes(key), value: bytes(value), ifVersion: options.ifVersion, expiresAtMs: options.expiresAtMs },
          options.signal
        ),
      delete: (namespace: string, key: Uint8Array | string, options: KvDeleteOptions = {}) =>
        write({ model: "kv_delete", namespace, key: bytes(key), ifVersion: options.ifVersion }, options.signal),
      scan: (namespace: string, options: KvScanOptions = {}) =>
        call(
          { type: "kv_scan", namespace, after: options.after === undefined ? undefined : bytes(options.after), limit: options.limit ?? SCAN_LIMIT },
          "kv_page",
          (entries) => entries.map(mapKvEntry),
          options.signal
        ),
    },
    json: {
      get: <T = JsonValue>(space: string, id: string, options: JsonGetOptions = {}) =>
        document<T>({ type: "json_get", space, id }, options.signal),
      extract: <T = JsonValue>(space: string, id: string, path: string, options: JsonGetOptions = {}) =>
        call({ type: "json_extract", space, id, path }, "json_extracted", (value) => (value === null ? undefined : (value as T)), options.signal),
      put: <T = JsonValue>(space: string, id: string, value: T, options: JsonPutOptions = {}) =>
        write({ model: "json_put", space, id, value: value as JsonValue, ifVersion: options.ifVersion }, options.signal),
      delete: (space: string, id: string, options: JsonDeleteOptions = {}) =>
        write({ model: "json_delete", space, id, ifVersion: options.ifVersion }, options.signal),
      scan: <T = JsonValue>(space: string, options: JsonScanOptions = {}) =>
        documents<T>({ type: "json_scan", space, after: options.after, limit: options.limit ?? SCAN_LIMIT }, options.signal),
      scanPrefix: <T = JsonValue>(space: string, prefix: string, options: JsonScanPrefixOptions = {}) =>
        documents<T>({ type: "json_scan_prefix", space, prefix, after_id: options.afterId, limit: options.limit ?? SCAN_LIMIT }, options.signal),
      find: <T = JsonValue>(space: string, query: Record<string, JsonValue>, options: JsonFindOptions = {}) =>
        documents<T>({ type: "json_find", space, query, after: options.after, limit: options.limit ?? SCAN_LIMIT }, options.signal),
      findByIndex: <T = JsonValue>(space: string, indexName: string, value: string | number | boolean, options: JsonFindOptions = {}) =>
        documents<T>(
          { type: "json_scan_by_index", space, index_name: indexName, value: String(value), after: options.after, limit: options.limit ?? SCAN_LIMIT },
          options.signal
        ),
      createIndex: (space: string, fieldOrPath: string, optionsOrPath?: string | CreateIndexOptions, options: Signal = {}) => {
        const { name, path, filter } = indexDefinition(fieldOrPath, optionsOrPath);
        return ack({ type: "json_create_index", space, name, path, filter }, options.signal);
      },
      dropIndex: (space: string, indexName: string, options: Signal = {}) =>
        flag({ type: "json_drop_index", space, name: indexName }, options.signal),
      listIndexes: (space: string, options: Signal = {}) =>
        call({ type: "json_list_indexes", space }, "json_indexes", mapJsonIndexes, options.signal),
      getAtVersion: <T = JsonValue>(space: string, id: string, version: bigint, options: JsonGetOptions = {}) =>
        document<T>({ type: "json_get_at_version", space, id, version }, options.signal),
      getAtTimestamp: <T = JsonValue>(space: string, id: string, timestampMs: number, options: JsonGetOptions = {}) =>
        document<T>({ type: "json_get_at_timestamp", space, id, timestamp_ms: timestampMs }, options.signal),
      history: <T = JsonValue>(space: string, id: string, options: JsonGetOptions = {}) =>
        call({ type: "json_history", space, id }, "json_revisions", (revisions) => mapJsonRevisions<T>(revisions), options.signal),
      diff: (space: string, id: string, fromVersion: bigint, toVersion: bigint, options: JsonGetOptions = {}) =>
        call(
          { type: "json_diff", space, id, from_version: fromVersion, to_version: toVersion },
          "json_value",
          (diff) => diff,
          options.signal
        ),
    },
    graph: {
      setNode: (graph: string, id: string, kind: string, metadata?: JsonValue, options: GraphSetNodeOptions = {}) =>
        write({ model: "graph_set_node", graph, id, kind, metadata, docSpace: options.docSpace, docId: options.docId }, options.signal),
      getNode: (graph: string, id: string, options: Signal = {}) =>
        call({ type: "graph_get_node", graph, id }, "graph_node", (node) => (node ? mapGraphNode(node) : undefined), options.signal),
      getNodeDocument: <T = JsonValue>(graph: string, id: string, options: Signal = {}) =>
        call({ type: "graph_get_node_document", graph, id }, "json_extracted", (value) => (value === null ? undefined : (value as T)), options.signal),
      deleteNode: (graph: string, id: string, options: Signal = {}) => flag({ type: "graph_delete_node", graph, id }, options.signal),
      listNodes: (graph: string, options: GraphListNodesOptions = {}) =>
        call({ type: "graph_list_nodes", graph, kind: options.kind, limit: options.limit ?? 0 }, "graph_nodes", (nodes) => nodes.map(mapGraphNode), options.signal),
      addEdge: (graph: string, source: string, target: string, kind: string, options: GraphAddEdgeOptions = {}) =>
        write({ model: "graph_add_edge", graph, source, target, kind, weight: options.weight }, options.signal),
      removeEdge: (graph: string, source: string, target: string, kind: string, options: Signal = {}) =>
        flag({ type: "graph_remove_edge", graph, source, target, kind }, options.signal),
      listEdges: (graph: string, options: GraphListEdgesOptions = {}) =>
        call(
          { type: "graph_list_edges", graph, source: options.source, target: options.target, kind: options.kind, limit: options.limit ?? 0 },
          "graph_edges",
          mapGraphEdges,
          options.signal
        ),
      neighbors: (graph: string, node: string, options: { direction?: GraphDirection; kind?: string } & Signal = {}) =>
        call({ type: "graph_neighbors", graph, node, direction: toWireDirection(options.direction), kind: options.kind }, "ids", (ids) => ids, options.signal),
      reachability: (graph: string, startNodes: string[], options: { direction?: GraphDirection } & Signal = {}) =>
        call(
          { type: "graph_reachability", graph, start_nodes: startNodes, direction: toWireDirection(options.direction) },
          "ids",
          (ids) => ids,
          options.signal
        ),
      impactAnalysis: (graph: string, changedNodes: string[], options: Signal = {}) =>
        call({ type: "graph_impact_analysis", graph, changed_nodes: changedNodes }, "graph_impact", mapImpact, options.signal),
      shortestPath: (graph: string, start: string, target: string, options: GraphTraversalOptions = {}) =>
        path(
          { type: "graph_shortest_path", graph, start, target, direction: toWireDirection(options.direction), kind_filter: options.kindFilter },
          options.signal
        ),
      dijkstra: (graph: string, start: string, target?: string, options: GraphTraversalOptions = {}) =>
        path({ type: "graph_dijkstra", graph, start, target, direction: toWireDirection(options.direction), kind_filter: options.kindFilter }, options.signal),
      allShortestPaths: (graph: string, start: string, options: GraphTraversalOptions = {}) =>
        call(
          { type: "graph_all_shortest_paths", graph, start, direction: toWireDirection(options.direction), kind_filter: options.kindFilter },
          "weighted_path_map",
          mapWeightedPaths,
          options.signal
        ),
      astarSpatial: (graph: string, start: string, target: string, options: GraphTraversalOptions = {}) =>
        path(
          { type: "graph_astar_spatial", graph, start, target, direction: toWireDirection(options.direction), kind_filter: options.kindFilter },
          options.signal
        ),
      pagerank: (graph: string, config: PageRankConfig & Signal = {}) => {
        const { signal, ...rest } = config;
        return call({ type: "graph_page_rank", graph, config: toWirePageRankConfig(rest) }, "node_scores", mapNodeScores, signal);
      },
      centrality: (graph: string, centrality: CentralityType, options: GraphCentralityOptions = {}) =>
        call(
          {
            type: "graph_centrality",
            graph,
            centrality,
            direction: toWireDirection(options.direction),
            kind_filter: options.kindFilter,
            normalized: options.normalized ?? false,
          },
          "node_scores",
          mapNodeScores,
          options.signal
        ),
      hasCycle: (graph: string, options: GraphTraversalOptions = {}) =>
        flag({ type: "graph_has_cycle", graph, direction: toWireDirection(options.direction), kind_filter: options.kindFilter }, options.signal),
      findCycles: (graph: string, options: GraphFindCyclesOptions = {}) =>
        call(
          { type: "graph_find_cycles", graph, max_cycles: options.maxCycles ?? 100, kind_filter: options.kindFilter },
          "node_groups",
          (cycles) => cycles,
          options.signal
        ),
      connectedComponents: (graph: string, componentType: ComponentType, options: { kindFilter?: string } & Signal = {}) =>
        call(
          { type: "graph_connected_components", graph, component_type: componentType, kind_filter: options.kindFilter },
          "node_groups",
          (components) => components,
          options.signal
        ),
      cypher: <T = Record<string, JsonValue>>(graph: string, query: string, options: Signal = {}) =>
        call({ type: "graph_cypher", graph, query }, "cypher_result", (result) => mapCypherResult<T>(result), options.signal),
    },
    fts: {
      index: (collection: string, docId: string, content: string, options: Signal = {}) =>
        write({ model: "fts_index", collection, docId, content }, options.signal),
      delete: (collection: string, docId: string, options: Signal = {}) => flag({ type: "fts_delete", collection, doc_id: docId }, options.signal),
      search: (collection: string, query: string, options: FtsSearchOptions = {}) =>
        call({ type: "fts_search", collection, query, limit: options.limit ?? SEARCH_LIMIT }, "fts_hits", mapFtsHits, options.signal),
      suggest: (collection: string, prefix: string, options: FtsSearchOptions = {}) =>
        call(
          { type: "fts_suggest", collection, prefix, max_distance: options.maxDistance ?? 1, limit: options.limit ?? SEARCH_LIMIT },
          "fts_suggestions",
          mapFtsSuggestions,
          options.signal
        ),
      fuzzySearch: (collection: string, query: string, options: FtsSearchOptions = {}) =>
        call(
          { type: "fts_fuzzy_search", collection, query, max_distance: options.maxDistance ?? 1, limit: options.limit ?? SEARCH_LIMIT },
          "fts_fuzzy_hits",
          mapFtsFuzzyHits,
          options.signal
        ),
      phoneticSearch: (collection: string, query: string, options: FtsSearchOptions = {}) =>
        call({ type: "fts_phonetic_search", collection, query, limit: options.limit ?? SEARCH_LIMIT }, "fts_phonetic_hits", mapFtsPhoneticHits, options.signal),
      loadDictionary: (collection: string, words: readonly string[], options: Signal = {}) =>
        ack({ type: "fts_load_dictionary", collection, words: [...words] }, options.signal),
    },
    vector: {
      set: (collection: string, id: string, vector: VectorInput, options: { dim?: number; elementType?: VectorElementType } & Signal = {}) =>
        write(
          { model: "vector_set", collection, id, dim: vectorDim(vector, options.elementType, options.dim), elementType: options.elementType, vector },
          options.signal
        ),
      get: (collection: string, id: string, options: Signal = {}) =>
        call({ type: "vector_get", collection, id }, "vector", (stored) => (stored ? new Uint8Array(stored.data) : null), options.signal),
      delete: (collection: string, id: string, options: Signal = {}) => write({ model: "vector_delete", collection, id }, options.signal),
      search: (collection: string, query: VectorInput, options: VectorSearchOptions = {}) => {
        const vector = queryVector(query, options.elementType, options.dim);
        return vectorHits(
          {
            type: "vector_search",
            collection,
            vector: vector.data,
            dim: vector.dim,
            element_type: vector.element_type,
            metric: options.metric ?? "cosine",
            limit: options.limit ?? SEARCH_LIMIT,
          },
          options.signal
        );
      },
      createIndex: (collection: string, config: HnswConfig, options: Signal = {}) =>
        ack({ type: "vector_create_index", collection, config: toWireHnswConfig(config) }, options.signal),
      getIndexInfo: (collection: string, options: Signal = {}) =>
        call({ type: "vector_get_index_info", collection }, "vector_index_info", (info) => (info ? mapVectorIndexInfo(info) : undefined), options.signal),
      dropIndex: (collection: string, options: Signal = {}) => flag({ type: "vector_drop_index", collection }, options.signal),
      searchFiltered: (collection: string, query: VectorInput, candidateIds: string[], options: VectorSearchOptions = {}) => {
        const vector = queryVector(query, options.elementType, options.dim);
        return vectorHits(
          {
            type: "vector_search_filtered",
            collection,
            query_vector: vector.data,
            dim: vector.dim,
            element_type: vector.element_type,
            metric: options.metric ?? "cosine",
            candidate_ids: candidateIds,
            limit: options.limit ?? SEARCH_LIMIT,
          },
          options.signal
        );
      },
      searchInGraphNeighbors: (
        collection: string,
        graph: string,
        startNode: string,
        query: VectorInput,
        options: VectorSearchOptions & { direction?: GraphDirection } = {}
      ) => {
        const vector = queryVector(query, options.elementType, options.dim);
        return vectorHits(
          {
            type: "vector_search_in_graph_neighbors",
            collection,
            graph,
            start_node: startNode,
            direction: toWireDirection(options.direction),
            query_vector: vector.data,
            dim: vector.dim,
            element_type: vector.element_type,
            metric: options.metric ?? "cosine",
            limit: options.limit ?? SEARCH_LIMIT,
          },
          options.signal
        );
      },
      searchByJsonIndex: (
        collection: string,
        space: string,
        indexName: string,
        indexValue: string | number | boolean,
        query: VectorInput,
        options: VectorSearchOptions = {}
      ) => {
        const vector = queryVector(query, options.elementType, options.dim);
        return vectorHits(
          {
            type: "vector_search_by_json_index",
            collection,
            space,
            index_name: indexName,
            index_value: String(indexValue),
            query_vector: vector.data,
            dim: vector.dim,
            element_type: vector.element_type,
            metric: options.metric ?? "cosine",
            limit: options.limit ?? SEARCH_LIMIT,
          },
          options.signal
        );
      },
    },
    hybrid: {
      search: (
        collection: string,
        options: {
          query?: string;
          vector?: VectorInput;
          dim?: number;
          elementType?: VectorElementType;
          metric?: VectorMetric;
          rrfK?: number;
          ftsWeight?: number;
          vectorWeight?: number;
          limit?: number;
        } & Signal = {}
      ) => {
        const vector = options.vector === undefined ? undefined : queryVector(options.vector, options.elementType, options.dim);
        return call(
          {
            type: "hybrid_search",
            collection,
            fts_query: options.query,
            vector: vector?.data,
            dim: vector?.dim,
            element_type: vector?.element_type,
            metric: options.metric,
            rrf_k: options.rrfK ?? 60,
            fts_weight: options.ftsWeight ?? 1,
            vector_weight: options.vectorWeight ?? 1,
            limit: options.limit ?? SEARCH_LIMIT,
          },
          "hybrid_hits",
          mapHybridHits,
          options.signal
        );
      },
    },
    timeseries: {
      appendPoints: (seriesId: string, points: TimeSeriesPoint[], options: { resolutionMs?: number } & Signal = {}) =>
        ack({ type: "ts_append_points", series_id: seriesId, points, resolution_ms: options.resolutionMs }, options.signal),
      queryRange: (seriesId: string, startTime: number, endTime: number, options: { resolutionMs?: number } & Signal = {}) =>
        call(
          { type: "ts_query_range", series_id: seriesId, start_time: startTime, end_time: endTime, resolution_ms: options.resolutionMs },
          "ts_points",
          mapPoints,
          options.signal
        ),
      queryAuto: (seriesId: string, startTime: number, endTime: number, maxPoints: number, options: Signal = {}) =>
        call(
          { type: "ts_query_auto", series_id: seriesId, start_time: startTime, end_time: endTime, max_points: maxPoints },
          "ts_points",
          mapPoints,
          options.signal
        ),
      queryStats: (seriesId: string, startTime: number, endTime: number, options: { resolutionMs?: number } & Signal = {}) =>
        call(
          { type: "ts_query_stats", series_id: seriesId, start_time: startTime, end_time: endTime, resolution_ms: options.resolutionMs },
          "ts_stats",
          (stats) => (stats ? mapSeriesStats(stats) : undefined),
          options.signal
        ),
      setPolicy: (policy: TimeSeriesLifecyclePolicy, options: Signal = {}) =>
        ack({ type: "ts_set_policy", policy: toWirePolicy(policy) }, options.signal),
      getPolicy: (seriesPattern: string, options: Signal = {}) =>
        call({ type: "ts_get_policy", series_pattern: seriesPattern }, "ts_policy", (policy) => (policy ? mapPolicy(policy) : undefined), options.signal),
      listPolicies: (options: Signal = {}) => call({ type: "ts_list_policies" }, "ts_policies", (policies) => policies.map(mapPolicy), options.signal),
      deletePolicy: (seriesPattern: string, options: Signal = {}) =>
        flag({ type: "ts_delete_policy", series_pattern: seriesPattern }, options.signal),
      /** Applies every series' lifecycle policy as of `nowMs` (now by default). */
      applyRetention: (options: { nowMs?: number } & Signal = {}) =>
        call({ type: "ts_apply_retention", now_ms: options.nowMs }, "ts_retention_stats", mapRetentionStats, options.signal),
    },
    geo: {
      addFence: (id: string, name: string, category: string, polygonJson: string, options: GeoAddFenceOptions = {}) =>
        ack({ type: "geo_add_fence", id, name, category, polygon_json: polygonJson, metadata: options.metadata }, options.signal),
      removeFence: (id: string, options: Signal = {}) => flag({ type: "geo_remove_fence", id }, options.signal),
      containsPoint: (polygonJson: string, lon: number, lat: number, options: Signal = {}) =>
        flag({ type: "geo_contains_point", polygon_json: polygonJson, lon, lat }, options.signal),
      findFencesForPoint: (lon: number, lat: number, options: Signal = {}) =>
        call({ type: "geo_find_fences_for_point", lon, lat }, "geofence_matches", mapGeofenceMatches, options.signal),
      recordGpsPoints: (trackId: string, points: GpsPoint[], options: Signal = {}) =>
        call({ type: "geo_record_gps_points", track_id: trackId, points }, "geofence_matches", mapGeofenceMatches, options.signal),
      getTrackPoints: (trackId: string, options: GeoTrackQueryOptions = {}) =>
        call(
          { type: "geo_get_track_points", track_id: trackId, start_time: options.startTime, end_time: options.endTime },
          "gps_points",
          mapGpsPoints,
          options.signal
        ),
      trackDistance: (trackId: string, options: Signal = {}) =>
        call({ type: "geo_track_distance", track_id: trackId }, "f64", (meters) => meters, options.signal),
    },
    crdt: {
      counter: {
        increment: (space: string, id: string, actorId: string, amount: number | bigint = 1, options: Signal = {}) =>
          write({ model: "crdt_pn_counter_increment", space, id, actorId, amount }, options.signal),
        decrement: (space: string, id: string, actorId: string, amount: number | bigint = 1, options: Signal = {}) =>
          write({ model: "crdt_pn_counter_decrement", space, id, actorId, amount }, options.signal),
        get: (space: string, id: string, options: Signal = {}) =>
          call({ type: "crdt_counter_get", space, id }, "crdt_counter", (value) => Number(value), options.signal),
        state: (space: string, id: string, options: Signal = {}) =>
          call({ type: "crdt_counter_state", space, id }, "crdt_counter_state", mapCounterState, options.signal),
        merge: (space: string, id: string, remote: CrdtCounterState, options: Signal = {}) =>
          call({ type: "crdt_counter_merge", space, id, remote: toWireCounterState(remote) }, "crdt_counter", (value) => Number(value), options.signal),
      },
      set: {
        add: (space: string, id: string, actorId: string, element: string, options: Signal = {}) =>
          write({ model: "crdt_or_set_add", space, id, actorId, element }, options.signal),
        remove: (space: string, id: string, element: string, options: { timestampMs?: number } & Signal = {}) =>
          write({ model: "crdt_or_set_remove", space, id, element, timestampMs: options.timestampMs }, options.signal),
        read: (space: string, id: string, options: Signal = {}) => call({ type: "crdt_set_get", space, id }, "ids", (elements) => elements, options.signal),
        contains: (space: string, id: string, element: string, options: Signal = {}) =>
          flag({ type: "crdt_set_contains", space, id, element }, options.signal),
        state: (space: string, id: string, options: Signal = {}) =>
          call({ type: "crdt_set_state", space, id }, "crdt_set_state", mapSetState, options.signal),
        /** Forgets the removals older than `cutoffMs`; whether any was. */
        prune: (space: string, id: string, cutoffMs: number, options: Signal = {}) =>
          flag({ type: "crdt_set_prune", space, id, cutoff_ms: cutoffMs }, options.signal),
        merge: (space: string, id: string, remote: CrdtOrSetState, options: Signal = {}) =>
          ack({ type: "crdt_set_merge", space, id, remote }, options.signal),
      },
      json: {
        setField: (space: string, id: string, actorId: string, path: string, value: JsonValue, options: { timestampMs?: number } & Signal = {}) =>
          write({ model: "crdt_json_set_field", space, id, actorId, path, value, timestampMs: options.timestampMs }, options.signal),
        deleteField: (space: string, id: string, actorId: string, path: string, options: { timestampMs?: number } & Signal = {}) =>
          write({ model: "crdt_json_delete_field", space, id, actorId, path, timestampMs: options.timestampMs }, options.signal),
        get: <T = JsonValue>(space: string, id: string, options: Signal = {}) =>
          call({ type: "crdt_json_get", space, id }, "crdt_json", (value) => (value === null ? undefined : (value as T)), options.signal),
        state: (space: string, id: string, options: Signal = {}) =>
          call({ type: "crdt_json_state", space, id }, "crdt_json_state", (state) => (state ? mapLwwDocument(state) : undefined), options.signal),
        /** Merges a remote state; the result has no version when the merge changed nothing. */
        merge: (space: string, id: string, remote: LwwJsonDocumentState, options: Signal = {}) =>
          call({ type: "crdt_json_merge", space, id, remote: toWireLwwDocument(remote) }, "mutations", (results) => mapMutations(results)[0]!, options.signal),
      },
    },
    batch: (operations: readonly BatchOperation[], options: Signal = {}) => batch(operations, options.signal),
    /** Undoes the latest commit not undone; the commit, or nothing when there is none. */
    undo: (options: Signal = {}) => call({ type: "undo" }, "undo_step", (step) => (step ? mapUndoStep(step) : undefined), options.signal),
    /** Redoes the latest undone commit; the commit, or nothing when there is none. */
    redo: (options: Signal = {}) => call({ type: "redo" }, "undo_step", (step) => (step ? mapUndoStep(step) : undefined), options.signal),
    undoHistory: (options: Signal = {}) => call({ type: "undo_history" }, "undo_steps", (steps) => steps.map(mapUndoStep), options.signal),
    /** Verifies the ledger, fast; true without a ledger. */
    quickCheck: (options: Signal = {}) => flag({ type: "quick_check" }, options.signal),
    /** Reads every stored entry, verifying each block's checksum, and verifies the ledger. */
    integrityCheck: (options: Signal = {}) => flag({ type: "integrity_check" }, options.signal),
    /** Writes the memtable to a sorted run. */
    checkpoint: (options: Signal = {}) => ack({ type: "checkpoint" }, options.signal),
    storageStats: (options: Signal = {}) => call({ type: "storage_stats" }, "storage_stats", mapStorageStats, options.signal),
    sql,
  };
}

export type Api<M extends Mode> = ReturnType<typeof buildApi<M>>;
