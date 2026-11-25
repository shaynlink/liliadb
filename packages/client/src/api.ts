// The calls of a database, each defined once over a runner: the asynchronous API runs them on
// the transport's workers and returns promises, the synchronous one runs them in place. Each
// model's calls are in its module of `api/`.

import { mapStorageStats, mapUndoStep } from "./mappers.js";
import type { BatchOperation, CallOptions } from "./types.js";
import { calls, sqlApi, type Mode, type Runner } from "./api/calls.js";
import { crdtApi } from "./api/crdt.js";
import { ftsApi } from "./api/fts.js";
import { geoApi } from "./api/geo.js";
import { graphApi } from "./api/graph.js";
import { hybridApi } from "./api/hybrid.js";
import { jsonApi } from "./api/json.js";
import { kvApi } from "./api/kv.js";
import { timeseriesApi } from "./api/timeseries.js";
import { vectorApi } from "./api/vector.js";

/** The calls of a database over `run`. */
export function buildApi<M extends Mode>(run: Runner<M>) {
  const helpers = calls(run);
  const { call, ack, batch, flag } = helpers;
  return {
    kv: kvApi(helpers),
    json: jsonApi(helpers),
    graph: graphApi(helpers),
    fts: ftsApi(helpers),
    vector: vectorApi(helpers),
    hybrid: hybridApi(helpers),
    timeseries: timeseriesApi(helpers),
    geo: geoApi(helpers),
    crdt: crdtApi(helpers),
    batch: (operations: readonly BatchOperation[], options: CallOptions = {}) =>
      batch(operations, options.signal),
    /** Undoes the latest commit not undone; the commit, or nothing when there is none. */
    undo: (options: CallOptions = {}) =>
      call(
        { type: "undo" },
        "undo_step",
        (step) => (step ? mapUndoStep(step) : undefined),
        options.signal,
      ),
    /** Redoes the latest undone commit; the commit, or nothing when there is none. */
    redo: (options: CallOptions = {}) =>
      call(
        { type: "redo" },
        "undo_step",
        (step) => (step ? mapUndoStep(step) : undefined),
        options.signal,
      ),
    undoHistory: (options: CallOptions = {}) =>
      call(
        { type: "undo_history" },
        "undo_steps",
        (steps) => steps.map(mapUndoStep),
        options.signal,
      ),
    /** Verifies the ledger, fast; true without a ledger. */
    quickCheck: (options: CallOptions = {}) => flag({ type: "quick_check" }, options.signal),
    /** Reads every stored entry, verifying each block's checksum, and verifies the ledger. */
    integrityCheck: (options: CallOptions = {}) =>
      flag({ type: "integrity_check" }, options.signal),
    /** Writes the memtable to a sorted run. */
    checkpoint: (options: CallOptions = {}) => ack({ type: "checkpoint" }, options.signal),
    storageStats: (options: CallOptions = {}) =>
      call({ type: "storage_stats" }, "storage_stats", mapStorageStats, options.signal),
    sql: sqlApi(helpers),
  };
}

export type Api<M extends Mode> = ReturnType<typeof buildApi<M>>;
