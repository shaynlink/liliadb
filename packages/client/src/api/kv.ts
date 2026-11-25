// The key/value calls.

import { mapKvEntry } from "../mappers.js";
import type { KvDeleteOptions, KvGetOptions, KvScanOptions, KvSetOptions } from "../types.js";
import { type Calls, type Mode, SCAN_LIMIT, bytes } from "./calls.js";

export function kvApi<M extends Mode>({ call, write }: Calls<M>) {
  return {
    get: (namespace: string, key: Uint8Array | string, options: KvGetOptions = {}) =>
      call(
        { type: "kv_get", namespace, key: bytes(key) },
        "kv",
        (entry) => (entry ? mapKvEntry(entry) : undefined),
        options.signal,
      ),
    set: (
      namespace: string,
      key: Uint8Array | string,
      value: Uint8Array | string,
      options: KvSetOptions = {},
    ) =>
      write(
        {
          model: "kv_set",
          namespace,
          key: bytes(key),
          value: bytes(value),
          ifVersion: options.ifVersion,
          expiresAtMs: options.expiresAtMs,
        },
        options.signal,
      ),
    delete: (namespace: string, key: Uint8Array | string, options: KvDeleteOptions = {}) =>
      write(
        { model: "kv_delete", namespace, key: bytes(key), ifVersion: options.ifVersion },
        options.signal,
      ),
    scan: (namespace: string, options: KvScanOptions = {}) =>
      call(
        {
          type: "kv_scan",
          namespace,
          after: options.after === undefined ? undefined : bytes(options.after),
          limit: options.limit ?? SCAN_LIMIT,
        },
        "kv_page",
        (entries) => entries.map(mapKvEntry),
        options.signal,
      ),
  };
}
