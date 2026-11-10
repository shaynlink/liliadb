import { useState, useEffect, useCallback, useRef, type DependencyList } from 'react';
import { LiliaDatabase } from './database.js';
import { useDatabase } from './context.js';
import type {
  OpenDatabaseOptions,
  UseKvOptions,
  UseKvReturn,
  UseDocumentOptions,
  UseDocumentReturn,
  UseSqlQueryOptions,
  UseSqlQueryReturn,
  UseMutationOptions,
  UseMutationReturn,
  UseFtsOptions,
  UseGraphNeighborsOptions,
  UseDatabaseMaintenanceReturn,
  FtsHit,
  GraphNode,
  GraphDirection,
  StorageMetrics,
  CheckpointMode,
} from './types.js';

/**
 * Safe dynamic integration with React Navigation and Expo Router.
 * Subscribes to screen 'focus' events when `refreshOnFocus: true`.
 */
function useFocusListener(callback: () => void, enabled?: boolean) {
  const cbRef = useRef(callback);
  cbRef.current = callback;

  useEffect(() => {
    if (!enabled) return;

    try {
      // Dynamic require avoids hard dependency on @react-navigation/native
      // eslint-disable-next-line @typescript-eslint/no-var-requires
      const nav = require('@react-navigation/native');
      if (typeof nav?.useNavigation === 'function') {
        const navigation = nav.useNavigation();
        if (navigation && typeof navigation.addListener === 'function') {
          return navigation.addListener('focus', () => {
            cbRef.current();
          });
        }
      }
    } catch {
      // Navigation is not installed or not in navigation context: transparent no-op
    }
  }, [enabled]);
}

/**
 * React hook to open and manage a LiliaDB database lifecycle in a component.
 */
export function useLiliaDatabase(options: OpenDatabaseOptions) {
  const [db, setDb] = useState<LiliaDatabase | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<Error | null>(null);

  useEffect(() => {
    let active = true;
    let databaseInstance: LiliaDatabase | null = null;

    setIsLoading(true);
    setError(null);

    LiliaDatabase.open(options)
      .then((database) => {
        if (active) {
          databaseInstance = database;
          setDb(database);
          setIsLoading(false);
        } else {
          database.close().catch(() => {});
        }
      })
      .catch((err) => {
        if (active) {
          setError(err instanceof Error ? err : new Error(String(err)));
          setIsLoading(false);
        }
      });

    return () => {
      active = false;
      if (databaseInstance) {
        databaseInstance.close().catch(() => {});
      }
    };
  }, [options.path, options.name, options.profile, options.useSecureStorage, options.passphrase]);

  return { db, isLoading, error };
}

// ==========================================
// 1. KEY / VALUE HOOKS (ZERO FOUC)
// ==========================================

export function useKv<T = string>(
  namespace: string,
  key: string,
  options?: UseKvOptions<T>
): UseKvReturn<T>;
export function useKv<T = string>(
  db: LiliaDatabase | null,
  namespace: string,
  key: string,
  options?: UseKvOptions<T>
): UseKvReturn<T>;

