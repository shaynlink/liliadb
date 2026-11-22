// The operations of the LiliaDB protocol and their results, as the native module and the
// daemon exchange them with JavaScript: operations in MessagePack, results in JS frames (one
// JSON text for V8's native `JSON.parse`, then the raw bytes it refers to). Field names are
// those of the Rust types (snake case); `mappers.ts` turns them into the public types.

import { Packr } from "msgpackr";
import { LiliaError, type JsonValue } from "./types.js";

/** An integer of a request; integers beyond 32 bits go as `bigint`s (see `encodeWire`). */
type Int = number | bigint;
/** A version in a result: a decimal string where the engine keeps all 64 bits, else a number. */
type Version = string | number;

export type WireDirection = "outbound" | "inbound" | "both";
export type WireElementType = "float32" | "int8" | "bit";
export type WireMetric = "cosine" | "l2" | "dot" | "hamming";
export type WireAggregation = "Avg" | "Min" | "Max" | "Sum" | "First" | "Last" | "Count";

export type WireBatchOperation =
  | { model: "kv_set"; namespace: string; key: Uint8Array; value: Uint8Array; if_version?: Int; expires_at_ms?: number }
  | { model: "kv_delete"; namespace: string; key: Uint8Array; if_version?: Int }
  | { model: "json_put"; space: string; id: string; value: JsonValue; if_version?: Int }
  | { model: "json_put_raw"; space: string; id: string; data: Uint8Array; if_version?: Int }
  | { model: "json_delete"; space: string; id: string; if_version?: Int }
  | { model: "graph_set_node"; graph: string; id: string; kind: string; metadata: JsonValue; doc_space?: string; doc_id?: string }
  | { model: "graph_delete_node"; graph: string; id: string }
  | { model: "graph_add_edge"; graph: string; source: string; target: string; kind: string; weight: number }
  | { model: "graph_remove_edge"; graph: string; source: string; target: string; kind: string }
  | { model: "fts_index"; collection: string; doc_id: string; content: string }
  | { model: "fts_delete"; collection: string; doc_id: string }
  | { model: "vector_set"; collection: string; id: string; dim: number; element_type: WireElementType; vector: Uint8Array }
  | { model: "vector_delete"; collection: string; id: string }
  | { model: "crdt_pn_counter_increment"; space: string; id: string; actor_id: string; amount: Int }
  | { model: "crdt_pn_counter_decrement"; space: string; id: string; actor_id: string; amount: Int }
  | { model: "crdt_or_set_add"; space: string; id: string; actor_id: string; element: string }
  | { model: "crdt_or_set_remove"; space: string; id: string; element: string; timestamp_ms?: number }
  | { model: "crdt_json_set_field"; space: string; id: string; actor_id: string; path: string; value: JsonValue; timestamp_ms?: number }
  | { model: "crdt_json_delete_field"; space: string; id: string; actor_id: string; path: string; timestamp_ms?: number }
  | { model: "crdt_json_merge"; space: string; id: string; state: Uint8Array };

export interface WirePageRankConfig {
  damping_factor?: number;
  max_iterations?: number;
  tolerance?: number;
  seeds?: string[];
  direction?: WireDirection;
  edge_kind?: string;
}

/** Fields left out take the engine's defaults. */
export interface WireHnswConfig {
  m?: number;
  ef_construction?: number;
  ef_search?: number;
  metric?: WireMetric;
  element_type?: WireElementType;
  dim: number;
}

export interface WireRetentionRule {
  resolution_ms: number;
  retention_ms: number | null;
  aggregation: WireAggregation;
}

export interface WirePolicy {
  series_pattern: string;
  rules: WireRetentionRule[];
}

export interface WirePoint {
  timestamp: number;
  value: number;
}

export interface WireGpsPoint {
  timestamp: number;
  latitude: number;
  longitude: number;
  altitude?: number;
  speed?: number;
  accuracy?: number;
}

export interface WireCounterState {
  p: Record<string, Int>;
  n: Record<string, Int>;
}

export interface WireOrSetState {
  entries: Record<string, string[]>;
  tombstones: Record<string, Int>;
}

export interface WireLwwField {
  value: JsonValue;
  clock: { clock: { timestamp_ms: Int; counter: Int }; actor_id: string };
  is_deleted: boolean;
}

