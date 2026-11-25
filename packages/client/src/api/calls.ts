// What every call of a database is built on: the runner of a mode, and the helpers that run an
// operation and map its response.

import {
  mapJsonEntry,
  mapMutations,
  mapSqlResult,
  mapVectorHits,
  mapWeightedPath,
  toWireBatchOperation,
} from "../mappers.js";
import { vectorDim, vectorToBytes } from "../math.js";
import type {
  BatchOperation,
  JsonValue,
  SqlFunction,
  SqlQueryOptions,
  SqlQueryResult,
  SyncSqlFunction,
  VectorElementType,
  VectorInput,
} from "../types.js";
import {
  expect,
  type Operation,
  type ResponseOf,
  type ResponseType,
  type ResponseValue,
  type WireElementType,
} from "../wire.js";

export type Mode = "async" | "sync";
/** A result in mode `M`: a promise of it, or itself. */
export type Out<M extends Mode, T> = M extends "async" ? Promise<T> : T;
/** Runs `operation` and maps its response; `signal` stops the wait of an asynchronous run. */
export type Runner<M extends Mode> = <T>(
  operation: Operation,
  map: (response: ResponseValue) => T,
  signal?: AbortSignal,
) => Out<M, T>;

/** Results count of a search when the caller gives none. */
export const SEARCH_LIMIT = 10;
/** Records of a scan when the caller gives no limit. */
export const SCAN_LIMIT = 100;

const encoder = new TextEncoder();
export const bytes = (value: Uint8Array | string): Uint8Array =>
  typeof value === "string" ? encoder.encode(value) : value;

/** A query vector: its bytes, dimension and element type. */
export function queryVector(
  vector: VectorInput,
  elementType?: VectorElementType,
  dim?: number,
): { data: Uint8Array; dim: number; element_type: WireElementType } {
  return {
    data: vectorToBytes(vector, elementType),
    dim: vectorDim(vector, elementType, dim),
    element_type: elementType ?? "float32",
  };
}

/** A tagged SQL template as a query with `$n` parameters. */
function taggedQuery(
  strings: TemplateStringsArray,
  values: JsonValue[],
): { sql: string; params: JsonValue[] } {
  const sql = strings.reduce(
    (query, part, i) => query + part + (i < values.length ? `$${i + 1}` : ""),
    "",
  );
  return { sql, params: values };
}

/** The helpers the calls of a database are written with, over `run`. */
export function calls<M extends Mode>(run: Runner<M>) {
  const call = <K extends ResponseType, T>(
    operation: Operation,
    type: K,
    map: (value: ResponseOf<K>) => T,
    signal?: AbortSignal,
  ): Out<M, T> => run(operation, (response) => map(expect(response, type)), signal);
  const ack = (operation: Operation, signal?: AbortSignal): Out<M, void> =>
    call(operation, "ack", () => undefined, signal);
  const batch = (operations: readonly BatchOperation[], signal?: AbortSignal) =>
    call(
      { type: "batch", operations: operations.map(toWireBatchOperation) },
      "mutations",
      mapMutations,
      signal,
    );
  const write = (operation: BatchOperation, signal?: AbortSignal) =>
    call(
      { type: "batch", operations: [toWireBatchOperation(operation)] },
      "mutations",
      (results) => mapMutations(results)[0]!,
      signal,
    );
  const document = <T>(operation: Operation, signal?: AbortSignal) =>
    call(operation, "json", (entry) => (entry ? mapJsonEntry<T>(entry) : undefined), signal);
  const documents = <T>(operation: Operation, signal?: AbortSignal) =>
    call(
      operation,
      "json_page",
      (entries) => entries.map((entry) => mapJsonEntry<T>(entry)),
      signal,
    );
  const weightedPath = (operation: Operation, signal?: AbortSignal) =>
    call(
      operation,
      "weighted_path",
      (found) => (found ? mapWeightedPath(found) : undefined),
      signal,
    );
  const vectorHits = (operation: Operation, signal?: AbortSignal) =>
    call(operation, "vector_hits", mapVectorHits, signal);
  const flag = (operation: Operation, signal?: AbortSignal) =>
    call(operation, "bool", (value) => value, signal);
  return { call, ack, batch, write, document, documents, weightedPath, vectorHits, flag };
}

export type Calls<M extends Mode> = ReturnType<typeof calls<M>>;

/** `sql`: a tagged template of records, with `query` (records) and `raw` (the full result). */
export function sqlApi<M extends Mode>({ call }: Calls<M>) {
  const sqlRaw = <T>(sql: string, params: JsonValue[] = [], options: SqlQueryOptions = {}) =>
    call(
      { type: "sql_query", sql, params },
      "sql",
      (result) => mapSqlResult<T>(result),
      options.signal,
    );
  const sqlRecords = <T>(sql: string, params: JsonValue[] = [], options: SqlQueryOptions = {}) =>
    call(
      { type: "sql_query", sql, params },
      "sql",
      (result) => mapSqlResult<T>(result).records,
      options.signal,
    );
  const sql = Object.assign(
    <T = Record<string, unknown>>(strings: TemplateStringsArray, ...values: JsonValue[]) => {
      const query = taggedQuery(strings, values);
      return sqlRecords<T>(query.sql, query.params);
    },
    {
      query: <T = Record<string, unknown>>(
        query: string,
        params?: JsonValue[],
        options?: SqlQueryOptions,
      ) => sqlRecords<T>(query, params, options),
      raw: <T = Record<string, unknown>>(
        query: string,
        params?: JsonValue[],
        options?: SqlQueryOptions,
      ): Out<M, SqlQueryResult<T>> => sqlRaw<T>(query, params, options),
    },
  ) as unknown as M extends "async" ? SqlFunction : SyncSqlFunction;
  return sql;
}
