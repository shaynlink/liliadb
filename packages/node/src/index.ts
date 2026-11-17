import { DaemonClient } from "./daemon.js";
import {
  EmbeddedClient,
  nativeGeoCircleToGeopoly,
  nativeGeoCoordsToGeopoly,
  nativeGeoHaversineDistance,
  nativeTsDownsamplePoints,
} from "./native.js";
import { inferVectorDim } from "./mappers.js";
import { Readable } from "node:stream";
import { LiliaError } from "./types.js";
import { getUuid } from "./version.js";
import type {
  AutoEmbedConfig,
  BatchOperation,
  BlobDescriptor,
  BlobGcStats,
  ShardArchiveResult,
  ShardInfo,
  CentralityType,
  CheckpointStats,
  CloseOptions,
  ComponentType,
  ConflictRecord,
  CrdtCounterState,
  CrdtOrSetState,
  CreateIndexOptions,
  CypherResult,
  DatabaseEvent,
  DictStats,
  EmbedStatus,
  FtsHit,
  GeoAddFenceOptions,
  GeoTrackQueryOptions,
  GeofenceMatch,
  GpsPoint,
  GraphAddEdgeOptions,
  GraphCentralityOptions,
  GraphDirection,
  GraphEdge,
  GraphFindCyclesOptions,
  GraphListEdgesOptions,
  GraphListNodesOptions,
  GraphNode,
  GraphSetNodeOptions,
  GraphTraversalOptions,
  HlcMutationOutcome,
  HlcRecord,
  HnswConfig,
  HybridHit,
  ImpactAnalysis,
  JsonDeleteOptions,
  JsonEntry,
  JsonFindOptions,
  JsonGetOptions,
  JsonIndexInfo,
  JsonPutHlcOptions,
  JsonPutOptions,
  JsonRevisionInfo,
  JsonScanOptions,
  JsonScanPrefixOptions,
  JsonTrainDictionaryOptions,
  JsonValue,
  KvDeleteOptions,
  KvEntry,
  KvGetOptions,
  KvScanOptions,
  KvSetOptions,
  LiveQueryDiff,
  LiveQuerySubscription,
  LwwJsonDocumentState,
  MaterializedViewInfo,
  MutationResult,
  NodeScore,
  OpenOptions,
  PageRankConfig,
  RedoResult,
  ScanStreamOptions,
  SqlFunction,
  SqlQueryOptions,
  SqlQueryResult,
  SyncSqlFunction,
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
  VectorMetric,
  VectorSearchOptions,
  WatchOptions,
  WatchSubscription,
  WeightedPath,
} from "./types.js";

export * from "./types.js";

type Client = EmbeddedClient | DaemonClient;

const textEncoder = new TextEncoder();

function toUint8Array(value: Uint8Array | string): Uint8Array {
  return typeof value === "string" ? textEncoder.encode(value) : value;
}

function toReadableStream<T>(iterable: AsyncIterable<T>): ReadableStream<T> {
  if (typeof (ReadableStream as unknown as { from?: (it: AsyncIterable<T>) => ReadableStream<T> }).from === "function") {
    return (ReadableStream as unknown as { from: (it: AsyncIterable<T>) => ReadableStream<T> }).from(iterable);
  }
  const iterator = iterable[Symbol.asyncIterator]();
  return new ReadableStream<T>({
    async pull(controller) {
      try {
        const { value, done } = await iterator.next();
        if (done) {
          controller.close();
        } else {
          controller.enqueue(value);
        }
      } catch (err) {
        controller.error(err);
      }
    },
    async cancel(reason) {
      await iterator.return?.(reason);
    },
  });
}

function buildTaggedQuery(strings: TemplateStringsArray, values: JsonValue[]): { sql: string; params: JsonValue[] } {
  let sql = "";
  for (let i = 0; i < strings.length; i++) {
    sql += strings[i];
    if (i < values.length) {
      sql += `?${i + 1}`;
    }
  }
  return { sql, params: values };
}

function resolveIndexNameAndPath(
  fieldOrPath: string,
  optionsOrPath?: string | CreateIndexOptions
): { name: string; path: string; filter?: string } {
  if (typeof optionsOrPath === "string") {
    return { name: fieldOrPath, path: optionsOrPath };
  }
  if (typeof optionsOrPath === "object" && optionsOrPath !== null) {
    return {
      name: optionsOrPath.name ?? fieldOrPath,
      path: optionsOrPath.path ?? (fieldOrPath.startsWith("$") || fieldOrPath.startsWith("/") || fieldOrPath.includes("(") ? fieldOrPath : `$.${fieldOrPath}`),
      filter: optionsOrPath.filter,
    };
  }
  return {
    name: fieldOrPath,
    path: fieldOrPath.startsWith("$") || fieldOrPath.startsWith("/") || fieldOrPath.includes("(") ? fieldOrPath : `$.${fieldOrPath}`,
  };
}

function normalizePageRankConfig(config?: PageRankConfig & { signal?: AbortSignal }): {
  prConfig: PageRankConfig | undefined;
  signal: AbortSignal | undefined;
} {
  if (!config) return { prConfig: undefined, signal: undefined };
  const { signal, ...rest } = config;
  const prConfig = Object.values(rest).some((v) => v !== undefined) ? rest : undefined;
  return { prConfig, signal };
}

export class Database implements AsyncDisposable {
  private closed = false;
  private readonly pending = new Set<Promise<unknown>>();
  private closing?: Promise<void>;
  private constructor(private readonly client: Client) {}

  static async open(options: OpenOptions): Promise<Database> {
    options.signal?.throwIfAborted();
    if (options.checkpointPolicy) {
      const { walBytes, intervalMs } = options.checkpointPolicy;
      if (walBytes !== undefined && (!Number.isInteger(walBytes) || walBytes <= 0 || walBytes > 0xffff_ffff)) {
        throw new RangeError("checkpoint wal_bytes must be positive and interval_ms must be in 1..=4294967295");
      }
      if (intervalMs !== undefined && (!Number.isInteger(intervalMs) || intervalMs <= 0 || intervalMs > 0xffff_ffff)) {
        throw new RangeError("checkpoint wal_bytes must be positive and interval_ms must be in 1..=4294967295");
      }
    }
    const client = options.mode === "daemon"
      ? await DaemonClient.open(
          options.endpoint ?? `${options.path}.sock`,
          options.tokenFile ?? `${options.path}.token`,
          { signal: options.signal }
        )
      : await EmbeddedClient.open(
          {
            path: options.path,
            durability: options.durability,
            profile: options.profile,
            busyTimeoutMs: options.busyTimeoutMs,
            writerQueueCapacity: options.writerQueueCapacity,
            readPoolSize: options.readPoolSize,
            mmapSizeBytes: options.mmapSizeBytes,
            cacheSizeKib: options.cacheSizeKib,
            undoLogCapacity: options.undoLogCapacity,
            checkpointPolicy: options.checkpointPolicy,
            jsonOptions: options.jsonOptions,
            timeseriesOptions: options.timeseriesOptions,
            casOptions: options.casOptions,
            sharding: options.sharding,
            nodeId: options.nodeId,
            maxDriftMs: options.maxDriftMs,
            encryptionKey: options.encryptionKey,
          },
          options.signal
        );
    return new Database(client);
  }

  async [Symbol.asyncDispose](): Promise<void> {
    await this.close();
  }