export interface WireLwwDocument {
  fields: Record<string, WireLwwField>;
}

/** One operation of the protocol. A `limit` of 0 reads everything. */
export type Operation =
  | { type: "handshake"; major: number; minor: number }
  | { type: "shutdown" }
  | { type: "integrity_check" }
  | { type: "quick_check" }
  | { type: "checkpoint" }
  | { type: "storage_stats" }
  | { type: "kv_get"; namespace: string; key: Uint8Array }
  | { type: "kv_scan"; namespace: string; after?: Uint8Array; limit: number }
  | { type: "json_get"; space: string; id: string }
  | { type: "json_scan"; space: string; after?: string; limit: number }
  | { type: "json_scan_prefix"; space: string; prefix: string; after_id?: string; limit: number }
  | { type: "json_extract"; space: string; id: string; path: string }
  | { type: "json_find"; space: string; query: Record<string, JsonValue>; after?: string; limit: number }
  | { type: "json_create_index"; space: string; name: string; path: string; filter?: string }
  | { type: "json_drop_index"; space: string; name: string }
  | { type: "json_list_indexes"; space: string }
  | { type: "json_scan_by_index"; space: string; index_name: string; value: string; after?: string; limit: number }
  | { type: "json_get_at_version"; space: string; id: string; version: bigint }
  | { type: "json_get_at_timestamp"; space: string; id: string; timestamp_ms: number }
  | { type: "json_history"; space: string; id: string }
  | { type: "json_diff"; space: string; id: string; from_version: bigint; to_version: bigint }
  | { type: "batch"; operations: WireBatchOperation[] }
  | { type: "sql_query"; sql: string; params: JsonValue[] }
  | { type: "graph_neighbors"; graph: string; node: string; direction: WireDirection; kind?: string }
  | { type: "graph_reachability"; graph: string; start_nodes: string[]; direction: WireDirection }
  | { type: "graph_get_node"; graph: string; id: string }
  | { type: "graph_get_node_document"; graph: string; id: string }
  | { type: "graph_delete_node"; graph: string; id: string }
  | { type: "graph_list_nodes"; graph: string; kind?: string; limit: number }
  | { type: "graph_remove_edge"; graph: string; source: string; target: string; kind: string }
  | { type: "graph_list_edges"; graph: string; source?: string; target?: string; kind?: string; limit: number }
  | { type: "graph_impact_analysis"; graph: string; changed_nodes: string[] }
  | { type: "graph_shortest_path"; graph: string; start: string; target: string; direction: WireDirection; kind_filter?: string }
  | { type: "graph_dijkstra"; graph: string; start: string; target?: string; direction: WireDirection; kind_filter?: string }
  | { type: "graph_all_shortest_paths"; graph: string; start: string; direction: WireDirection; kind_filter?: string }
  | { type: "graph_astar_spatial"; graph: string; start: string; target: string; direction: WireDirection; kind_filter?: string }
  | { type: "graph_page_rank"; graph: string; config: WirePageRankConfig }
  | { type: "graph_centrality"; graph: string; centrality: string; direction: WireDirection; kind_filter?: string; normalized: boolean }
  | { type: "graph_has_cycle"; graph: string; direction: WireDirection; kind_filter?: string }
  | { type: "graph_find_cycles"; graph: string; max_cycles: number; kind_filter?: string }
  | { type: "graph_connected_components"; graph: string; component_type: string; kind_filter?: string }
  | { type: "graph_cypher"; graph: string; query: string }
  | { type: "fts_search"; collection: string; query: string; limit: number }
  | { type: "fts_delete"; collection: string; doc_id: string }
  | { type: "fts_suggest"; collection: string; prefix: string; max_distance: number; limit: number }
  | { type: "fts_fuzzy_search"; collection: string; query: string; max_distance: number; limit: number }
  | { type: "fts_phonetic_search"; collection: string; query: string; limit: number }
  | { type: "fts_load_dictionary"; collection: string; words: string[] }
  | { type: "vector_get"; collection: string; id: string }
  | { type: "vector_search"; collection: string; vector: Uint8Array; dim: number; element_type: WireElementType; metric: WireMetric; limit: number }
  | { type: "vector_create_index"; collection: string; config: WireHnswConfig }
  | { type: "vector_get_index_info"; collection: string }
  | { type: "vector_drop_index"; collection: string }
  | {
      type: "vector_search_filtered";
      collection: string;
      query_vector: Uint8Array;
      dim: number;
      element_type: WireElementType;
      metric: WireMetric;
      candidate_ids: string[];
      limit: number;
    }
  | {
      type: "vector_search_in_graph_neighbors";
      collection: string;
      graph: string;
      start_node: string;
      direction: WireDirection;
      query_vector: Uint8Array;
      dim: number;
      element_type: WireElementType;
      metric: WireMetric;
      limit: number;
    }
  | {
      type: "vector_search_by_json_index";
      collection: string;
      space: string;
      index_name: string;
      index_value: string;
      query_vector: Uint8Array;
      dim: number;
      element_type: WireElementType;
      metric: WireMetric;
      limit: number;
    }
  | {
      type: "hybrid_search";
      collection: string;
      fts_query?: string;
      vector?: Uint8Array;
      dim?: number;
      element_type?: WireElementType;
      metric?: WireMetric;
      rrf_k: number;
      fts_weight: number;
      vector_weight: number;
      limit: number;
    }
  | { type: "ts_append_points"; series_id: string; points: WirePoint[]; resolution_ms?: number }
  | { type: "ts_query_range"; series_id: string; start_time: number; end_time: number; resolution_ms?: number }
  | { type: "ts_query_auto"; series_id: string; start_time: number; end_time: number; max_points: number }
  | { type: "ts_query_stats"; series_id: string; start_time: number; end_time: number; resolution_ms?: number }
  | { type: "ts_set_policy"; policy: WirePolicy }
  | { type: "ts_get_policy"; series_pattern: string }
  | { type: "ts_list_policies" }
  | { type: "ts_delete_policy"; series_pattern: string }
  | { type: "ts_apply_retention"; now_ms?: number }
  | { type: "geo_add_fence"; id: string; name: string; category: string; polygon_json: string; metadata?: string }
  | { type: "geo_remove_fence"; id: string }
  | { type: "geo_contains_point"; polygon_json: string; lon: number; lat: number }
  | { type: "geo_find_fences_for_point"; lon: number; lat: number }
  | { type: "geo_record_gps_points"; track_id: string; points: WireGpsPoint[] }
  | { type: "geo_get_track_points"; track_id: string; start_time?: number; end_time?: number }
  | { type: "geo_track_distance"; track_id: string }
  | { type: "crdt_counter_get"; space: string; id: string }
  | { type: "crdt_counter_state"; space: string; id: string }
  | { type: "crdt_counter_merge"; space: string; id: string; remote: WireCounterState }
  | { type: "crdt_set_get"; space: string; id: string }
  | { type: "crdt_set_contains"; space: string; id: string; element: string }
  | { type: "crdt_set_state"; space: string; id: string }
  | { type: "crdt_set_prune"; space: string; id: string; cutoff_ms: number }
  | { type: "crdt_set_merge"; space: string; id: string; remote: WireOrSetState }
  | { type: "crdt_json_get"; space: string; id: string }
  | { type: "crdt_json_state"; space: string; id: string }
  | { type: "crdt_json_merge"; space: string; id: string; remote: WireLwwDocument }
  | { type: "undo" }
  | { type: "redo" }
  | { type: "undo_history" };

