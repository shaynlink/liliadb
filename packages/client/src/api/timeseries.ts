// The time-series calls.

import {
  mapPoints,
  mapPolicy,
  mapRetentionStats,
  mapSeriesStats,
  toWirePolicy,
} from "../mappers.js";
import type { CallOptions, TimeSeriesLifecyclePolicy, TimeSeriesPoint } from "../types.js";
import { type Calls, type Mode } from "./calls.js";

export function timeseriesApi<M extends Mode>({ call, ack, flag }: Calls<M>) {
  return {
    appendPoints: (
      seriesId: string,
      points: TimeSeriesPoint[],
      options: { resolutionMs?: number } & CallOptions = {},
    ) =>
      ack(
        {
          type: "ts_append_points",
          series_id: seriesId,
          points,
          resolution_ms: options.resolutionMs,
        },
        options.signal,
      ),
    queryRange: (
      seriesId: string,
      startTime: number,
      endTime: number,
      options: { resolutionMs?: number } & CallOptions = {},
    ) =>
      call(
        {
          type: "ts_query_range",
          series_id: seriesId,
          start_time: startTime,
          end_time: endTime,
          resolution_ms: options.resolutionMs,
        },
        "ts_points",
        mapPoints,
        options.signal,
      ),
    queryAuto: (
      seriesId: string,
      startTime: number,
      endTime: number,
      maxPoints: number,
      options: CallOptions = {},
    ) =>
      call(
        {
          type: "ts_query_auto",
          series_id: seriesId,
          start_time: startTime,
          end_time: endTime,
          max_points: maxPoints,
        },
        "ts_points",
        mapPoints,
        options.signal,
      ),
    queryStats: (
      seriesId: string,
      startTime: number,
      endTime: number,
      options: { resolutionMs?: number } & CallOptions = {},
    ) =>
      call(
        {
          type: "ts_query_stats",
          series_id: seriesId,
          start_time: startTime,
          end_time: endTime,
          resolution_ms: options.resolutionMs,
        },
        "ts_stats",
        (stats) => (stats ? mapSeriesStats(stats) : undefined),
        options.signal,
      ),
    setPolicy: (policy: TimeSeriesLifecyclePolicy, options: CallOptions = {}) =>
      ack({ type: "ts_set_policy", policy: toWirePolicy(policy) }, options.signal),
    getPolicy: (seriesPattern: string, options: CallOptions = {}) =>
      call(
        { type: "ts_get_policy", series_pattern: seriesPattern },
        "ts_policy",
        (policy) => (policy ? mapPolicy(policy) : undefined),
        options.signal,
      ),
    listPolicies: (options: CallOptions = {}) =>
      call(
        { type: "ts_list_policies" },
        "ts_policies",
        (policies) => policies.map(mapPolicy),
        options.signal,
      ),
    deletePolicy: (seriesPattern: string, options: CallOptions = {}) =>
      flag({ type: "ts_delete_policy", series_pattern: seriesPattern }, options.signal),
    /** Applies every series' lifecycle policy as of `nowMs` (now by default). */
    applyRetention: (options: { nowMs?: number } & CallOptions = {}) =>
      call(
        { type: "ts_apply_retention", now_ms: options.nowMs },
        "ts_retention_stats",
        mapRetentionStats,
        options.signal,
      ),
  };
}
