import {
  NativeModules,
  AppState,
  type AppStateStatus,
  type NativeEventSubscription,
} from 'react-native';
import type {
  OpenDatabaseOptions,
  KvEntry,
  JsonEntry,
  GraphNode,
  GraphDirection,
  FtsHit,
  SqlQueryResult,
  StorageMetrics,
  CheckpointMode,
  DatabaseChangeEvent,
} from './types.js';
import { DatabaseObserver } from './observer.js';
import { resolveDatabasePath } from './sandbox.js';

const { LiliaReactNativeModule } = NativeModules;

// Dev Singleton Registry to prevent SQLITE_BUSY during Fast Refresh
declare const global: {
  __lilia_open_sync?: (options: any) => any;
  __LILIA_SINGLETON_REGISTRY__?: Map<string, LiliaDatabase>;
  __DEV__?: boolean;
};

if (typeof global !== 'undefined' && !global.__LILIA_SINGLETON_REGISTRY__) {
  global.__LILIA_SINGLETON_REGISTRY__ = new Map();
}

function getNativeModule() {
  if (!LiliaReactNativeModule) {
    throw new Error(
      `@liliadb/react-native: Native module not found. ` +
      `Ensure that you have run 'pod install' on iOS or configured autolinking correctly.`
    );
  }
  return LiliaReactNativeModule;
}

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('');
}

function fromHex(hex: string): Uint8Array {
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < hex.length; i += 2) {
    bytes[i / 2] = parseInt(hex.substring(i, i + 2), 16);
  }
  return bytes;
}

export class LiliaDatabase {
  private appStateSubscription?: NativeEventSubscription;
  private readonly observer = new DatabaseObserver();
  private isClosed = false;

  // In-memory sync KV cache for instant 0ms reads (Zero FOUC)
  private readonly memoryKvCache = new Map<string, { value: string; version: number }>();

  private constructor(
    private readonly handle: string,
    public readonly path: string,
    autoFlush: boolean,
    private readonly jsiHostObj?: any
  ) {
    if (autoFlush) {
      this.appStateSubscription = AppState.addEventListener('change', (state: AppStateStatus) => {
        if (state === 'background' || state === 'inactive') {
          this.prepareForBackground().catch(() => {});
        }
      });
    }
  }

  /**
   * Open a LiliaDB embedded database.
   *
   * Automatically:
   * - Resolves optimal sandbox location if `name` is provided.
   * - Derives or retrieves hardware enclave key if `useSecureStorage: true`.
   * - Injects JSI synchronous host object if available.
   * - Protects against Fast Refresh locks during development.
   */
  static async open(options: OpenDatabaseOptions): Promise<LiliaDatabase> {
    const resolvedPath = resolveDatabasePath(options);

    // Fast Refresh Singleton check in development
    const isDev = typeof __DEV__ !== 'undefined' && __DEV__;
    const enableSingleton = options.dev?.enableFastRefreshSingleton ?? true;
    if (isDev && enableSingleton && global.__LILIA_SINGLETON_REGISTRY__?.has(resolvedPath)) {
      const existing = global.__LILIA_SINGLETON_REGISTRY__.get(resolvedPath)!;
      if (!existing.isClosed) {
        return existing;
      }
    }

    let encryptionKeyHex: string | undefined;
    if (options.encryptionKey instanceof Uint8Array) {
      encryptionKeyHex = toHex(options.encryptionKey);
    } else if (typeof options.encryptionKey === 'string') {
      encryptionKeyHex = options.encryptionKey;
    }

    const native = getNativeModule();
    const res = await native.openDatabase({
      path: resolvedPath,
      name: options.name,
      profile: options.profile ?? 'mobile',
      useSecureStorage: options.useSecureStorage ?? false,
      encryptionKeyHex,
      passphrase: options.passphrase,
      ios: options.ios,
      android: options.android,
    });

    // Attempt synchronous JSI hook if present
    let jsiHostObj: any;
    if (typeof global !== 'undefined' && typeof global.__lilia_open_sync === 'function') {
      try {
        jsiHostObj = global.__lilia_open_sync({
          path: res.path,
          profile: options.profile ?? 'mobile',
          encryptionKeyHex,
          passphrase: options.passphrase,
        });
      } catch {
        // Fallback to bridged mode
      }
    }

    const db = new LiliaDatabase(
      res.handle,
      res.path,
      options.autoFlushOnBackground ?? true,
      jsiHostObj
    );

    if (isDev && enableSingleton && global.__LILIA_SINGLETON_REGISTRY__) {
      global.__LILIA_SINGLETON_REGISTRY__.set(resolvedPath, db);
    }

    return db;
  }