export function useKv<T = string>(
  arg1: string | LiliaDatabase | null,
  arg2: string,
  arg3?: string | UseKvOptions<T>,
  arg4?: UseKvOptions<T>
): UseKvReturn<T> {
  let explicitDb: LiliaDatabase | null | undefined;
  let namespace: string;
  let key: string;
  let options: UseKvOptions<T> | undefined;

  if (typeof arg1 === 'string') {
    namespace = arg1;
    key = arg2;
    options = arg3 as UseKvOptions<T> | undefined;
  } else {
    explicitDb = arg1;
    namespace = arg2;
    key = arg3 as string;
    options = arg4;
  }

  let db: LiliaDatabase | null = null;
  try {
    db = useDatabase(explicitDb ?? options?.db);
  } catch {
    db = explicitDb ?? null;
  }

  const defaultValue = options?.defaultValue as T;

  const deserialize = useCallback(
    (raw: string): T => {
      if (options?.deserialize) return options.deserialize(raw);
      try {
        return JSON.parse(raw) as T;
      } catch {
        return raw as unknown as T;
      }
    },
    [options?.deserialize]
  );

  const serialize = useCallback(
    (val: T): string => {
      if (options?.serialize) return options.serialize(val);
      if (typeof val === 'string') return val;
      return JSON.stringify(val);
    },
    [options?.serialize]
  );

  // Instant synchronous initial value (Zero FOUC via JSI / in-memory cache)
  const initialSyncVal = (() => {
    if (!db) return defaultValue;
    const peeked = db.sync.kv.peek(namespace, key);
    return peeked != null ? deserialize(peeked) : defaultValue;
  })();

  const [value, setValueState] = useState<T>(initialSyncVal);
  const [isLoading, setIsLoading] = useState(initialSyncVal === defaultValue);
  const [error, setError] = useState<Error | null>(null);

  const fetchValue = useCallback(async () => {
    if (!db) return;
    try {
      const entry = await db.kv.get(namespace, key);
      if (entry) {
        const rawStr = new TextDecoder().decode(entry.value);
        setValueState(deserialize(rawStr));
      } else {
        setValueState(defaultValue);
      }
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err : new Error(String(err)));
    } finally {
      setIsLoading(false);
    }
  }, [db, namespace, key, deserialize, defaultValue]);

  useEffect(() => {
    fetchValue();
    if (!db) return;

    const pattern = `kv:${namespace}:${key}`;
    return db.subscribe(pattern, () => {
      fetchValue();
    });
  }, [db, namespace, key, fetchValue]);

  useFocusListener(fetchValue, options?.refreshOnFocus);

  const setValue = useCallback(
    async (newValueOrUpdater: T | ((prev: T) => T), expiresAtMs?: number) => {
      if (!db) throw new Error('[LiliaDB] Database is not initialized');

      let resolvedVal: T;
      if (typeof newValueOrUpdater === 'function') {
        resolvedVal = (newValueOrUpdater as (prev: T) => T)(value);
      } else {
        resolvedVal = newValueOrUpdater;
      }

      const str = serialize(resolvedVal);
      // Synchronous instant update in JSI + async disk commit
      db.sync.kv.set(namespace, key, str, expiresAtMs);
      await db.kv.set(namespace, key, str, expiresAtMs);
      setValueState(resolvedVal);
    },
    [db, namespace, key, value, serialize]
  );

  const remove = useCallback(async () => {
    if (!db) throw new Error('[LiliaDB] Database is not initialized');
    db.sync.kv.delete(namespace, key);
    await db.kv.delete(namespace, key);
    setValueState(defaultValue);
  }, [db, namespace, key, defaultValue]);

  const meta = {
    value,
    setValue,
    isLoading,
    error,
    remove,
    reload: fetchValue,
  };

  const tuple = [value, setValue, meta] as UseKvReturn<T>;
  Object.assign(tuple, meta);
  return tuple;
}

export function useKvState<T>(
  namespace: string,
  key: string,
  defaultValue: T,
  options?: UseKvOptions<T>
): [T, (newValue: T | ((prev: T) => T)) => Promise<void>] {
  const [val, setVal] = useKv<T>(namespace, key, { ...options, defaultValue });
  return [val, setVal];
}

// ==========================================
// 2. JSON DOCUMENT HOOKS (ZERO FOUC)
// ==========================================

export function useDocument<T = unknown>(
  space: string,
  id: string,
  options?: UseDocumentOptions<T>
): UseDocumentReturn<T>;
export function useDocument<T = unknown>(
  db: LiliaDatabase | null,
  space: string,
  id: string,
  options?: UseDocumentOptions<T>
): UseDocumentReturn<T>;

