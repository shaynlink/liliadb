// React hooks over a database. Each reads what it shows, then reads it again after every write
// of the database that changes it (see `changes.ts`); key/value and document hooks read their
// first value synchronously through JSI, so the first render already shows it.

import { useCallback, useContext, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { FtsHit, JsonValue, SqlQueryResult, StorageStats } from "@liliadb/client";
import { topics } from "./changes.js";
import { LiliaContext } from "./context.js";
import { LiliaDatabase } from "./database.js";
import { ANY_CHANGE } from "./observer.js";
import { decodeUtf8 } from "./utf8.js";
import type {
  OpenDatabaseOptions,
  UseDatabaseMaintenanceReturn,
  UseDocumentOptions,
  UseDocumentReturn,
  UseFtsOptions,
  UseFtsReturn,
  UseGraphNeighborsOptions,
  UseGraphNeighborsReturn,
  UseKvOptions,
  UseKvReturn,
  UseMutationOptions,
  UseMutationReturn,
  UseSqlQueryOptions,
  UseSqlQueryReturn,
} from "./types.js";

function asError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}

function notOpen(): Error {
  return new Error("[LiliaDB] the database is not open yet");
}

/** `explicit`, else the database of the nearest `<LiliaProvider>`; null while it opens. */
function useResolvedDatabase(explicit?: LiliaDatabase | null): LiliaDatabase | null {
  const provided = useContext(LiliaContext);
  return explicit ?? provided;
}

interface LiveState<T> {
  value: T;
  isLoading: boolean;
  error: Error | null;
}

/** A state, with the database and key it was read for. */
interface KeyedState<T> extends LiveState<T> {
  db: LiliaDatabase | null;
  key: string;
}

interface Live<T> extends LiveState<T> {
  reload: () => Promise<void>;
  /** Shows `value` until the next read. */
  show: (value: T) => void;
}

/**
 * The value `load` reads from `db`, read again after each write that changes `watched`. `peek`
 * reads it synchronously, for the first render and when `key` (what is read) changes.
 */
function useLive<T>(
  db: LiliaDatabase | null,
  key: string,
  watched: string | readonly string[],
  load: (db: LiliaDatabase) => Promise<T>,
  fallback: T,
  peek?: (db: LiliaDatabase) => T,
): Live<T> {
  const loadRef = useRef(load);
  useLayoutEffect(() => {
    loadRef.current = load;
  });

  const read = (database: LiliaDatabase | null): KeyedState<T> => {
    if (!database || !peek)
      return { db: database, key, value: fallback, isLoading: true, error: null };
    try {
      return { db: database, key, value: peek(database), isLoading: false, error: null };
    } catch (error) {
      return { db: database, key, value: fallback, isLoading: false, error: asError(error) };
    }
  };

  const [state, setState] = useState<KeyedState<T>>(() => read(db));
  // A new database or key: peeked now, during this render; without `peek`, the last value stays
  // shown until the read the effect starts.
  let current = state;
  if (state.db !== db || state.key !== key) {
    current = peek ? read(db) : { ...state, db, key };
    setState(current);
  }

  /** The last read started: an earlier one that ends after it is dropped. */
  const sequence = useRef(0);
  const reload = useCallback(async () => {
    if (!db) return;
    const run = ++sequence.current;
    const keep = (next: (state: KeyedState<T>) => KeyedState<T>) =>
      setState((latest) =>
        run === sequence.current && latest.db === db && latest.key === key ? next(latest) : latest,
      );
    try {
      const value = await loadRef.current(db);
      keep((latest) => ({ ...latest, value, isLoading: false, error: null }));
    } catch (error) {
      keep((latest) => ({ ...latest, isLoading: false, error: asError(error) }));
    }
  }, [db, key]);

  // `watched` is a new array at each render: its JSON says when the topics change.
  const watchedKey = JSON.stringify(watched);
  const peeks = peek !== undefined;
  useEffect(() => {
    if (!db) return;
    if (!peeks) void reload();
    return db.subscribe(JSON.parse(watchedKey) as string | string[], () => void reload());
  }, [db, watchedKey, peeks, reload]);

  const show = useCallback((value: T) => {
    sequence.current++;
    setState((latest) => ({ ...latest, value, isLoading: false, error: null }));
  }, []);

  return { value: current.value, isLoading: current.isLoading, error: current.error, reload, show };
}

