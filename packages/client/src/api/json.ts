// The JSON document calls.

import { mapJsonIndexes, mapJsonRevisions } from "../mappers.js";
import type {
  CallOptions,
  CreateIndexOptions,
  JsonDeleteOptions,
  JsonFindOptions,
  JsonGetOptions,
  JsonPutOptions,
  JsonScanOptions,
  JsonScanPrefixOptions,
  JsonValue,
} from "../types.js";
import { type Calls, type Mode, SCAN_LIMIT } from "./calls.js";

/** Whether an index target is already a path (`$.a`, `/a`, an expression) rather than a field name. */
const isPath = (value: string) =>
  value.startsWith("$") || value.startsWith("/") || value.includes("(");

/** `fieldOrPath` as an index name and path: a field `a.b` indexes `$.a.b`. */
function indexDefinition(
  fieldOrPath: string,
  optionsOrPath?: string | CreateIndexOptions,
): { name: string; path: string; filter?: string } {
  const defaultPath = isPath(fieldOrPath) ? fieldOrPath : `$.${fieldOrPath}`;
  if (typeof optionsOrPath === "string") return { name: fieldOrPath, path: optionsOrPath };
  return {
    name: optionsOrPath?.name ?? fieldOrPath,
    path: optionsOrPath?.path ?? defaultPath,
    filter: optionsOrPath?.filter,
  };
}

export function jsonApi<M extends Mode>({ call, ack, write, document, documents, flag }: Calls<M>) {
  return {
    get: <T = JsonValue>(space: string, id: string, options: JsonGetOptions = {}) =>
      document<T>({ type: "json_get", space, id }, options.signal),
    extract: <T = JsonValue>(
      space: string,
      id: string,
      path: string,
      options: JsonGetOptions = {},
    ) =>
      call(
        { type: "json_extract", space, id, path },
        "json_extracted",
        (value) => (value === null ? undefined : (value as T)),
        options.signal,
      ),
    put: <T = JsonValue>(space: string, id: string, value: T, options: JsonPutOptions = {}) =>
      write(
        { model: "json_put", space, id, value: value as JsonValue, ifVersion: options.ifVersion },
        options.signal,
      ),
    delete: (space: string, id: string, options: JsonDeleteOptions = {}) =>
      write({ model: "json_delete", space, id, ifVersion: options.ifVersion }, options.signal),
    scan: <T = JsonValue>(space: string, options: JsonScanOptions = {}) =>
      documents<T>(
        { type: "json_scan", space, after: options.after, limit: options.limit ?? SCAN_LIMIT },
        options.signal,
      ),
    scanPrefix: <T = JsonValue>(
      space: string,
      prefix: string,
      options: JsonScanPrefixOptions = {},
    ) =>
      documents<T>(
        {
          type: "json_scan_prefix",
          space,
          prefix,
          after_id: options.afterId,
          limit: options.limit ?? SCAN_LIMIT,
        },
        options.signal,
      ),
    find: <T = JsonValue>(
      space: string,
      query: Record<string, JsonValue>,
      options: JsonFindOptions = {},
    ) =>
      documents<T>(
        {
          type: "json_find",
          space,
          query,
          after: options.after,
          limit: options.limit ?? SCAN_LIMIT,
        },
        options.signal,
      ),
    findByIndex: <T = JsonValue>(
      space: string,
      indexName: string,
      value: string | number | boolean,
      options: JsonFindOptions = {},
    ) =>
      documents<T>(
        {
          type: "json_scan_by_index",
          space,
          index_name: indexName,
          value: String(value),
          after: options.after,
          limit: options.limit ?? SCAN_LIMIT,
        },
        options.signal,
      ),
    createIndex: (
      space: string,
      fieldOrPath: string,
      optionsOrPath?: string | CreateIndexOptions,
      options: CallOptions = {},
    ) => {
      const { name, path, filter } = indexDefinition(fieldOrPath, optionsOrPath);
      return ack({ type: "json_create_index", space, name, path, filter }, options.signal);
    },
    dropIndex: (space: string, indexName: string, options: CallOptions = {}) =>
      flag({ type: "json_drop_index", space, name: indexName }, options.signal),
    listIndexes: (space: string, options: CallOptions = {}) =>
      call({ type: "json_list_indexes", space }, "json_indexes", mapJsonIndexes, options.signal),
    getAtVersion: <T = JsonValue>(
      space: string,
      id: string,
      version: bigint,
      options: JsonGetOptions = {},
    ) => document<T>({ type: "json_get_at_version", space, id, version }, options.signal),
    getAtTimestamp: <T = JsonValue>(
      space: string,
      id: string,
      timestampMs: number,
      options: JsonGetOptions = {},
    ) =>
      document<T>(
        { type: "json_get_at_timestamp", space, id, timestamp_ms: timestampMs },
        options.signal,
      ),
    history: <T = JsonValue>(space: string, id: string, options: JsonGetOptions = {}) =>
      call(
        { type: "json_history", space, id },
        "json_revisions",
        (revisions) => mapJsonRevisions<T>(revisions),
        options.signal,
      ),
    diff: (
      space: string,
      id: string,
      fromVersion: bigint,
      toVersion: bigint,
      options: JsonGetOptions = {},
    ) =>
      call(
        { type: "json_diff", space, id, from_version: fromVersion, to_version: toVersion },
        "json_value",
        (diff) => diff,
        options.signal,
      ),
  };
}