  get sync() {
    if (!(this.client instanceof EmbeddedClient)) {
      throw lifecycleError("NOT_SUPPORTED", "Synchronous operations are only supported in embedded mode", false);
    }
    const client = this.client;
    if (this.closed) throw lifecycleError("CLOSED", "database is closing or closed", false);

    const syncSql: SyncSqlFunction = ((
      stringsOrQuery: TemplateStringsArray | string,
      ...valuesOrParams: unknown[]
    ): unknown => {
      if (Array.isArray(stringsOrQuery) && "raw" in stringsOrQuery) {
        const { sql, params } = buildTaggedQuery(stringsOrQuery as TemplateStringsArray, valuesOrParams as JsonValue[]);
        return client.sqlQuerySync(sql, params).records;
      }
      return client.sqlQuerySync(stringsOrQuery as string, (valuesOrParams[0] as JsonValue[]) ?? []).records;
    }) as unknown as SyncSqlFunction;

    syncSql.query = <T = Record<string, unknown>>(sql: string, params: JsonValue[] = []): T[] =>
      client.sqlQuerySync(sql, params).records as T[];

    syncSql.execute = (sql: string, params: JsonValue[] = []): { rowsAffected: number } =>
      ({ rowsAffected: client.sqlQuerySync(sql, params).rowsAffected });

    syncSql.raw = <T = Record<string, unknown>>(sql: string, params: JsonValue[] = []): SqlQueryResult<T> =>
      client.sqlQuerySync(sql, params) as SqlQueryResult<T>;

    return {
      kv: {
        get: (namespace: string, key: Uint8Array | string): KvEntry | undefined =>
          client.kvGetSync(namespace, toUint8Array(key)),
        set: (
          namespace: string,
          key: Uint8Array | string,
          value: Uint8Array | string,
          options: { ifVersion?: bigint; expiresAtMs?: number } = {}
        ): MutationResult =>
          client.batchSync([{ model: "kv_set", namespace, key: toUint8Array(key), value: toUint8Array(value), ...options }])[0]!,
        delete: (
          namespace: string,
          key: Uint8Array | string,
          options: { ifVersion?: bigint } = {}
        ): MutationResult =>
          client.batchSync([{ model: "kv_delete", namespace, key: toUint8Array(key), ...options }])[0]!,
        scan: (
          namespace: string,
          options: { after?: Uint8Array | string; limit?: number } = {}
        ): KvEntry[] =>
          client.kvScanSync(namespace, options.after ? toUint8Array(options.after) : undefined, options.limit ?? 100),
      },
      json: {
        get: <T = JsonValue>(space: string, id: string): JsonEntry<T> | undefined =>
          client.jsonGetSync<T>(space, id),
        extract: <T = JsonValue>(space: string, id: string, path: string): T | undefined =>
          client.jsonExtractSync(space, id, path) as T | undefined,
        put: <T = JsonValue>(
          space: string,
          id: string,
          value: T,
          options: { ifVersion?: bigint } = {}
        ): MutationResult =>
          client.batchSync([{ model: "json_put", space, id, value: value as unknown as JsonValue, ...options }])[0]!,
        delete: (
          space: string,
          id: string,
          options: { ifVersion?: bigint } = {}
        ): MutationResult =>
          client.batchSync([{ model: "json_delete", space, id, ...options }])[0]!,
        scan: <T = JsonValue>(
          space: string,
          options: { after?: string; limit?: number } = {}
        ): JsonEntry<T>[] =>
          client.jsonScanSync<T>(space, options.after, options.limit ?? 100),
        scanPrefix: <T = JsonValue>(
          space: string,
          prefix: string,
          options: { afterId?: string; limit?: number } = {}
        ): JsonEntry<T>[] =>
          client.jsonScanPrefixSync<T>(space, prefix, options.afterId, options.limit ?? 100),
        createIndex: (space: string, fieldOrPath: string, optionsOrPath?: string | CreateIndexOptions): void => {
          const { name, path, filter } = resolveIndexNameAndPath(fieldOrPath, optionsOrPath);
          client.jsonCreateIndexSync(space, name, path, filter);
        },
        dropIndex: (space: string, indexName: string): boolean =>
          client.jsonDropIndexSync(space, indexName),
        listIndexes: (space: string): JsonIndexInfo[] =>
          client.jsonListIndexesSync(space),
        findByIndex: <T = JsonValue>(
          space: string,
          indexName: string,
          value: string | number | boolean,
          options: { after?: string; limit?: number } = {}
        ): JsonEntry<T>[] =>
          client.jsonScanByIndexSync<T>(space, indexName, String(value), options.after, options.limit ?? 100),
        find: <T = JsonValue>(
          space: string,
          query: Record<string, JsonValue>,
          options: { after?: string; limit?: number } = {}
        ): JsonEntry<T>[] =>
          client.jsonFindSync<T>(space, query, options.after, options.limit ?? 100),
        getAtVersion: <T = JsonValue>(space: string, id: string, version: bigint): JsonEntry<T> | undefined =>
          client.jsonGetAtVersionSync<T>(space, id, version),
        getAtTimestamp: <T = JsonValue>(space: string, id: string, timestampMs: number): JsonEntry<T> | undefined =>
          client.jsonGetAtTimestampSync<T>(space, id, timestampMs),
        history: (space: string, id: string): JsonRevisionInfo[] =>
          client.jsonHistorySync(space, id),
        diff: (space: string, id: string, fromVersion: bigint, toVersion: bigint): JsonValue =>
          client.jsonDiffSync(space, id, fromVersion, toVersion),
        trainDictionary: (space: string, options: { sampleCount?: number; dictSize?: number } = {}): DictStats =>
          client.jsonTrainDictionarySync(space, options.sampleCount, options.dictSize),
        setAutoTrain: (space: string, enabled: boolean): void =>
          client.jsonSetAutoTrainSync(space, enabled),
        isAutoTrainEnabled: (space: string): boolean =>
          client.jsonIsAutoTrainEnabledSync(space),
        putHlc: <T = JsonValue>(
          space: string,
          id: string,
          value: T,
          options: Omit<JsonPutHlcOptions, "signal"> = {}
        ): HlcMutationOutcome =>
          client.jsonPutHlcSync(space, id, value as unknown as JsonValue, options.hlc, options.parentHlc, options.strategy),
        getHlc: (space: string, id: string): HlcRecord | undefined =>
          client.jsonGetHlcSync(space, id),
      },
      graph: {
        setNode: (
          graph: string,
          id: string,
          kind: string,
          metadata?: JsonValue,
          options: Omit<GraphSetNodeOptions, "signal"> = {}
        ): MutationResult =>
          client.batchSync([
            {
              model: "graph_set_node",
              graph,
              id,
              kind,
              metadata,
              docSpace: options.docSpace,
              docId: options.docId,
            },
          ])[0]!,
        getNode: (graph: string, id: string): GraphNode | undefined =>
          client.graphGetNodeSync(graph, id),
        getNodeDocument: <T = JsonValue>(graph: string, id: string): JsonEntry<T> | undefined =>
          client.graphGetNodeDocumentSync<T>(graph, id),
        deleteNode: (graph: string, id: string): boolean =>
          client.graphDeleteNodeSync(graph, id),
        listNodes: (graph: string, options: Omit<GraphListNodesOptions, "signal"> = {}): GraphNode[] =>
          client.graphListNodesSync(graph, options.kind, options.limit),
        addEdge: (
          graph: string,
          source: string,
          target: string,
          kind: string,
          options: Omit<GraphAddEdgeOptions, "signal"> = {}
        ): MutationResult =>
          client.batchSync([{ model: "graph_add_edge", graph, source, target, kind, weight: options.weight }])[0]!,
        removeEdge: (graph: string, source: string, target: string, kind: string): boolean =>
          client.graphRemoveEdgeSync(graph, source, target, kind),
        listEdges: (graph: string, options: Omit<GraphListEdgesOptions, "signal"> = {}): GraphEdge[] =>
          client.graphListEdgesSync(graph, options.source, options.target, options.kind, options.limit),
        neighbors: (graph: string, node: string, options: { direction?: GraphDirection; kind?: string } = {}): string[] =>
          client.graphNeighborsSync(graph, node, options.direction, options.kind),
        reachability: (graph: string, startNodes: string[], options: { direction?: GraphDirection } = {}): string[] =>
          client.graphReachabilitySync(graph, startNodes, options.direction),
        impactAnalysis: (graph: string, changedNodes: string[]): ImpactAnalysis =>
          client.graphImpactAnalysisSync(graph, changedNodes),
        shortestPath: (
          graph: string,
          start: string,
          target: string,
          options: Omit<GraphTraversalOptions, "signal"> = {}
        ): WeightedPath | undefined =>
          client.graphShortestPathSync(graph, start, target, options.direction, options.kindFilter),
        dijkstra: (
          graph: string,
          start: string,
          target?: string,
          options: Omit<GraphTraversalOptions, "signal"> = {}
        ): WeightedPath | undefined =>
          client.graphDijkstraSync(graph, start, target, options.direction, options.kindFilter),
        allShortestPaths: (
          graph: string,
          start: string,
          options: Omit<GraphTraversalOptions, "signal"> = {}
        ): Record<string, WeightedPath> =>
          client.graphAllShortestPathsSync(graph, start, options.direction, options.kindFilter),
        astarSpatial: (
          graph: string,
          start: string,
          target: string,
          options: Omit<GraphTraversalOptions, "signal"> = {}
        ): WeightedPath | undefined =>
          client.graphAstarSpatialSync(graph, start, target, options.direction, options.kindFilter),
        pagerank: (graph: string, config?: PageRankConfig): NodeScore[] =>
          client.graphPagerankSync(graph, normalizePageRankConfig(config).prConfig),
        centrality: (
          graph: string,
          centrality: CentralityType,
          options: Omit<GraphCentralityOptions, "signal"> = {}
        ): NodeScore[] =>
          client.graphCentralitySync(graph, centrality, options.direction, options.kindFilter, options.normalized),
        hasCycle: (graph: string, options: Omit<GraphTraversalOptions, "signal"> = {}): boolean =>
          client.graphHasCycleSync(graph, options.direction, options.kindFilter),
        findCycles: (graph: string, options: Omit<GraphFindCyclesOptions, "signal"> = {}): string[][] =>
          client.graphFindCyclesSync(graph, options.maxCycles, options.kindFilter),
        connectedComponents: (
          graph: string,
          componentType: ComponentType,
          options: { kindFilter?: string } = {}
        ): string[][] =>
          client.graphConnectedComponentsSync(graph, componentType, options.kindFilter),
        cypher: <T = Record<string, JsonValue>>(graph: string, query: string): CypherResult<T> =>
          client.graphCypherSync<T>(graph, query),
      },
      fts: {
        index: (collection: string, docId: string, content: string): MutationResult =>
          client.batchSync([{ model: "fts_index", collection, docId, content }])[0]!,
        delete: (collection: string, docId: string): boolean =>
          client.ftsDeleteSync(collection, docId),
        search: (collection: string, query: string, options: { limit?: number } = {}): FtsHit[] =>
          client.ftsSearchSync(collection, query, options.limit),
      },
      vector: {
        set: (
          collection: string,
          id: string,
          vector: Float32Array | number[] | Uint8Array,
          options: { dim?: number; elementType?: VectorElementType } = {}
        ): MutationResult => {
          const dim = inferVectorDim(vector, options.elementType, options.dim);
          return client.batchSync([{ model: "vector_set", collection, id, dim, elementType: options.elementType, vector }])[0]!;
        },
        get: (collection: string, id: string): Uint8Array | null =>
          client.vectorGetSync(collection, id),
        delete: (collection: string, id: string): MutationResult =>
          client.batchSync([{ model: "vector_delete", collection, id }])[0]!,
        search: (
          collection: string,
          query: Float32Array | number[] | Uint8Array,
          options: Omit<VectorSearchOptions, "signal"> = {}
        ): VectorHit[] =>
          client.vectorSearchSync(collection, query, options.dim, options.elementType, options.metric, options.limit),
        createIndex: (collection: string, config: HnswConfig): void =>
          client.vectorCreateIndexSync(collection, config),
        getIndexInfo: (collection: string): VectorIndexInfo | undefined =>
          client.vectorGetIndexInfoSync(collection),
        dropIndex: (collection: string): boolean =>
          client.vectorDropIndexSync(collection),
        searchFiltered: (
          collection: string,
          query: Float32Array | number[] | Uint8Array,
          candidateIds: string[],
          options: Omit<VectorSearchOptions, "signal"> = {}
        ): VectorHit[] =>
          client.vectorSearchFilteredSync(
            collection,
            query,
            candidateIds,
            options.dim,
            options.elementType,
            options.metric,
            options.limit
          ),
        searchInGraphNeighbors: (
          collection: string,
          graph: string,
          startNode: string,
          query: Float32Array | number[] | Uint8Array,
          options: Omit<VectorSearchOptions, "signal"> & { direction?: GraphDirection } = {}
        ): VectorHit[] =>
          client.vectorSearchInGraphNeighborsSync(
            collection,
            graph,
            startNode,
            options.direction,
            query,
            options.dim,
            options.elementType,
            options.metric,
            options.limit
          ),
        searchByJsonIndex: (
          collection: string,
          space: string,
          indexName: string,
          indexValue: string | number | boolean,
          query: Float32Array | number[] | Uint8Array,
          options: Omit<VectorSearchOptions, "signal"> = {}
        ): VectorHit[] =>
          client.vectorSearchByJsonIndexSync(
            collection,
            space,
            indexName,
            String(indexValue),
            query,
            options.dim,
            options.elementType,
            options.metric,
            options.limit
          ),
      },
      hybrid: {
        search: (
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
        ): HybridHit[] =>
          client.hybridSearchSync(collection, options),
      },
      timeseries: {
        appendPoints: (seriesId: string, points: TimeSeriesPoint[], options: { resolutionMs?: number } = {}): void =>
          client.tsAppendPointsSync(seriesId, points, options.resolutionMs),
        queryRange: (
          seriesId: string,
          startTime: number,
          endTime: number,
          options: { resolutionMs?: number } = {}
        ): TimeSeriesPoint[] =>
          client.tsQueryRangeSync(seriesId, startTime, endTime, options.resolutionMs),
        queryAuto: (seriesId: string, startTime: number, endTime: number, maxPoints: number): TimeSeriesPoint[] =>
          client.tsQueryAutoSync(seriesId, startTime, endTime, maxPoints),
        queryStats: (
          seriesId: string,
          startTime: number,
          endTime: number,
          options: { resolutionMs?: number } = {}
        ): TimeSeriesStats | undefined =>
          client.tsQueryStatsSync(seriesId, startTime, endTime, options.resolutionMs),
        setPolicy: (policy: TimeSeriesLifecyclePolicy): void =>
          client.tsSetPolicySync(policy),
        getPolicy: (seriesPattern: string): TimeSeriesLifecyclePolicy | undefined =>
          client.tsGetPolicySync(seriesPattern),
        listPolicies: (): TimeSeriesLifecyclePolicy[] =>
          client.tsListPoliciesSync(),
        deletePolicy: (seriesPattern: string): boolean =>
          client.tsDeletePolicySync(seriesPattern),
        applyRetention: (options: { seriesId?: string; policy?: TimeSeriesLifecyclePolicy; nowMs?: number } = {}): TimeSeriesRetentionStats =>
          client.tsApplyRetentionSync(options.seriesId, options.policy, options.nowMs),
      },
      geo: {
        addFence: (
          id: string,
          name: string,
          category: string,
          polygonJson: string,
          options: Omit<GeoAddFenceOptions, "signal"> = {}
        ): void =>
          client.geoAddFenceSync(id, name, category, polygonJson, options.metadata),
        removeFence: (id: string): boolean =>
          client.geoRemoveFenceSync(id),
        containsPoint: (polygonJson: string, lon: number, lat: number): boolean =>
          client.geoContainsPointSync(polygonJson, lon, lat),
        findFencesForPoint: (lon: number, lat: number): GeofenceMatch[] =>
          client.geoFindFencesForPointSync(lon, lat),
        recordGpsPoints: (trackId: string, points: GpsPoint[]): GeofenceMatch[] =>
          client.geoRecordGpsPointsSync(trackId, points),
        getTrackPoints: (trackId: string, options: Omit<GeoTrackQueryOptions, "signal"> = {}): GpsPoint[] =>
          client.geoGetTrackPointsSync(trackId, options.startTime, options.endTime),
        trackDistance: (trackId: string): number =>
          client.geoTrackDistanceSync(trackId),
      },
      crdt: {
        counter: {
          increment: (space: string, id: string, actorId: string, amount: number | bigint = 1): MutationResult =>
            client.batchSync([{ model: "crdt_pn_counter_increment", space, id, actorId, amount }])[0]!,
          decrement: (space: string, id: string, actorId: string, amount: number | bigint = 1): MutationResult =>
            client.batchSync([{ model: "crdt_pn_counter_decrement", space, id, actorId, amount }])[0]!,
          get: (space: string, id: string): number =>
            client.crdtCounterGetSync(space, id),
          state: (space: string, id: string): CrdtCounterState =>
            client.crdtCounterStateSync(space, id),
          merge: (space: string, id: string, remote: CrdtCounterState): number =>
            client.crdtCounterMergeSync(space, id, remote),
        },
        set: {
          add: (space: string, id: string, actorId: string, element: string): MutationResult =>
            client.batchSync([{ model: "crdt_or_set_add", space, id, actorId, element }])[0]!,
          remove: (space: string, id: string, element: string, options: { timestampMs?: number } = {}): MutationResult =>
            client.batchSync([{ model: "crdt_or_set_remove", space, id, element, timestampMs: options.timestampMs }])[0]!,
          read: (space: string, id: string): string[] =>
            client.crdtSetReadSync(space, id),
          contains: (space: string, id: string, element: string): boolean =>
            client.crdtSetContainsSync(space, id, element),
          state: (space: string, id: string): CrdtOrSetState =>
            client.crdtSetStateSync(space, id),
          prune: (space: string, id: string, cutoffMs: number): number =>
            client.crdtSetPruneSync(space, id, cutoffMs),
          merge: (space: string, id: string, remote: CrdtOrSetState): void =>
            client.crdtSetMergeSync(space, id, remote),
        },
        json: {
          setField: (
            space: string,
            id: string,
            actorId: string,
            path: string,
            value: JsonValue,
            options: { timestampMs?: number } = {}
          ): MutationResult =>
            client.batchSync([
              { model: "crdt_json_set_field", space, id, actorId, path, value, timestampMs: options.timestampMs },
            ])[0]!,
          deleteField: (
            space: string,
            id: string,
            actorId: string,
            path: string,
            options: { timestampMs?: number } = {}
          ): MutationResult =>
            client.batchSync([
              { model: "crdt_json_delete_field", space, id, actorId, path, timestampMs: options.timestampMs },
            ])[0]!,
          get: <T = JsonValue>(space: string, id: string): T | undefined =>
            client.crdtJsonGetSync<T>(space, id),
          state: (space: string, id: string): LwwJsonDocumentState | undefined =>
            client.crdtJsonStateSync(space, id),
          merge: (space: string, id: string, remote: LwwJsonDocumentState): bigint =>
            client.crdtJsonMergeSync(space, id, remote),
        },
      },
      hlc: {
        next: (): string => client.hlcNextSync(),
        observe: (remote: string): string => client.hlcObserveSync(remote),
      },
      conflicts: {
        list: (space?: string): ConflictRecord[] => client.listConflictsSync(space),
        resolve: (conflictId: number, resolvedValue: JsonValue): void => client.resolveConflictSync(conflictId, resolvedValue),
      },
      undo: (): UndoResult | undefined => client.undoSync(),
      redo: (): RedoResult | undefined => client.redoSync(),
      undoHistory: (): UndoEntry[] => client.undoHistorySync(),
      createMaterializedView: (name: string, query: string): void => client.createMaterializedViewSync(name, query),
      dropMaterializedView: (name: string): void => client.dropMaterializedViewSync(name),
      refreshMaterializedView: (name: string): void => client.refreshMaterializedViewSync(name),
      listMaterializedViews: (): MaterializedViewInfo[] => client.listMaterializedViewsSync(),
      quickCheck: (): boolean => client.quickCheckSync(),
      integrityCheck: (): boolean => client.integrityCheckSync(),
      checkpoint: (): void => client.checkpointSync(),
      checkpointStats: (): CheckpointStats => client.checkpointStatsSync(),
      storageStats: (): TableStorageStat[] => client.storageStatsSync(),
      backup: (destination: string): void => client.backupSync(destination),
      batch: (operations: readonly BatchOperation[]): MutationResult[] => client.batchSync(operations),
      blob: {
        store: (bytes: Uint8Array | string, mimeType?: string): BlobDescriptor =>
          client.blobStoreSync(toUint8Array(bytes), mimeType),
        get: (hash: string): Uint8Array | null =>
          client.blobGetSync(hash),
        path: (hash: string): string =>
          client.blobPath(hash),
        exists: (hash: string): boolean =>
          client.blobExists(hash),
        gc: (): BlobGcStats =>
          client.blobGcSync(),
      },
      sql: syncSql,
    };
  }

