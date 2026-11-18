import { createRequire } from "node:module";
import { existsSync, copyFileSync, statSync } from "node:fs";
import { encode, decode } from "@msgpack/msgpack";
import type {
  ShardArchiveResult,
  ShardInfo,
  BatchOperation,
  BlobDescriptor,
  BlobGcStats,
  CausalRelation,
  CentralityType,
  CheckpointStats,
  ComponentType,
  ConflictRecord,
  ConflictResolutionStrategy,
  CrdtCounterState,
  CrdtOrSetState,
  CypherResult,
  DatabaseEvent,
  AutoEmbedConfig,
  DictStats,
  DownsampleAggregation,
  EmbedStatus,
  FtsHit,
  GeofenceMatch,
  GpsPoint,
  GraphDirection,
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
  KvEntry,
  LiveQueryDiff,
  LiveQuerySubscription,
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
  VectorMetric,
  WatchSubscription,
  WeightedPath,
} from "./types.js";
import { LiliaError } from "./types.js";
import { jsonNumbers, readVersion } from "./version.js";
import {
  batchOpToWire,
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

export { mapDatabaseEvent, mapSqlQueryResult, vectorToBytes };
export type { CausalRelation };

interface NativeKvEntry {
  namespace: string;
  key: Buffer;
  value: Buffer;
  version: string;
  expiresAtMs?: number;
}
type NativeRecord = Record<string, unknown>;
interface NativeSubscriptionHandle {
  unsubscribe(): void;
}

interface NativeDatabaseHandle {
  kvGet(namespace: string, key: Buffer): Promise<NativeKvEntry | null>;
  kvGetSync(namespace: string, key: Buffer): NativeKvEntry | null;
  kvScan(namespace: string, after: Buffer | undefined, limit: number): Promise<NativeKvEntry[]>;
  kvScanSync(namespace: string, after: Buffer | undefined, limit: number): NativeKvEntry[];

  jsonGet(space: string, id: string): Promise<string | null>;
  jsonGetSync(space: string, id: string): string | null;
  jsonExtract(space: string, id: string, path: string): Promise<string | null>;
  jsonExtractSync(space: string, id: string, path: string): string | null;
  jsonScan(space: string, after: string | undefined, limit: number): Promise<string>;
  jsonScanSync(space: string, after: string | undefined, limit: number): string;
  jsonScanPrefix(space: string, prefix: string, afterId: string | undefined, limit: number): Promise<string>;
  jsonScanPrefixSync(space: string, prefix: string, afterId: string | undefined, limit: number): string;
  jsonCreateIndex(space: string, name: string, path: string, filter?: string): Promise<void>;
  jsonCreateIndexSync(space: string, name: string, path: string, filter?: string): void;
  jsonDropIndex(space: string, name: string): Promise<boolean>;
  jsonDropIndexSync(space: string, name: string): boolean;
  jsonListIndexes(space: string): Promise<string>;
  jsonListIndexesSync(space: string): string;
  jsonScanByIndex(space: string, indexName: string, value: string, after: string | undefined, limit: number): Promise<string>;
  jsonScanByIndexSync(space: string, indexName: string, value: string, after: string | undefined, limit: number): string;
  jsonFind(space: string, queryJson: string, after: string | undefined, limit: number): Promise<string>;
  jsonFindSync(space: string, queryJson: string, after: string | undefined, limit: number): string;
  jsonGetAtVersion(space: string, id: string, version: string): Promise<string | null>;
  jsonGetAtVersionSync(space: string, id: string, version: string): string | null;
  jsonGetAtTimestamp(space: string, id: string, timestampMs: number): Promise<string | null>;
  jsonGetAtTimestampSync(space: string, id: string, timestampMs: number): string | null;
  jsonHistory(space: string, id: string): Promise<string>;
  jsonHistorySync(space: string, id: string): string;
  jsonDiff(space: string, id: string, fromVersion: string, toVersion: string): Promise<string>;
  jsonDiffSync(space: string, id: string, fromVersion: string, toVersion: string): string;
  jsonTrainDictionary(space: string, sampleCount: number | undefined, dictSize: number | undefined): Promise<string>;
  jsonTrainDictionarySync(space: string, sampleCount: number | undefined, dictSize: number | undefined): string;
  jsonSetAutoTrain(space: string, enabled: boolean): Promise<void>;
  jsonSetAutoTrainSync(space: string, enabled: boolean): void;
  jsonIsAutoTrainEnabled(space: string): Promise<boolean>;
  jsonIsAutoTrainEnabledSync(space: string): boolean;
  jsonPutHlc(space: string, id: string, valueJson: string, hlc: string | undefined, parentHlc: string | undefined, strategy: string | undefined): Promise<string>;
  jsonPutHlcSync(space: string, id: string, valueJson: string, hlc: string | undefined, parentHlc: string | undefined, strategy: string | undefined): string;
  jsonGetHlc(space: string, id: string): Promise<string | null>;
  jsonGetHlcSync(space: string, id: string): string | null;

  batch(operations: string): Promise<string>;
  batchSync(operations: string): string;
  batchMsgpack(payload: Uint8Array): Promise<Buffer>;
  batchMsgpackSync(payload: Uint8Array): Buffer;
  sqlQuery(sql: string, paramsJson?: string): Promise<string>;
  sqlQuerySync(sql: string, paramsJson?: string): string;

  graphNeighbors(graph: string, node: string, direction: string | undefined, kind: string | undefined): Promise<string[]>;
  graphNeighborsSync(graph: string, node: string, direction: string | undefined, kind: string | undefined): string[];
  graphReachability(graph: string, startNodes: string[], direction: string | undefined): Promise<string[]>;
  graphReachabilitySync(graph: string, startNodes: string[], direction: string | undefined): string[];
  graphGetNode(graph: string, id: string): Promise<string | null>;
  graphGetNodeSync(graph: string, id: string): string | null;
  graphGetNodeDocument(graph: string, id: string): Promise<string | null>;
  graphGetNodeDocumentSync(graph: string, id: string): string | null;
  graphDeleteNode(graph: string, id: string): Promise<boolean>;
  graphDeleteNodeSync(graph: string, id: string): boolean;
  graphListNodes(graph: string, kind: string | undefined, limit: number | undefined): Promise<string>;
  graphListNodesSync(graph: string, kind: string | undefined, limit: number | undefined): string;
  graphRemoveEdge(graph: string, source: string, target: string, kind: string): Promise<boolean>;
  graphRemoveEdgeSync(graph: string, source: string, target: string, kind: string): boolean;
  graphListEdges(graph: string, source: string | undefined, target: string | undefined, kind: string | undefined, limit: number | undefined): Promise<string>;
  graphListEdgesSync(graph: string, source: string | undefined, target: string | undefined, kind: string | undefined, limit: number | undefined): string;
  graphImpactAnalysis(graph: string, changedNodes: string[]): Promise<string>;
  graphImpactAnalysisSync(graph: string, changedNodes: string[]): string;
  graphShortestPath(graph: string, start: string, target: string, direction: string | undefined, kindFilter: string | undefined): Promise<string | null>;
  graphShortestPathSync(graph: string, start: string, target: string, direction: string | undefined, kindFilter: string | undefined): string | null;
  graphDijkstra(graph: string, start: string, target: string | undefined, direction: string | undefined, kindFilter: string | undefined): Promise<string | null>;
  graphDijkstraSync(graph: string, start: string, target: string | undefined, direction: string | undefined, kindFilter: string | undefined): string | null;
  graphAllShortestPaths(graph: string, start: string, direction: string | undefined, kindFilter: string | undefined): Promise<string>;
  graphAllShortestPathsSync(graph: string, start: string, direction: string | undefined, kindFilter: string | undefined): string;
  graphAstarSpatial(graph: string, start: string, target: string, direction: string | undefined, kindFilter: string | undefined): Promise<string | null>;
  graphAstarSpatialSync(graph: string, start: string, target: string, direction: string | undefined, kindFilter: string | undefined): string | null;
  graphPagerank(graph: string, configJson: string | undefined): Promise<string>;
  graphPagerankSync(graph: string, configJson: string | undefined): string;
  graphCentrality(graph: string, centrality: string, direction: string | undefined, kindFilter: string | undefined, normalized: boolean | undefined): Promise<string>;
  graphCentralitySync(graph: string, centrality: string, direction: string | undefined, kindFilter: string | undefined, normalized: boolean | undefined): string;
  graphHasCycle(graph: string, direction: string | undefined, kindFilter: string | undefined): Promise<boolean>;
  graphHasCycleSync(graph: string, direction: string | undefined, kindFilter: string | undefined): boolean;
  graphFindCycles(graph: string, maxCycles: number | undefined, kindFilter: string | undefined): Promise<string>;
  graphFindCyclesSync(graph: string, maxCycles: number | undefined, kindFilter: string | undefined): string;
  graphConnectedComponents(graph: string, componentType: string, kindFilter: string | undefined): Promise<string>;
  graphConnectedComponentsSync(graph: string, componentType: string, kindFilter: string | undefined): string;
  graphCypher(graph: string, query: string): Promise<string>;
  graphCypherSync(graph: string, query: string): string;

  ftsDelete(collection: string, docId: string): Promise<boolean>;
  ftsDeleteSync(collection: string, docId: string): boolean;
  ftsSearch(collection: string, query: string, limit: number | undefined): Promise<NativeRecord[]>;
  ftsSearchSync(collection: string, query: string, limit: number | undefined): NativeRecord[];

  vectorGet(collection: string, id: string): Promise<Buffer | null>;
  vectorGetSync(collection: string, id: string): Buffer | null;
  vectorSearch(collection: string, query: Buffer, dim: number, elementType: string | undefined, metric: string | undefined, limit: number | undefined): Promise<NativeRecord[]>;
  vectorSearchSync(collection: string, query: Buffer, dim: number, elementType: string | undefined, metric: string | undefined, limit: number | undefined): NativeRecord[];
  vectorSearchText(collection: string, text: string, limit: number | undefined): Promise<NativeRecord[]>;
  vectorSearchTextSync(collection: string, text: string, limit: number | undefined): NativeRecord[];
  waitForEmbeddings(timeoutMs: number | undefined): Promise<void>;
  waitForEmbeddingsSync(timeoutMs: number | undefined): void;
  jsonSetAutoEmbedSync(collection: string, configJson: string | undefined): void;
  jsonEmbedStatusSync(collection: string): string;
  vectorCreateIndex(collection: string, configJson: string): Promise<void>;
  vectorCreateIndexSync(collection: string, configJson: string): void;
  vectorGetIndexInfo(collection: string): Promise<string | null>;
  vectorGetIndexInfoSync(collection: string): string | null;
  vectorDropIndex(collection: string): Promise<boolean>;
  vectorDropIndexSync(collection: string): boolean;
  vectorSearchFiltered(collection: string, query: Buffer, dim: number, elementType: string | undefined, metric: string | undefined, candidateIds: string[], limit: number | undefined): Promise<NativeRecord[]>;
  vectorSearchFilteredSync(collection: string, query: Buffer, dim: number, elementType: string | undefined, metric: string | undefined, candidateIds: string[], limit: number | undefined): NativeRecord[];
  vectorSearchInGraphNeighbors(collection: string, graph: string, startNode: string, direction: string | undefined, query: Buffer, dim: number, elementType: string | undefined, metric: string | undefined, limit: number | undefined): Promise<NativeRecord[]>;
  vectorSearchInGraphNeighborsSync(collection: string, graph: string, startNode: string, direction: string | undefined, query: Buffer, dim: number, elementType: string | undefined, metric: string | undefined, limit: number | undefined): NativeRecord[];
  vectorSearchByJsonIndex(collection: string, space: string, indexName: string, indexValue: string, query: Buffer, dim: number, elementType: string | undefined, metric: string | undefined, limit: number | undefined): Promise<NativeRecord[]>;
  vectorSearchByJsonIndexSync(collection: string, space: string, indexName: string, indexValue: string, query: Buffer, dim: number, elementType: string | undefined, metric: string | undefined, limit: number | undefined): NativeRecord[];
  hybridSearch(collection: string, ftsQuery: string | undefined, vectorQuery: Buffer | undefined, dim: number | undefined, elementType: string | undefined, metric: string | undefined, rrfK: number | undefined, ftsWeight: number | undefined, vectorWeight: number | undefined, limit: number | undefined): Promise<NativeRecord[]>;
  hybridSearchSync(collection: string, ftsQuery: string | undefined, vectorQuery: Buffer | undefined, dim: number | undefined, elementType: string | undefined, metric: string | undefined, rrfK: number | undefined, ftsWeight: number | undefined, vectorWeight: number | undefined, limit: number | undefined): NativeRecord[];

  tsAppendPoints(seriesId: string, pointsJson: string, resolutionMs: number | undefined): Promise<void>;
  tsAppendPointsSync(seriesId: string, pointsJson: string, resolutionMs: number | undefined): void;
  tsQueryRange(seriesId: string, startTime: number, endTime: number, resolutionMs: number | undefined): Promise<string>;
  tsQueryRangeSync(seriesId: string, startTime: number, endTime: number, resolutionMs: number | undefined): string;
  tsQueryAuto(seriesId: string, startTime: number, endTime: number, maxPoints: number): Promise<string>;
  tsQueryAutoSync(seriesId: string, startTime: number, endTime: number, maxPoints: number): string;
  tsQueryStats(seriesId: string, startTime: number, endTime: number, resolutionMs: number | undefined): Promise<string | null>;
  tsQueryStatsSync(seriesId: string, startTime: number, endTime: number, resolutionMs: number | undefined): string | null;
  tsSetPolicy(policyJson: string): Promise<void>;
  tsSetPolicySync(policyJson: string): void;
  tsGetPolicy(seriesPattern: string): Promise<string | null>;
  tsGetPolicySync(seriesPattern: string): string | null;
  tsListPolicies(): Promise<string>;
  tsListPoliciesSync(): string;
  tsDeletePolicy(seriesPattern: string): Promise<boolean>;
  tsDeletePolicySync(seriesPattern: string): boolean;
  tsApplyRetention(seriesId: string | undefined, policyJson: string | undefined, nowMs: number | undefined): Promise<string>;
  tsApplyRetentionSync(seriesId: string | undefined, policyJson: string | undefined, nowMs: number | undefined): string;

  geoAddFence(id: string, name: string, category: string, polygonJson: string, metadata: string | undefined): Promise<void>;
  geoAddFenceSync(id: string, name: string, category: string, polygonJson: string, metadata: string | undefined): void;
  geoRemoveFence(id: string): Promise<boolean>;
  geoRemoveFenceSync(id: string): boolean;
  geoContainsPoint(polygonJson: string, lon: number, lat: number): Promise<boolean>;
  geoContainsPointSync(polygonJson: string, lon: number, lat: number): boolean;
  geoFindFencesForPoint(lon: number, lat: number): Promise<string>;
  geoFindFencesForPointSync(lon: number, lat: number): string;
  geoRecordGpsPoints(trackId: string, pointsJson: string): Promise<string>;
  geoRecordGpsPointsSync(trackId: string, pointsJson: string): string;
  geoGetTrackPoints(trackId: string, startTime: number | undefined, endTime: number | undefined): Promise<string>;
  geoGetTrackPointsSync(trackId: string, startTime: number | undefined, endTime: number | undefined): string;
  geoTrackDistance(trackId: string): Promise<number>;
  geoTrackDistanceSync(trackId: string): number;

  crdtCounterGet(space: string, id: string): Promise<number>;
  crdtCounterGetSync(space: string, id: string): number;
  crdtCounterState(space: string, id: string): Promise<string>;
  crdtCounterStateSync(space: string, id: string): string;
  crdtCounterMerge(space: string, id: string, remoteJson: string): Promise<number>;
  crdtCounterMergeSync(space: string, id: string, remoteJson: string): number;
  crdtSetRead(space: string, id: string): Promise<string[]>;
  crdtSetReadSync(space: string, id: string): string[];
  crdtSetContains(space: string, id: string, element: string): Promise<boolean>;
  crdtSetContainsSync(space: string, id: string, element: string): boolean;
  crdtSetState(space: string, id: string): Promise<string>;
  crdtSetStateSync(space: string, id: string): string;
  crdtSetPrune(space: string, id: string, cutoffMs: number): Promise<number>;
  crdtSetPruneSync(space: string, id: string, cutoffMs: number): number;
  crdtSetMerge(space: string, id: string, remoteJson: string): Promise<void>;
  crdtSetMergeSync(space: string, id: string, remoteJson: string): void;
  crdtJsonGet(space: string, id: string): Promise<string | null>;
  crdtJsonGetSync(space: string, id: string): string | null;
  crdtJsonState(space: string, id: string): Promise<string | null>;
  crdtJsonStateSync(space: string, id: string): string | null;
  crdtJsonMerge(space: string, id: string, remoteJson: string): Promise<string>;
  crdtJsonMergeSync(space: string, id: string, remoteJson: string): string;

  hlcNext(): Promise<string>;
  hlcNextSync(): string;
  hlcObserve(remote: string): Promise<string>;
  hlcObserveSync(remote: string): string;
  listConflicts(space: string | undefined): Promise<string>;
  listConflictsSync(space: string | undefined): string;
  resolveConflict(conflictId: number, resolvedValueJson: string): Promise<void>;
  resolveConflictSync(conflictId: number, resolvedValueJson: string): void;

  undo(): Promise<string | null>;
  undoSync(): string | null;
  redo(): Promise<string | null>;
  redoSync(): string | null;
  undoHistory(): Promise<string>;
  undoHistorySync(): string;

  watch(callback: (event: string) => void, namespace?: string): Promise<NativeSubscriptionHandle>;
  liveQuery(sql: string, callback: (diff: string) => void, paramsJson?: string): Promise<NativeSubscriptionHandle>;
  createMaterializedView(name: string, query: string): Promise<void>;
  createMaterializedViewSync(name: string, query: string): void;
  dropMaterializedView(name: string): Promise<void>;
  dropMaterializedViewSync(name: string): void;
  refreshMaterializedView(name: string): Promise<void>;
  refreshMaterializedViewSync(name: string): void;
  listMaterializedViews(): Promise<string>;
  listMaterializedViewsSync(): string;

  quickCheck(): Promise<boolean>;
  quickCheckSync(): boolean;
  integrityCheck(): Promise<boolean>;
  integrityCheckSync(): boolean;
  checkpoint(): Promise<void>;
  checkpointSync(): void;
  checkpointStats(): Promise<string>;
  checkpointStatsSync(): string;
  storageStats(): Promise<string>;
  storageStatsSync(): string;
  backup(destination: string): Promise<void>;
  backupSync(destination: string): void;
  blobStore(bytes: Buffer, mimeType: string | undefined): Promise<string>;
  blobStoreSync(bytes: Buffer, mimeType: string | undefined): string;
  blobGet(hash: string): Promise<Buffer | null>;
  blobGetSync(hash: string): Buffer | null;
  blobPath(hash: string): string;
  blobExists(hash: string): boolean;
  blobGc(): Promise<string>;
  blobGcSync(): string;
  shardingListShards(): Promise<string>;
  shardingListShardsSync(): string;
  shardingActiveShard(): Promise<string>;
  shardingActiveShardSync(): string;
  shardingRollover(): Promise<string>;
  shardingRolloverSync(): string;
  shardingDetachAndCompress(shardId: string): Promise<string>;
  shardingDetachAndCompressSync(shardId: string): string;
  shardingDecompressAndAttach(shardId: string): Promise<string>;
  shardingDecompressAndAttachSync(shardId: string): string;
  shardingDetach(shardId: string): Promise<void>;
  shardingDetachSync(shardId: string): void;
  shardingAttach(shardId: string, path: string): Promise<void>;
  shardingAttachSync(shardId: string, path: string): void;
  close(timeoutMs?: number): Promise<void>;
}

interface NativeModule {
  NativeDatabase: { open(options: Record<string, unknown>): Promise<NativeDatabaseHandle> };
  tsDownsamplePoints?(pointsJson: string, targetBucketMs: number, aggregation?: string): string;
  geoHaversineDistance?(lat1: number, lon1: number, lat2: number, lon2: number): number;
  geoCircleToGeopoly?(centerLon: number, centerLat: number, radiusMeters: number, nVertices?: number): string;
  geoCoordsToGeopoly?(coordsJson: string): string;
}

const require = createRequire(import.meta.url);
const MSGPACK_ENCODE_OPTS = { useBigInt64: true, ignoreUndefined: true } as const;

export function tryLoadNative(): NativeModule | undefined {
  try {
    return loadNative();
  } catch {
    return undefined;
  }
}

export function nativeTsDownsamplePoints(
  points: TimeSeriesPoint[],
  targetBucketMs: number,
  aggregation?: DownsampleAggregation
): TimeSeriesPoint[] {
  const mod = tryLoadNative();
  if (mod?.tsDownsamplePoints) {
    return mapTsPoints(JSON.parse(mod.tsDownsamplePoints(JSON.stringify(points), targetBucketMs, aggregation)) as NativeRecord[]);
  }
  if (points.length === 0 || targetBucketMs <= 0) return points.map((p) => ({ ...p }));
  const sorted = [...points].sort((a, b) => a.timestamp - b.timestamp);
  const agg = aggregation ?? "avg";
  const result: TimeSeriesPoint[] = [];
  let curBucket = Math.trunc(sorted[0]!.timestamp / targetBucketMs) * targetBucketMs;
  let vals: number[] = [];
  const flush = (bucket: number, bucketVals: number[]) => {
    if (bucketVals.length === 0) return;
    let out = 0;
    switch (agg) {
      case "min":
        out = Math.min(...bucketVals);
        break;
      case "max":
        out = Math.max(...bucketVals);
        break;
      case "sum":
        out = bucketVals.reduce((a, b) => a + b, 0);
        break;
      case "first":
        out = bucketVals[0]!;
        break;
      case "last":
        out = bucketVals[bucketVals.length - 1]!;
        break;
      case "count":
        out = bucketVals.length;
        break;
      case "avg":
      default:
        out = bucketVals.reduce((a, b) => a + b, 0) / bucketVals.length;
        break;
    }
    result.push({ timestamp: bucket, value: out });
  };
  for (const pt of sorted) {
    const bucket = Math.trunc(pt.timestamp / targetBucketMs) * targetBucketMs;
    if (bucket !== curBucket) {
      flush(curBucket, vals);
      curBucket = bucket;
      vals = [pt.value];
    } else {
      vals.push(pt.value);
    }
  }
  flush(curBucket, vals);
  return result;
}

export function nativeGeoHaversineDistance(lat1: number, lon1: number, lat2: number, lon2: number): number {
  const mod = tryLoadNative();
  if (mod?.geoHaversineDistance) return mod.geoHaversineDistance(lat1, lon1, lat2, lon2);
  const R = 6_371_000.0;
  const toRad = Math.PI / 180;
  const dLat = (lat2 - lat1) * toRad;
  const dLon = (lon2 - lon1) * toRad;
  const a =
    Math.sin(dLat / 2) ** 2 +
    Math.cos(lat1 * toRad) * Math.cos(lat2 * toRad) * Math.sin(dLon / 2) ** 2;
  return R * 2 * Math.atan2(Math.sqrt(a), Math.sqrt(1 - a));
}

export function nativeGeoCircleToGeopoly(
  centerLon: number,
  centerLat: number,
  radiusMeters: number,
  nVertices?: number
): string {
  const mod = tryLoadNative();
  if (mod?.geoCircleToGeopoly) return mod.geoCircleToGeopoly(centerLon, centerLat, radiusMeters, nVertices);
  const n = Math.max(4, nVertices ?? 16);
  const metersPerDegLat = 111_320.0;
  const metersPerDegLon = 111_320.0 * Math.cos((centerLat * Math.PI) / 180.0);
  const pts: [number, number][] = [];
  for (let i = 0; i < n; i++) {
    const theta = (2 * Math.PI * i) / n;
    pts.push([
      centerLon + (radiusMeters * Math.cos(theta)) / metersPerDegLon,
      centerLat + (radiusMeters * Math.sin(theta)) / metersPerDegLat,
    ]);
  }
  pts.push(pts[0]!);
  return JSON.stringify(pts);
}

export function nativeGeoCoordsToGeopoly(coords: ReadonlyArray<[number, number]>): string {
  const mod = tryLoadNative();
  if (mod?.geoCoordsToGeopoly) return mod.geoCoordsToGeopoly(JSON.stringify(coords));
  const ring = coords.map(([lon, lat]) => [lon, lat]);
  if (ring.length > 0) {
    const first = ring[0]!;
    const last = ring[ring.length - 1]!;
    if (first[0] !== last[0] || first[1] !== last[1]) {
      ring.push([first[0]!, first[1]!]);
    }
  }
  return JSON.stringify(ring);
}

export class EmbeddedClient {
  private constructor(private readonly native: NativeDatabaseHandle) {}

  static async open(options: Record<string, unknown>, signal?: AbortSignal): Promise<EmbeddedClient> {
    signal?.throwIfAborted();
    const module = loadNative();
    return new EmbeddedClient(await nativeCall(module.NativeDatabase.open(options), signal));
  }

  // KV
  async kvGet(namespace: string, key: Uint8Array, signal?: AbortSignal): Promise<KvEntry | undefined> {
    const entry = await nativeCall(this.native.kvGet(namespace, Buffer.from(key)), signal);
    return entry ? mapKv(entry) : undefined;
  }
  kvGetSync(namespace: string, key: Uint8Array): KvEntry | undefined {
    const entry = nativeSyncCall(() => this.native.kvGetSync(namespace, Buffer.from(key)));
    return entry ? mapKv(entry) : undefined;
  }
  async kvScan(namespace: string, after: Uint8Array | undefined, limit: number, signal?: AbortSignal): Promise<KvEntry[]> {
    return (await nativeCall(this.native.kvScan(namespace, after ? Buffer.from(after) : undefined, limit), signal)).map(mapKv);
  }
  kvScanSync(namespace: string, after: Uint8Array | undefined, limit: number): KvEntry[] {
    return nativeSyncCall(() => this.native.kvScanSync(namespace, after ? Buffer.from(after) : undefined, limit)).map(mapKv);
  }

  // JSON + Time-Travel + Zstd + HLC
  async jsonGet<T = JsonValue>(space: string, id: string, signal?: AbortSignal): Promise<JsonEntry<T> | undefined> {
    const entry = await nativeCall(this.native.jsonGet(space, id), signal);
    return entry ? parseJsonEntry<T>(entry) : undefined;
  }
  jsonGetSync<T = JsonValue>(space: string, id: string): JsonEntry<T> | undefined {
    const entry = nativeSyncCall(() => this.native.jsonGetSync(space, id));
    return entry ? parseJsonEntry<T>(entry) : undefined;
  }
  async jsonExtract(space: string, id: string, path: string, signal?: AbortSignal): Promise<JsonValue | undefined> {
    const raw = await nativeCall(this.native.jsonExtract(space, id, path), signal);
    return raw == null ? undefined : (JSON.parse(raw) as JsonValue);
  }
  jsonExtractSync(space: string, id: string, path: string): JsonValue | undefined {
    const raw = nativeSyncCall(() => this.native.jsonExtractSync(space, id, path));
    return raw == null ? undefined : (JSON.parse(raw) as JsonValue);
  }
  async jsonScan<T = JsonValue>(space: string, after: string | undefined, limit: number, signal?: AbortSignal): Promise<JsonEntry<T>[]> {
    return (JSON.parse(await nativeCall(this.native.jsonScan(space, after, limit), signal)) as NativeRecord[]).map((r) => mapJsonEntry<T>(r));
  }
  jsonScanSync<T = JsonValue>(space: string, after: string | undefined, limit: number): JsonEntry<T>[] {
    return (JSON.parse(nativeSyncCall(() => this.native.jsonScanSync(space, after, limit))) as NativeRecord[]).map((r) => mapJsonEntry<T>(r));
  }
  async jsonScanPrefix<T = JsonValue>(space: string, prefix: string, afterId: string | undefined, limit: number, signal?: AbortSignal): Promise<JsonEntry<T>[]> {
    return (JSON.parse(await nativeCall(this.native.jsonScanPrefix(space, prefix, afterId, limit), signal)) as NativeRecord[]).map((r) => mapJsonEntry<T>(r));
  }
  jsonScanPrefixSync<T = JsonValue>(space: string, prefix: string, afterId: string | undefined, limit: number): JsonEntry<T>[] {
    return (JSON.parse(nativeSyncCall(() => this.native.jsonScanPrefixSync(space, prefix, afterId, limit))) as NativeRecord[]).map((r) => mapJsonEntry<T>(r));
  }
  async jsonCreateIndex(space: string, name: string, path: string, filter?: string, signal?: AbortSignal): Promise<void> {
    await nativeCall(this.native.jsonCreateIndex(space, name, path, filter), signal);
  }
  jsonCreateIndexSync(space: string, name: string, path: string, filter?: string): void {
    nativeSyncCall(() => this.native.jsonCreateIndexSync(space, name, path, filter));
  }
  async jsonDropIndex(space: string, name: string, signal?: AbortSignal): Promise<boolean> {
    return nativeCall(this.native.jsonDropIndex(space, name), signal);
  }
  jsonDropIndexSync(space: string, name: string): boolean {
    return nativeSyncCall(() => this.native.jsonDropIndexSync(space, name));
  }
  async jsonListIndexes(space: string, signal?: AbortSignal): Promise<JsonIndexInfo[]> {
    return mapJsonIndexList(JSON.parse(await nativeCall(this.native.jsonListIndexes(space), signal)) as unknown[]);
  }
  jsonListIndexesSync(space: string): JsonIndexInfo[] {
    return mapJsonIndexList(JSON.parse(nativeSyncCall(() => this.native.jsonListIndexesSync(space))) as unknown[]);
  }
  async jsonScanByIndex<T = JsonValue>(space: string, indexName: string, value: string, after: string | undefined, limit: number, signal?: AbortSignal): Promise<JsonEntry<T>[]> {
    return (JSON.parse(await nativeCall(this.native.jsonScanByIndex(space, indexName, value, after, limit), signal)) as NativeRecord[]).map((r) => mapJsonEntry<T>(r));
  }
  jsonScanByIndexSync<T = JsonValue>(space: string, indexName: string, value: string, after: string | undefined, limit: number): JsonEntry<T>[] {
    return (JSON.parse(nativeSyncCall(() => this.native.jsonScanByIndexSync(space, indexName, value, after, limit))) as NativeRecord[]).map((r) => mapJsonEntry<T>(r));
  }
  async jsonFind<T = JsonValue>(space: string, query: Record<string, JsonValue>, after: string | undefined, limit: number, signal?: AbortSignal): Promise<JsonEntry<T>[]> {
    return (JSON.parse(await nativeCall(this.native.jsonFind(space, JSON.stringify(query), after, limit), signal)) as NativeRecord[]).map((r) => mapJsonEntry<T>(r));
  }
  jsonFindSync<T = JsonValue>(space: string, query: Record<string, JsonValue>, after: string | undefined, limit: number): JsonEntry<T>[] {
    return (JSON.parse(nativeSyncCall(() => this.native.jsonFindSync(space, JSON.stringify(query), after, limit))) as NativeRecord[]).map((r) => mapJsonEntry<T>(r));
  }
  async jsonGetAtVersion<T = JsonValue>(space: string, id: string, version: bigint, signal?: AbortSignal): Promise<JsonEntry<T> | undefined> {
    const raw = await nativeCall(this.native.jsonGetAtVersion(space, id, version.toString()), signal);
    return raw ? parseJsonEntry<T>(raw) : undefined;
  }
  jsonGetAtVersionSync<T = JsonValue>(space: string, id: string, version: bigint): JsonEntry<T> | undefined {
    const raw = nativeSyncCall(() => this.native.jsonGetAtVersionSync(space, id, version.toString()));
    return raw ? parseJsonEntry<T>(raw) : undefined;
  }
  async jsonGetAtTimestamp<T = JsonValue>(space: string, id: string, timestampMs: number, signal?: AbortSignal): Promise<JsonEntry<T> | undefined> {
    const raw = await nativeCall(this.native.jsonGetAtTimestamp(space, id, timestampMs), signal);
    return raw ? parseJsonEntry<T>(raw) : undefined;
  }
  jsonGetAtTimestampSync<T = JsonValue>(space: string, id: string, timestampMs: number): JsonEntry<T> | undefined {
    const raw = nativeSyncCall(() => this.native.jsonGetAtTimestampSync(space, id, timestampMs));
    return raw ? parseJsonEntry<T>(raw) : undefined;
  }
  async jsonHistory(space: string, id: string, signal?: AbortSignal): Promise<JsonRevisionInfo[]> {
    return (JSON.parse(await nativeCall(this.native.jsonHistory(space, id), signal)) as NativeRecord[]).map(mapJsonRevision);
  }
  jsonHistorySync(space: string, id: string): JsonRevisionInfo[] {
    return (JSON.parse(nativeSyncCall(() => this.native.jsonHistorySync(space, id))) as NativeRecord[]).map(mapJsonRevision);
  }
  async jsonDiff(space: string, id: string, fromVersion: bigint, toVersion: bigint, signal?: AbortSignal): Promise<JsonValue> {
    return JSON.parse(await nativeCall(this.native.jsonDiff(space, id, fromVersion.toString(), toVersion.toString()), signal)) as JsonValue;
  }
  jsonDiffSync(space: string, id: string, fromVersion: bigint, toVersion: bigint): JsonValue {
    return JSON.parse(nativeSyncCall(() => this.native.jsonDiffSync(space, id, fromVersion.toString(), toVersion.toString()))) as JsonValue;
  }
  async jsonTrainDictionary(space: string, sampleCount?: number, dictSize?: number, signal?: AbortSignal): Promise<DictStats> {
    return mapDictStats(JSON.parse(await nativeCall(this.native.jsonTrainDictionary(space, sampleCount, dictSize), signal)) as NativeRecord);
  }
  jsonTrainDictionarySync(space: string, sampleCount?: number, dictSize?: number): DictStats {
    return mapDictStats(JSON.parse(nativeSyncCall(() => this.native.jsonTrainDictionarySync(space, sampleCount, dictSize))) as NativeRecord);
  }
  async jsonSetAutoTrain(space: string, enabled: boolean, signal?: AbortSignal): Promise<void> {
    await nativeCall(this.native.jsonSetAutoTrain(space, enabled), signal);
  }
  jsonSetAutoTrainSync(space: string, enabled: boolean): void {
    nativeSyncCall(() => this.native.jsonSetAutoTrainSync(space, enabled));
  }
  async jsonIsAutoTrainEnabled(space: string, signal?: AbortSignal): Promise<boolean> {
    return nativeCall(this.native.jsonIsAutoTrainEnabled(space), signal);
  }
  jsonIsAutoTrainEnabledSync(space: string): boolean {
    return nativeSyncCall(() => this.native.jsonIsAutoTrainEnabledSync(space));
  }
  async jsonPutHlc(space: string, id: string, value: JsonValue, hlc?: string, parentHlc?: string, strategy?: ConflictResolutionStrategy, signal?: AbortSignal): Promise<HlcMutationOutcome> {
    return mapHlcMutationOutcome(JSON.parse(await nativeCall(this.native.jsonPutHlc(space, id, JSON.stringify(value), hlc, parentHlc, strategy), signal)) as NativeRecord);
  }
  jsonPutHlcSync(space: string, id: string, value: JsonValue, hlc?: string, parentHlc?: string, strategy?: ConflictResolutionStrategy): HlcMutationOutcome {
    return mapHlcMutationOutcome(JSON.parse(nativeSyncCall(() => this.native.jsonPutHlcSync(space, id, JSON.stringify(value), hlc, parentHlc, strategy))) as NativeRecord);
  }
  async jsonGetHlc(space: string, id: string, signal?: AbortSignal): Promise<HlcRecord | undefined> {
    const raw = await nativeCall(this.native.jsonGetHlc(space, id), signal);
    return raw ? mapHlcRecord(JSON.parse(raw) as NativeRecord) : undefined;
  }
  jsonGetHlcSync(space: string, id: string): HlcRecord | undefined {
    const raw = nativeSyncCall(() => this.native.jsonGetHlcSync(space, id));
    return raw ? mapHlcRecord(JSON.parse(raw) as NativeRecord) : undefined;
  }

  // Batch & SQL
  async batch(operations: readonly BatchOperation[], signal?: AbortSignal): Promise<MutationResult[]> {
    const wire = operations.map((op) => batchOpToWire(op, "msgpack-native"));
    const payload = encode(wire, MSGPACK_ENCODE_OPTS);
    const resBuffer = await nativeCall(this.native.batchMsgpack(payload), signal);
    return mapMutationResults(decode(resBuffer, { useBigInt64: true }) as unknown[]);
  }
  batchSync(operations: readonly BatchOperation[]): MutationResult[] {
    const wire = operations.map((op) => batchOpToWire(op, "msgpack-native"));
    const payload = encode(wire, MSGPACK_ENCODE_OPTS);
    const resBuffer = nativeSyncCall(() => this.native.batchMsgpackSync(payload));
    return mapMutationResults(decode(resBuffer, { useBigInt64: true }) as unknown[]);
  }
  async sqlQuery<T = Record<string, unknown>>(sql: string, params: unknown[] = [], signal?: AbortSignal): Promise<SqlQueryResult<T>> {
    const raw = await nativeCall(this.native.sqlQuery(sql, JSON.stringify(params)), signal);
    return mapSqlQueryResult<T>(JSON.parse(raw) as NativeRecord);
  }
  sqlQuerySync<T = Record<string, unknown>>(sql: string, params: unknown[] = []): SqlQueryResult<T> {
    const raw = nativeSyncCall(() => this.native.sqlQuerySync(sql, JSON.stringify(params)));
    return mapSqlQueryResult<T>(JSON.parse(raw) as NativeRecord);
  }

  // Graph
  async graphNeighbors(graph: string, node: string, direction?: GraphDirection, kind?: string, signal?: AbortSignal): Promise<string[]> {
    return nativeCall(this.native.graphNeighbors(graph, node, direction, kind), signal);
  }
  graphNeighborsSync(graph: string, node: string, direction?: GraphDirection, kind?: string): string[] {
    return nativeSyncCall(() => this.native.graphNeighborsSync(graph, node, direction, kind));
  }
  async graphReachability(graph: string, startNodes: string[], direction?: GraphDirection, signal?: AbortSignal): Promise<string[]> {
    return nativeCall(this.native.graphReachability(graph, startNodes, direction), signal);
  }
  graphReachabilitySync(graph: string, startNodes: string[], direction?: GraphDirection): string[] {
    return nativeSyncCall(() => this.native.graphReachabilitySync(graph, startNodes, direction));
  }
  async graphGetNode(graph: string, id: string, signal?: AbortSignal): Promise<GraphNode | undefined> {
    const raw = await nativeCall(this.native.graphGetNode(graph, id), signal);
    return raw ? mapGraphNode(JSON.parse(raw) as NativeRecord) : undefined;
  }
  graphGetNodeSync(graph: string, id: string): GraphNode | undefined {
    const raw = nativeSyncCall(() => this.native.graphGetNodeSync(graph, id));
    return raw ? mapGraphNode(JSON.parse(raw) as NativeRecord) : undefined;
  }
  async graphGetNodeDocument<T = JsonValue>(graph: string, id: string, signal?: AbortSignal): Promise<JsonEntry<T> | undefined> {
    const raw = await nativeCall(this.native.graphGetNodeDocument(graph, id), signal);
    return raw ? parseJsonEntry<T>(raw) : undefined;
  }
  graphGetNodeDocumentSync<T = JsonValue>(graph: string, id: string): JsonEntry<T> | undefined {
    const raw = nativeSyncCall(() => this.native.graphGetNodeDocumentSync(graph, id));
    return raw ? parseJsonEntry<T>(raw) : undefined;
  }
  async graphDeleteNode(graph: string, id: string, signal?: AbortSignal): Promise<boolean> {
    return nativeCall(this.native.graphDeleteNode(graph, id), signal);
  }
  graphDeleteNodeSync(graph: string, id: string): boolean {
    return nativeSyncCall(() => this.native.graphDeleteNodeSync(graph, id));
  }
  async graphListNodes(graph: string, kind?: string, limit?: number, signal?: AbortSignal): Promise<GraphNode[]> {
    return (JSON.parse(await nativeCall(this.native.graphListNodes(graph, kind, limit), signal)) as NativeRecord[]).map(mapGraphNode);
  }
  graphListNodesSync(graph: string, kind?: string, limit?: number): GraphNode[] {
    return (JSON.parse(nativeSyncCall(() => this.native.graphListNodesSync(graph, kind, limit))) as NativeRecord[]).map(mapGraphNode);
  }
  async graphRemoveEdge(graph: string, source: string, target: string, kind: string, signal?: AbortSignal): Promise<boolean> {
    return nativeCall(this.native.graphRemoveEdge(graph, source, target, kind), signal);
  }
  graphRemoveEdgeSync(graph: string, source: string, target: string, kind: string): boolean {
    return nativeSyncCall(() => this.native.graphRemoveEdgeSync(graph, source, target, kind));
  }
  async graphListEdges(graph: string, source?: string, target?: string, kind?: string, limit?: number, signal?: AbortSignal): Promise<GraphEdge[]> {
    return (JSON.parse(await nativeCall(this.native.graphListEdges(graph, source, target, kind, limit), signal)) as NativeRecord[]).map(mapGraphEdge);
  }
  graphListEdgesSync(graph: string, source?: string, target?: string, kind?: string, limit?: number): GraphEdge[] {
    return (JSON.parse(nativeSyncCall(() => this.native.graphListEdgesSync(graph, source, target, kind, limit))) as NativeRecord[]).map(mapGraphEdge);
  }
  async graphImpactAnalysis(graph: string, changedNodes: string[], signal?: AbortSignal): Promise<ImpactAnalysis> {
    return mapImpactAnalysis(JSON.parse(await nativeCall(this.native.graphImpactAnalysis(graph, changedNodes), signal)) as NativeRecord);
  }
  graphImpactAnalysisSync(graph: string, changedNodes: string[]): ImpactAnalysis {
    return mapImpactAnalysis(JSON.parse(nativeSyncCall(() => this.native.graphImpactAnalysisSync(graph, changedNodes))) as NativeRecord);
  }
  async graphShortestPath(graph: string, start: string, target: string, direction?: GraphDirection, kindFilter?: string, signal?: AbortSignal): Promise<WeightedPath | undefined> {
    const raw = await nativeCall(this.native.graphShortestPath(graph, start, target, direction, kindFilter), signal);
    return raw ? mapWeightedPath(JSON.parse(raw) as NativeRecord) : undefined;
  }
  graphShortestPathSync(graph: string, start: string, target: string, direction?: GraphDirection, kindFilter?: string): WeightedPath | undefined {
    const raw = nativeSyncCall(() => this.native.graphShortestPathSync(graph, start, target, direction, kindFilter));
    return raw ? mapWeightedPath(JSON.parse(raw) as NativeRecord) : undefined;
  }
  async graphDijkstra(graph: string, start: string, target?: string, direction?: GraphDirection, kindFilter?: string, signal?: AbortSignal): Promise<WeightedPath | undefined> {
    const raw = await nativeCall(this.native.graphDijkstra(graph, start, target, direction, kindFilter), signal);
    return raw ? mapWeightedPath(JSON.parse(raw) as NativeRecord) : undefined;
  }
  graphDijkstraSync(graph: string, start: string, target?: string, direction?: GraphDirection, kindFilter?: string): WeightedPath | undefined {
    const raw = nativeSyncCall(() => this.native.graphDijkstraSync(graph, start, target, direction, kindFilter));
    return raw ? mapWeightedPath(JSON.parse(raw) as NativeRecord) : undefined;
  }
  async graphAllShortestPaths(graph: string, start: string, direction?: GraphDirection, kindFilter?: string, signal?: AbortSignal): Promise<Record<string, WeightedPath>> {
    const parsed = JSON.parse(await nativeCall(this.native.graphAllShortestPaths(graph, start, direction, kindFilter), signal)) as Record<string, NativeRecord>;
    return Object.fromEntries(Object.entries(parsed).map(([k, v]) => [k, mapWeightedPath(v)]));
  }
  graphAllShortestPathsSync(graph: string, start: string, direction?: GraphDirection, kindFilter?: string): Record<string, WeightedPath> {
    const parsed = JSON.parse(nativeSyncCall(() => this.native.graphAllShortestPathsSync(graph, start, direction, kindFilter))) as Record<string, NativeRecord>;
    return Object.fromEntries(Object.entries(parsed).map(([k, v]) => [k, mapWeightedPath(v)]));
  }
  async graphAstarSpatial(graph: string, start: string, target: string, direction?: GraphDirection, kindFilter?: string, signal?: AbortSignal): Promise<WeightedPath | undefined> {
    const raw = await nativeCall(this.native.graphAstarSpatial(graph, start, target, direction, kindFilter), signal);
    return raw ? mapWeightedPath(JSON.parse(raw) as NativeRecord) : undefined;
  }
  graphAstarSpatialSync(graph: string, start: string, target: string, direction?: GraphDirection, kindFilter?: string): WeightedPath | undefined {
    const raw = nativeSyncCall(() => this.native.graphAstarSpatialSync(graph, start, target, direction, kindFilter));
    return raw ? mapWeightedPath(JSON.parse(raw) as NativeRecord) : undefined;
  }
  async graphPagerank(graph: string, config?: PageRankConfig, signal?: AbortSignal): Promise<NodeScore[]> {
    const raw = await nativeCall(this.native.graphPagerank(graph, config ? JSON.stringify(toWirePageRankConfig(config)) : undefined), signal);
    return (JSON.parse(raw) as NativeRecord[]).map(mapNodeScore);
  }
  graphPagerankSync(graph: string, config?: PageRankConfig): NodeScore[] {
    const raw = nativeSyncCall(() => this.native.graphPagerankSync(graph, config ? JSON.stringify(toWirePageRankConfig(config)) : undefined));
    return (JSON.parse(raw) as NativeRecord[]).map(mapNodeScore);
  }
  async graphCentrality(graph: string, centrality: CentralityType, direction?: GraphDirection, kindFilter?: string, normalized?: boolean, signal?: AbortSignal): Promise<NodeScore[]> {
    return (JSON.parse(await nativeCall(this.native.graphCentrality(graph, centrality, direction, kindFilter, normalized), signal)) as NativeRecord[]).map(mapNodeScore);
  }
  graphCentralitySync(graph: string, centrality: CentralityType, direction?: GraphDirection, kindFilter?: string, normalized?: boolean): NodeScore[] {
    return (JSON.parse(nativeSyncCall(() => this.native.graphCentralitySync(graph, centrality, direction, kindFilter, normalized))) as NativeRecord[]).map(mapNodeScore);
  }
  async graphHasCycle(graph: string, direction?: GraphDirection, kindFilter?: string, signal?: AbortSignal): Promise<boolean> {
    return nativeCall(this.native.graphHasCycle(graph, direction, kindFilter), signal);
  }
  graphHasCycleSync(graph: string, direction?: GraphDirection, kindFilter?: string): boolean {
    return nativeSyncCall(() => this.native.graphHasCycleSync(graph, direction, kindFilter));
  }
  async graphFindCycles(graph: string, maxCycles?: number, kindFilter?: string, signal?: AbortSignal): Promise<string[][]> {
    return JSON.parse(await nativeCall(this.native.graphFindCycles(graph, maxCycles, kindFilter), signal)) as string[][];
  }
  graphFindCyclesSync(graph: string, maxCycles?: number, kindFilter?: string): string[][] {
    return JSON.parse(nativeSyncCall(() => this.native.graphFindCyclesSync(graph, maxCycles, kindFilter))) as string[][];
  }
  async graphConnectedComponents(graph: string, componentType: ComponentType, kindFilter?: string, signal?: AbortSignal): Promise<string[][]> {
    return JSON.parse(await nativeCall(this.native.graphConnectedComponents(graph, componentType, kindFilter), signal)) as string[][];
  }
  graphConnectedComponentsSync(graph: string, componentType: ComponentType, kindFilter?: string): string[][] {
    return JSON.parse(nativeSyncCall(() => this.native.graphConnectedComponentsSync(graph, componentType, kindFilter))) as string[][];
  }
  async graphCypher<T = Record<string, JsonValue>>(graph: string, query: string, signal?: AbortSignal): Promise<CypherResult<T>> {
    return mapCypherResult<T>(JSON.parse(await nativeCall(this.native.graphCypher(graph, query), signal)) as NativeRecord);
  }
  graphCypherSync<T = Record<string, JsonValue>>(graph: string, query: string): CypherResult<T> {
    return mapCypherResult<T>(JSON.parse(nativeSyncCall(() => this.native.graphCypherSync(graph, query))) as NativeRecord);
  }

  // FTS
  async ftsDelete(collection: string, docId: string, signal?: AbortSignal): Promise<boolean> {
    return nativeCall(this.native.ftsDelete(collection, docId), signal);
  }
  ftsDeleteSync(collection: string, docId: string): boolean {
    return nativeSyncCall(() => this.native.ftsDeleteSync(collection, docId));
  }
  async ftsSearch(collection: string, query: string, limit?: number, signal?: AbortSignal): Promise<FtsHit[]> {
    return mapFtsHits(await nativeCall(this.native.ftsSearch(collection, query, limit), signal));
  }
  ftsSearchSync(collection: string, query: string, limit?: number): FtsHit[] {
    return mapFtsHits(nativeSyncCall(() => this.native.ftsSearchSync(collection, query, limit)));
  }

  // Vector & Hybrid
  async vectorGet(collection: string, id: string, signal?: AbortSignal): Promise<Uint8Array | null> {
    const buf = await nativeCall(this.native.vectorGet(collection, id), signal);
    return buf ? new Uint8Array(buf) : null;
  }
  vectorGetSync(collection: string, id: string): Uint8Array | null {
    const buf = nativeSyncCall(() => this.native.vectorGetSync(collection, id));
    return buf ? new Uint8Array(buf) : null;
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
    const raw = Buffer.from(vectorToBytes(query, elementType));
    const d = inferVectorDim(query, elementType, dim);
    return mapVectorHits(await nativeCall(this.native.vectorSearch(collection, raw, d, elementType, metric, limit), signal));
  }
  vectorSearchSync(
    collection: string,
    query: Float32Array | number[] | Uint8Array,
    dim?: number,
    elementType?: VectorElementType,
    metric?: VectorMetric,
    limit?: number
  ): VectorHit[] {
    const raw = Buffer.from(vectorToBytes(query, elementType));
    const d = inferVectorDim(query, elementType, dim);
    return mapVectorHits(nativeSyncCall(() => this.native.vectorSearchSync(collection, raw, d, elementType, metric, limit)));
  }
  async vectorSearchText(
    collection: string,
    text: string,
    limit?: number,
    signal?: AbortSignal
  ): Promise<VectorHit[]> {
    return mapVectorHits(await nativeCall(this.native.vectorSearchText(collection, text, limit), signal));
  }
  vectorSearchTextSync(
    collection: string,
    text: string,
    limit?: number
  ): VectorHit[] {
    return mapVectorHits(nativeSyncCall(() => this.native.vectorSearchTextSync(collection, text, limit)));
  }
  async waitForEmbeddings(timeoutMs?: number, signal?: AbortSignal): Promise<void> {
    await nativeCall(this.native.waitForEmbeddings(timeoutMs), signal);
  }
  waitForEmbeddingsSync(timeoutMs?: number): void {
    nativeSyncCall(() => this.native.waitForEmbeddingsSync(timeoutMs));
  }
  jsonSetAutoEmbedSync(collection: string, config?: AutoEmbedConfig): void {
    const configJson = config
      ? JSON.stringify({
          source_field: config.sourceField,
          target_field: config.targetField ?? "embedding",
          model: config.model,
        })
      : undefined;
    nativeSyncCall(() => this.native.jsonSetAutoEmbedSync(collection, configJson));
  }
  jsonEmbedStatusSync(collection: string): EmbedStatus {
    const raw = nativeSyncCall(() => this.native.jsonEmbedStatusSync(collection));
    const parsed = JSON.parse(raw);
    return {
      pending: parsed.pending ?? 0,
      failed: parsed.failed ?? 0,
      lastError: parsed.last_error,
    };
  }
  async vectorCreateIndex(collection: string, config: HnswConfig, signal?: AbortSignal): Promise<void> {
    await nativeCall(this.native.vectorCreateIndex(collection, JSON.stringify(toWireHnswConfig(config))), signal);
  }
  vectorCreateIndexSync(collection: string, config: HnswConfig): void {
    nativeSyncCall(() => this.native.vectorCreateIndexSync(collection, JSON.stringify(toWireHnswConfig(config))));
  }
  async vectorGetIndexInfo(collection: string, signal?: AbortSignal): Promise<VectorIndexInfo | undefined> {
    const raw = await nativeCall(this.native.vectorGetIndexInfo(collection), signal);
    return raw ? mapVectorIndexInfo(JSON.parse(raw) as NativeRecord) : undefined;
  }
  vectorGetIndexInfoSync(collection: string): VectorIndexInfo | undefined {
    const raw = nativeSyncCall(() => this.native.vectorGetIndexInfoSync(collection));
    return raw ? mapVectorIndexInfo(JSON.parse(raw) as NativeRecord) : undefined;
  }
  async vectorDropIndex(collection: string, signal?: AbortSignal): Promise<boolean> {
    return nativeCall(this.native.vectorDropIndex(collection), signal);
  }
  vectorDropIndexSync(collection: string): boolean {
    return nativeSyncCall(() => this.native.vectorDropIndexSync(collection));
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
    const raw = Buffer.from(vectorToBytes(query, elementType));
    const d = inferVectorDim(query, elementType, dim);
    return mapVectorHits(await nativeCall(this.native.vectorSearchFiltered(collection, raw, d, elementType, metric, candidateIds, limit), signal));
  }
  vectorSearchFilteredSync(
    collection: string,
    query: Float32Array | number[] | Uint8Array,
    candidateIds: string[],
    dim?: number,
    elementType?: VectorElementType,
    metric?: VectorMetric,
    limit?: number
  ): VectorHit[] {
    const raw = Buffer.from(vectorToBytes(query, elementType));
    const d = inferVectorDim(query, elementType, dim);
    return mapVectorHits(nativeSyncCall(() => this.native.vectorSearchFilteredSync(collection, raw, d, elementType, metric, candidateIds, limit)));
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
    const raw = Buffer.from(vectorToBytes(query, elementType));
    const d = inferVectorDim(query, elementType, dim);
    return mapVectorHits(await nativeCall(this.native.vectorSearchInGraphNeighbors(collection, graph, startNode, direction, raw, d, elementType, metric, limit), signal));
  }
  vectorSearchInGraphNeighborsSync(
    collection: string,
    graph: string,
    startNode: string,
    direction: GraphDirection | undefined,
    query: Float32Array | number[] | Uint8Array,
    dim?: number,
    elementType?: VectorElementType,
    metric?: VectorMetric,
    limit?: number
  ): VectorHit[] {
    const raw = Buffer.from(vectorToBytes(query, elementType));
    const d = inferVectorDim(query, elementType, dim);
    return mapVectorHits(nativeSyncCall(() => this.native.vectorSearchInGraphNeighborsSync(collection, graph, startNode, direction, raw, d, elementType, metric, limit)));
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
    const raw = Buffer.from(vectorToBytes(query, elementType));
    const d = inferVectorDim(query, elementType, dim);
    return mapVectorHits(await nativeCall(this.native.vectorSearchByJsonIndex(collection, space, indexName, indexValue, raw, d, elementType, metric, limit), signal));
  }
  vectorSearchByJsonIndexSync(
    collection: string,
    space: string,
    indexName: string,
    indexValue: string,
    query: Float32Array | number[] | Uint8Array,
    dim?: number,
    elementType?: VectorElementType,
    metric?: VectorMetric,
    limit?: number
  ): VectorHit[] {
    const raw = Buffer.from(vectorToBytes(query, elementType));
    const d = inferVectorDim(query, elementType, dim);
    return mapVectorHits(nativeSyncCall(() => this.native.vectorSearchByJsonIndexSync(collection, space, indexName, indexValue, raw, d, elementType, metric, limit)));
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
    const vecBuf = options.vector ? Buffer.from(vectorToBytes(options.vector, options.elementType)) : undefined;
    const d = options.vector ? inferVectorDim(options.vector, options.elementType, options.dim) : undefined;
    const hits = await nativeCall(
      this.native.hybridSearch(
        collection,
        options.query,
        vecBuf,
        d,
        options.elementType,
        options.metric,
        options.rrfK,
        options.ftsWeight,
        options.vectorWeight,
        options.limit
      ),
      options.signal
    );
    return mapHybridHits(hits);
  }
  hybridSearchSync(
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
  ): HybridHit[] {
    const vecBuf = options.vector ? Buffer.from(vectorToBytes(options.vector, options.elementType)) : undefined;
    const d = options.vector ? inferVectorDim(options.vector, options.elementType, options.dim) : undefined;
    const hits = nativeSyncCall(() =>
      this.native.hybridSearchSync(
        collection,
        options.query,
        vecBuf,
        d,
        options.elementType,
        options.metric,
        options.rrfK,
        options.ftsWeight,
        options.vectorWeight,
        options.limit
      )
    );
    return mapHybridHits(hits);
  }

  // TimeSeries
  async tsAppendPoints(seriesId: string, points: TimeSeriesPoint[], resolutionMs?: number, signal?: AbortSignal): Promise<void> {
    await nativeCall(this.native.tsAppendPoints(seriesId, JSON.stringify(points), resolutionMs), signal);
  }
  tsAppendPointsSync(seriesId: string, points: TimeSeriesPoint[], resolutionMs?: number): void {
    nativeSyncCall(() => this.native.tsAppendPointsSync(seriesId, JSON.stringify(points), resolutionMs));
  }
  async tsQueryRange(seriesId: string, startTime: number, endTime: number, resolutionMs?: number, signal?: AbortSignal): Promise<TimeSeriesPoint[]> {
    return mapTsPoints(JSON.parse(await nativeCall(this.native.tsQueryRange(seriesId, startTime, endTime, resolutionMs), signal)) as NativeRecord[]);
  }
  tsQueryRangeSync(seriesId: string, startTime: number, endTime: number, resolutionMs?: number): TimeSeriesPoint[] {
    return mapTsPoints(JSON.parse(nativeSyncCall(() => this.native.tsQueryRangeSync(seriesId, startTime, endTime, resolutionMs))) as NativeRecord[]);
  }
  async tsQueryAuto(seriesId: string, startTime: number, endTime: number, maxPoints: number, signal?: AbortSignal): Promise<TimeSeriesPoint[]> {
    return mapTsPoints(JSON.parse(await nativeCall(this.native.tsQueryAuto(seriesId, startTime, endTime, maxPoints), signal)) as NativeRecord[]);
  }
  tsQueryAutoSync(seriesId: string, startTime: number, endTime: number, maxPoints: number): TimeSeriesPoint[] {
    return mapTsPoints(JSON.parse(nativeSyncCall(() => this.native.tsQueryAutoSync(seriesId, startTime, endTime, maxPoints))) as NativeRecord[]);
  }
  async tsQueryStats(seriesId: string, startTime: number, endTime: number, resolutionMs?: number, signal?: AbortSignal): Promise<TimeSeriesStats | undefined> {
    const raw = await nativeCall(this.native.tsQueryStats(seriesId, startTime, endTime, resolutionMs), signal);
    return raw ? mapTsStats(JSON.parse(raw) as NativeRecord) : undefined;
  }
  tsQueryStatsSync(seriesId: string, startTime: number, endTime: number, resolutionMs?: number): TimeSeriesStats | undefined {
    const raw = nativeSyncCall(() => this.native.tsQueryStatsSync(seriesId, startTime, endTime, resolutionMs));
    return raw ? mapTsStats(JSON.parse(raw) as NativeRecord) : undefined;
  }
  async tsSetPolicy(policy: TimeSeriesLifecyclePolicy, signal?: AbortSignal): Promise<void> {
    await nativeCall(this.native.tsSetPolicy(JSON.stringify(toWireTsPolicy(policy))), signal);
  }
  tsSetPolicySync(policy: TimeSeriesLifecyclePolicy): void {
    nativeSyncCall(() => this.native.tsSetPolicySync(JSON.stringify(toWireTsPolicy(policy))));
  }
  async tsGetPolicy(seriesPattern: string, signal?: AbortSignal): Promise<TimeSeriesLifecyclePolicy | undefined> {
    const raw = await nativeCall(this.native.tsGetPolicy(seriesPattern), signal);
    return raw ? mapTsPolicy(JSON.parse(raw) as NativeRecord) : undefined;
  }
  tsGetPolicySync(seriesPattern: string): TimeSeriesLifecyclePolicy | undefined {
    const raw = nativeSyncCall(() => this.native.tsGetPolicySync(seriesPattern));
    return raw ? mapTsPolicy(JSON.parse(raw) as NativeRecord) : undefined;
  }
  async tsListPolicies(signal?: AbortSignal): Promise<TimeSeriesLifecyclePolicy[]> {
    return (JSON.parse(await nativeCall(this.native.tsListPolicies(), signal)) as NativeRecord[]).map(mapTsPolicy);
  }
  tsListPoliciesSync(): TimeSeriesLifecyclePolicy[] {
    return (JSON.parse(nativeSyncCall(() => this.native.tsListPoliciesSync())) as NativeRecord[]).map(mapTsPolicy);
  }
  async tsDeletePolicy(seriesPattern: string, signal?: AbortSignal): Promise<boolean> {
    return nativeCall(this.native.tsDeletePolicy(seriesPattern), signal);
  }
  tsDeletePolicySync(seriesPattern: string): boolean {
    return nativeSyncCall(() => this.native.tsDeletePolicySync(seriesPattern));
  }
  async tsApplyRetention(seriesId?: string, policy?: TimeSeriesLifecyclePolicy, nowMs?: number, signal?: AbortSignal): Promise<TimeSeriesRetentionStats> {
    const raw = await nativeCall(
      this.native.tsApplyRetention(seriesId, policy ? JSON.stringify(toWireTsPolicy(policy)) : undefined, nowMs),
      signal
    );
    return mapTsRetentionStats(JSON.parse(raw) as NativeRecord);
  }
  tsApplyRetentionSync(seriesId?: string, policy?: TimeSeriesLifecyclePolicy, nowMs?: number): TimeSeriesRetentionStats {
    const raw = nativeSyncCall(() =>
      this.native.tsApplyRetentionSync(seriesId, policy ? JSON.stringify(toWireTsPolicy(policy)) : undefined, nowMs)
    );
    return mapTsRetentionStats(JSON.parse(raw) as NativeRecord);
  }

  // Geo & GPS
  async geoAddFence(id: string, name: string, category: string, polygonJson: string, metadata?: string, signal?: AbortSignal): Promise<void> {
    await nativeCall(this.native.geoAddFence(id, name, category, polygonJson, metadata), signal);
  }
  geoAddFenceSync(id: string, name: string, category: string, polygonJson: string, metadata?: string): void {
    nativeSyncCall(() => this.native.geoAddFenceSync(id, name, category, polygonJson, metadata));
  }
  async geoRemoveFence(id: string, signal?: AbortSignal): Promise<boolean> {
    return nativeCall(this.native.geoRemoveFence(id), signal);
  }
  geoRemoveFenceSync(id: string): boolean {
    return nativeSyncCall(() => this.native.geoRemoveFenceSync(id));
  }
  async geoContainsPoint(polygonJson: string, lon: number, lat: number, signal?: AbortSignal): Promise<boolean> {
    return nativeCall(this.native.geoContainsPoint(polygonJson, lon, lat), signal);
  }
  geoContainsPointSync(polygonJson: string, lon: number, lat: number): boolean {
    return nativeSyncCall(() => this.native.geoContainsPointSync(polygonJson, lon, lat));
  }
  async geoFindFencesForPoint(lon: number, lat: number, signal?: AbortSignal): Promise<GeofenceMatch[]> {
    return mapGeofenceMatches(JSON.parse(await nativeCall(this.native.geoFindFencesForPoint(lon, lat), signal)) as NativeRecord[]);
  }
  geoFindFencesForPointSync(lon: number, lat: number): GeofenceMatch[] {
    return mapGeofenceMatches(JSON.parse(nativeSyncCall(() => this.native.geoFindFencesForPointSync(lon, lat))) as NativeRecord[]);
  }
  async geoRecordGpsPoints(trackId: string, points: GpsPoint[], signal?: AbortSignal): Promise<GeofenceMatch[]> {
    return mapGeofenceMatches(JSON.parse(await nativeCall(this.native.geoRecordGpsPoints(trackId, JSON.stringify(points)), signal)) as NativeRecord[]);
  }
  geoRecordGpsPointsSync(trackId: string, points: GpsPoint[]): GeofenceMatch[] {
    return mapGeofenceMatches(JSON.parse(nativeSyncCall(() => this.native.geoRecordGpsPointsSync(trackId, JSON.stringify(points)))) as NativeRecord[]);
  }
  async geoGetTrackPoints(trackId: string, startTime?: number, endTime?: number, signal?: AbortSignal): Promise<GpsPoint[]> {
    return mapGpsPoints(JSON.parse(await nativeCall(this.native.geoGetTrackPoints(trackId, startTime, endTime), signal)) as NativeRecord[]);
  }
  geoGetTrackPointsSync(trackId: string, startTime?: number, endTime?: number): GpsPoint[] {
    return mapGpsPoints(JSON.parse(nativeSyncCall(() => this.native.geoGetTrackPointsSync(trackId, startTime, endTime))) as NativeRecord[]);
  }
  async geoTrackDistance(trackId: string, signal?: AbortSignal): Promise<number> {
    return nativeCall(this.native.geoTrackDistance(trackId), signal);
  }
  geoTrackDistanceSync(trackId: string): number {
    return nativeSyncCall(() => this.native.geoTrackDistanceSync(trackId));
  }

  // CRDT
  async crdtCounterGet(space: string, id: string, signal?: AbortSignal): Promise<number> {
    return nativeCall(this.native.crdtCounterGet(space, id), signal);
  }
  crdtCounterGetSync(space: string, id: string): number {
    return nativeSyncCall(() => this.native.crdtCounterGetSync(space, id));
  }
  async crdtCounterState(space: string, id: string, signal?: AbortSignal): Promise<CrdtCounterState> {
    return mapCrdtCounterState(JSON.parse(await nativeCall(this.native.crdtCounterState(space, id), signal)) as NativeRecord);
  }
  crdtCounterStateSync(space: string, id: string): CrdtCounterState {
    return mapCrdtCounterState(JSON.parse(nativeSyncCall(() => this.native.crdtCounterStateSync(space, id))) as NativeRecord);
  }
  async crdtCounterMerge(space: string, id: string, remote: CrdtCounterState, signal?: AbortSignal): Promise<number> {
    return nativeCall(this.native.crdtCounterMerge(space, id, JSON.stringify(remote)), signal);
  }
  crdtCounterMergeSync(space: string, id: string, remote: CrdtCounterState): number {
    return nativeSyncCall(() => this.native.crdtCounterMergeSync(space, id, JSON.stringify(remote)));
  }
  async crdtSetRead(space: string, id: string, signal?: AbortSignal): Promise<string[]> {
    return nativeCall(this.native.crdtSetRead(space, id), signal);
  }
  crdtSetReadSync(space: string, id: string): string[] {
    return nativeSyncCall(() => this.native.crdtSetReadSync(space, id));
  }
  async crdtSetContains(space: string, id: string, element: string, signal?: AbortSignal): Promise<boolean> {
    return nativeCall(this.native.crdtSetContains(space, id, element), signal);
  }
  crdtSetContainsSync(space: string, id: string, element: string): boolean {
    return nativeSyncCall(() => this.native.crdtSetContainsSync(space, id, element));
  }
  async crdtSetState(space: string, id: string, signal?: AbortSignal): Promise<CrdtOrSetState> {
    return mapCrdtOrSetState(JSON.parse(await nativeCall(this.native.crdtSetState(space, id), signal)) as NativeRecord);
  }
  crdtSetStateSync(space: string, id: string): CrdtOrSetState {
    return mapCrdtOrSetState(JSON.parse(nativeSyncCall(() => this.native.crdtSetStateSync(space, id))) as NativeRecord);
  }
  async crdtSetPrune(space: string, id: string, cutoffMs: number, signal?: AbortSignal): Promise<number> {
    return nativeCall(this.native.crdtSetPrune(space, id, cutoffMs), signal);
  }
  crdtSetPruneSync(space: string, id: string, cutoffMs: number): number {
    return nativeSyncCall(() => this.native.crdtSetPruneSync(space, id, cutoffMs));
  }
  async crdtSetMerge(space: string, id: string, remote: CrdtOrSetState, signal?: AbortSignal): Promise<void> {
    await nativeCall(this.native.crdtSetMerge(space, id, JSON.stringify(remote)), signal);
  }
  crdtSetMergeSync(space: string, id: string, remote: CrdtOrSetState): void {
    nativeSyncCall(() => this.native.crdtSetMergeSync(space, id, JSON.stringify(remote)));
  }
  async crdtJsonGet<T = JsonValue>(space: string, id: string, signal?: AbortSignal): Promise<T | undefined> {
    const raw = await nativeCall(this.native.crdtJsonGet(space, id), signal);
    return raw == null ? undefined : (JSON.parse(raw) as T);
  }
  crdtJsonGetSync<T = JsonValue>(space: string, id: string): T | undefined {
    const raw = nativeSyncCall(() => this.native.crdtJsonGetSync(space, id));
    return raw == null ? undefined : (JSON.parse(raw) as T);
  }
  async crdtJsonState(space: string, id: string, signal?: AbortSignal): Promise<LwwJsonDocumentState | undefined> {
    const raw = await nativeCall(this.native.crdtJsonState(space, id), signal);
    return raw ? mapLwwJsonState(JSON.parse(raw) as NativeRecord) : undefined;
  }
  crdtJsonStateSync(space: string, id: string): LwwJsonDocumentState | undefined {
    const raw = nativeSyncCall(() => this.native.crdtJsonStateSync(space, id));
    return raw ? mapLwwJsonState(JSON.parse(raw) as NativeRecord) : undefined;
  }
  async crdtJsonMerge(space: string, id: string, remote: LwwJsonDocumentState, signal?: AbortSignal): Promise<bigint> {
    const ver = await nativeCall(this.native.crdtJsonMerge(space, id, JSON.stringify(toWireLwwJsonState(remote))), signal);
    return readVersion(ver);
  }
  crdtJsonMergeSync(space: string, id: string, remote: LwwJsonDocumentState): bigint {
    return readVersion(nativeSyncCall(() => this.native.crdtJsonMergeSync(space, id, JSON.stringify(toWireLwwJsonState(remote)))));
  }

  // HLC & Conflicts
  hlcNext(signal?: AbortSignal): Promise<string> {
    return nativeCall(this.native.hlcNext(), signal);
  }
  hlcNextSync(): string {
    return nativeSyncCall(() => this.native.hlcNextSync());
  }
  hlcObserve(remote: string, signal?: AbortSignal): Promise<string> {
    return nativeCall(this.native.hlcObserve(remote), signal);
  }
  hlcObserveSync(remote: string): string {
    return nativeSyncCall(() => this.native.hlcObserveSync(remote));
  }
  async listConflicts(space?: string, signal?: AbortSignal): Promise<ConflictRecord[]> {
    return (JSON.parse(await nativeCall(this.native.listConflicts(space), signal)) as NativeRecord[]).map(mapConflictRecord);
  }
  listConflictsSync(space?: string): ConflictRecord[] {
    return (JSON.parse(nativeSyncCall(() => this.native.listConflictsSync(space))) as NativeRecord[]).map(mapConflictRecord);
  }
  async resolveConflict(conflictId: number, resolvedValue: JsonValue, signal?: AbortSignal): Promise<void> {
    await nativeCall(this.native.resolveConflict(conflictId, JSON.stringify(resolvedValue)), signal);
  }
  resolveConflictSync(conflictId: number, resolvedValue: JsonValue): void {
    nativeSyncCall(() => this.native.resolveConflictSync(conflictId, JSON.stringify(resolvedValue)));
  }

  // Undo / Redo
  async undo(signal?: AbortSignal): Promise<UndoResult | undefined> {
    const raw = await nativeCall(this.native.undo(), signal);
    return raw ? mapUndoResult(JSON.parse(raw) as NativeRecord) : undefined;
  }
  undoSync(): UndoResult | undefined {
    const raw = nativeSyncCall(() => this.native.undoSync());
    return raw ? mapUndoResult(JSON.parse(raw) as NativeRecord) : undefined;
  }
  async redo(signal?: AbortSignal): Promise<RedoResult | undefined> {
    const raw = await nativeCall(this.native.redo(), signal);
    return raw ? mapRedoResult(JSON.parse(raw) as NativeRecord) : undefined;
  }
  redoSync(): RedoResult | undefined {
    const raw = nativeSyncCall(() => this.native.redoSync());
    return raw ? mapRedoResult(JSON.parse(raw) as NativeRecord) : undefined;
  }
  async undoHistory(signal?: AbortSignal): Promise<UndoEntry[]> {
    return (JSON.parse(await nativeCall(this.native.undoHistory(), signal)) as NativeRecord[]).map(mapUndoEntry);
  }
  undoHistorySync(): UndoEntry[] {
    return (JSON.parse(nativeSyncCall(() => this.native.undoHistorySync())) as NativeRecord[]).map(mapUndoEntry);
  }

  // Streams & IVM
  async watch(
    namespace: string | undefined,
    callback: (event: DatabaseEvent) => void,
    options?: { signal?: AbortSignal }
  ): Promise<WatchSubscription> {
    options?.signal?.throwIfAborted();
    const handle = await nativeCall(
      this.native.watch((...args: unknown[]) => {
        const rawJson = args.length > 1 ? args[1] : args[0];
        try {
          const parsed = typeof rawJson === "string" ? JSON.parse(rawJson) : rawJson;
          const event = mapDatabaseEvent(parsed);
          if (event) callback(event);
        } catch {}
      }, namespace),
      options?.signal
    );
    return makeSubscription<WatchSubscription>(() => handle.unsubscribe(), options?.signal);
  }

  async liveQuery<T = Record<string, unknown>>(
    sql: string,
    params: JsonValue[] = [],
    callback: (diff: LiveQueryDiff<T>) => void,
    options?: { signal?: AbortSignal }
  ): Promise<LiveQuerySubscription> {
    options?.signal?.throwIfAborted();
    const paramsJson = params.length > 0 ? JSON.stringify(params) : undefined;
    const handle = await nativeCall(
      this.native.liveQuery(
        sql,
        (...args: unknown[]) => {
          const rawJson = args.length > 1 ? args[1] : args[0];
          try {
            const parsed = (typeof rawJson === "string" ? JSON.parse(rawJson) : rawJson) as NativeRecord;
            callback(mapLiveQueryDiff<T>(parsed));
          } catch {}
        },
        paramsJson
      ),
      options?.signal
    );
    return makeSubscription<LiveQuerySubscription>(() => handle.unsubscribe(), options?.signal);
  }

  createMaterializedView(name: string, query: string, signal?: AbortSignal): Promise<void> {
    return nativeCall(this.native.createMaterializedView(name, query), signal);
  }
  createMaterializedViewSync(name: string, query: string): void {
    nativeSyncCall(() => this.native.createMaterializedViewSync(name, query));
  }
  dropMaterializedView(name: string, signal?: AbortSignal): Promise<void> {
    return nativeCall(this.native.dropMaterializedView(name), signal);
  }
  dropMaterializedViewSync(name: string): void {
    nativeSyncCall(() => this.native.dropMaterializedViewSync(name));
  }
  refreshMaterializedView(name: string, signal?: AbortSignal): Promise<void> {
    return nativeCall(this.native.refreshMaterializedView(name), signal);
  }
  refreshMaterializedViewSync(name: string): void {
    nativeSyncCall(() => this.native.refreshMaterializedViewSync(name));
  }
  async listMaterializedViews(signal?: AbortSignal): Promise<MaterializedViewInfo[]> {
    const raw = await nativeCall(this.native.listMaterializedViews(), signal);
    return mapMaterializedViews(JSON.parse(raw) as NativeRecord[]);
  }
  listMaterializedViewsSync(): MaterializedViewInfo[] {
    return mapMaterializedViews(JSON.parse(nativeSyncCall(() => this.native.listMaterializedViewsSync())) as NativeRecord[]);
  }

  // Diagnostics & Lifecycle
  quickCheck(signal?: AbortSignal): Promise<boolean> {
    return nativeCall(this.native.quickCheck(), signal);
  }
  quickCheckSync(): boolean {
    return nativeSyncCall(() => this.native.quickCheckSync());
  }
  integrityCheck(signal?: AbortSignal): Promise<boolean> {
    return nativeCall(this.native.integrityCheck(), signal);
  }
  integrityCheckSync(): boolean {
    return nativeSyncCall(() => this.native.integrityCheckSync());
  }
  checkpoint(signal?: AbortSignal): Promise<void> {
    return nativeCall(this.native.checkpoint(), signal);
  }
  checkpointSync(): void {
    nativeSyncCall(() => this.native.checkpointSync());
  }
  async checkpointStats(signal?: AbortSignal): Promise<CheckpointStats> {
    return mapCheckpointStats(JSON.parse(await nativeCall(this.native.checkpointStats(), signal)) as NativeRecord);
  }
  checkpointStatsSync(): CheckpointStats {
    return mapCheckpointStats(JSON.parse(nativeSyncCall(() => this.native.checkpointStatsSync())) as NativeRecord);
  }
  async storageStats(signal?: AbortSignal): Promise<TableStorageStat[]> {
    return mapStorageStats(JSON.parse(await nativeCall(this.native.storageStats(), signal)) as NativeRecord[]);
  }
  storageStatsSync(): TableStorageStat[] {
    return mapStorageStats(JSON.parse(nativeSyncCall(() => this.native.storageStatsSync())) as NativeRecord[]);
  }
  backup(destination: string, signal?: AbortSignal): Promise<void> {
    return nativeCall(this.native.backup(destination), signal);
  }
  backupSync(destination: string): void {
    nativeSyncCall(() => this.native.backupSync(destination));
  }

  // CAS / BLOB
  async blobStore(bytes: Uint8Array, mimeType?: string, signal?: AbortSignal): Promise<BlobDescriptor> {
    const raw = await nativeCall(this.native.blobStore(Buffer.from(bytes), mimeType), signal);
    const parsed = JSON.parse(raw);
    return {
      $blob: parsed.$blob ?? parsed.hash,
      hash: parsed.hash,
      sizeBytes: parsed.size_bytes ?? parsed.sizeBytes,
      mimeType: parsed.mime_type ?? parsed.mimeType,
    };
  }
  blobStoreSync(bytes: Uint8Array, mimeType?: string): BlobDescriptor {
    const raw = nativeSyncCall(() => this.native.blobStoreSync(Buffer.from(bytes), mimeType));
    const parsed = JSON.parse(raw);
    return {
      $blob: parsed.$blob ?? parsed.hash,
      hash: parsed.hash,
      sizeBytes: parsed.size_bytes ?? parsed.sizeBytes,
      mimeType: parsed.mime_type ?? parsed.mimeType,
    };
  }
  async blobGet(hash: string, signal?: AbortSignal): Promise<Uint8Array | null> {
    const buf = await nativeCall(this.native.blobGet(hash), signal);
    return buf ? new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength) : null;
  }
  blobGetSync(hash: string): Uint8Array | null {
    const buf = nativeSyncCall(() => this.native.blobGetSync(hash));
    return buf ? new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength) : null;
  }
  blobPath(hash: string): string {
    return this.native.blobPath(hash);
  }
  blobExists(hash: string): boolean {
    return this.native.blobExists(hash);
  }
  async blobGc(signal?: AbortSignal): Promise<BlobGcStats> {
    const raw = await nativeCall(this.native.blobGc(), signal);
    const parsed = JSON.parse(raw);
    return {
      orphansPurged: parsed.orphans_purged ?? parsed.orphansPurged ?? 0,
      bytesReclaimed: parsed.bytes_reclaimed ?? parsed.bytesReclaimed ?? 0,
    };
  }
  blobGcSync(): BlobGcStats {
    const raw = nativeSyncCall(() => this.native.blobGcSync());
    const parsed = JSON.parse(raw);
    return {
      orphansPurged: parsed.orphans_purged ?? parsed.orphansPurged ?? 0,
      bytesReclaimed: parsed.bytes_reclaimed ?? parsed.bytesReclaimed ?? 0,
    };
  }

  // Temporal sharding
  async shardingListShards(signal?: AbortSignal): Promise<ShardInfo[]> {
    return (JSON.parse(await nativeCall(this.native.shardingListShards(), signal)) as RawShardInfo[]).map(mapShardInfo);
  }
  shardingListShardsSync(): ShardInfo[] {
    return (JSON.parse(nativeSyncCall(() => this.native.shardingListShardsSync())) as RawShardInfo[]).map(mapShardInfo);
  }
  async shardingActiveShard(signal?: AbortSignal): Promise<ShardInfo> {
    return mapShardInfo(JSON.parse(await nativeCall(this.native.shardingActiveShard(), signal)) as RawShardInfo);
  }
  shardingActiveShardSync(): ShardInfo {
    return mapShardInfo(JSON.parse(nativeSyncCall(() => this.native.shardingActiveShardSync())) as RawShardInfo);
  }
  async shardingRollover(signal?: AbortSignal): Promise<ShardInfo> {
    return mapShardInfo(JSON.parse(await nativeCall(this.native.shardingRollover(), signal)) as RawShardInfo);
  }
  shardingRolloverSync(): ShardInfo {
    return mapShardInfo(JSON.parse(nativeSyncCall(() => this.native.shardingRolloverSync())) as RawShardInfo);
  }
  async shardingDetachAndCompress(shardId: string, signal?: AbortSignal): Promise<ShardArchiveResult> {
    return mapShardArchiveResult(
      JSON.parse(await nativeCall(this.native.shardingDetachAndCompress(shardId), signal)) as RawShardArchiveResult
    );
  }
  shardingDetachAndCompressSync(shardId: string): ShardArchiveResult {
    return mapShardArchiveResult(
      JSON.parse(nativeSyncCall(() => this.native.shardingDetachAndCompressSync(shardId))) as RawShardArchiveResult
    );
  }
  async shardingDecompressAndAttach(shardId: string, signal?: AbortSignal): Promise<ShardInfo> {
    return mapShardInfo(
      JSON.parse(await nativeCall(this.native.shardingDecompressAndAttach(shardId), signal)) as RawShardInfo
    );
  }
  shardingDecompressAndAttachSync(shardId: string): ShardInfo {
    return mapShardInfo(
      JSON.parse(nativeSyncCall(() => this.native.shardingDecompressAndAttachSync(shardId))) as RawShardInfo
    );
  }
  shardingDetach(shardId: string, signal?: AbortSignal): Promise<void> {
    return nativeCall(this.native.shardingDetach(shardId), signal);
  }
  shardingDetachSync(shardId: string): void {
    nativeSyncCall(() => this.native.shardingDetachSync(shardId));
  }
  shardingAttach(shardId: string, path: string, signal?: AbortSignal): Promise<void> {
    return nativeCall(this.native.shardingAttach(shardId, path), signal);
  }
  shardingAttachSync(shardId: string, path: string): void {
    nativeSyncCall(() => this.native.shardingAttachSync(shardId, path));
  }

  close(timeoutMs?: number): Promise<void> {
    return nativeCall(this.native.close(timeoutMs));
  }
}