export function useDocument<T = unknown>(
  arg1: string | LiliaDatabase | null,
  arg2: string,
  arg3?: string | UseDocumentOptions<T>,
  arg4?: UseDocumentOptions<T>
): UseDocumentReturn<T> {
  let explicitDb: LiliaDatabase | null | undefined;
  let space: string;
  let id: string;
  let options: UseDocumentOptions<T> | undefined;

  if (typeof arg1 === 'string') {
    space = arg1;
    id = arg2;
    options = arg3 as UseDocumentOptions<T> | undefined;
  } else {
    explicitDb = arg1;
    space = arg2;
    id = arg3 as string;
    options = arg4;
  }

  let db: LiliaDatabase | null = null;
  try {
    db = useDatabase(explicitDb ?? options?.db);
  } catch {
    db = explicitDb ?? null;
  }

  const initialSyncDoc = (() => {
    if (!db) return options?.defaultValue ?? null;
    const syncDoc = db.sync.json.get<T>(space, id);
    return syncDoc ?? (options?.defaultValue ?? null);
  })();

  const [data, setData] = useState<T | null>(initialSyncDoc);
  const [isLoading, setIsLoading] = useState(initialSyncDoc == null);
  const [error, setError] = useState<Error | null>(null);

  const fetchDoc = useCallback(async () => {
    if (!db) return;
    try {
      const doc = await db.json.get<T>(space, id);
      setData(doc?.value ?? (options?.defaultValue ?? null));
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err : new Error(String(err)));
    } finally {
      setIsLoading(false);
    }
  }, [db, space, id, options?.defaultValue]);

  useEffect(() => {
    fetchDoc();
    if (!db) return;

    const pattern = `json:${space}:${id}`;
    return db.subscribe(pattern, () => {
      fetchDoc();
    });
  }, [db, space, id, fetchDoc]);

  useFocusListener(fetchDoc, options?.refreshOnFocus);

  const update = useCallback(
    async (updater: Partial<T> | T | ((prev: T | null) => T)) => {
      if (!db) throw new Error('[LiliaDB] Database is not initialized');

      let nextVal: T;
      if (typeof updater === 'function') {
        nextVal = (updater as (prev: T | null) => T)(data);
      } else if (data && typeof data === 'object' && typeof updater === 'object') {
        nextVal = { ...data, ...updater } as T;
      } else {
        nextVal = updater as T;
      }

      db.sync.json.put(space, id, nextVal);
      await db.json.put(space, id, nextVal);
      setData(nextVal);
    },
    [db, space, id, data]
  );

  const remove = useCallback(async () => {
    if (!db) throw new Error('[LiliaDB] Database is not initialized');
    db.sync.json.delete(space, id);
    await db.json.delete(space, id);
    setData(null);
  }, [db, space, id]);

  const meta = {
    data,
    update,
    isLoading,
    error,
    remove,
    reload: fetchDoc,
  };

  const tuple = [data, update, meta] as UseDocumentReturn<T>;
  Object.assign(tuple, meta);
  return tuple;
}

export const useJson = useDocument;

// ==========================================
// 3. GRAPH HOOKS
// ==========================================

export function useGraphNeighbors(
  graph: string,
  node: string,
  options?: UseGraphNeighborsOptions
) {
  let db: LiliaDatabase | null = null;
  try {
    db = useDatabase(options?.db);
  } catch {
    db = options?.db ?? null;
  }

  const [neighbors, setNeighbors] = useState<string[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<Error | null>(null);

  const fetchNeighbors = useCallback(async () => {
    if (!db) return;
    try {
      const res = await db.graph.neighbors(
        graph,
        node,
        options?.direction ?? 'out',
        options?.kindFilter
      );
      setNeighbors(res);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err : new Error(String(err)));
    } finally {
      setIsLoading(false);
    }
  }, [db, graph, node, options?.direction, options?.kindFilter]);

  useEffect(() => {
    fetchNeighbors();
    if (!db) return;

    return db.subscribe(`graph:${graph}`, () => {
      fetchNeighbors();
    });
  }, [db, graph, fetchNeighbors]);

  useFocusListener(fetchNeighbors, options?.refreshOnFocus);

  return { neighbors, isLoading, error, reload: fetchNeighbors };
}

// ==========================================
// 4. FULL-TEXT SEARCH (DEBOUNCED)
// ==========================================