export interface WireKvEntry {
  namespace: string;
  key: Uint8Array;
  value: Uint8Array;
  version: Version;
  expires_at_ms: number | null;
}

export interface WireJsonEntry {
  space: string;
  id: string;
  value: JsonValue;
  version: Version;
}

export interface WireGraphNode {
  graph: string;
  id: string;
  kind: string;
  metadata: JsonValue;
  doc_space: string | null;
  doc_id: string | null;
}

export interface WireGraphEdge {
  graph: string;
  source: string;
  target: string;
  kind: string;
  weight: number;
}

export interface WireWeightedPath {
  nodes: string[];
  total_weight: number;
  edges: { source: string; target: string; kind: string; weight: number }[];
}

export interface WireFtsHit {
  collection: string;
  doc_id: string;
  snippet: string;
  rank: number;
}

export interface WireVectorHit {
  collection: string;
  id: string;
  distance: number;
}

export interface WireUndoStep {
  version: Version;
  keys: number;
}

/** The result of an operation, by its `type`. */
export type ResponseValue =
  | { type: "handshake"; value: { major: number; minor: number; capabilities: string[] } }
  | { type: "shutdown" }
  | { type: "ack" }
  | { type: "bool"; value: boolean }
  | { type: "f64"; value: number }
  | { type: "u64"; value: Int }
  | {
      type: "storage_stats";
      value: {
        levels: { runs: Int; bytes: Int }[];
        memtable_bytes: Int;
        immutable_memtables: Int;
        version: Version;
        oldest_version: Version;
      };
    }
  | { type: "kv"; value: WireKvEntry | null }
  | { type: "kv_page"; value: WireKvEntry[] }
  | { type: "json"; value: WireJsonEntry | null }
  | { type: "json_page"; value: WireJsonEntry[] }
  | { type: "json_indexes"; value: { space: string; index_name: string; json_path: string }[] }
  | { type: "json_extracted"; value: JsonValue | null }
  | { type: "json_value"; value: JsonValue }
  | { type: "json_revisions"; value: { version: Version; doc: WireJsonEntry | null }[] }
  | { type: "mutations"; value: { version?: Version | null; deleted: boolean }[] }
  | { type: "sql"; value: { columns: string[]; rows: JsonValue[][]; rows_affected: Int } }
  | { type: "ids"; value: string[] }
  | { type: "graph_node"; value: WireGraphNode | null }
  | { type: "graph_nodes"; value: WireGraphNode[] }
  | { type: "graph_edges"; value: WireGraphEdge[] }
  | { type: "graph_impact"; value: { direct: string[]; dependants: string[]; total_affected: Int } }
  | { type: "weighted_path"; value: WireWeightedPath | null }
  | { type: "weighted_path_map"; value: Record<string, WireWeightedPath> }
  | { type: "node_scores"; value: { node: string; score: number }[] }
  | { type: "node_groups"; value: string[][] }
  | { type: "cypher_result"; value: { columns: string[]; rows: JsonValue[][] } }
  | { type: "fts_hits"; value: WireFtsHit[] }
  | { type: "fts_suggestions"; value: { term: string; distance: number; doc_count: number; exact_prefix: boolean }[] }
  | { type: "fts_fuzzy_hits"; value: (WireFtsHit & { edit_distance: number; matched_terms: string[] })[] }
  | { type: "fts_phonetic_hits"; value: (WireFtsHit & { matched_terms: string[]; phonetic_codes: string[] })[] }
  | { type: "vector"; value: { dim: number; element_type: WireElementType; data: Uint8Array } | null }
  | { type: "vector_hits"; value: WireVectorHit[] }
  | { type: "vector_index_info"; value: (Required<WireHnswConfig> & { collection: string; node_count: Int; max_level: Int }) | null }
  | {
      type: "hybrid_hits";
      value: {
        id: string;
        rrf_score: number;
        fts_rank: number | null;
        fts_snippet: string | null;
        vector_rank: number | null;
        vector_distance: number | null;
      }[];
    }
  | { type: "ts_points"; value: WirePoint[] }
  | { type: "ts_stats"; value: { min: number; max: number; avg: number; sum: number; count: Int } | null }
  | { type: "ts_policy"; value: WirePolicy | null }
  | { type: "ts_policies"; value: WirePolicy[] }
  | {
      type: "ts_retention_stats";
      value: { series_processed: Int; downsampled_points_created: Int; chunks_pruned: Int; bytes_reclaimed_approx: Int };
    }
  | { type: "geofence_matches"; value: { id: string; name: string; category: string; metadata?: string | null }[] }
  | { type: "gps_points"; value: WireGpsPoint[] }
  | { type: "crdt_counter"; value: Int }
  | { type: "crdt_counter_state"; value: WireCounterState }
  | { type: "crdt_set_state"; value: WireOrSetState }
  | { type: "crdt_json"; value: JsonValue | null }
  | { type: "crdt_json_state"; value: WireLwwDocument | null }
  | { type: "undo_step"; value: WireUndoStep | null }
  | { type: "undo_steps"; value: WireUndoStep[] };