  readonly kv = {
    get: (namespace: string, key: Uint8Array | string, options: KvGetOptions = {}): Promise<KvEntry | undefined> =>
      this.run(client => client.kvGet(namespace, toUint8Array(key), options.signal), options.signal),
    set: async (
      namespace: string,
      key: Uint8Array | string,
      value: Uint8Array | string,
      options: KvSetOptions = {}
    ): Promise<MutationResult> => {
      const { signal, ...rest } = options;
      return (await this.batch([{ model: "kv_set", namespace, key: toUint8Array(key), value: toUint8Array(value), ...rest }], { signal }))[0]!;
    },
    delete: async (
      namespace: string,
      key: Uint8Array | string,
      options: KvDeleteOptions = {}
    ): Promise<MutationResult> => {
      const { signal, ...rest } = options;
      return (await this.batch([{ model: "kv_delete", namespace, key: toUint8Array(key), ...rest }], { signal }))[0]!;
    },
    scan: (namespace: string, options: KvScanOptions = {}): Promise<KvEntry[]> =>
      this.run(
        client => client.kvScan(namespace, options.after ? toUint8Array(options.after) : undefined, options.limit ?? 100, options.signal),
        options.signal
      ),
    scanStream: (namespace: string, options: ScanStreamOptions<Uint8Array | string> = {}): AsyncIterableIterator<KvEntry> =>
      this.kvScanStream(namespace, options),
    stream: (namespace: string, options: ScanStreamOptions<Uint8Array | string> = {}): AsyncIterableIterator<KvEntry> =>
      this.kvScanStream(namespace, options),
    readableStream: (namespace: string, options: ScanStreamOptions<Uint8Array | string> = {}): ReadableStream<KvEntry> =>
      toReadableStream(this.kvScanStream(namespace, options)),
    createReadStream: (namespace: string, options: ScanStreamOptions<Uint8Array | string> = {}): Readable =>
      Readable.from(this.kvScanStream(namespace, options), { objectMode: true }),
    watch: (
      namespace: string,
      callback: (event: DatabaseEvent) => void,
      options: WatchOptions = {}
    ): Promise<WatchSubscription> =>
      this.watch(namespace, callback, options),
  };