export function useFts(
  collection: string,
  query: string,
  options?: UseFtsOptions
) {
  let db: LiliaDatabase | null = null;
  try {
    db = useDatabase(options?.db);
  } catch {
    db = options?.db ?? null;
  }

  const [hits, setHits] = useState<FtsHit[]>([]);
  const [isSearching, setIsSearching] = useState(false);
  const [error, setError] = useState<Error | null>(null);
  const debounceTimer = useRef<NodeJS.Timeout | null>(null);

  const runSearch = useCallback(async () => {
    if (!db || !query.trim()) {
      setHits([]);
      setIsSearching(false);
      return;
    }

    try {
      const results = await db.fts.search(collection, query, options?.limit ?? 20);
      setHits(results);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err : new Error(String(err)));
    } finally {
      setIsSearching(false);
    }
  }, [db, collection, query, options?.limit]);

  useEffect(() => {
    if (debounceTimer.current) clearTimeout(debounceTimer.current);

    if (!query.trim()) {
      setHits([]);
      setIsSearching(false);
      return;
    }

    setIsSearching(true);
    debounceTimer.current = setTimeout(runSearch, options?.debounceMs ?? 200);

    return () => {
      if (debounceTimer.current) clearTimeout(debounceTimer.current);
    };
  }, [query, runSearch, options?.debounceMs]);

  useFocusListener(runSearch, options?.refreshOnFocus);

  return { hits, isSearching, error };
}

// ==========================================
// 5. SQL ANALYTICS HOOK
// ==========================================

export function useSqlQuery<T = Record<string, unknown>>(
  sql: string,
  params?: unknown[],
  options?: UseSqlQueryOptions
): UseSqlQueryReturn<T>;
export function useSqlQuery<T = Record<string, unknown>>(
  db: LiliaDatabase | null,
  sql: string,
  params?: unknown[],
  deps?: DependencyList
): UseSqlQueryReturn<T>;