  // ==========================================
  // REACTIVE OBSERVER
  // ==========================================

  subscribe(pattern: string, callback: () => void): () => void {
    this.checkClosed();
    return this.observer.subscribe(pattern, callback);
  }

  notifyChange(event: DatabaseChangeEvent): void {
    this.checkClosed();
    this.observer.emit(event);
  }

  // ==========================================
  // SYNCHRONOUS C++ JSI API (0 ms, SUB-MILLISECOND)
  // ==========================================

  readonly sync = {
    kv: {
      get: (namespace: string, key: Uint8Array | string): string | null => {
        this.checkClosed();
        const k = typeof key === 'string' ? key : toHex(key);
        const cacheKey = `${namespace}:${k}`;

        if (this.jsiHostObj) {
          const raw = this.jsiHostObj.kvGet(namespace, k);
          if (raw != null) {
            this.memoryKvCache.set(cacheKey, { value: raw, version: 1 });
          }
          return raw;
        }

        return this.memoryKvCache.get(cacheKey)?.value ?? null;
      },

      set: (
        namespace: string,
        key: Uint8Array | string,
        value: Uint8Array | string,
        expiresAtMs?: number
      ): number => {
        this.checkClosed();
        const k = typeof key === 'string' ? key : toHex(key);
        const v = typeof value === 'string' ? value : toHex(value);
        const cacheKey = `${namespace}:${k}`;

        let version = 1;
        if (this.jsiHostObj) {
          version = this.jsiHostObj.kvSet(namespace, k, v, expiresAtMs ?? 0);
        }

        this.memoryKvCache.set(cacheKey, { value: v, version });
        this.notifyChange({ type: 'kv', namespace, key: k, op: 'set' });
        return version;
      },

      delete: (namespace: string, key: Uint8Array | string): boolean => {
        this.checkClosed();
        const k = typeof key === 'string' ? key : toHex(key);
        const cacheKey = `${namespace}:${k}`;
        this.memoryKvCache.delete(cacheKey);

        let deleted = true;
        if (this.jsiHostObj) {
          deleted = this.jsiHostObj.kvDelete(namespace, k);
        }

        this.notifyChange({ type: 'kv', namespace, key: k, op: 'delete' });
        return deleted;
      },

      peek: (namespace: string, key: Uint8Array | string): string | null => {
        return this.sync.kv.get(namespace, key);
      },
    },

    json: {
      get: <T = unknown>(space: string, id: string): T | null => {
        this.checkClosed();
        if (this.jsiHostObj) {
          const str = this.jsiHostObj.jsonGet(space, id);
          return str ? (JSON.parse(str) as T) : null;
        }
        return null;
      },

      put: <T = unknown>(space: string, id: string, doc: T): number => {
        this.checkClosed();
        const jsonStr = JSON.stringify(doc);
        let version = 1;
        if (this.jsiHostObj) {
          version = this.jsiHostObj.jsonPut(space, id, jsonStr);
        }
        this.notifyChange({ type: 'json', space, id, op: 'put' });
        return version;
      },

      delete: (space: string, id: string): boolean => {
        this.checkClosed();
        let deleted = true;
        if (this.jsiHostObj) {
          deleted = this.jsiHostObj.jsonDelete(space, id);
        }
        this.notifyChange({ type: 'json', space, id, op: 'delete' });
        return deleted;
      },

      find: <T = unknown>(space: string, query: Record<string, unknown>, afterId?: string, limit = 50): JsonEntry<T>[] => {
        this.checkClosed();
        if (this.jsiHostObj) {
          const resStr = this.jsiHostObj.jsonFind(space, JSON.stringify(query), afterId ?? null, limit);
          return JSON.parse(resStr) as JsonEntry<T>[];
        }
        return [];
      },
    },

    graph: {
      setNode: <T = unknown>(graph: string, id: string, kind: string, metadata: T = {} as T): void => {
        this.checkClosed();
        if (this.jsiHostObj) {
          this.jsiHostObj.graphSetNode(graph, id, kind, JSON.stringify(metadata));
        }
        this.notifyChange({ type: 'graph', graph, op: 'node' });
      },

      getNode: <T = unknown>(graph: string, id: string): GraphNode<T> | null => {
        this.checkClosed();
        if (this.jsiHostObj) {
          const str = this.jsiHostObj.graphGetNode(graph, id);
          return str ? (JSON.parse(str) as GraphNode<T>) : null;
        }
        return null;
      },

      deleteNode: (graph: string, id: string): boolean => {
        this.checkClosed();
        let del = true;
        if (this.jsiHostObj) {
          del = this.jsiHostObj.graphDeleteNode(graph, id);
        }
        this.notifyChange({ type: 'graph', graph, op: 'node' });
        return del;
      },

      addEdge: (graph: string, source: string, target: string, kind: string): void => {
        this.checkClosed();
        if (this.jsiHostObj) {
          this.jsiHostObj.graphAddEdge(graph, source, target, kind);
        }
        this.notifyChange({ type: 'graph', graph, op: 'edge' });
      },

      removeEdge: (graph: string, source: string, target: string, kind: string): boolean => {
        this.checkClosed();
        let del = true;
        if (this.jsiHostObj) {
          del = this.jsiHostObj.graphRemoveEdge(graph, source, target, kind);
        }
        this.notifyChange({ type: 'graph', graph, op: 'edge' });
        return del;
      },

      neighbors: (graph: string, node: string, direction: GraphDirection = 'out', kindFilter?: string): string[] => {
        this.checkClosed();
        if (this.jsiHostObj) {
          const dirCode = direction === 'in' ? 0 : direction === 'out' ? 1 : 2;
          const jsonStr = this.jsiHostObj.graphNeighbors(graph, node, dirCode, kindFilter ?? null);
          return JSON.parse(jsonStr) as string[];
        }
        return [];
      },

      reachability: (graph: string, startNodes: string[], direction: GraphDirection = 'out'): string[] => {
        this.checkClosed();
        if (this.jsiHostObj) {
          const dirCode = direction === 'in' ? 0 : direction === 'out' ? 1 : 2;
          const jsonStr = this.jsiHostObj.graphReachability(graph, JSON.stringify(startNodes), dirCode);
          return JSON.parse(jsonStr) as string[];
        }
        return [];
      },
    },

    fts: {
      search: (collection: string, query: string, limit = 20): FtsHit[] => {
        this.checkClosed();
        if (this.jsiHostObj) {
          const jsonStr = this.jsiHostObj.ftsSearch(collection, query, limit);
          return JSON.parse(jsonStr) as FtsHit[];
        }
        return [];
      },
    },

    sql: {
      query: <T = Record<string, unknown>>(sql: string, params?: unknown[]): SqlQueryResult<T> => {
        this.checkClosed();
        if (this.jsiHostObj) {
          const res = this.jsiHostObj.sqlQuery(sql, params ? JSON.stringify(params) : null);
          const columns = JSON.parse(res.columnsJson) as string[];
          const rawRows = JSON.parse(res.rowsJson) as unknown[][];
          const rows: T[] = rawRows.map((r) => {
            const rowObj: Record<string, unknown> = {};
            columns.forEach((c, idx) => {
              rowObj[c] = r[idx];
            });
            return rowObj as T;
          });
          return {
            columns,
            rows,
            rowsAffected: res.rowsAffected,
          };
        }
        return { columns: [], rows: [], rowsAffected: 0 };
      },

      mutate: (sql: string, params?: unknown[], tablesToInvalidate: string[] = []): number => {
        const res = this.sync.sql.query(sql, params);
        for (const table of tablesToInvalidate) {
          this.notifyChange({ type: 'table', table });
        }
        return res.rowsAffected;
      },
    },

    maintenance: {
      vacuum: (): void => {
        this.checkClosed();
        if (this.jsiHostObj) {
          this.jsiHostObj.vacuum();
        }
      },

      checkpoint: (): void => {
        this.checkClosed();
        if (this.jsiHostObj) {
          this.jsiHostObj.checkpoint();
        }
      },

      getFileSize: (): StorageMetrics => {
        this.checkClosed();
        if (this.jsiHostObj) {
          return this.jsiHostObj.getFileSize();
        }
        return { mainBytes: 0, walBytes: 0, shmBytes: 0, totalBytes: 0 };
      },
    },
  };

