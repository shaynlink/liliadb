// The CRDT calls: counters, sets and JSON documents.

import {
  mapCounterState,
  mapLwwDocument,
  mapMutations,
  mapSetState,
  toWireCounterState,
  toWireLwwDocument,
} from "../mappers.js";
import type {
  CallOptions,
  CrdtCounterState,
  CrdtOrSetState,
  JsonValue,
  LwwJsonDocumentState,
} from "../types.js";
import { type Calls, type Mode } from "./calls.js";

export function crdtApi<M extends Mode>({ call, ack, write, flag }: Calls<M>) {
  return {
    counter: {
      increment: (
        space: string,
        id: string,
        actorId: string,
        amount: number | bigint = 1,
        options: CallOptions = {},
      ) =>
        write({ model: "crdt_pn_counter_increment", space, id, actorId, amount }, options.signal),
      decrement: (
        space: string,
        id: string,
        actorId: string,
        amount: number | bigint = 1,
        options: CallOptions = {},
      ) =>
        write({ model: "crdt_pn_counter_decrement", space, id, actorId, amount }, options.signal),
      get: (space: string, id: string, options: CallOptions = {}) =>
        call(
          { type: "crdt_counter_get", space, id },
          "crdt_counter",
          (value) => Number(value),
          options.signal,
        ),
      state: (space: string, id: string, options: CallOptions = {}) =>
        call(
          { type: "crdt_counter_state", space, id },
          "crdt_counter_state",
          mapCounterState,
          options.signal,
        ),
      merge: (space: string, id: string, remote: CrdtCounterState, options: CallOptions = {}) =>
        call(
          { type: "crdt_counter_merge", space, id, remote: toWireCounterState(remote) },
          "crdt_counter",
          (value) => Number(value),
          options.signal,
        ),
    },
    set: {
      add: (
        space: string,
        id: string,
        actorId: string,
        element: string,
        options: CallOptions = {},
      ) => write({ model: "crdt_or_set_add", space, id, actorId, element }, options.signal),
      remove: (
        space: string,
        id: string,
        element: string,
        options: { timestampMs?: number } & CallOptions = {},
      ) =>
        write(
          { model: "crdt_or_set_remove", space, id, element, timestampMs: options.timestampMs },
          options.signal,
        ),
      read: (space: string, id: string, options: CallOptions = {}) =>
        call({ type: "crdt_set_get", space, id }, "ids", (elements) => elements, options.signal),
      contains: (space: string, id: string, element: string, options: CallOptions = {}) =>
        flag({ type: "crdt_set_contains", space, id, element }, options.signal),
      state: (space: string, id: string, options: CallOptions = {}) =>
        call({ type: "crdt_set_state", space, id }, "crdt_set_state", mapSetState, options.signal),
      /** Forgets the removals older than `cutoffMs`; whether any was. */
      prune: (space: string, id: string, cutoffMs: number, options: CallOptions = {}) =>
        flag({ type: "crdt_set_prune", space, id, cutoff_ms: cutoffMs }, options.signal),
      merge: (space: string, id: string, remote: CrdtOrSetState, options: CallOptions = {}) =>
        ack({ type: "crdt_set_merge", space, id, remote }, options.signal),
    },
    json: {
      setField: (
        space: string,
        id: string,
        actorId: string,
        path: string,
        value: JsonValue,
        options: { timestampMs?: number } & CallOptions = {},
      ) =>
        write(
          {
            model: "crdt_json_set_field",
            space,
            id,
            actorId,
            path,
            value,
            timestampMs: options.timestampMs,
          },
          options.signal,
        ),
      deleteField: (
        space: string,
        id: string,
        actorId: string,
        path: string,
        options: { timestampMs?: number } & CallOptions = {},
      ) =>
        write(
          {
            model: "crdt_json_delete_field",
            space,
            id,
            actorId,
            path,
            timestampMs: options.timestampMs,
          },
          options.signal,
        ),
      get: <T = JsonValue>(space: string, id: string, options: CallOptions = {}) =>
        call(
          { type: "crdt_json_get", space, id },
          "crdt_json",
          (value) => (value === null ? undefined : (value as T)),
          options.signal,
        ),
      state: (space: string, id: string, options: CallOptions = {}) =>
        call(
          { type: "crdt_json_state", space, id },
          "crdt_json_state",
          (state) => (state ? mapLwwDocument(state) : undefined),
          options.signal,
        ),
      /** Merges a remote state; the result has no version when the merge changed nothing. */
      merge: (space: string, id: string, remote: LwwJsonDocumentState, options: CallOptions = {}) =>
        call(
          { type: "crdt_json_merge", space, id, remote: toWireLwwDocument(remote) },
          "mutations",
          (results) => mapMutations(results)[0]!,
          options.signal,
        ),
    },
  };
}