export function useSqlQuery<T = Record<string, unknown>>(
  arg1: string | LiliaDatabase | null,
  arg2?: string | unknown[],
  arg3?: unknown[] | DependencyList | UseSqlQueryOptions,
  arg4?: DependencyList
): UseSqlQueryReturn<T> {
  let explicitDb: LiliaDatabase | null | undefined;
  let sql: string;
  let params: unknown[] | undefined;
  let options: UseSqlQueryOptions | undefined;
  let deps: DependencyList = [];

  if (typeof arg1 === 'string') {
    sql = arg1;
    params = Array.isArray(arg2) ? arg2 : undefined;
    options = arg3 as UseSqlQueryOptions | undefined;
    deps = options?.deps ?? [];
  } else {
    explicitDb = arg1;
    sql = arg2 as string;
    params = Array.isArray(arg3) ? arg3 : undefined;
    deps = arg4 ?? [];
  }

  let db: LiliaDatabase | null = null;
  try {
    db = useDatabase(explicitDb ?? options?.db);
  } catch {
    db = explicitDb ?? null;
  }

  // Instant synchronous first query if JSI is available
  const initialSyncResult = (() => {
    if (!db || options?.enabled === false) return null;
    try {
      return db.sync.sql.query<T>(sql, params);
    } catch {
      return null;
    }
  })();

  const [data, setData] = useState<T[]>(initialSyncResult?.rows ?? []);
  const [columns, setColumns] = useState<string[]>(initialSyncResult?.columns ?? []);
  const [isLoading, setIsLoading] = useState(initialSyncResult == null);
  const [isRefetching, setIsRefetching] = useState(false);
  const [error, setError] = useState<Error | null>(null);

  const fetchQuery = useCallback(
    async (isRefetch = false) => {
      if (!db || options?.enabled === false) return;
      if (isRefetch) setIsRefetching(true);
      else setIsLoading(true);

      try {
        const res = await db.sql.query<T>(sql, params);
        setData(res.rows);
        setColumns(res.columns);
        setError(null);
      } catch (err) {
        setError(err instanceof Error ? err : new Error(String(err)));
      } finally {
        setIsLoading(false);
        setIsRefetching(false);
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [db, sql, JSON.stringify(params), options?.enabled, ...deps]
  );

  useEffect(() => {
    fetchQuery();

    if (!db || !options?.tables || options.tables.length === 0) return;

    const unsubs = options.tables.map((table) =>
      db!.subscribe(`table:${table}`, () => {
        fetchQuery(true);
      })
    );

    return () => {
      unsubs.forEach((unsub) => unsub());
    };
  }, [db, fetchQuery, options?.tables]);

  useFocusListener(() => fetchQuery(true), options?.refreshOnFocus);

  return {
    data,
    rows: data,
    columns,
    isLoading,
    isRefetching,
    error,
    refetch: () => fetchQuery(true),
  };
}

// ==========================================
// 6. TANSTACK QUERY STYLE USE MUTATION
// ==========================================

export function useMutation<TData = unknown, TVariables = void>(
  mutationFn: (variables: TVariables, db: LiliaDatabase) => Promise<TData>,
  options?: UseMutationOptions<TData, TVariables>
): UseMutationReturn<TData, TVariables> {
  let db: LiliaDatabase | null = null;
  try {
    db = useDatabase(options?.db);
  } catch {
    db = options?.db ?? null;
  }

  const [isPending, setIsPending] = useState(false);
  const [error, setError] = useState<Error | null>(null);
  const [data, setData] = useState<TData | undefined>(undefined);

  const mutateAsync = useCallback(
    async (variables: TVariables): Promise<TData> => {
      if (!db) throw new Error('[LiliaDB] Database is not initialized for mutation');
      setIsPending(true);
      setError(null);

      try {
        const result = await mutationFn(variables, db);
        setData(result);

        if (options?.invalidateTables) {
          for (const table of options.invalidateTables) {
            db.notifyChange({ type: 'table', table });
          }
        }
        if (options?.invalidateKeys) {
          for (const item of options.invalidateKeys) {
            db.notifyChange({ type: 'kv', namespace: item.namespace, key: item.key, op: 'set' });
          }
        }
        if (options?.invalidateSpaces) {
          for (const space of options.invalidateSpaces) {
            db.notifyChange({ type: 'json', space, id: '*', op: 'put' });
          }
        }

        await options?.onSuccess?.(result, variables);
        await options?.onSettled?.(result, null, variables);
        return result;
      } catch (err) {
        const errObj = err instanceof Error ? err : new Error(String(err));
        setError(errObj);
        await options?.onError?.(errObj, variables);
        await options?.onSettled?.(undefined, errObj, variables);
        throw errObj;
      } finally {
        setIsPending(false);
      }
    },
    [db, mutationFn, options]
  );

  const mutate = useCallback(
    (variables: TVariables) => {
      mutateAsync(variables).catch(() => {});
    },
    [mutateAsync]
  );

  const reset = useCallback(() => {
    setIsPending(false);
    setError(null);
    setData(undefined);
  }, []);

  return {
    mutate,
    mutateAsync,
    isPending,
    error,
    data,
    reset,
  };
}

// ==========================================
// 7. MAINTENANCE & STORAGE METRICS HOOK
// ==========================================

export function useDatabaseMaintenance(dbOverride?: LiliaDatabase | null): UseDatabaseMaintenanceReturn {
  let db: LiliaDatabase | null = null;
  try {
    db = useDatabase(dbOverride);
  } catch {
    db = dbOverride ?? null;
  }

  const [metrics, setMetrics] = useState<StorageMetrics | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<Error | null>(null);

  const refresh = useCallback(async () => {
    if (!db) return;
    setIsLoading(true);
    try {
      const res = await db.maintenance.getFileSize();
      setMetrics(res);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err : new Error(String(err)));
    } finally {
      setIsLoading(false);
    }
  }, [db]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const vacuum = useCallback(async () => {
    if (!db) throw new Error('[LiliaDB] Database is not initialized');
    await db.maintenance.vacuum();
    await refresh();
  }, [db, refresh]);

  const checkpoint = useCallback(
    async (mode?: CheckpointMode) => {
      if (!db) throw new Error('[LiliaDB] Database is not initialized');
      await db.maintenance.checkpoint(mode);
      await refresh();
    },
    [db, refresh]
  );

  return {
    metrics,
    isLoading,
    error,
    vacuum,
    checkpoint,
    refresh,
  };
}
