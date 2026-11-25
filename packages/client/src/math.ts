// Pure helpers that need no database: vector quantization, downsampling of series, and geodesy.
// They follow the Rust definitions in `lilia-core`, so client and engine agree.

import type { DownsampleAggregation, TimeSeriesPoint, VectorElementType } from "./types.js";

const EARTH_RADIUS_METERS = 6_371_000;
const METERS_PER_DEGREE = 111_320;

export const quantization = {
  /** `floats` scaled into `i8` by their largest magnitude, and the scale to undo it. */
  toInt8(floats: Float32Array | readonly number[]): { quantized: Int8Array; scale: number } {
    let maxAbs = 0;
    for (const value of floats) maxAbs = Math.max(maxAbs, Math.abs(value));
    const quantized = new Int8Array(floats.length);
    if (maxAbs === 0) return { quantized, scale: 1 };
    const scale = maxAbs / 127;
    for (let i = 0; i < floats.length; i++) {
      const scaled = floats[i]! / scale;
      // Halves round away from zero, as Rust's `f32::round` does in the engine's quantizer.
      quantized[i] = Math.max(
        -128,
        Math.min(127, Math.sign(scaled) * Math.round(Math.abs(scaled))),
      );
    }
    return { quantized, scale };
  },

  fromInt8(quantized: Int8Array, scale: number): Float32Array {
    return Float32Array.from(quantized, (value) => value * scale);
  },

  /** One bit per element, set when it is positive, most significant bit first. */
  toBit(floats: Float32Array | readonly number[]): Uint8Array {
    const packed = new Uint8Array(Math.ceil(floats.length / 8));
    for (let i = 0; i < floats.length; i++) {
      if (floats[i]! > 0) packed[i >> 3]! |= 1 << (7 - (i & 7));
    }
    return packed;
  },

  /** `1 - cos(a, b)`, in `[0, 2]`; 1 when either vector is null. */
  cosineDistance(a: Float32Array | readonly number[], b: Float32Array | readonly number[]): number {
    const length = Math.min(a.length, b.length);
    let dot = 0;
    let normA = 0;
    let normB = 0;
    for (let i = 0; i < length; i++) {
      dot += a[i]! * b[i]!;
      normA += a[i]! * a[i]!;
      normB += b[i]! * b[i]!;
    }
    if (normA <= 0 || normB <= 0) return 1;
    return Math.max(0, Math.min(2, 1 - dot / Math.sqrt(normA * normB)));
  },
};

/** The bytes of `vector` as elements of `elementType`: given bytes as they are, numbers encoded or quantized. */
export function vectorToBytes(
  vector: Float32Array | readonly number[] | Uint8Array,
  elementType?: VectorElementType,
): Uint8Array {
  if (vector instanceof Uint8Array) return vector;
  if (elementType === "int8") {
    const { quantized } = quantization.toInt8(vector);
    return new Uint8Array(quantized.buffer, quantized.byteOffset, quantized.byteLength);
  }
  if (elementType === "bit") return quantization.toBit(vector);
  const floats = vector instanceof Float32Array ? vector : Float32Array.from(vector);
  return new Uint8Array(floats.buffer, floats.byteOffset, floats.byteLength);
}

/** The dimension of `vector`: `dim` when given, else its element count. */
export function vectorDim(
  vector: Float32Array | readonly number[] | Uint8Array,
  elementType?: VectorElementType,
  dim?: number,
): number {
  if (dim !== undefined) return dim;
  if (!(vector instanceof Uint8Array)) return vector.length;
  if (elementType === "int8") return vector.byteLength;
  if (elementType === "bit") return vector.byteLength * 8;
  return vector.byteLength / 4;
}

/** `points` aggregated by `aggregation` (`avg` by default) into buckets of `bucketMs` aligned on 0. */
function downsample(
  points: readonly TimeSeriesPoint[],
  bucketMs: number,
  aggregation: DownsampleAggregation = "avg",
): TimeSeriesPoint[] {
  if (points.length === 0 || bucketMs <= 0) return points.map((point) => ({ ...point }));
  const sorted = points.toSorted((a, b) => a.timestamp - b.timestamp);
  const result: TimeSeriesPoint[] = [];
  let bucket: number | undefined;
  let values: number[] = [];
  const flush = () => {
    if (bucket === undefined || values.length === 0) return;
    const sum = values.reduce((total, value) => total + value, 0);
    const value = {
      avg: sum / values.length,
      min: Math.min(...values),
      max: Math.max(...values),
      sum,
      first: values[0]!,
      last: values[values.length - 1]!,
      count: values.length,
    }[aggregation];
    result.push({ timestamp: bucket, value });
  };
  for (const point of sorted) {
    const start = Math.floor(point.timestamp / bucketMs) * bucketMs;
    if (start !== bucket) {
      flush();
      bucket = start;
      values = [];
    }
    values.push(point.value);
  }
  flush();
  return result;
}

export const timeseries = { downsample };

const radians = (degrees: number) => (degrees * Math.PI) / 180;
const fixed = (value: number) => Number(value.toFixed(7));

export const geo = {
  /** The great-circle distance in meters between two positions. */
  haversineDistance(lat1: number, lon1: number, lat2: number, lon2: number): number {
    const a =
      Math.sin(radians(lat2 - lat1) / 2) ** 2 +
      Math.cos(radians(lat1)) * Math.cos(radians(lat2)) * Math.sin(radians(lon2 - lon1) / 2) ** 2;
    return EARTH_RADIUS_METERS * 2 * Math.atan2(Math.sqrt(a), Math.sqrt(1 - a));
  },

  /** A closed polygon of `vertices` (4 to 128, 16 by default) approximating a circle, as JSON `[lon, lat]` pairs. */
  circleToGeopoly(
    centerLon: number,
    centerLat: number,
    radiusMeters: number,
    vertices = 16,
  ): string {
    const count = Math.max(4, Math.min(128, vertices));
    const dLat = radiusMeters / METERS_PER_DEGREE;
    const dLon = radiusMeters / (METERS_PER_DEGREE * Math.max(Math.cos(radians(centerLat)), 1e-6));
    const ring: [number, number][] = [];
    for (let i = 0; i < count; i++) {
      const angle = (i * 2 * Math.PI) / count;
      ring.push([
        fixed(centerLon + dLon * Math.cos(angle)),
        fixed(centerLat + dLat * Math.sin(angle)),
      ]);
    }
    ring.push(ring[0]!);
    return JSON.stringify(ring);
  },

  /** `[lon, lat]` pairs as polygon JSON. */
  coordsToGeopoly(coords: ReadonlyArray<readonly [number, number]>): string {
    return JSON.stringify(coords.map(([lon, lat]) => [fixed(lon), fixed(lat)]));
  },
};
