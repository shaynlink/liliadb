// The full-text search calls.

import { mapFtsFuzzyHits, mapFtsHits, mapFtsPhoneticHits, mapFtsSuggestions } from "../mappers.js";
import type { CallOptions, FtsSearchOptions } from "../types.js";
import { type Calls, type Mode, SEARCH_LIMIT } from "./calls.js";

export function ftsApi<M extends Mode>({ call, ack, write, flag }: Calls<M>) {
  return {
    index: (collection: string, docId: string, content: string, options: CallOptions = {}) =>
      write({ model: "fts_index", collection, docId, content }, options.signal),
    delete: (collection: string, docId: string, options: CallOptions = {}) =>
      flag({ type: "fts_delete", collection, doc_id: docId }, options.signal),
    search: (collection: string, query: string, options: FtsSearchOptions = {}) =>
      call(
        { type: "fts_search", collection, query, limit: options.limit ?? SEARCH_LIMIT },
        "fts_hits",
        mapFtsHits,
        options.signal,
      ),
    suggest: (collection: string, prefix: string, options: FtsSearchOptions = {}) =>
      call(
        {
          type: "fts_suggest",
          collection,
          prefix,
          max_distance: options.maxDistance ?? 1,
          limit: options.limit ?? SEARCH_LIMIT,
        },
        "fts_suggestions",
        mapFtsSuggestions,
        options.signal,
      ),
    fuzzySearch: (collection: string, query: string, options: FtsSearchOptions = {}) =>
      call(
        {
          type: "fts_fuzzy_search",
          collection,
          query,
          max_distance: options.maxDistance ?? 1,
          limit: options.limit ?? SEARCH_LIMIT,
        },
        "fts_fuzzy_hits",
        mapFtsFuzzyHits,
        options.signal,
      ),
    phoneticSearch: (collection: string, query: string, options: FtsSearchOptions = {}) =>
      call(
        { type: "fts_phonetic_search", collection, query, limit: options.limit ?? SEARCH_LIMIT },
        "fts_phonetic_hits",
        mapFtsPhoneticHits,
        options.signal,
      ),
    loadDictionary: (collection: string, words: readonly string[], options: CallOptions = {}) =>
      ack({ type: "fts_load_dictionary", collection, words: [...words] }, options.signal),
  };
}