  readonly json = {
    setAutoEmbed: (
      space: string,
      config?: AutoEmbedConfig
    ): Promise<void> =>
      this.run(client => {
        client.jsonSetAutoEmbedSync(space, config);
        return Promise.resolve();
      }),
    embedStatus: (
      space: string
    ): Promise<EmbedStatus> =>
      this.run(client => Promise.resolve(client.jsonEmbedStatusSync(space))),
    searchText: (
      space: string,
      text: string,
      options: { limit?: number; signal?: AbortSignal } = {}
    ): Promise<VectorHit[]> =>
      this.run(client => client.vectorSearchText(space, text, options.limit, options.signal), options.signal),
    get: <T = JsonValue>(space: string, id: string, options: JsonGetOptions = {}): Promise<JsonEntry<T> | undefined> =>
      this.run(client => client.jsonGet<T>(space, id, options.signal), options.signal),
    extract: <T = JsonValue>(space: string, id: string, path: string, options: JsonGetOptions = {}): Promise<T | undefined> =>
      this.run(client => client.jsonExtract(space, id, path, options.signal) as Promise<T | undefined>, options.signal),
    put: async <T = JsonValue>(
      space: string,
      id: string,
      value: T,
      options: JsonPutOptions = {}
    ): Promise<MutationResult> => {
      const { signal, ...rest } = options;
      return (await this.batch([{ model: "json_put", space, id, value: value as unknown as JsonValue, ...rest }], { signal }))[0]!;
    },
    delete: async (
      space: string,
      id: string,
      options: JsonDeleteOptions = {}
    ): Promise<MutationResult> => {
      const { signal, ...rest } = options;
      return (await this.batch([{ model: "json_delete", space, id, ...rest }], { signal }))[0]!;
    },
    scan: <T = JsonValue>(space: string, options: JsonScanOptions = {}): Promise<JsonEntry<T>[]> =>
      this.run(client => client.jsonScan<T>(space, options.after, options.limit ?? 100, options.signal), options.signal),
    scanPrefix: <T = JsonValue>(space: string, prefix: string, options: JsonScanPrefixOptions = {}): Promise<JsonEntry<T>[]> =>
      this.run(
        client => client.jsonScanPrefix<T>(space, prefix, options.afterId, options.limit ?? 100, options.signal),
        options.signal
      ),
    scanStream: <T = JsonValue>(space: string, options: ScanStreamOptions<string> = {}): AsyncIterableIterator<JsonEntry<T>> =>
      this.jsonScanStream<T>(space, options),
    scanPrefixStream: <T = JsonValue>(space: string, prefix: string, options: ScanStreamOptions<string> = {}): AsyncIterableIterator<JsonEntry<T>> =>
      this.jsonScanPrefixStream<T>(space, prefix, options),
    stream: <T = JsonValue>(space: string, options: ScanStreamOptions<string> = {}): AsyncIterableIterator<JsonEntry<T>> =>
      this.jsonScanStream<T>(space, options),
    readableStream: <T = JsonValue>(space: string, options: ScanStreamOptions<string> = {}): ReadableStream<JsonEntry<T>> =>
      toReadableStream(this.jsonScanStream<T>(space, options)),
    createReadStream: <T = JsonValue>(space: string, options: ScanStreamOptions<string> = {}): Readable =>
      Readable.from(this.jsonScanStream<T>(space, options), { objectMode: true }),
    createIndex: (
      space: string,
      fieldOrPath: string,
      optionsOrPath?: string | CreateIndexOptions,
      options: { signal?: AbortSignal } = {}
    ): Promise<void> => {
      const { name, path, filter } = resolveIndexNameAndPath(fieldOrPath, optionsOrPath);
      return this.run(client => client.jsonCreateIndex(space, name, path, filter, options.signal), options.signal);
    },
    dropIndex: (space: string, indexName: string, options: { signal?: AbortSignal } = {}): Promise<boolean> =>
      this.run(client => client.jsonDropIndex(space, indexName, options.signal), options.signal),
    listIndexes: (space: string, options: { signal?: AbortSignal } = {}): Promise<JsonIndexInfo[]> =>
      this.run(client => client.jsonListIndexes(space, options.signal), options.signal),
    findByIndex: <T = JsonValue>(
      space: string,
      indexName: string,
      value: string | number | boolean,
      options: JsonFindOptions = {}
    ): Promise<JsonEntry<T>[]> =>
      this.run(client => client.jsonScanByIndex<T>(space, indexName, String(value), options.after, options.limit ?? 100, options.signal), options.signal),
    find: <T = JsonValue>(
      space: string,
      query: Record<string, JsonValue>,
      options: JsonFindOptions = {}
    ): Promise<JsonEntry<T>[]> =>
      this.run(client => client.jsonFind<T>(space, query, options.after, options.limit ?? 100, options.signal), options.signal),
    findStream: <T = JsonValue>(
      space: string,
      query: Record<string, JsonValue>,
      options: ScanStreamOptions<string> = {}
    ): AsyncIterableIterator<JsonEntry<T>> =>
      this.jsonFindStream<T>(space, query, options),
    findReadableStream: <T = JsonValue>(
      space: string,
      query: Record<string, JsonValue>,
      options: ScanStreamOptions<string> = {}
    ): ReadableStream<JsonEntry<T>> =>
      toReadableStream(this.jsonFindStream<T>(space, query, options)),
    getAtVersion: <T = JsonValue>(
      space: string,
      id: string,
      version: bigint,
      options: JsonGetOptions = {}
    ): Promise<JsonEntry<T> | undefined> =>
      this.run(client => client.jsonGetAtVersion<T>(space, id, version, options.signal), options.signal),
    getAtTimestamp: <T = JsonValue>(
      space: string,
      id: string,
      timestampMs: number,
      options: JsonGetOptions = {}
    ): Promise<JsonEntry<T> | undefined> =>
      this.run(client => client.jsonGetAtTimestamp<T>(space, id, timestampMs, options.signal), options.signal),
    history: (space: string, id: string, options: JsonGetOptions = {}): Promise<JsonRevisionInfo[]> =>
      this.run(client => client.jsonHistory(space, id, options.signal), options.signal),
    diff: (
      space: string,
      id: string,
      fromVersion: bigint,
      toVersion: bigint,
      options: JsonGetOptions = {}
    ): Promise<JsonValue> =>
      this.run(client => client.jsonDiff(space, id, fromVersion, toVersion, options.signal), options.signal),
    trainDictionary: (space: string, options: JsonTrainDictionaryOptions = {}): Promise<DictStats> =>
      this.run(client => client.jsonTrainDictionary(space, options.sampleCount, options.dictSize, options.signal), options.signal),
    setAutoTrain: (space: string, enabled: boolean, options: { signal?: AbortSignal } = {}): Promise<void> =>
      this.run(client => client.jsonSetAutoTrain(space, enabled, options.signal), options.signal),
    isAutoTrainEnabled: (space: string, options: { signal?: AbortSignal } = {}): Promise<boolean> =>
      this.run(client => client.jsonIsAutoTrainEnabled(space, options.signal), options.signal),
    putHlc: <T = JsonValue>(
      space: string,
      id: string,
      value: T,
      options: JsonPutHlcOptions = {}
    ): Promise<HlcMutationOutcome> =>
      this.run(
        client => client.jsonPutHlc(space, id, value as unknown as JsonValue, options.hlc, options.parentHlc, options.strategy, options.signal),
        options.signal
      ),
    getHlc: (space: string, id: string, options: JsonGetOptions = {}): Promise<HlcRecord | undefined> =>
      this.run(client => client.jsonGetHlc(space, id, options.signal), options.signal),
    watch: (space: string, callback: (event: DatabaseEvent) => void, options: WatchOptions = {}): Promise<WatchSubscription> =>
      this.watch(space, callback, options),
  };

