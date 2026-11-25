import type React from "react";
import type { DependencyList } from "react";
import type { FlatListProps, ListRenderItem } from "react-native";
import type {
  EngineOptions,
  FtsHit,
  GraphDirection,
  JsonValue,
  StorageStats,
} from "@liliadb/client";
import type { LiliaDatabase } from "./database.js";

export interface IosDatabaseConfig {
  /**
   * The directory the database lives in:
   * - `applicationSupport`: persistent, not shown in the Files app;
   * - `documents`: shown in the Files app when the app enables file sharing;
   * - `caches`: the system may delete it when storage runs low.
   * @default "applicationSupport"
   */
  directory?: "applicationSupport" | "documents" | "caches";
  /**
   * Excludes the database from iCloud backups (`NSURLIsExcludedFromBackupKey`), as Apple asks
   * of data the app can rebuild or download again.
   * @default true
   */
  excludeFromCloudBackup?: boolean;
  /**
   * An App Group (`group.com.example.app`) whose container holds the database, to share it with
   * widgets, App Clips and watch apps of the group.
   */
  groupContainerIdentifier?: string;
}

export interface AndroidDatabaseConfig {
  /**
   * The directory the database lives in:
   * - `noBackup`: left out of Auto Backup (whose quota is 25 MB);
   * - `files`: the app's internal storage;
   * - `cache`: the system may delete it when storage runs low.
   * @default "noBackup"
   */
  directory?: "noBackup" | "files" | "cache";
}

export interface OpenDatabaseOptions extends EngineOptions {
  /** The database's name: it lives in `<name>.lilia`, in the directory the platform options choose. */
  name?: string;
  /** The database's directory, instead of `name`. */
  path?: string;
  /**
   * Checkpoints the database (writes its memtables to sorted runs) when the app goes to the
   * background, where the system may end it: the commits are then in the database's files
   * whatever the durability, and the next open has less journal to replay.
   * @default true
   */
  checkpointOnBackground?: boolean;
  ios?: IosDatabaseConfig;
  android?: AndroidDatabaseConfig;
  dev?: {
    /**
     * In development, `open` returns the database already open at the same path, so that Fast
     * Refresh, which runs the app's modules again, does not open it twice.
     * @default true
     */
    reuseAcrossFastRefresh?: boolean;
  };
}

// ==========================================
// HOOKS
// ==========================================

export interface UseKvOptions<T> {
  defaultValue?: T;
  /** The value as the text stored; JSON by default. */
  serialize?: (value: T) => string;
  /** The value from the text stored; JSON by default, text that is not JSON as it is. */
  deserialize?: (raw: string) => T;
  db?: LiliaDatabase | null;
}

export interface UseKvMeta<T> {
  value: T;
  setValue: (newValue: T | ((prev: T) => T), expiresAtMs?: number) => Promise<void>;
  isLoading: boolean;
  error: Error | null;
  remove: () => Promise<void>;
  reload: () => Promise<void>;
}

export type UseKvReturn<T> = [
  T,
  (newValue: T | ((prev: T) => T), expiresAtMs?: number) => Promise<void>,
  UseKvMeta<T>,
] &
  UseKvMeta<T>;

export interface UseDocumentOptions<T> {
  defaultValue?: T;
  db?: LiliaDatabase | null;
}

export interface UseDocumentMeta<T> {
  data: T | null;
  update: (updater: Partial<T> | T | ((prev: T | null) => T)) => Promise<void>;
  isLoading: boolean;
  error: Error | null;
  remove: () => Promise<void>;
  reload: () => Promise<void>;
}

export type UseDocumentReturn<T> = [
  T | null,
  (updater: Partial<T> | T | ((prev: T | null) => T)) => Promise<void>,
  UseDocumentMeta<T>,
] &
  UseDocumentMeta<T>;

export interface UseSqlQueryOptions {
  /**
   * The tables whose writes run the query again (`lilia_kv`, `lilia_json`, a JSON space...);
   * any write does when none are given.
   */
  tables?: string[];
  enabled?: boolean;
  deps?: DependencyList;
  db?: LiliaDatabase | null;
}

export interface UseSqlQueryReturn<T> {
  data: T[];
  rows: T[];
  columns: string[];
  isLoading: boolean;
  isRefetching: boolean;
  error: Error | null;
  refetch: () => Promise<void>;
}

export interface UseMutationOptions<TData, TVariables> {
  onSuccess?: (data: TData, variables: TVariables) => void | Promise<void>;
  onError?: (error: Error, variables: TVariables) => void | Promise<void>;
  onSettled?: (
    data: TData | undefined,
    error: Error | null,
    variables: TVariables,
  ) => void | Promise<void>;
  db?: LiliaDatabase | null;
}

export interface UseMutationReturn<TData, TVariables> {
  mutate: (variables: TVariables) => void;
  mutateAsync: (variables: TVariables) => Promise<TData>;
  isPending: boolean;
  error: Error | null;
  data: TData | undefined;
  reset: () => void;
}

export interface UseFtsOptions {
  limit?: number;
  debounceMs?: number;
  db?: LiliaDatabase | null;
}

export interface UseFtsReturn {
  hits: FtsHit[];
  isSearching: boolean;
  error: Error | null;
}

export interface UseGraphNeighborsOptions {
  direction?: GraphDirection;
  kind?: string;
  db?: LiliaDatabase | null;
}

export interface UseGraphNeighborsReturn {
  neighbors: string[];
  isLoading: boolean;
  error: Error | null;
  reload: () => Promise<void>;
}

export interface UseDatabaseMaintenanceReturn {
  stats: StorageStats | null;
  isLoading: boolean;
  error: Error | null;
  /** Writes the memtables to sorted runs. */
  checkpoint: () => Promise<void>;
  /** Whether the database's structures are consistent. */
  integrityCheck: () => Promise<boolean>;
  refresh: () => Promise<void>;
}

// ==========================================
// COMPONENTS
// ==========================================

export type LiliaProviderProps =
  | {
      database: LiliaDatabase;
      children: React.ReactNode;
      options?: never;
      fallback?: never;
      errorFallback?: never;
    }
  | {
      options: OpenDatabaseOptions;
      children: React.ReactNode;
      database?: never;
      fallback?: React.ReactNode;
      errorFallback?: (error: Error) => React.ReactNode;
    };

export interface LiliaFlatListProps<T> extends Omit<FlatListProps<T>, "data"> {
  /** The SQL query whose records the list shows. */
  sql?: string;
  sqlParams?: JsonValue[];
  /** The tables whose writes run the query again; any write does when none are given. */
  tables?: string[];
  /** The list's records, instead of a query. */
  data?: T[];
  renderItem: ListRenderItem<T>;
  loadingComponent?: React.ReactNode;
  emptyComponent?: React.ReactNode;
  errorComponent?: (error: Error) => React.ReactNode;
  db?: LiliaDatabase | null;
}