  // ==========================================
  // ASYNCHRONOUS API (NON-BLOCKING THREAD POOL)
  // ==========================================

  readonly kv = {
    get: async (namespace: string, key: Uint8Array | string): Promise<KvEntry | null> => {
      this.checkClosed();
      const native = getNativeModule();
      const k = typeof key === 'string' ? new TextEncoder().encode(key) : key;
      const kStr = typeof key === 'string' ? key : toHex(key);
      const raw = await native.kvGet(this.handle, namespace, toHex(k));
      if (!raw) return null;

      const valBytes = fromHex(raw.valueHex);
      this.memoryKvCache.set(`${namespace}:${kStr}`, {
        value: new TextDecoder().decode(valBytes),
        version: raw.version,
      });

      return {
        namespace: raw.namespace,
        key: fromHex(raw.keyHex),
        value: valBytes,
        version: raw.version,
        expiresAtMs: raw.expiresAtMs ?? undefined,
      };
    },

    set: async (
      namespace: string,
      key: Uint8Array | string,
      value: Uint8Array | string,
      expiresAtMs?: number
    ): Promise<number> => {
      this.checkClosed();
      const native = getNativeModule();
      const k = typeof key === 'string' ? new TextEncoder().encode(key) : key;
      const v = typeof value === 'string' ? new TextEncoder().encode(value) : value;
      const keyStr = typeof key === 'string' ? key : toHex(key);
      const valStr = typeof value === 'string' ? value : toHex(value);

      const version = await native.kvSet(this.handle, namespace, toHex(k), toHex(v), expiresAtMs ?? null);
      this.memoryKvCache.set(`${namespace}:${keyStr}`, { value: valStr, version });
      this.notifyChange({ type: 'kv', namespace, key: keyStr, op: 'set' });
      return version;
    },

    delete: async (namespace: string, key: Uint8Array | string): Promise<boolean> => {
      this.checkClosed();
      const native = getNativeModule();
      const k = typeof key === 'string' ? new TextEncoder().encode(key) : key;
      const keyStr = typeof key === 'string' ? key : toHex(key);

      const del = typeof native.kvDelete === 'function' ? await native.kvDelete(this.handle, namespace, toHex(k)) : true;
      this.memoryKvCache.delete(`${namespace}:${keyStr}`);
      this.notifyChange({ type: 'kv', namespace, key: keyStr, op: 'delete' });
      return del;
    },
  };