/** Opens a database for the component's lifetime and closes it when the component unmounts. */
export function useLiliaDatabase(options: OpenDatabaseOptions): {
  db: LiliaDatabase | null;
  isLoading: boolean;
  error: Error | null;
} {
  // The options are compared by their JSON; the effect opens with the latest ones.
  const optionsKey = JSON.stringify(options);
  const latestOptions = useRef(options);
  useLayoutEffect(() => {
    latestOptions.current = options;
  });
  const [opened, setOpened] = useState<{
    key: string;
    db: LiliaDatabase | null;
    error: Error | null;
  }>({
    key: optionsKey,
    db: null,
    error: null,
  });

  useEffect(() => {
    let active = true;
    let database: LiliaDatabase | null = null;
    const open = async () => {
      try {
        database = await LiliaDatabase.open(latestOptions.current);
        if (active) setOpened({ key: optionsKey, db: database, error: null });
        else await database.close();
      } catch (failure) {
        if (active) setOpened({ key: optionsKey, db: null, error: asError(failure) });
      }
    };
    void open();
    return () => {
      active = false;
      if (database) void database.close();
    };
  }, [optionsKey]);

  // Until the database of these options opens, the one of the previous options is not shown.
  const { db, error } = opened.key === optionsKey ? opened : { db: null, error: null };
  return { db, isLoading: db === null && error === null, error };
}

// ==========================================
// KEY / VALUE
// ==========================================

/** The value of JSON text; text that is not JSON (written by other code) as it is. */
function defaultDeserialize<T>(raw: string): T {
  try {
    return JSON.parse(raw) as T;
  } catch {
    return raw as T;
  }
}

function defaultSerialize<T>(value: T): string {
  return JSON.stringify(value);
}

/**
 * The value of `key` in `namespace`, stored as text through `options.serialize` and
 * `options.deserialize` (JSON by default), with its setter.
 */
export function useKv<T = string>(
  namespace: string,
  key: string,
  options: UseKvOptions<T> = {},
): UseKvReturn<T> {
  const db = useResolvedDatabase(options.db);
  const deserialize = options.deserialize ?? defaultDeserialize<T>;
  const serialize = options.serialize ?? defaultSerialize<T>;
  const defaultValue = options.defaultValue as T;
  const decode = (entry: { value: Uint8Array } | undefined): T =>
    entry ? deserialize(decodeUtf8(entry.value)) : defaultValue;

  const live = useLive<T>(
    db,
    `${namespace}\n${key}`,
    topics.kv(namespace, key),
    async (database) => decode(await database.kv.get(namespace, key)),
    defaultValue,
    (database) => decode(database.sync.kv.get(namespace, key)),
  );
  const { show } = live;
  // The value and `serialize` (which may be a new function each render) when the setter runs.
  const latest = useRef({ value: live.value, serialize });
  useLayoutEffect(() => {
    latest.current = { value: live.value, serialize };
  });

  const setValue = useCallback(
    async (next: T | ((previous: T) => T), expiresAtMs?: number) => {
      if (!db) throw notOpen();
      const { value: previous, serialize: write } = latest.current;
      const value = typeof next === "function" ? (next as (previous: T) => T)(previous) : next;
      await db.kv.set(namespace, key, write(value), { expiresAtMs });
      show(value);
    },
    [db, namespace, key, show],
  );

  const remove = useCallback(async () => {
    if (!db) throw notOpen();
    await db.kv.delete(namespace, key);
    show(defaultValue);
  }, [db, namespace, key, show, defaultValue]);

  const meta = {
    value: live.value,
    setValue,
    isLoading: live.isLoading,
    error: live.error,
    remove,
    reload: live.reload,
  };
  // `Object.assign` only attaches the setters; it does not call them, so no ref is read here.
  // oxlint-disable-next-line react/refs
  return Object.assign([live.value, setValue, meta] as [T, typeof setValue, typeof meta], meta);
}

