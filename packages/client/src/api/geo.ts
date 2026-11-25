// The geospatial calls: fences and tracks.

import { mapGeofenceMatches, mapGpsPoints } from "../mappers.js";
import type { CallOptions, GeoAddFenceOptions, GeoTrackQueryOptions, GpsPoint } from "../types.js";
import { type Calls, type Mode } from "./calls.js";

export function geoApi<M extends Mode>({ call, ack, flag }: Calls<M>) {
  return {
    addFence: (
      id: string,
      name: string,
      category: string,
      polygonJson: string,
      options: GeoAddFenceOptions = {},
    ) =>
      ack(
        {
          type: "geo_add_fence",
          id,
          name,
          category,
          polygon_json: polygonJson,
          metadata: options.metadata,
        },
        options.signal,
      ),
    removeFence: (id: string, options: CallOptions = {}) =>
      flag({ type: "geo_remove_fence", id }, options.signal),
    containsPoint: (polygonJson: string, lon: number, lat: number, options: CallOptions = {}) =>
      flag({ type: "geo_contains_point", polygon_json: polygonJson, lon, lat }, options.signal),
    findFencesForPoint: (lon: number, lat: number, options: CallOptions = {}) =>
      call(
        { type: "geo_find_fences_for_point", lon, lat },
        "geofence_matches",
        mapGeofenceMatches,
        options.signal,
      ),
    recordGpsPoints: (trackId: string, points: GpsPoint[], options: CallOptions = {}) =>
      call(
        { type: "geo_record_gps_points", track_id: trackId, points },
        "geofence_matches",
        mapGeofenceMatches,
        options.signal,
      ),
    getTrackPoints: (trackId: string, options: GeoTrackQueryOptions = {}) =>
      call(
        {
          type: "geo_get_track_points",
          track_id: trackId,
          start_time: options.startTime,
          end_time: options.endTime,
        },
        "gps_points",
        mapGpsPoints,
        options.signal,
      ),
    trackDistance: (trackId: string, options: CallOptions = {}) =>
      call(
        { type: "geo_track_distance", track_id: trackId },
        "f64",
        (meters) => meters,
        options.signal,
      ),
  };
}