  readonly json = {
    get: async <T = unknown>(space: string, id: string): Promise<JsonEntry<T> | null> => {
      this.checkClosed();
      const native = getNativeModule();
      const raw = await native.jsonGet(this.handle, space, id);
      if (!raw) return null;
      return {
        space: raw.space,
        id: raw.id,
        value: JSON.parse(raw.valueJson) as T,
        version: raw.version,
      };
    },

    put: async <T = unknown>(space: string, id: string, doc: T): Promise<number> => {
      this.checkClosed();
      const native = getNativeModule();
      const jsonStr = JSON.stringify(doc);
      const version = await native.jsonPut(this.handle, space, id, jsonStr);
      this.notifyChange({ type: 'json', space, id, op: 'put' });
      return version;
    },

    delete: async (space: string, id: string): Promise<boolean> => {
      this.checkClosed();
      const native = getNativeModule();
      const del = typeof native.jsonDelete === 'function' ? await native.jsonDelete(this.handle, space, id) : true;
      this.notifyChange({ type: 'json', space, id, op: 'delete' });
      return del;
    },

    find: async <T = unknown>(space: string, query: Record<string, unknown>, afterId?: string, limit = 50): Promise<JsonEntry<T>[]> => {
      this.checkClosed();
      const native = getNativeModule();
      if (typeof native.jsonFind === 'function') {
        const rawList = await native.jsonFind(this.handle, space, JSON.stringify(query), afterId ?? null, limit);
        return rawList.map((r: any) => ({
          space: r.space,
          id: r.id,
          value: JSON.parse(r.valueJson) as T,
          version: r.version,
        }));
      }
      return this.sync.json.find<T>(space, query, afterId, limit);
    },
  };