interface RawShardInfo {
  shard_id: string;
  status: ShardInfo["status"];
  period_start_ms: number;
  period_end_ms: number;
  file_path: string;
  is_compressed: boolean;
  uncompressed_bytes: number;
  compressed_bytes: number;
  checksum: string | null;
  created_at_ms: number;
  archived_at_ms: number | null;
}

interface RawShardArchiveResult {
  shard_id: string;
  archive_path: string;
  original_size_bytes: number;
  compressed_size_bytes: number;
  compression_ratio: number;
  checksum: string;
  duration_ms: number;
  archived_at_ms: number;
}

function mapShardInfo(raw: RawShardInfo): ShardInfo {
  return {
    shardId: raw.shard_id,
    status: raw.status,
    periodStartMs: raw.period_start_ms,
    periodEndMs: raw.period_end_ms,
    filePath: raw.file_path,
    isCompressed: raw.is_compressed,
    uncompressedBytes: raw.uncompressed_bytes,
    compressedBytes: raw.compressed_bytes,
    ...(raw.checksum === null ? {} : { checksum: raw.checksum }),
    createdAtMs: raw.created_at_ms,
    ...(raw.archived_at_ms === null ? {} : { archivedAtMs: raw.archived_at_ms }),
  };
}

function mapShardArchiveResult(raw: RawShardArchiveResult): ShardArchiveResult {
  return {
    shardId: raw.shard_id,
    archivePath: raw.archive_path,
    originalSizeBytes: raw.original_size_bytes,
    compressedSizeBytes: raw.compressed_size_bytes,
    compressionRatio: raw.compression_ratio,
    checksum: raw.checksum,
    durationMs: raw.duration_ms,
    archivedAtMs: raw.archived_at_ms,
  };
}