/** `useKv` as `useState`: the value and its setter. */
export function useKvState<T>(
  namespace: string,
  key: string,
  defaultValue: T,
  options: Omit<UseKvOptions<T>, "defaultValue"> = {},
): [T, (next: T | ((previous: T) => T)) => Promise<void>] {
  const [value, setValue] = useKv<T>(namespace, key, { ...options, defaultValue });
  return [value, setValue];
}

// ==========================================
// JSON DOCUMENTS
// ==========================================

/** The document `id` of `space`, with an `update` that merges partial objects. */
export function useDocument<T = JsonValue>(
  space: string,
  id: string,
  options: UseDocumentOptions<T> = {},
): UseDocumentReturn<T> {
  const db = useResolvedDatabase(options.db);
  const fallback = options.defaultValue ?? null;
  const live = useLive<T | null>(
    db,
    `${space}\n${id}`,
    topics.json(space, id),
    async (database) => (await database.json.get<T>(space, id))?.value ?? fallback,
    fallback,
    (database) => database.sync.json.get<T>(space, id)?.value ?? fallback,
  );
  const { show } = live;
  const latest = useRef(live.value);
  useLayoutEffect(() => {
    latest.current = live.value;
  });

  const update = useCallback(
    async (updater: Partial<T> | T | ((previous: T | null) => T)) => {
      if (!db) throw notOpen();
      const previous = latest.current;
      const next =
        typeof updater === "function"
          ? (updater as (previous: T | null) => T)(previous)
          : previous !== null &&
              typeof previous === "object" &&
              !Array.isArray(previous) &&
              typeof updater === "object" &&
              updater !== null &&
              !Array.isArray(updater)
            ? ({ ...previous, ...updater } as T)
            : (updater as T);
      await db.json.put(space, id, next);
      show(next);
    },
    [db, space, id, show],
  );

  const remove = useCallback(async () => {
    if (!db) throw notOpen();
    await db.json.delete(space, id);
    show(null);
  }, [db, space, id, show]);

  const meta = {
    data: live.value,
    update,
    isLoading: live.isLoading,
    error: live.error,
    remove,
    reload: live.reload,
  };
  // `Object.assign` only attaches the setters; it does not call them, so no ref is read here.
  // oxlint-disable-next-line react/refs
  return Object.assign([live.value, update, meta] as [T | null, typeof update, typeof meta], meta);
}

export const useJson = useDocument;

// ==========================================
// GRAPH
// ==========================================

/** The neighbors of `node` in `graph`. */
export function useGraphNeighbors(
  graph: string,
  node: string,
  options: UseGraphNeighborsOptions = {},
): UseGraphNeighborsReturn {
  const db = useResolvedDatabase(options.db);
  const { direction, kind } = options;
  const live = useLive<string[]>(
    db,
    JSON.stringify([graph, node, direction, kind]),
    topics.graph(graph),
    (database) => database.graph.neighbors(graph, node, { direction, kind }),
    [],
  );
  return {
    neighbors: live.value,
    isLoading: live.isLoading,
    error: live.error,
    reload: live.reload,
  };
}

// ==========================================
// FULL-TEXT SEARCH
// ==========================================

/** The hits of `query` in `collection`, searched once the query stops changing for `debounceMs`. */
export function useFts(
  collection: string,
  query: string,
  options: UseFtsOptions = {},
): UseFtsReturn {
  const db = useResolvedDatabase(options.db);
  const { limit = 20, debounceMs = 200 } = options;
  const [debounced, setDebounced] = useState(query);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(query), debounceMs);
    return () => clearTimeout(timer);
  }, [query, debounceMs]);

  const text = debounced.trim();
  const live = useLive<FtsHit[]>(
    db,
    JSON.stringify([collection, text, limit]),
    topics.fts(collection),
    async (database) => (text === "" ? [] : database.fts.search(collection, text, { limit })),
    [],
  );
  return {
    hits: live.value,
    isSearching: live.isLoading || debounced !== query,
    error: live.error,
  };
}

// ==========================================
// SQL
// ==========================================