  readonly graph = {
    setNode: async <T = unknown>(graph: string, id: string, kind: string, metadata: T = {} as T): Promise<void> => {
      this.checkClosed();
      const native = getNativeModule();
      if (typeof native.graphSetNode === 'function') {
        await native.graphSetNode(this.handle, graph, id, kind, JSON.stringify(metadata));
      } else {
        this.sync.graph.setNode(graph, id, kind, metadata);
      }
      this.notifyChange({ type: 'graph', graph, op: 'node' });
    },

    getNode: async <T = unknown>(graph: string, id: string): Promise<GraphNode<T> | null> => {
      this.checkClosed();
      const native = getNativeModule();
      if (typeof native.graphGetNode === 'function') {
        const raw = await native.graphGetNode(this.handle, graph, id);
        return raw ? { graph: raw.graph, id: raw.id, kind: raw.kind, metadata: JSON.parse(raw.metadataJson) as T } : null;
      }
      return this.sync.graph.getNode<T>(graph, id);
    },

    addEdge: async (graph: string, source: string, target: string, kind: string): Promise<void> => {
      this.checkClosed();
      const native = getNativeModule();
      if (typeof native.graphAddEdge === 'function') {
        await native.graphAddEdge(this.handle, graph, source, target, kind);
      } else {
        this.sync.graph.addEdge(graph, source, target, kind);
      }
      this.notifyChange({ type: 'graph', graph, op: 'edge' });
    },

    neighbors: async (graph: string, node: string, direction: GraphDirection = 'out', kindFilter?: string): Promise<string[]> => {
      this.checkClosed();
      const native = getNativeModule();
      if (typeof native.graphNeighbors === 'function') {
        const dirCode = direction === 'in' ? 0 : direction === 'out' ? 1 : 2;
        return await native.graphNeighbors(this.handle, graph, node, dirCode, kindFilter ?? null);
      }
      return this.sync.graph.neighbors(graph, node, direction, kindFilter);
    },

    reachability: async (graph: string, startNodes: string[], direction: GraphDirection = 'out'): Promise<string[]> => {
      this.checkClosed();
      const native = getNativeModule();
      if (typeof native.graphReachability === 'function') {
        const dirCode = direction === 'in' ? 0 : direction === 'out' ? 1 : 2;
        return await native.graphReachability(this.handle, graph, JSON.stringify(startNodes), dirCode);
      }
      return this.sync.graph.reachability(graph, startNodes, direction);
    },
  };

