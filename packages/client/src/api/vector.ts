// The vector calls: collections, indexes and nearest-neighbor search.

import { mapVectorIndexInfo, toWireDirection, toWireHnswConfig } from "../mappers.js";
import { vectorDim } from "../math.js";
import type {
  CallOptions,
  GraphDirection,
  HnswConfig,
  VectorElementType,
  VectorInput,
  VectorSearchOptions,
} from "../types.js";
import { type Calls, type Mode, SEARCH_LIMIT, queryVector } from "./calls.js";

export function vectorApi<M extends Mode>({ call, ack, write, vectorHits, flag }: Calls<M>) {
  return {
    set: (
      collection: string,
      id: string,
      vector: VectorInput,
      options: { dim?: number; elementType?: VectorElementType } & CallOptions = {},
    ) =>
      write(
        {
          model: "vector_set",
          collection,
          id,
          dim: vectorDim(vector, options.elementType, options.dim),
          elementType: options.elementType,
          vector,
        },
        options.signal,
      ),
    get: (collection: string, id: string, options: CallOptions = {}) =>
      call(
        { type: "vector_get", collection, id },
        "vector",
        (stored) => stored?.data ?? null,
        options.signal,
      ),
    delete: (collection: string, id: string, options: CallOptions = {}) =>
      write({ model: "vector_delete", collection, id }, options.signal),
    search: (collection: string, query: VectorInput, options: VectorSearchOptions = {}) => {
      const vector = queryVector(query, options.elementType, options.dim);
      return vectorHits(
        {
          type: "vector_search",
          collection,
          vector: vector.data,
          dim: vector.dim,
          element_type: vector.element_type,
          metric: options.metric ?? "cosine",
          limit: options.limit ?? SEARCH_LIMIT,
        },
        options.signal,
      );
    },
    createIndex: (collection: string, config: HnswConfig, options: CallOptions = {}) =>
      ack(
        { type: "vector_create_index", collection, config: toWireHnswConfig(config) },
        options.signal,
      ),
    getIndexInfo: (collection: string, options: CallOptions = {}) =>
      call(
        { type: "vector_get_index_info", collection },
        "vector_index_info",
        (info) => (info ? mapVectorIndexInfo(info) : undefined),
        options.signal,
      ),
    dropIndex: (collection: string, options: CallOptions = {}) =>
      flag({ type: "vector_drop_index", collection }, options.signal),
    searchFiltered: (
      collection: string,
      query: VectorInput,
      candidateIds: string[],
      options: VectorSearchOptions = {},
    ) => {
      const vector = queryVector(query, options.elementType, options.dim);
      return vectorHits(
        {
          type: "vector_search_filtered",
          collection,
          query_vector: vector.data,
          dim: vector.dim,
          element_type: vector.element_type,
          metric: options.metric ?? "cosine",
          candidate_ids: candidateIds,
          limit: options.limit ?? SEARCH_LIMIT,
        },
        options.signal,
      );
    },
    searchInGraphNeighbors: (
      collection: string,
      graph: string,
      startNode: string,
      query: VectorInput,
      options: VectorSearchOptions & { direction?: GraphDirection } = {},
    ) => {
      const vector = queryVector(query, options.elementType, options.dim);
      return vectorHits(
        {
          type: "vector_search_in_graph_neighbors",
          collection,
          graph,
          start_node: startNode,
          direction: toWireDirection(options.direction),
          query_vector: vector.data,
          dim: vector.dim,
          element_type: vector.element_type,
          metric: options.metric ?? "cosine",
          limit: options.limit ?? SEARCH_LIMIT,
        },
        options.signal,
      );
    },
    searchByJsonIndex: (
      collection: string,
      space: string,
      indexName: string,
      indexValue: string | number | boolean,
      query: VectorInput,
      options: VectorSearchOptions = {},
    ) => {
      const vector = queryVector(query, options.elementType, options.dim);
      return vectorHits(
        {
          type: "vector_search_by_json_index",
          collection,
          space,
          index_name: indexName,
          index_value: String(indexValue),
          query_vector: vector.data,
          dim: vector.dim,
          element_type: vector.element_type,
          metric: options.metric ?? "cosine",
          limit: options.limit ?? SEARCH_LIMIT,
        },
        options.signal,
      );
    },
  };
}