/**
 * The records of the read-only SQL query `sql` (parameters `$1`, `$2`... from `params`), run
 * again after writes to `options.tables`, or after any write when none are given.
 */
export function useSqlQuery<T = Record<string, unknown>>(
  sql: string,
  params: JsonValue[] = [],
  options: UseSqlQueryOptions = {},
): UseSqlQueryReturn<T> {
  const db = useResolvedDatabase(options.db);
  const enabled = options.enabled ?? true;
  const empty: SqlQueryResult<T> = { columns: [], rows: [], rowsAffected: 0, records: [] };
  const [isRefetching, setIsRefetching] = useState(false);
  const live = useLive<SqlQueryResult<T>>(
    enabled ? db : null,
    JSON.stringify([sql, params, options.deps ?? []]),
    options.tables?.map(topics.table) ?? ANY_CHANGE,
    (database) => database.sql.raw<T>(sql, params),
    empty,
  );

  const { reload } = live;
  const refetch = useCallback(async () => {
    setIsRefetching(true);
    try {
      await reload();
    } finally {
      setIsRefetching(false);
    }
  }, [reload]);

  return {
    data: live.value.records,
    rows: live.value.records,
    columns: live.value.columns,
    isLoading: enabled && live.isLoading,
    isRefetching,
    error: live.error,
    refetch,
  };
}

// ==========================================
// MUTATIONS
// ==========================================

/**
 * `mutationFn` with its pending state, result and error. The hooks that read what it writes run
 * again by themselves.
 */
export function useMutation<TData = unknown, TVariables = void>(
  mutationFn: (variables: TVariables, db: LiliaDatabase) => Promise<TData>,
  options: UseMutationOptions<TData, TVariables> = {},
): UseMutationReturn<TData, TVariables> {
  const db = useResolvedDatabase(options.db);
  const [isPending, setIsPending] = useState(false);
  const [error, setError] = useState<Error | null>(null);
  const [data, setData] = useState<TData | undefined>(undefined);
  const callbacks = useRef({ mutationFn, options });
  useLayoutEffect(() => {
    callbacks.current = { mutationFn, options };
  });

  const mutateAsync = useCallback(
    async (variables: TVariables): Promise<TData> => {
      if (!db) throw notOpen();
      const { mutationFn: run, options: hooks } = callbacks.current;
      setIsPending(true);
      setError(null);
      try {
        const result = await run(variables, db);
        setData(result);
        await hooks.onSuccess?.(result, variables);
        await hooks.onSettled?.(result, null, variables);
        return result;
      } catch (failure) {
        const thrown = asError(failure);
        setError(thrown);
        await hooks.onError?.(thrown, variables);
        await hooks.onSettled?.(undefined, thrown, variables);
        throw thrown;
      } finally {
        setIsPending(false);
      }
    },
    [db],
  );

  const mutate = useCallback(
    (variables: TVariables) => {
      // The error is in `error`, and given to `onError`.
      mutateAsync(variables).catch(() => undefined);
    },
    [mutateAsync],
  );

  const reset = useCallback(() => {
    setIsPending(false);
    setError(null);
    setData(undefined);
  }, []);

  return { mutate, mutateAsync, isPending, error, data, reset };
}

// ==========================================
// MAINTENANCE
// ==========================================

/** The storage statistics of the database, kept current, with its maintenance calls. */
export function useDatabaseMaintenance(
  explicitDb?: LiliaDatabase | null,
): UseDatabaseMaintenanceReturn {
  const db = useResolvedDatabase(explicitDb);
  const live = useLive<StorageStats | null>(
    db,
    "storage",
    ANY_CHANGE,
    (database) => database.storageStats(),
    null,
  );

  const { reload } = live;
  const checkpoint = useCallback(async () => {
    if (!db) throw notOpen();
    await db.checkpoint();
    await reload();
  }, [db, reload]);

  const integrityCheck = useCallback(async () => {
    if (!db) throw notOpen();
    return db.integrityCheck();
  }, [db]);

  return {
    stats: live.value,
    isLoading: live.isLoading,
    error: live.error,
    checkpoint,
    integrityCheck,
    refresh: live.reload,
  };
}