  readonly graph = {
    setNode: async (
      graph: string,
      id: string,
      kind: string,
      metadata?: JsonValue,
      options: GraphSetNodeOptions = {}
    ): Promise<MutationResult> =>
      (
        await this.batch(
          [
            {
              model: "graph_set_node",
              graph,
              id,
              kind,
              metadata,
              docSpace: options.docSpace,
              docId: options.docId,
            },
          ],
          { signal: options.signal }
        )
      )[0]!,
    getNode: (graph: string, id: string, options: { signal?: AbortSignal } = {}): Promise<GraphNode | undefined> =>
      this.run(client => client.graphGetNode(graph, id, options.signal), options.signal),
    getNodeDocument: <T = JsonValue>(
      graph: string,
      id: string,
      options: { signal?: AbortSignal } = {}
    ): Promise<JsonEntry<T> | undefined> =>
      this.run(client => client.graphGetNodeDocument<T>(graph, id, options.signal), options.signal),
    deleteNode: (graph: string, id: string, options: { signal?: AbortSignal } = {}): Promise<boolean> =>
      this.run(client => client.graphDeleteNode(graph, id, options.signal), options.signal),
    listNodes: (graph: string, options: GraphListNodesOptions = {}): Promise<GraphNode[]> =>
      this.run(client => client.graphListNodes(graph, options.kind, options.limit, options.signal), options.signal),
    addEdge: async (
      graph: string,
      source: string,
      target: string,
      kind: string,
      options: GraphAddEdgeOptions = {}
    ): Promise<MutationResult> =>
      (await this.batch([{ model: "graph_add_edge", graph, source, target, kind, weight: options.weight }], { signal: options.signal }))[0]!,
    removeEdge: (
      graph: string,
      source: string,
      target: string,
      kind: string,
      options: { signal?: AbortSignal } = {}
    ): Promise<boolean> =>
      this.run(client => client.graphRemoveEdge(graph, source, target, kind, options.signal), options.signal),
    listEdges: (graph: string, options: GraphListEdgesOptions = {}): Promise<GraphEdge[]> =>
      this.run(
        client => client.graphListEdges(graph, options.source, options.target, options.kind, options.limit, options.signal),
        options.signal
      ),
    neighbors: (
      graph: string,
      node: string,
      options: { direction?: GraphDirection; kind?: string; signal?: AbortSignal } = {}
    ): Promise<string[]> =>
      this.run(client => client.graphNeighbors(graph, node, options.direction, options.kind, options.signal), options.signal),
    reachability: (
      graph: string,
      startNodes: string[],
      options: { direction?: GraphDirection; signal?: AbortSignal } = {}
    ): Promise<string[]> =>
      this.run(client => client.graphReachability(graph, startNodes, options.direction, options.signal), options.signal),
    impactAnalysis: (
      graph: string,
      changedNodes: string[],
      options: { signal?: AbortSignal } = {}
    ): Promise<ImpactAnalysis> =>
      this.run(client => client.graphImpactAnalysis(graph, changedNodes, options.signal), options.signal),
    shortestPath: (
      graph: string,
      start: string,
      target: string,
      options: GraphTraversalOptions = {}
    ): Promise<WeightedPath | undefined> =>
      this.run(client => client.graphShortestPath(graph, start, target, options.direction, options.kindFilter, options.signal), options.signal),
    dijkstra: (
      graph: string,
      start: string,
      target?: string,
      options: GraphTraversalOptions = {}
    ): Promise<WeightedPath | undefined> =>
      this.run(client => client.graphDijkstra(graph, start, target, options.direction, options.kindFilter, options.signal), options.signal),
    allShortestPaths: (
      graph: string,
      start: string,
      options: GraphTraversalOptions = {}
    ): Promise<Record<string, WeightedPath>> =>
      this.run(client => client.graphAllShortestPaths(graph, start, options.direction, options.kindFilter, options.signal), options.signal),
    astarSpatial: (
      graph: string,
      start: string,
      target: string,
      options: GraphTraversalOptions = {}
    ): Promise<WeightedPath | undefined> =>
      this.run(client => client.graphAstarSpatial(graph, start, target, options.direction, options.kindFilter, options.signal), options.signal),
    pagerank: (graph: string, config?: PageRankConfig & { signal?: AbortSignal }): Promise<NodeScore[]> => {
      const { prConfig, signal } = normalizePageRankConfig(config);
      return this.run(client => client.graphPagerank(graph, prConfig, signal), signal);
    },
    centrality: (
      graph: string,
      centrality: CentralityType,
      options: GraphCentralityOptions = {}
    ): Promise<NodeScore[]> =>
      this.run(
        client => client.graphCentrality(graph, centrality, options.direction, options.kindFilter, options.normalized, options.signal),
        options.signal
      ),
    hasCycle: (graph: string, options: GraphTraversalOptions = {}): Promise<boolean> =>
      this.run(client => client.graphHasCycle(graph, options.direction, options.kindFilter, options.signal), options.signal),
    findCycles: (graph: string, options: GraphFindCyclesOptions = {}): Promise<string[][]> =>
      this.run(client => client.graphFindCycles(graph, options.maxCycles, options.kindFilter, options.signal), options.signal),
    connectedComponents: (
      graph: string,
      componentType: ComponentType,
      options: { kindFilter?: string; signal?: AbortSignal } = {}
    ): Promise<string[][]> =>
      this.run(client => client.graphConnectedComponents(graph, componentType, options.kindFilter, options.signal), options.signal),
    cypher: <T = Record<string, JsonValue>>(
      graph: string,
      query: string,
      options: { signal?: AbortSignal } = {}
    ): Promise<CypherResult<T>> =>
      this.run(client => client.graphCypher<T>(graph, query, options.signal), options.signal),
  };

  readonly fts = {
    index: async (
      collection: string,
      docId: string,
      content: string,
      options: { signal?: AbortSignal } = {}
    ): Promise<MutationResult> =>
      (await this.batch([{ model: "fts_index", collection, docId, content }], { signal: options.signal }))[0]!,
    delete: (collection: string, docId: string, options: { signal?: AbortSignal } = {}): Promise<boolean> =>
      this.run(client => client.ftsDelete(collection, docId, options.signal), options.signal),
    search: (
      collection: string,
      query: string,
      options: { limit?: number; signal?: AbortSignal } = {}
    ): Promise<FtsHit[]> =>
      this.run(client => client.ftsSearch(collection, query, options.limit, options.signal), options.signal),
  };

  readonly vector = {
    set: async (
      collection: string,
      id: string,
      vector: Float32Array | number[] | Uint8Array,
      options: { dim?: number; elementType?: VectorElementType; signal?: AbortSignal } = {}
    ): Promise<MutationResult> => {
      const dim = inferVectorDim(vector, options.elementType, options.dim);
      return (await this.batch([{ model: "vector_set", collection, id, dim, elementType: options.elementType, vector }], { signal: options.signal }))[0]!;
    },
    get: (
      collection: string,
      id: string,
      options: { signal?: AbortSignal } = {}
    ): Promise<Uint8Array | null> =>
      this.run(client => client.vectorGet(collection, id, options.signal), options.signal),
    delete: async (
      collection: string,
      id: string,
      options: { signal?: AbortSignal } = {}
    ): Promise<MutationResult> =>
      (await this.batch([{ model: "vector_delete", collection, id }], { signal: options.signal }))[0]!,
    search: (
      collection: string,
      query: Float32Array | number[] | Uint8Array,
      options: VectorSearchOptions = {}
    ): Promise<VectorHit[]> =>
      this.run(client => client.vectorSearch(collection, query, options.dim, options.elementType, options.metric, options.limit, options.signal), options.signal),
    searchText: (
      collection: string,
      text: string,
      options: { limit?: number; signal?: AbortSignal } = {}
    ): Promise<VectorHit[]> =>
      this.run(client => client.vectorSearchText(collection, text, options.limit, options.signal), options.signal),
    waitForEmbeddings: (
      timeoutMs: number = 60_000,
      options: { signal?: AbortSignal } = {}
    ): Promise<void> =>
      this.run(client => client.waitForEmbeddings(timeoutMs, options.signal), options.signal),
    createIndex: (collection: string, config: HnswConfig, options: { signal?: AbortSignal } = {}): Promise<void> =>
      this.run(client => client.vectorCreateIndex(collection, config, options.signal), options.signal),
    getIndexInfo: (collection: string, options: { signal?: AbortSignal } = {}): Promise<VectorIndexInfo | undefined> =>
      this.run(client => client.vectorGetIndexInfo(collection, options.signal), options.signal),
    dropIndex: (collection: string, options: { signal?: AbortSignal } = {}): Promise<boolean> =>
      this.run(client => client.vectorDropIndex(collection, options.signal), options.signal),
    searchFiltered: (
      collection: string,
      query: Float32Array | number[] | Uint8Array,
      candidateIds: string[],
      options: VectorSearchOptions = {}
    ): Promise<VectorHit[]> =>
      this.run(
        client =>
          client.vectorSearchFiltered(
            collection,
            query,
            candidateIds,
            options.dim,
            options.elementType,
            options.metric,
            options.limit,
            options.signal
          ),
        options.signal
      ),
    searchInGraphNeighbors: (
      collection: string,
      graph: string,
      startNode: string,
      query: Float32Array | number[] | Uint8Array,
      options: VectorSearchOptions & { direction?: GraphDirection } = {}
    ): Promise<VectorHit[]> =>
      this.run(
        client =>
          client.vectorSearchInGraphNeighbors(
            collection,
            graph,
            startNode,
            options.direction,
            query,
            options.dim,
            options.elementType,
            options.metric,
            options.limit,
            options.signal
          ),
        options.signal
      ),
    searchByJsonIndex: (
      collection: string,
      space: string,
      indexName: string,
      indexValue: string | number | boolean,
      query: Float32Array | number[] | Uint8Array,
      options: VectorSearchOptions = {}
    ): Promise<VectorHit[]> =>
      this.run(
        client =>
          client.vectorSearchByJsonIndex(
            collection,
            space,
            indexName,
            String(indexValue),
            query,
            options.dim,
            options.elementType,
            options.metric,
            options.limit,
            options.signal
          ),
        options.signal
      ),
  };

