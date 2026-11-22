// React hooks over a database. Each reads what it shows, then reads it again after every write
// of the database that changes it (see `changes.ts`); key/value and document hooks read their
// first value synchronously through JSI, so the first render already shows it.

import { useCallback, useContext, useEffect, useRef, useState } from "react";
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
  peek?: (db: LiliaDatabase) => T
): Live<T> {
  const loadRef = useRef(load);
  loadRef.current = load;

  const read = (database: LiliaDatabase | null): LiveState<T> => {
    if (!database || !peek) return { value: fallback, isLoading: true, error: null };
    try {
      return { value: peek(database), isLoading: false, error: null };
    } catch (error) {
      return { value: fallback, isLoading: false, error: asError(error) };
    }
  };

  const [state, setState] = useState<LiveState<T>>(() => read(db));
  /** What the first state was peeked for: it needs no read until the next change. */
  const peeked = useRef<{ db: LiliaDatabase; key: string } | null>(db && peek ? { db, key } : null);
  const sequence = useRef(0);

  const reload = useCallback(async () => {
    if (!db) return;
    const run = ++sequence.current;
    try {
      const value = await loadRef.current(db);
      if (run === sequence.current) setState({ value, isLoading: false, error: null });
    } catch (error) {
      if (run === sequence.current) setState((current) => ({ ...current, isLoading: false, error: asError(error) }));
    }
  }, [db, key]);

  const watchedKey = typeof watched === "string" ? watched : watched.join("\n");
  useEffect(() => {
    if (!db) return;
    if (peeked.current?.db === db && peeked.current.key === key) {
      peeked.current = null;
    } else if (peek) {
      setState(read(db));
    } else {
      void reload();
    }
    const unsubscribe = db.subscribe(watched, () => void reload());
    return () => {
      unsubscribe();
      // Reads still running belong to the previous key or database.
      sequence.current++;
    };
    // `watched` is compared by `watchedKey`; `read` and `peek` change with `key`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [db, key, watchedKey, reload]);

  const show = useCallback((value: T) => {
    sequence.current++;
    setState({ value, isLoading: false, error: null });
  }, []);

  return { ...state, reload, show };
}

/** Opens a database for the component's lifetime and closes it when the component unmounts. */
export function useLiliaDatabase(options: OpenDatabaseOptions): { db: LiliaDatabase | null; isLoading: boolean; error: Error | null } {
  const [db, setDb] = useState<LiliaDatabase | null>(null);
  const [error, setError] = useState<Error | null>(null);
  const optionsKey = JSON.stringify(options);

  useEffect(() => {
    let active = true;
    let opened: LiliaDatabase | null = null;
    setDb(null);
    setError(null);
    LiliaDatabase.open(options).then(
      (database) => {
        opened = database;
        if (active) setDb(database);
        else void database.close();
      },
      (failure: unknown) => {
        if (active) setError(asError(failure));
      }
    );
    return () => {
      active = false;
      if (opened) void opened.close();
    };
    // The options are compared by their JSON.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [optionsKey]);

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
export function useKv<T = string>(namespace: string, key: string, options: UseKvOptions<T> = {}): UseKvReturn<T> {
  const db = useResolvedDatabase(options.db);
  const deserialize = options.deserialize ?? defaultDeserialize<T>;
  const serialize = options.serialize ?? defaultSerialize<T>;
  const defaultValue = options.defaultValue as T;
  const decode = (entry: { value: Uint8Array } | undefined): T => (entry ? deserialize(decodeUtf8(entry.value)) : defaultValue);

  const live = useLive<T>(
    db,
    `${namespace}\n${key}`,
    topics.kv(namespace, key),
    async (database) => decode(await database.kv.get(namespace, key)),
    defaultValue,
    (database) => decode(database.sync.kv.get(namespace, key))
  );
  const latest = useRef(live.value);
  latest.current = live.value;

  const setValue = useCallback(
    async (next: T | ((previous: T) => T), expiresAtMs?: number) => {
      if (!db) throw notOpen();
      const value = typeof next === "function" ? (next as (previous: T) => T)(latest.current) : next;
      await db.kv.set(namespace, key, serialize(value), { expiresAtMs });
      live.show(value);
    },
    // `serialize` may be a new function each render; the namespace and key name the value.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [db, namespace, key, live.show]
  );

  const remove = useCallback(async () => {
    if (!db) throw notOpen();
    await db.kv.delete(namespace, key);
    live.show(defaultValue);
  }, [db, namespace, key, live.show, defaultValue]);

  const meta = { value: live.value, setValue, isLoading: live.isLoading, error: live.error, remove, reload: live.reload };
  return Object.assign([live.value, setValue, meta] as [T, typeof setValue, typeof meta], meta);
}

/** `useKv` as `useState`: the value and its setter. */
export function useKvState<T>(
  namespace: string,
  key: string,
  defaultValue: T,
  options: Omit<UseKvOptions<T>, "defaultValue"> = {}
): [T, (next: T | ((previous: T) => T)) => Promise<void>] {
  const [value, setValue] = useKv<T>(namespace, key, { ...options, defaultValue });
  return [value, setValue];
}

// ==========================================
// JSON DOCUMENTS
// ==========================================

/** The document `id` of `space`, with an `update` that merges partial objects. */
export function useDocument<T = JsonValue>(space: string, id: string, options: UseDocumentOptions<T> = {}): UseDocumentReturn<T> {
  const db = useResolvedDatabase(options.db);
  const fallback = options.defaultValue ?? null;
  const live = useLive<T | null>(
    db,
    `${space}\n${id}`,
    topics.json(space, id),
    async (database) => (await database.json.get<T>(space, id))?.value ?? fallback,
    fallback,
    (database) => database.sync.json.get<T>(space, id)?.value ?? fallback
  );
  const latest = useRef(live.value);
  latest.current = live.value;

  const update = useCallback(
    async (updater: Partial<T> | T | ((previous: T | null) => T)) => {
      if (!db) throw notOpen();
      const previous = latest.current;
      const next =
        typeof updater === "function"
          ? (updater as (previous: T | null) => T)(previous)
          : previous !== null && typeof previous === "object" && !Array.isArray(previous) && typeof updater === "object" && updater !== null && !Array.isArray(updater)
            ? ({ ...previous, ...updater } as T)
            : (updater as T);
      await db.json.put(space, id, next);
      live.show(next);
    },
    [db, space, id, live.show]
  );

  const remove = useCallback(async () => {
    if (!db) throw notOpen();
    await db.json.delete(space, id);
    live.show(null);
  }, [db, space, id, live.show]);

  const meta = { data: live.value, update, isLoading: live.isLoading, error: live.error, remove, reload: live.reload };
  return Object.assign([live.value, update, meta] as [T | null, typeof update, typeof meta], meta);
}

export const useJson = useDocument;

// ==========================================
// GRAPH
// ==========================================

/** The neighbors of `node` in `graph`. */
export function useGraphNeighbors(graph: string, node: string, options: UseGraphNeighborsOptions = {}): UseGraphNeighborsReturn {
  const db = useResolvedDatabase(options.db);
  const { direction, kind } = options;
  const live = useLive<string[]>(
    db,
    JSON.stringify([graph, node, direction, kind]),
    topics.graph(graph),
    (database) => database.graph.neighbors(graph, node, { direction, kind }),
    []
  );
  return { neighbors: live.value, isLoading: live.isLoading, error: live.error, reload: live.reload };
}

// ==========================================
// FULL-TEXT SEARCH
// ==========================================

/** The hits of `query` in `collection`, searched once the query stops changing for `debounceMs`. */
export function useFts(collection: string, query: string, options: UseFtsOptions = {}): UseFtsReturn {
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
    []
  );
  return { hits: live.value, isSearching: live.isLoading || debounced !== query, error: live.error };
}

// ==========================================
// SQL
// ==========================================

/**
 * The records of the read-only SQL query `sql` (parameters `$1`, `$2`... from `params`), run
 * again after writes to `options.tables`, or after any write when none are given.
 */
export function useSqlQuery<T = Record<string, unknown>>(sql: string, params: JsonValue[] = [], options: UseSqlQueryOptions = {}): UseSqlQueryReturn<T> {
  const db = useResolvedDatabase(options.db);
  const enabled = options.enabled ?? true;
  const empty: SqlQueryResult<T> = { columns: [], rows: [], rowsAffected: 0, records: [] };
  const [isRefetching, setIsRefetching] = useState(false);
  const live = useLive<SqlQueryResult<T>>(
    enabled ? db : null,
    JSON.stringify([sql, params, options.deps ?? []]),
    options.tables?.map(topics.table) ?? ANY_CHANGE,
    (database) => database.sql.raw<T>(sql, params),
    empty
  );

  const refetch = useCallback(async () => {
    setIsRefetching(true);
    try {
      await live.reload();
    } finally {
      setIsRefetching(false);
    }
  }, [live.reload]);

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
  options: UseMutationOptions<TData, TVariables> = {}
): UseMutationReturn<TData, TVariables> {
  const db = useResolvedDatabase(options.db);
  const [isPending, setIsPending] = useState(false);
  const [error, setError] = useState<Error | null>(null);
  const [data, setData] = useState<TData | undefined>(undefined);
  const callbacks = useRef({ mutationFn, options });
  callbacks.current = { mutationFn, options };

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
    [db]
  );

  const mutate = useCallback(
    (variables: TVariables) => {
      // The error is in `error`, and given to `onError`.
      mutateAsync(variables).catch(() => undefined);
    },
    [mutateAsync]
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
export function useDatabaseMaintenance(explicitDb?: LiliaDatabase | null): UseDatabaseMaintenanceReturn {
  const db = useResolvedDatabase(explicitDb);
  const live = useLive<StorageStats | null>(db, "storage", ANY_CHANGE, (database) => database.storageStats(), null);

  const checkpoint = useCallback(async () => {
    if (!db) throw notOpen();
    await db.checkpoint();
    await live.reload();
  }, [db, live.reload]);

  const integrityCheck = useCallback(async () => {
    if (!db) throw notOpen();
    return db.integrityCheck();
  }, [db]);

  return { stats: live.value, isLoading: live.isLoading, error: live.error, checkpoint, integrityCheck, refresh: live.reload };
}