export function makeSubscription<T extends WatchSubscription | LiveQuerySubscription>(
  onUnsubscribe: () => void | Promise<void>,
  signal?: AbortSignal
): T {
  let unsubscribed = false;
  const doUnsubscribe = () => {
    if (!unsubscribed) {
      unsubscribed = true;
      void onUnsubscribe();
    }
  };
  const sub = (() => {
    doUnsubscribe();
  }) as T;
  sub.unsubscribe = () => {
    doUnsubscribe();
  };
  sub[Symbol.dispose] = () => {
    doUnsubscribe();
  };
  sub[Symbol.asyncDispose] = async () => {
    doUnsubscribe();
  };
  if (signal) {
    if (signal.aborted) {
      doUnsubscribe();
    } else {
      signal.addEventListener("abort", () => doUnsubscribe(), { once: true });
    }
  }
  return sub;
}

async function nativeCall<T>(operation: Promise<T>, signal?: AbortSignal): Promise<T> {
  signal?.throwIfAborted();
  if (!signal) {
    try {
      return await operation;
    } catch (error) {
      handleNativeError(error);
    }
  }

  const resolvers = Promise.withResolvers<T>();
  const onAbort = () => {
    resolvers.reject(signal.reason ?? new DOMException("The operation was aborted", "AbortError"));
  };
  signal.addEventListener("abort", onAbort, { once: true });

  operation
    .then(resolvers.resolve, resolvers.reject)
    .finally(() => signal.removeEventListener("abort", onAbort));

  try {
    return await resolvers.promise;
  } catch (error) {
    if (signal.aborted) throw error;
    handleNativeError(error);
  }
}