  readonly fts = {
    search: async (collection: string, query: string, limit = 20): Promise<FtsHit[]> => {
      this.checkClosed();
      const native = getNativeModule();
      if (typeof native.ftsSearch === 'function') {
        return await native.ftsSearch(this.handle, collection, query, limit);
      }
      return this.sync.fts.search(collection, query, limit);
    },
  };

  readonly sql = {
    query: async <T = Record<string, unknown>>(sql: string, params?: unknown[]): Promise<SqlQueryResult<T>> => {
      this.checkClosed();
      const native = getNativeModule();
      const paramsJson = params ? JSON.stringify(params) : null;
      const raw = await native.sqlQuery(this.handle, sql, paramsJson);

      const rows: T[] = (raw.rowsJson as string[]).map((rStr: string) => {
        const parsedVals = JSON.parse(rStr) as unknown[];
        const obj: Record<string, unknown> = {};
        raw.columns.forEach((col: string, idx: number) => {
          obj[col] = parsedVals[idx];
        });
        return obj as T;
      });

      return {
        columns: raw.columns,
        rows,
        rowsAffected: raw.rowsAffected,
      };
    },

    mutate: async (sql: string, params?: unknown[], tablesToInvalidate: string[] = []): Promise<number> => {
      this.checkClosed();
      const res = await this.sql.query(sql, params);
      for (const table of tablesToInvalidate) {
        this.notifyChange({ type: 'table', table });
      }
      return res.rowsAffected;
    },
  };

  readonly maintenance = {
    vacuum: async (): Promise<void> => {
      this.checkClosed();
      const native = getNativeModule();
      if (typeof native.vacuum === 'function') {
        await native.vacuum(this.handle);
      } else {
        await this.sql.query('VACUUM;');
      }
    },

    checkpoint: async (mode?: CheckpointMode): Promise<void> => {
      this.checkClosed();
      const native = getNativeModule();
      if (typeof native.checkpoint === 'function') {
        await native.checkpoint(this.handle, mode ?? 'full');
      } else {
        await this.sql.query('PRAGMA wal_checkpoint(FULL);');
      }
    },

    getFileSize: async (): Promise<StorageMetrics> => {
      this.checkClosed();
      const native = getNativeModule();
      if (typeof native.getFileSize === 'function') {
        return await native.getFileSize(this.handle);
      }
      return this.sync.maintenance.getFileSize();
    },

    backup: async (destinationPath: string): Promise<void> => {
      this.checkClosed();
      const native = getNativeModule();
      if (typeof native.backup === 'function') {
        await native.backup(this.handle, destinationPath);
      }
    },
  };

  async prepareForBackground(): Promise<void> {
    if (this.isClosed) return;
    const native = getNativeModule();
    await native.prepareForBackground(this.handle);
  }

  async close(timeoutMs = 5000): Promise<void> {
    if (this.isClosed) return;
    this.isClosed = true;
    this.appStateSubscription?.remove();
    this.observer.clear();
    this.memoryKvCache.clear();

    if (global.__LILIA_SINGLETON_REGISTRY__) {
      global.__LILIA_SINGLETON_REGISTRY__.delete(this.path);
    }

    if (this.jsiHostObj) {
      try {
        this.jsiHostObj.close(timeoutMs);
      } catch {}
    }

    const native = getNativeModule();
    await native.close(this.handle, timeoutMs);
  }

  private checkClosed() {
    if (this.isClosed) {
      throw new Error(`Database at ${this.path} is closed.`);
    }
  }
}

/**
 * Returns the first active LiliaDatabase instance from the global registry, if any.
 */
export function getDefaultDatabase(): LiliaDatabase | null {
  if (global.__LILIA_SINGLETON_REGISTRY__ && global.__LILIA_SINGLETON_REGISTRY__.size > 0) {
    return global.__LILIA_SINGLETON_REGISTRY__.values().next().value ?? null;
  }
  return null;
}