export type ResponseType = ResponseValue["type"];
/** The value a response of type `K` carries. */
export type ResponseOf<K extends ResponseType> = Extract<ResponseValue, { type: K }> extends { value: infer V } ? V : undefined;

/** An error as the protocol carries it. */
export interface WireError {
  code: string;
  message: string;
  retryable: boolean;
  details?: JsonValue;
  request_id: string;
}

export type WireResult = { Ok: ResponseValue } | { Err: WireError };

/** Absent fields go as nil, which the engine reads as `None`. */
const packr = new Packr({ useRecords: false, encodeUndefinedAsNil: true });

/**
 * `value` without its undefined fields, and with its integers beyond 32 bits as `bigint`s: the
 * encoder writes such numbers as floats, which would turn the engine's integer fields
 * (timestamps, counts) and the integers of documents into floats.
 */
function integers(value: unknown): unknown {
  if (typeof value === "number") {
    return Number.isSafeInteger(value) && (value < -0x8000_0000 || value > 0xffff_ffff) ? BigInt(value) : value;
  }
  if (value === null || typeof value !== "object" || ArrayBuffer.isView(value)) return value;
  if (Array.isArray(value)) return value.map(integers);
  const copy: Record<string, unknown> = {};
  for (const key in value) {
    const item = (value as Record<string, unknown>)[key];
    // An absent field takes its default (`None`, or the field's serde default).
    if (item !== undefined) copy[key] = integers(item);
  }
  return copy;
}