  readonly hybrid = {
    search: (
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
    ): Promise<HybridHit[]> =>
      this.run(client => client.hybridSearch(collection, options), options.signal),
  };

  readonly timeseries = {
    appendPoints: (
      seriesId: string,
      points: TimeSeriesPoint[],
      options: { resolutionMs?: number; signal?: AbortSignal } = {}
    ): Promise<void> =>
      this.run(client => client.tsAppendPoints(seriesId, points, options.resolutionMs, options.signal), options.signal),
    queryRange: (
      seriesId: string,
      startTime: number,
      endTime: number,
      options: { resolutionMs?: number; signal?: AbortSignal } = {}
    ): Promise<TimeSeriesPoint[]> =>
      this.run(client => client.tsQueryRange(seriesId, startTime, endTime, options.resolutionMs, options.signal), options.signal),
    queryAuto: (
      seriesId: string,
      startTime: number,
      endTime: number,
      maxPoints: number,
      options: { signal?: AbortSignal } = {}
    ): Promise<TimeSeriesPoint[]> =>
      this.run(client => client.tsQueryAuto(seriesId, startTime, endTime, maxPoints, options.signal), options.signal),
    queryStats: (
      seriesId: string,
      startTime: number,
      endTime: number,
      options: { resolutionMs?: number; signal?: AbortSignal } = {}
    ): Promise<TimeSeriesStats | undefined> =>
      this.run(client => client.tsQueryStats(seriesId, startTime, endTime, options.resolutionMs, options.signal), options.signal),
    setPolicy: (policy: TimeSeriesLifecyclePolicy, options: { signal?: AbortSignal } = {}): Promise<void> =>
      this.run(client => client.tsSetPolicy(policy, options.signal), options.signal),
    getPolicy: (
      seriesPattern: string,
      options: { signal?: AbortSignal } = {}
    ): Promise<TimeSeriesLifecyclePolicy | undefined> =>
      this.run(client => client.tsGetPolicy(seriesPattern, options.signal), options.signal),
    listPolicies: (options: { signal?: AbortSignal } = {}): Promise<TimeSeriesLifecyclePolicy[]> =>
      this.run(client => client.tsListPolicies(options.signal), options.signal),
    deletePolicy: (seriesPattern: string, options: { signal?: AbortSignal } = {}): Promise<boolean> =>
      this.run(client => client.tsDeletePolicy(seriesPattern, options.signal), options.signal),
    applyRetention: (
      options: { seriesId?: string; policy?: TimeSeriesLifecyclePolicy; nowMs?: number; signal?: AbortSignal } = {}
    ): Promise<TimeSeriesRetentionStats> =>
      this.run(client => client.tsApplyRetention(options.seriesId, options.policy, options.nowMs, options.signal), options.signal),
  };

  readonly geo = {
    addFence: (
      id: string,
      name: string,
      category: string,
      polygonJson: string,
      options: GeoAddFenceOptions = {}
    ): Promise<void> =>
      this.run(client => client.geoAddFence(id, name, category, polygonJson, options.metadata, options.signal), options.signal),
    removeFence: (id: string, options: { signal?: AbortSignal } = {}): Promise<boolean> =>
      this.run(client => client.geoRemoveFence(id, options.signal), options.signal),
    containsPoint: (
      polygonJson: string,
      lon: number,
      lat: number,
      options: { signal?: AbortSignal } = {}
    ): Promise<boolean> =>
      this.run(client => client.geoContainsPoint(polygonJson, lon, lat, options.signal), options.signal),
    findFencesForPoint: (lon: number, lat: number, options: { signal?: AbortSignal } = {}): Promise<GeofenceMatch[]> =>
      this.run(client => client.geoFindFencesForPoint(lon, lat, options.signal), options.signal),
    recordGpsPoints: (
      trackId: string,
      points: GpsPoint[],
      options: { signal?: AbortSignal } = {}
    ): Promise<GeofenceMatch[]> =>
      this.run(client => client.geoRecordGpsPoints(trackId, points, options.signal), options.signal),
    getTrackPoints: (trackId: string, options: GeoTrackQueryOptions = {}): Promise<GpsPoint[]> =>
      this.run(client => client.geoGetTrackPoints(trackId, options.startTime, options.endTime, options.signal), options.signal),
    trackDistance: (trackId: string, options: { signal?: AbortSignal } = {}): Promise<number> =>
      this.run(client => client.geoTrackDistance(trackId, options.signal), options.signal),
  };

  readonly crdt = {
    counter: {
      increment: async (
        space: string,
        id: string,
        actorId: string,
        amount: number | bigint = 1,
        options: { signal?: AbortSignal } = {}
      ): Promise<MutationResult> =>
        (await this.batch([{ model: "crdt_pn_counter_increment", space, id, actorId, amount }], { signal: options.signal }))[0]!,
      decrement: async (
        space: string,
        id: string,
        actorId: string,
        amount: number | bigint = 1,
        options: { signal?: AbortSignal } = {}
      ): Promise<MutationResult> =>
        (await this.batch([{ model: "crdt_pn_counter_decrement", space, id, actorId, amount }], { signal: options.signal }))[0]!,
      get: (space: string, id: string, options: { signal?: AbortSignal } = {}): Promise<number> =>
        this.run(client => client.crdtCounterGet(space, id, options.signal), options.signal),
      state: (space: string, id: string, options: { signal?: AbortSignal } = {}): Promise<CrdtCounterState> =>
        this.run(client => client.crdtCounterState(space, id, options.signal), options.signal),
      merge: (
        space: string,
        id: string,
        remote: CrdtCounterState,
        options: { signal?: AbortSignal } = {}
      ): Promise<number> =>
        this.run(client => client.crdtCounterMerge(space, id, remote, options.signal), options.signal),
    },
    set: {
      add: async (
        space: string,
        id: string,
        actorId: string,
        element: string,
        options: { signal?: AbortSignal } = {}
      ): Promise<MutationResult> =>
        (await this.batch([{ model: "crdt_or_set_add", space, id, actorId, element }], { signal: options.signal }))[0]!,
      remove: async (
        space: string,
        id: string,
        element: string,
        options: { timestampMs?: number; signal?: AbortSignal } = {}
      ): Promise<MutationResult> =>
        (
          await this.batch(
            [{ model: "crdt_or_set_remove", space, id, element, timestampMs: options.timestampMs }],
            { signal: options.signal }
          )
        )[0]!,
      read: (space: string, id: string, options: { signal?: AbortSignal } = {}): Promise<string[]> =>
        this.run(client => client.crdtSetRead(space, id, options.signal), options.signal),
      contains: (
        space: string,
        id: string,
        element: string,
        options: { signal?: AbortSignal } = {}
      ): Promise<boolean> =>
        this.run(client => client.crdtSetContains(space, id, element, options.signal), options.signal),
      state: (space: string, id: string, options: { signal?: AbortSignal } = {}): Promise<CrdtOrSetState> =>
        this.run(client => client.crdtSetState(space, id, options.signal), options.signal),
      prune: (space: string, id: string, cutoffMs: number, options: { signal?: AbortSignal } = {}): Promise<number> =>
        this.run(client => client.crdtSetPrune(space, id, cutoffMs, options.signal), options.signal),
      merge: (
        space: string,
        id: string,
        remote: CrdtOrSetState,
        options: { signal?: AbortSignal } = {}
      ): Promise<void> =>
        this.run(client => client.crdtSetMerge(space, id, remote, options.signal), options.signal),
    },
    json: {
      setField: async (
        space: string,
        id: string,
        actorId: string,
        path: string,
        value: JsonValue,
        options: { timestampMs?: number; signal?: AbortSignal } = {}
      ): Promise<MutationResult> =>
        (
          await this.batch(
            [{ model: "crdt_json_set_field", space, id, actorId, path, value, timestampMs: options.timestampMs }],
            { signal: options.signal }
          )
        )[0]!,
      deleteField: async (
        space: string,
        id: string,
        actorId: string,
        path: string,
        options: { timestampMs?: number; signal?: AbortSignal } = {}
      ): Promise<MutationResult> =>
        (
          await this.batch(
            [{ model: "crdt_json_delete_field", space, id, actorId, path, timestampMs: options.timestampMs }],
            { signal: options.signal }
          )
        )[0]!,
      get: <T = JsonValue>(space: string, id: string, options: { signal?: AbortSignal } = {}): Promise<T | undefined> =>
        this.run(client => client.crdtJsonGet<T>(space, id, options.signal), options.signal),
      state: (space: string, id: string, options: { signal?: AbortSignal } = {}): Promise<LwwJsonDocumentState | undefined> =>
        this.run(client => client.crdtJsonState(space, id, options.signal), options.signal),
      merge: (
        space: string,
        id: string,
        remote: LwwJsonDocumentState,
        options: { signal?: AbortSignal } = {}
      ): Promise<bigint> =>
        this.run(client => client.crdtJsonMerge(space, id, remote, options.signal), options.signal),
    },
  };

  readonly hlc = {
    next: (options: { signal?: AbortSignal } = {}): Promise<string> =>
      this.run(client => client.hlcNext(options.signal), options.signal),
    observe: (remote: string, options: { signal?: AbortSignal } = {}): Promise<string> =>
      this.run(client => client.hlcObserve(remote, options.signal), options.signal),
  };