function handleNativeError(error: unknown): never {
  if (error instanceof Error) {
    let shape;
    try {
      shape = JSON.parse(error.message);
    } catch {
      throw error;
    }
    if (typeof shape?.code === "string" && typeof shape?.message === "string") throw new LiliaError(shape);
  }
  throw error;
}

function nativeSyncCall<T>(operation: () => T): T {
  try {
    return operation();
  } catch (error) {
    handleNativeError(error);
  }
}

function loadNative(): NativeModule {
  let explicit = process.env.LILIA_NATIVE_PATH;
  if (!explicit) {
    const platform = nativePackagePlatform();
    const candidates = [
      new URL("../../../target/release/liblilia_node_native.dylib", import.meta.url).pathname,
      new URL("../../../target/release/liblilia_node_native.so", import.meta.url).pathname,
      new URL("../../../target/release/lilia_node_native.dll", import.meta.url).pathname,
      new URL(`../../node-${platform}/lilia_node_native.node`, import.meta.url).pathname,
      new URL(`../lilia-node-native.${platform}.node`, import.meta.url).pathname,
      new URL("../../../target/debug/liblilia_node_native.dylib", import.meta.url).pathname,
      new URL("../../../target/debug/liblilia_node_native.so", import.meta.url).pathname,
      new URL("../../../target/debug/lilia_node_native.dll", import.meta.url).pathname,
    ];
    for (const candidate of candidates) {
      if (existsSync(candidate)) {
        explicit = candidate;
        break;
      }
    }
  }
  if (explicit) {
    if (!explicit.endsWith(".node")) {
      const nodePath = `${explicit}.node`;
      try {
        const srcStat = statSync(explicit);
        const destStat = existsSync(nodePath) ? statSync(nodePath) : null;
        if (!destStat || srcStat.mtimeMs > destStat.mtimeMs) {
          copyFileSync(explicit, nodePath);
        }
      } catch {}
      if (existsSync(nodePath)) {
        explicit = nodePath;
      }
    }
    return require(explicit) as NativeModule;
  }
  const platform = nativePackagePlatform();
  try {
    return require(`@liliadb/node-${platform}`) as NativeModule;
  } catch (error) {
    throw new Error(`No LiliaDB native build for ${platform}; set LILIA_NATIVE_PATH`, { cause: error });
  }
}

function nativePackagePlatform(): string {
  if (process.platform === "darwin") return `darwin-${process.arch}`;
  if (process.platform === "linux") return `linux-${process.arch}-gnu`;
  if (process.platform === "win32") return "win32-x64-msvc";
  return `${process.platform}-${process.arch}`;
}

function mapKv(entry: NativeKvEntry): KvEntry {
  return {
    namespace: entry.namespace,
    key: entry.key,
    value: entry.value,
    version: readVersion(entry.version),
    expiresAtMs: entry.expiresAtMs,
  };
}

function parseJsonEntry<T = JsonValue>(value: string): JsonEntry<T> {
  return mapJsonEntry<T>(JSON.parse(value) as NativeRecord);
}
