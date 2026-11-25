// The hybrid search: vector and full-text hits ranked together.

import { mapHybridHits } from "../mappers.js";
import type { CallOptions, VectorElementType, VectorInput, VectorMetric } from "../types.js";
import { type Calls, type Mode, SEARCH_LIMIT, queryVector } from "./calls.js";

export function hybridApi<M extends Mode>({ call }: Calls<M>) {
  return {
    search: (
      collection: string,
      options: {
        query?: string;
        vector?: VectorInput;
        dim?: number;
        elementType?: VectorElementType;
        metric?: VectorMetric;
        rrfK?: number;
        ftsWeight?: number;
        vectorWeight?: number;
        limit?: number;
      } & CallOptions = {},
    ) => {
      const vector =
        options.vector === undefined
          ? undefined
          : queryVector(options.vector, options.elementType, options.dim);
      return call(
        {
          type: "hybrid_search",
          collection,
          fts_query: options.query,
          vector: vector?.data,
          dim: vector?.dim,
          element_type: vector?.element_type,
          metric: options.metric,
          rrf_k: options.rrfK ?? 60,
          fts_weight: options.ftsWeight ?? 1,
          vector_weight: options.vectorWeight ?? 1,
          limit: options.limit ?? SEARCH_LIMIT,
        },
        "hybrid_hits",
        mapHybridHits,
        options.signal,
      );
    },
  };
}