/** The MessagePack of an operation (or a request); absent fields are left out. The buffer is
 * valid until the next call: callers hand it over (or copy it) at once. */
export function encodeWire(value: unknown): Buffer {
  return packr.pack(integers(value));
}

/**
 * The value of the JS frame in `frame` (from its start): `[u32 JSON length][JSON][u32 segment
 * count][(u32 offset, u32 length) per segment][segment bytes]`, all little-endian. Each byte
 * string of the value is the number of its segment until `bytesOf` replaces it.
 */
export function readFrame<T>(frame: Buffer): { value: T; segment: (index: number) => Uint8Array } {
  const json = frame.readUInt32LE(0);
  const value = JSON.parse(frame.toString("utf8", 4, 4 + json)) as T;
  const table = 8 + json;
  const bytes = table + frame.readUInt32LE(4 + json) * 8;
  const segment = (index: number): Uint8Array => {
    const at = table + index * 8;
    const start = bytes + frame.readUInt32LE(at);
    // A copy: the frame may be a buffer the next call writes over.
    return new Uint8Array(frame.subarray(start, start + frame.readUInt32LE(at + 4)));
  };
  return { value, segment };
}

/** `result` with its byte strings (keys, values, vectors) taken from their segments. */
export function bytesOf(result: WireResult, segment: (index: number) => Uint8Array): WireResult {
  if (!("Ok" in result)) return result;
  const response = result.Ok;
  const entry = (kv: WireKvEntry) => {
    kv.key = segment(kv.key as unknown as number);
    kv.value = segment(kv.value as unknown as number);
  };
  if (response.type === "kv" && response.value) entry(response.value);
  else if (response.type === "kv_page") response.value.forEach(entry);
  else if (response.type === "vector" && response.value) response.value.data = segment(response.value.data as unknown as number);
  return result;
}

/** The error the protocol reports, as a `LiliaError`. */
export function wireError(error: WireError): LiliaError {
  return new LiliaError({
    code: error.code,
    message: error.message,
    retryable: error.retryable,
    details: error.details,
    request_id: error.request_id,
  });
}

/** The value of a result; its error, thrown. */
export function unwrap(result: WireResult): ResponseValue {
  if ("Err" in result) throw wireError(result.Err);
  return result.Ok;
}

/** The value `response` carries when it is of type `type`; a `PROTOCOL` error otherwise. */
export function expect<K extends ResponseType>(response: ResponseValue, type: K): ResponseOf<K> {
  if (response.type !== type) {
    throw new LiliaError({
      code: "PROTOCOL",
      message: `expected a ${type} response, got ${response.type}`,
      retryable: false,
    });
  }
  return (response as { value?: unknown }).value as ResponseOf<K>;
}
