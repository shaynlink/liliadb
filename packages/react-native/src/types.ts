import type React from 'react';
import type { DependencyList } from 'react';
import type { FlatListProps, ListRenderItem } from 'react-native';
import type { LiliaDatabase } from './database.js';

export type ResourceProfile = 'mobile' | 'constrained' | 'server';

export interface IosDatabaseConfig {
  /**
   * Sandbox storage directory:
   * - 'applicationSupport': standard persistent location (not exposed in Files app)
   * - 'documents': backed up to iCloud, visible if File Sharing is enabled
   * - 'caches': purgeable by OS under low storage pressure
   * @default 'applicationSupport'
   */
  directory?: 'applicationSupport' | 'documents' | 'caches';
  /**
   * Explicitly exclude database from iCloud backup via NSURLIsExcludedFromBackupKey.
   * Highly recommended by Apple to avoid App Store review rejections on large databases.
   * @default true
   */
  excludeFromCloudBackup?: boolean;
  /**
   * App Group container identifier (e.g. 'group.com.example.app') for sharing
   * database storage with iOS Widgets (WidgetKit), App Clips, and watchOS companions.
   */
  groupContainerIdentifier?: string;
}

export interface AndroidDatabaseConfig {
  /**
   * Sandbox storage directory:
   * - 'noBackup': excluded from Google Drive auto-backup (avoids exceeding 25MB backup quota)
   * - 'files': standard app internal storage
   * - 'cache': purgeable cache
   * @default 'noBackup'
   */
  directory?: 'noBackup' | 'files' | 'cache';
}

export interface OpenDatabaseOptions {
  /**
   * Database name (e.g. 'app', 'user_vault').
   * When provided, automatically resolves the optimal sandbox directory based on the OS.
   */
  name?: string;
  /** Explicit absolute file path (alternative to `name`). */
  path?: string;
  /**
   * Hardware-calibrated resource profile:
   * - 'mobile': 8MB cache, optimized WAL (Smartphones, Tablets, Android TV)
   * - 'constrained': 2MB cache, mmap disabled (watchOS, Wear OS, tvOS)
   * - 'server': 64MB cache, 256MB mmap
   * @default 'mobile'
   */
  profile?: ResourceProfile;
  /**
   * Automatically generate and retrieve a 256-bit AES-GCM encryption key
   * in the device's hardware enclave (iOS Keychain / Android Keystore).
   * @default false
   */
  useSecureStorage?: boolean;
  /**
   * Optional manual 32-byte encryption key (Uint8Array or 64-character hex string).
   */
  encryptionKey?: Uint8Array | string;
  /**
   * Optional passphrase for PBKDF2-HMAC-SHA256 key derivation.
   */
  passphrase?: string;
  /**
   * Automatically flush the SQLite WAL when the mobile app transitions to the background.
   * @default true
   */
  autoFlushOnBackground?: boolean;
  /** iOS & Apple platforms specific tuning. */
  ios?: IosDatabaseConfig;
  /** Android & Wear OS platforms specific tuning. */
  android?: AndroidDatabaseConfig;
  /** Developer settings. */
  dev?: {
    /**
     * Reuse open database instance across Fast Refresh cycles during development.
     * Prevents 'SQLITE_BUSY: database is locked' during hot reloads.
     * @default true
     */
    enableFastRefreshSingleton?: boolean;
  };
}

// ==========================================
// MULTIMODEL ENTITY TYPES
// ==========================================

export interface KvEntry {
  namespace: string;
  key: Uint8Array;
  value: Uint8Array;
  version: number;
  expiresAtMs?: number;
}

export interface JsonEntry<T = unknown> {
  space: string;
  id: string;
  value: T;
  version: number;
}

export interface GraphNode<T = unknown> {
  graph: string;
  id: string;
  kind: string;
  metadata: T;
}

export interface GraphEdge {
  graph: string;
  source: string;
  target: string;
  kind: string;
}

export type GraphDirection = 'in' | 'out' | 'both';

export interface FtsHit {
  collection: string;
  docId: string;
  snippet: string;
  rank: number;
}

export interface SqlQueryResult<T = Record<string, unknown>> {
  columns: string[];
  rows: T[];
  rowsAffected: number;
}

export interface StorageMetrics {
  mainBytes: number;
  walBytes: number;
  shmBytes: number;
  totalBytes: number;
}

export type CheckpointMode = 'passive' | 'full' | 'restart' | 'truncate';

// ==========================================
// REACTIVE EVENT TYPES
// ==========================================

export type DatabaseChangeEvent =
  | { type: 'kv'; namespace: string; key: string; op: 'set' | 'delete' }
  | { type: 'json'; space: string; id: string; op: 'put' | 'delete' }
  | { type: 'graph'; graph: string; op: 'node' | 'edge' }
  | { type: 'fts'; collection: string }
  | { type: 'table'; table: string }
  | { type: 'custom'; tag: string };

// ==========================================
// REACT HOOKS OPTIONS & RETURNS
// ==========================================

export interface UseKvOptions<T> {
  defaultValue?: T;
  serialize?: (value: T) => string;
  deserialize?: (raw: string) => T;
  refreshOnFocus?: boolean;
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
  UseKvMeta<T>
] & UseKvMeta<T>;

export interface UseDocumentOptions<T> {
  defaultValue?: T;
  refreshOnFocus?: boolean;
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
  UseDocumentMeta<T>
] & UseDocumentMeta<T>;

export interface UseSqlQueryOptions {
  tables?: string[];
  refreshOnFocus?: boolean;
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
  onSettled?: (data: TData | undefined, error: Error | null, variables: TVariables) => void | Promise<void>;
  invalidateTables?: string[];
  invalidateKeys?: Array<{ namespace: string; key: string }>;
  invalidateSpaces?: string[];
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
  refreshOnFocus?: boolean;
  db?: LiliaDatabase | null;
}

export interface UseGraphNeighborsOptions {
  direction?: GraphDirection;
  kindFilter?: string;
  refreshOnFocus?: boolean;
  db?: LiliaDatabase | null;
}

export interface UseDatabaseMaintenanceReturn {
  metrics: StorageMetrics | null;
  isLoading: boolean;
  error: Error | null;
  vacuum: () => Promise<void>;
  checkpoint: (mode?: CheckpointMode) => Promise<void>;
  refresh: () => Promise<void>;
}

// ==========================================
// COMPONENT PROPS
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

export interface LiliaFlatListProps<T> extends Omit<FlatListProps<T>, 'data'> {
  /** SQL query to fetch and observe */
  sql?: string;
  sqlParams?: unknown[];
  /** Table names to observe for live reactivity */
  tables?: string[];
  /** Custom data override */
  data?: T[];
  renderItem: ListRenderItem<T>;
  loadingComponent?: React.ReactNode;
  emptyComponent?: React.ReactNode;
  errorComponent?: (error: Error) => React.ReactNode;
  db?: LiliaDatabase | null;
}