  readonly conflicts = {
    list: (space?: string, options: { signal?: AbortSignal } = {}): Promise<ConflictRecord[]> =>
      this.run(client => client.listConflicts(space, options.signal), options.signal),
    resolve: (
      conflictId: number,
      resolvedValue: JsonValue,
      options: { signal?: AbortSignal } = {}
    ): Promise<void> =>
      this.run(client => client.resolveConflict(conflictId, resolvedValue, options.signal), options.signal),
  };

  readonly sql: SqlFunction = (() => {
    const fn = (async <T = Record<string, unknown>>(
      stringsOrQuery: TemplateStringsArray | string,
      ...valuesOrParams: unknown[]
    ): Promise<T[]> => {
      if (Array.isArray(stringsOrQuery) && "raw" in stringsOrQuery) {
        const { sql, params } = buildTaggedQuery(stringsOrQuery as TemplateStringsArray, valuesOrParams as JsonValue[]);
        return (await this.run(client => client.sqlQuery<T>(sql, params))).records;
      }
      return (await this.run(client => client.sqlQuery<T>(stringsOrQuery as string, (valuesOrParams[0] as JsonValue[]) ?? []))).records;
    }) as unknown as SqlFunction;

    fn.query = <T = Record<string, unknown>>(sql: string, params: JsonValue[] = [], options: SqlQueryOptions = {}): Promise<T[]> =>
      this.run(async client => (await client.sqlQuery<T>(sql, params, options.signal)).records, options.signal);

    fn.execute = (sql: string, params: JsonValue[] = [], options: SqlQueryOptions = {}): Promise<{ rowsAffected: number }> =>
      this.run(async client => ({ rowsAffected: (await client.sqlQuery(sql, params, options.signal)).rowsAffected }), options.signal);

    fn.raw = <T = Record<string, unknown>>(sql: string, params: JsonValue[] = [], options: SqlQueryOptions = {}): Promise<SqlQueryResult<T>> =>
      this.run(client => client.sqlQuery<T>(sql, params, options.signal), options.signal);

    return fn;
  })();

  watch(
    namespaceOrCallback?: string | ((event: DatabaseEvent) => void),
    callbackOrOptions?: ((event: DatabaseEvent) => void) | WatchOptions,
    maybeOptions?: WatchOptions
  ): Promise<WatchSubscription> {
    const namespace = typeof namespaceOrCallback === "string" ? namespaceOrCallback : undefined;
    const cb = typeof namespaceOrCallback === "function" ? namespaceOrCallback : (typeof callbackOrOptions === "function" ? callbackOrOptions : undefined);
    const options = typeof callbackOrOptions === "object" ? callbackOrOptions : maybeOptions;

    if (!cb) {
      throw new TypeError("watch callback must be a function");
    }
    return this.run(client => client.watch(namespace, cb, options), options?.signal);
  }

  liveQuery<T = Record<string, unknown>>(
    sql: string,
    callback: (diff: LiveQueryDiff<T>) => void,
    options?: { params?: JsonValue[]; signal?: AbortSignal }
  ): Promise<LiveQuerySubscription>;
  liveQuery<T = Record<string, unknown>>(
    sql: string,
    params: JsonValue[],
    callback: (diff: LiveQueryDiff<T>) => void,
    options?: { signal?: AbortSignal }
  ): Promise<LiveQuerySubscription>;
  liveQuery<T = Record<string, unknown>>(
    sql: string,
    paramOrCallback: JsonValue[] | ((diff: LiveQueryDiff<T>) => void),
    callbackOrOptions?: ((diff: LiveQueryDiff<T>) => void) | { params?: JsonValue[]; signal?: AbortSignal },
    maybeOptions?: { signal?: AbortSignal }
  ): Promise<LiveQuerySubscription> {
    let params: JsonValue[] = [];
    let cb: (diff: LiveQueryDiff<T>) => void;
    let signal: AbortSignal | undefined;

    if (typeof paramOrCallback === "function") {
      cb = paramOrCallback;
      const opts = callbackOrOptions as { params?: JsonValue[]; signal?: AbortSignal } | undefined;
      params = opts?.params ?? [];
      signal = opts?.signal;
    } else {
      params = paramOrCallback;
      cb = callbackOrOptions as (diff: LiveQueryDiff<T>) => void;
      signal = maybeOptions?.signal;
    }

    if (!cb) {
      throw new TypeError("liveQuery callback must be a function");
    }

    return this.run(client => client.liveQuery<T>(sql, params, cb, { signal }), signal);
  }

  createMaterializedView(name: string, query: string, options: { signal?: AbortSignal } = {}): Promise<void> {
    return this.run(client => client.createMaterializedView(name, query, options.signal), options.signal);
  }

  dropMaterializedView(name: string, options: { signal?: AbortSignal } = {}): Promise<void> {
    return this.run(client => client.dropMaterializedView(name, options.signal), options.signal);
  }

  refreshMaterializedView(name: string, options: { signal?: AbortSignal } = {}): Promise<void> {
    return this.run(client => client.refreshMaterializedView(name, options.signal), options.signal);
  }

  listMaterializedViews(options: { signal?: AbortSignal } = {}): Promise<MaterializedViewInfo[]> {
    return this.run(client => client.listMaterializedViews(options.signal), options.signal);
  }

  undo(options: { signal?: AbortSignal } = {}): Promise<UndoResult | undefined> {
    return this.run(client => client.undo(options.signal), options.signal);
  }

  redo(options: { signal?: AbortSignal } = {}): Promise<RedoResult | undefined> {
    return this.run(client => client.redo(options.signal), options.signal);
  }

  undoHistory(options: { signal?: AbortSignal } = {}): Promise<UndoEntry[]> {
    return this.run(client => client.undoHistory(options.signal), options.signal);
  }

  waitForEmbeddings(timeoutMs: number = 60_000, options: { signal?: AbortSignal } = {}): Promise<void> {
    return this.run(client => client.waitForEmbeddings(timeoutMs, options.signal), options.signal);
  }

  vectorSearchText(
    collection: string,
    text: string,
    options: { limit?: number; signal?: AbortSignal } = {}
  ): Promise<VectorHit[]> {
    return this.run(client => client.vectorSearchText(collection, text, options.limit, options.signal), options.signal);
  }

  batch(operations: readonly BatchOperation[], options: { signal?: AbortSignal } = {}): Promise<MutationResult[]> {
    return this.run(client => client.batch(operations, options.signal), options.signal);
  }

  quickCheck(options: { signal?: AbortSignal } = {}): Promise<boolean> {
    return this.run(client => client.quickCheck(options.signal), options.signal);
  }

  integrityCheck(options: { signal?: AbortSignal } = {}): Promise<boolean> {
    return this.run(client => client.integrityCheck(options.signal), options.signal);
  }

  checkpoint(options: { signal?: AbortSignal } = {}): Promise<void> {
    return this.run(client => client.checkpoint(options.signal), options.signal);
  }

  checkpointStats(options: { signal?: AbortSignal } = {}): Promise<CheckpointStats> {
    return this.run(client => client.checkpointStats(options.signal), options.signal);
  }

  storageStats(options: { signal?: AbortSignal } = {}): Promise<TableStorageStat[]> {
    return this.run(client => client.storageStats(options.signal), options.signal);
  }

  backup(destination: string, options: { signal?: AbortSignal } = {}): Promise<void> {
    return this.run(client => client.backup(destination, options.signal), options.signal);
  }

  private embeddedForSharding(): EmbeddedClient {
    if (this.closed) throw lifecycleError("CLOSED", "database is closing or closed", false);
    if (!(this.client instanceof EmbeddedClient)) {
      throw lifecycleError("NOT_SUPPORTED", "temporal sharding is currently supported in embedded mode", false);
    }
    return this.client;
  }

  /**
   * Local temporal sharding and cold-tier archiving. Requires `sharding` in the
   * open options; every call rejects when sharding is not enabled.
   */
  readonly sharding = {
    list: (options: { signal?: AbortSignal } = {}): Promise<ShardInfo[]> => {
      const client = this.embeddedForSharding();
      return this.run(() => client.shardingListShards(options.signal), options.signal);
    },
    active: (options: { signal?: AbortSignal } = {}): Promise<ShardInfo> => {
      const client = this.embeddedForSharding();
      return this.run(() => client.shardingActiveShard(options.signal), options.signal);
    },
    rollover: (options: { signal?: AbortSignal } = {}): Promise<ShardInfo> => {
      const client = this.embeddedForSharding();
      return this.run(() => client.shardingRollover(options.signal), options.signal);
    },
    detachAndCompress: (shardId: string, options: { signal?: AbortSignal } = {}): Promise<ShardArchiveResult> => {
      const client = this.embeddedForSharding();
      return this.run(() => client.shardingDetachAndCompress(shardId, options.signal), options.signal);
    },
    decompressAndAttach: (shardId: string, options: { signal?: AbortSignal } = {}): Promise<ShardInfo> => {
      const client = this.embeddedForSharding();
      return this.run(() => client.shardingDecompressAndAttach(shardId, options.signal), options.signal);
    },
    detach: (shardId: string, options: { signal?: AbortSignal } = {}): Promise<void> => {
      const client = this.embeddedForSharding();
      return this.run(() => client.shardingDetach(shardId, options.signal), options.signal);
    },
    attach: (shardId: string, path: string, options: { signal?: AbortSignal } = {}): Promise<void> => {
      const client = this.embeddedForSharding();
      return this.run(() => client.shardingAttach(shardId, path, options.signal), options.signal);
    },
  };

  // CAS / BLOB
  async blobStore(bytes: Uint8Array | string, mimeType?: string, options: { signal?: AbortSignal } = {}): Promise<BlobDescriptor> {
    if (this.closed) throw lifecycleError("CLOSED", "database is closing or closed", false);
    if (!(this.client instanceof EmbeddedClient)) {
      throw lifecycleError("NOT_SUPPORTED", "CAS blob operations are currently supported in embedded mode", false);
    }
    const client = this.client;
    return this.run(() => client.blobStore(toUint8Array(bytes), mimeType, options.signal), options.signal);
  }

