// What a write changes, as topics: hooks subscribe to the topics of what they read and run
// again when a write of this database touches them. Topics name a model's container and, where
// there is one, its record (`kv:<namespace>:<hex key>`, `json:<space>:<id>`...), and the SQL
// tables a query reads (`table:<name>`).

import type { Operation, WireBatchOperation } from "@liliadb/client";

const encoder = new TextEncoder();

function hex(bytes: Uint8Array): string {
  let text = "";
  for (const byte of bytes) text += byte.toString(16).padStart(2, "0");
  return text;
}

/** The topics of each model. */
export const topics = {
  /** A namespace, or one of its keys. */
  kv: (namespace: string, key?: Uint8Array | string): string =>
    key === undefined
      ? `kv:${namespace}`
      : `kv:${namespace}:${hex(typeof key === "string" ? encoder.encode(key) : key)}`,
  /** A space, or one of its documents. */
  json: (space: string, id?: string): string =>
    id === undefined ? `json:${space}` : `json:${space}:${id}`,
  graph: (graph: string): string => `graph:${graph}`,
  fts: (collection: string): string => `fts:${collection}`,
  vector: (collection: string): string => `vector:${collection}`,
  /** Every series, or one. */
  timeseries: (seriesId?: string): string =>
    seriesId === undefined ? "timeseries" : `timeseries:${seriesId}`,
  geofences: (): string => "geo:fences",
  track: (trackId: string): string => `geo:track:${trackId}`,
  /** A space of CRDTs, or one of them. */
  crdt: (space: string, id?: string): string =>
    id === undefined ? `crdt:${space}` : `crdt:${space}:${id}`,
  /** A table of SQL queries: `lilia_kv`, `lilia_json`, a JSON space... */
  table: (name: string): string => `table:${name}`,
};

function kv(namespace: string, key: Uint8Array): string[] {
  return [topics.kv(namespace), topics.kv(namespace, key), topics.table("lilia_kv")];
}

function json(space: string, id: string): string[] {
  // Every JSON space is a table of its own, besides `lilia_json`.
  return [
    topics.json(space),
    topics.json(space, id),
    topics.table("lilia_json"),
    topics.table(space),
  ];
}

function nodes(graph: string): string[] {
  return [
    topics.graph(graph),
    topics.table("lilia_graph_nodes"),
    topics.table("lilia_graph_edges"),
  ];
}

function edges(graph: string): string[] {
  return [topics.graph(graph), topics.table("lilia_graph_edges")];
}

function crdt(space: string, id: string): string[] {
  return [topics.crdt(space), topics.crdt(space, id)];
}

function series(seriesId?: string): string[] {
  const all = [topics.timeseries(), topics.table("lilia_timeseries")];
  return seriesId === undefined ? all : [...all, topics.timeseries(seriesId)];
}

function batchTopics(operation: WireBatchOperation): string[] {
  switch (operation.model) {
    case "kv_set":
    case "kv_delete":
      return kv(operation.namespace, operation.key);
    case "json_put":
    case "json_put_raw":
    case "json_delete":
      return json(operation.space, operation.id);
    case "graph_set_node":
    case "graph_delete_node":
      return nodes(operation.graph);
    case "graph_add_edge":
    case "graph_remove_edge":
      return edges(operation.graph);
    case "fts_index":
    case "fts_delete":
      return [topics.fts(operation.collection)];
    case "vector_set":
    case "vector_delete":
      return [topics.vector(operation.collection)];
    case "crdt_pn_counter_increment":
    case "crdt_pn_counter_decrement":
    case "crdt_or_set_add":
    case "crdt_or_set_remove":
    case "crdt_json_set_field":
    case "crdt_json_delete_field":
    case "crdt_json_merge":
      return crdt(operation.space, operation.id);
  }
}

/**
 * The topics `operation` changed once it succeeded: none for a read, `undefined` when it may
 * have changed anything (`undo`, `redo`).
 */
export function changedTopics(operation: Operation): string[] | undefined {
  switch (operation.type) {
    case "batch":
      return [...new Set(operation.operations.flatMap(batchTopics))];
    case "undo":
    case "redo":
      return undefined;
    case "graph_delete_node":
      return nodes(operation.graph);
    case "graph_remove_edge":
      return edges(operation.graph);
    case "fts_delete":
    case "fts_load_dictionary":
      return [topics.fts(operation.collection)];
    case "vector_create_index":
    case "vector_drop_index":
      return [topics.vector(operation.collection)];
    case "ts_append_points":
      return series(operation.series_id);
    case "ts_apply_retention":
    case "ts_set_policy":
    case "ts_delete_policy":
      return series();
    case "geo_add_fence":
    case "geo_remove_fence":
      return [topics.geofences()];
    case "geo_record_gps_points":
      return [topics.track(operation.track_id)];
    case "crdt_counter_merge":
    case "crdt_set_merge":
    case "crdt_set_prune":
    case "crdt_json_merge":
      return crdt(operation.space, operation.id);
    default:
      return [];
  }
}
