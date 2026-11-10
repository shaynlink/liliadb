import type {
  BatchOperation,
  DatabaseEvent,
  FtsHit,
  GraphDirection,
  JsonEntry,
  JsonIndexInfo,
  JsonValue,
  KvEntry,
  MutationResult,
  SqlQueryResult,
} from "@liliadb/node";

export type ConnectionMode = "embedded" | "daemon";

export interface DatabaseConnectionInfo {
  path: string;
  mode: ConnectionMode;
  connected: boolean;
  endpoint?: string;
  tokenFile?: string;
  databaseName: string;
  metadata?: Record<string, string>;
  stats?: {
    spacesCount: number;
    namespacesCount: number;
    graphsCount: number;
    ftsCollectionsCount: number;
  };
}

export interface GraphNode {
  graph: string;
  id: string;
  kind: string;
  metadata: JsonValue;
}

export interface GraphEdge {
  graph: string;
  source: string;
  target: string;
  kind: string;
}

export interface SpaceSummary {
  space: string;
  count: number;
  indexes: JsonIndexInfo[];
}

export interface NamespaceSummary {
  namespace: string;
  count: number;
}

export interface GraphSummary {
  graph: string;
  nodeCount: number;
  edgeCount: number;
}

export interface FtsSummary {
  collection: string;
  count: number;
}

export interface CommandExecutionResult {
  ok: boolean;
  durationMs: number;
  type: "sql" | "batch" | "json" | "kv";
  sqlResult?: SqlQueryResult;
  mutations?: MutationResult[];
  error?: string;
  timestamp: number;
}

export type WebviewMessage =
  | { type: "ready" }
  | { type: "refresh" }
  | { type: "selectSpace"; space: string }
  | { type: "selectNamespace"; namespace: string }
  | { type: "selectGraph"; graph: string }
  | { type: "selectFts"; collection: string }
  | { type: "loadJsonPage"; space: string; after?: string; limit?: number }
  | { type: "loadKvPage"; namespace: string; after?: string; limit?: number }
  | { type: "putJson"; space: string; id: string; value: JsonValue; ifVersion?: string }
  | { type: "deleteJson"; space: string; id: string; ifVersion?: string }
  | { type: "setKv"; namespace: string; key: string; value: string; encoding?: "text" | "hex" | "base64"; expiresAtMs?: number; ifVersion?: string }
  | { type: "deleteKv"; namespace: string; key: string; encoding?: "text" | "hex" | "base64"; ifVersion?: string }
  | { type: "searchFts"; collection: string; query: string; limit?: number }
  | { type: "graphTraverse"; graph: string; node: string; direction: GraphDirection; kind?: string }
  | { type: "executeSql"; sql: string; params?: unknown[] }
  | { type: "executeBatch"; operations: BatchOperation[] };