  async blobGet(hash: string, options: { signal?: AbortSignal } = {}): Promise<Uint8Array | null> {
    if (this.closed) throw lifecycleError("CLOSED", "database is closing or closed", false);
    if (!(this.client instanceof EmbeddedClient)) {
      throw lifecycleError("NOT_SUPPORTED", "CAS blob operations are currently supported in embedded mode", false);
    }
    const client = this.client;
    return this.run(() => client.blobGet(hash, options.signal), options.signal);
  }

  blobPath(hash: string): string {
    if (this.closed) throw lifecycleError("CLOSED", "database is closing or closed", false);
    if (!(this.client instanceof EmbeddedClient)) {
      throw lifecycleError("NOT_SUPPORTED", "CAS blob operations are currently supported in embedded mode", false);
    }
    return this.client.blobPath(hash);
  }

  blobExists(hash: string): boolean {
    if (this.closed) throw lifecycleError("CLOSED", "database is closing or closed", false);
    if (!(this.client instanceof EmbeddedClient)) {
      throw lifecycleError("NOT_SUPPORTED", "CAS blob operations are currently supported in embedded mode", false);
    }
    return this.client.blobExists(hash);
  }

  async blobGc(options: { signal?: AbortSignal } = {}): Promise<BlobGcStats> {
    if (this.closed) throw lifecycleError("CLOSED", "database is closing or closed", false);
    if (!(this.client instanceof EmbeddedClient)) {
      throw lifecycleError("NOT_SUPPORTED", "CAS blob operations are currently supported in embedded mode", false);
    }
    const client = this.client;
    return this.run(() => client.blobGc(options.signal), options.signal);
  }

  async close(options: CloseOptions = {}): Promise<void> {
    options.signal?.throwIfAborted();
    const timeout = options.timeoutMs ?? 5_000;
    if (!Number.isInteger(timeout) || timeout < 0 || timeout > 0x7fff_ffff) {
      throw new RangeError("close timeoutMs must be an integer in 0..=2147483647");
    }
    if (!this.closing) {
      this.closed = true;
      // Include calls accepted by JS but not yet admitted by a native worker.
      // Caller timeout does not cancel these promises or the eventual cleanup.
      this.closing = Promise.allSettled([...this.pending]).then(() => this.client.close(0xffff_ffff));
    }

    const resolvers = Promise.withResolvers<void>();
    const timer = setTimeout(() => {
      resolvers.reject(lifecycleError("TIMEOUT", "close is still draining; operations are not cancelled", true));
    }, timeout);

    let onAbort: (() => void) | undefined;
    if (options.signal) {
      onAbort = () => {
        resolvers.reject(options.signal!.reason ?? new DOMException("The operation was aborted", "AbortError"));
      };
      options.signal.addEventListener("abort", onAbort, { once: true });
    }

    this.closing
      .then(() => resolvers.resolve(), (err) => resolvers.reject(err))
      .finally(() => {
        clearTimeout(timer);
        if (options.signal && onAbort) {
          options.signal.removeEventListener("abort", onAbort);
        }
      });

    return resolvers.promise;
  }

  private async run<T>(action: (client: Client) => Promise<T>, signal?: AbortSignal): Promise<T> {
    signal?.throwIfAborted();
    if (this.closed) throw lifecycleError("CLOSED", "database is closing or closed", false);

    const operation = Promise.resolve().then(() => action(this.client));
    this.pending.add(operation);

    const cleanup = () => this.pending.delete(operation);
    void operation.then(cleanup, cleanup);

    if (!signal) return await operation;

    const resolvers = Promise.withResolvers<T>();
    const onAbort = () => {
      resolvers.reject(signal.reason ?? new DOMException("The operation was aborted", "AbortError"));
    };
    signal.addEventListener("abort", onAbort, { once: true });

    operation
      .then(resolvers.resolve, resolvers.reject)
      .finally(() => signal.removeEventListener("abort", onAbort));

    return await resolvers.promise;
  }

  private async *kvScanStream(namespace: string, options: ScanStreamOptions<Uint8Array | string>): AsyncIterableIterator<KvEntry> {
    options.signal?.throwIfAborted();
    const batchSize = Math.max(1, options.batchSize ?? 100);
    let remaining = options.limit !== undefined ? Math.max(0, options.limit) : Infinity;
    let cursor = options.after ? toUint8Array(options.after) : undefined;

    while (remaining > 0) {
      options.signal?.throwIfAborted();
      const fetchLimit = Math.min(batchSize, remaining);
      const batch = await this.run(client => client.kvScan(namespace, cursor, fetchLimit, options.signal), options.signal);
      if (batch.length === 0) {
        break;
      }
      for (const entry of batch) {
        options.signal?.throwIfAborted();
        yield entry;
        remaining--;
        if (remaining <= 0) {
          break;
        }
      }
      if (batch.length < fetchLimit) {
        break;
      }
      cursor = batch[batch.length - 1]!.key;
    }
  }

  private async *paginateJsonStream<T = JsonValue>(
    fetchPage: (cursor: string | undefined, fetchLimit: number) => Promise<JsonEntry<T>[]>,
    options: ScanStreamOptions<string>
  ): AsyncIterableIterator<JsonEntry<T>> {
    options.signal?.throwIfAborted();
    const batchSize = Math.max(1, options.batchSize ?? 100);
    let remaining = options.limit !== undefined ? Math.max(0, options.limit) : Infinity;
    let cursor = options.after;

    while (remaining > 0) {
      options.signal?.throwIfAborted();
      const fetchLimit = Math.min(batchSize, remaining);
      const batch = await fetchPage(cursor, fetchLimit);
      if (batch.length === 0) {
        break;
      }
      for (const entry of batch) {
        options.signal?.throwIfAborted();
        yield entry;
        remaining--;
        if (remaining <= 0) {
          break;
        }
      }
      if (batch.length < fetchLimit) {
        break;
      }
      cursor = batch[batch.length - 1]!.id;
    }
  }

  private jsonScanStream<T = JsonValue>(space: string, options: ScanStreamOptions<string>): AsyncIterableIterator<JsonEntry<T>> {
    return this.paginateJsonStream<T>(
      (cursor, fetchLimit) => this.run(client => client.jsonScan<T>(space, cursor, fetchLimit, options.signal), options.signal),
      options
    );
  }

  private jsonScanPrefixStream<T = JsonValue>(space: string, prefix: string, options: ScanStreamOptions<string>): AsyncIterableIterator<JsonEntry<T>> {
    return this.paginateJsonStream<T>(
      (cursor, fetchLimit) => this.run(client => client.jsonScanPrefix<T>(space, prefix, cursor, fetchLimit, options.signal), options.signal),
      options
    );
  }

  private jsonFindStream<T = JsonValue>(space: string, query: Record<string, JsonValue>, options: ScanStreamOptions<string>): AsyncIterableIterator<JsonEntry<T>> {
    return this.paginateJsonStream<T>(
      (cursor, fetchLimit) => this.run(client => client.jsonFind<T>(space, query, cursor, fetchLimit, options.signal), options.signal),
      options
    );
  }
}

function lifecycleError(code: string, message: string, retryable: boolean): LiliaError {
  return new LiliaError({ code, message, retryable, request_id: getUuid() });
}

export const quantization = {
  toInt8(floats: Float32Array | number[]): { quantized: Int8Array; scale: number } {
    let maxAbs = 0;
    for (let i = 0; i < floats.length; i++) {
      const abs = Math.abs(floats[i]!);
      if (abs > maxAbs) maxAbs = abs;
    }
    if (maxAbs === 0) {
      return { quantized: new Int8Array(floats.length), scale: 1.0 };
    }
    const scale = maxAbs / 127.0;
    const invScale = 1.0 / scale;
    const quantized = new Int8Array(floats.length);
    for (let i = 0; i < floats.length; i++) {
      quantized[i] = Math.max(-128, Math.min(127, Math.round(floats[i]! * invScale)));
    }
    return { quantized, scale };
  },

  fromInt8(quantized: Int8Array, scale: number): Float32Array {
    const out = new Float32Array(quantized.length);
    for (let i = 0; i < quantized.length; i++) {
      out[i] = quantized[i]! * scale;
    }
    return out;
  },

  toBit(floats: Float32Array | number[]): Uint8Array {
    const numBytes = Math.ceil(floats.length / 8);
    const packed = new Uint8Array(numBytes);
    for (let i = 0; i < floats.length; i++) {
      if (floats[i]! > 0) {
        const byteIdx = Math.floor(i / 8);
        const bitIdx = 7 - (i % 8);
        packed[byteIdx]! |= 1 << bitIdx;
      }
    }
    return packed;
  },

  cosineDistance(a: Float32Array | number[], b: Float32Array | number[]): number {
    const len = Math.min(a.length, b.length);
    if (len === 0) return 1.0;
    let dot = 0,
      normA = 0,
      normB = 0;
    for (let i = 0; i < len; i++) {
      const va = a[i]!,
        vb = b[i]!;
      dot += va * vb;
      normA += va * va;
      normB += vb * vb;
    }
    if (normA <= 0 || normB <= 0) return 1.0;
    const sim = dot / (Math.sqrt(normA) * Math.sqrt(normB));
    return Math.max(0, Math.min(2, 1.0 - sim));
  },
};

export const timeseries = {
  downsample: nativeTsDownsamplePoints,
};

export const geo = {
  haversineDistance: nativeGeoHaversineDistance,
  circleToGeopoly: nativeGeoCircleToGeopoly,
  coordsToGeopoly: nativeGeoCoordsToGeopoly,
};
