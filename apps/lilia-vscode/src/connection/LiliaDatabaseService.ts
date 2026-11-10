import {
  Database,
  type BatchOperation,
  type FtsHit,
  type GraphDirection,
  type JsonEntry,
  type JsonIndexInfo,
  type JsonValue,
  type KvEntry,
  type MutationResult,
  type SqlQueryResult,
} from "@liliadb/node";
import type {
  ConnectionMode,
  DatabaseConnectionInfo,
  FtsSummary,
  GraphEdge,
  GraphNode,
  GraphSummary,
  NamespaceSummary,
  SpaceSummary,
} from "../types.js";
import * as path from "node:path";

export class LiliaDatabaseService {
  private db?: Database;
  private currentInfo?: DatabaseConnectionInfo;

  get isConnected(): boolean {
    return this.db !== undefined;
  }

  get info(): DatabaseConnectionInfo | undefined {
    return this.currentInfo;
  }

  async open(options: {
    path: string;
    mode: ConnectionMode;
    endpoint?: string;
    tokenFile?: string;
  }): Promise<DatabaseConnectionInfo> {
    await this.close();

    const normalizedPath = path.resolve(options.path);
    const dbName = path.basename(normalizedPath);

    if (options.mode === "daemon") {
      const endpoint = options.endpoint || `${normalizedPath}.sock`;
      const tokenFile = options.tokenFile || `${normalizedPath}.token`;
      this.db = await Database.open({
        path: normalizedPath,
        mode: "daemon",
        endpoint,
        tokenFile,
      });
      this.currentInfo = {
        path: normalizedPath,
        mode: "daemon",
        connected: true,
        endpoint,
        tokenFile,
        databaseName: dbName,
      };
    } else {
      this.db = await Database.open({
        path: normalizedPath,
        mode: "embedded",
      });
      this.currentInfo = {
        path: normalizedPath,
        mode: "embedded",
        connected: true,
        databaseName: dbName,
      };
    }

    // Load initial metadata and stats
    try {
      this.currentInfo.metadata = await this.getMetadata();
      const [spaces, namespaces, graphs, fts] = await Promise.all([
        this.getSpacesSummary(),
        this.getNamespacesSummary(),
        this.getGraphsSummary(),
        this.getFtsSummary(),
      ]);
      this.currentInfo.stats = {
        spacesCount: spaces.length,
        namespacesCount: namespaces.length,
        graphsCount: graphs.length,
        ftsCollectionsCount: fts.length,
      };
    } catch {
      // Ignore if stats collection fails on fresh or uninitialized db
    }

    return this.currentInfo;
  }

  async close(): Promise<void> {
    if (this.db) {
      try {
        await this.db.close();
      } catch {
        // ignore close error
      }
      this.db = undefined;
    }
    this.currentInfo = undefined;
  }

  private requireDb(): Database {
    if (!this.db) {
      throw new Error("No database currently opened. Please connect to a LiliaDB database first.");
    }
    return this.db;
  }

  // System & Inspection
  async getMetadata(): Promise<Record<string, string>> {
    const db = this.requireDb();
    try {
      const res = await db.sql.query<{ key: string; value: string }>(
        "SELECT key, value FROM _lilia_metadata"
      );
      const meta: Record<string, string> = {};
      for (const row of res) {
        meta[row.key] = row.value;
      }
      return meta;
    } catch {
      return {};
    }
  }

  async integrityCheck(): Promise<boolean> {
    const db = this.requireDb();
    return db.integrityCheck();
  }

  async backup(destination: string): Promise<void> {
    const db = this.requireDb();
    await db.backup(destination);
  }

  // Summaries
  async getSpacesSummary(): Promise<SpaceSummary[]> {
    const db = this.requireDb();
    try {
      const rows = await db.sql.query<{ space: string; count: number }>(
        "SELECT space, COUNT(*) as count FROM _lilia_json GROUP BY space ORDER BY space ASC"
      );
      const summaries: SpaceSummary[] = [];
      for (const row of rows) {
        let indexes: JsonIndexInfo[] = [];
        try {
          indexes = await db.json.listIndexes(row.space);
        } catch {
          indexes = [];
        }
        summaries.push({
          space: row.space,
          count: Number(row.count),
          indexes,
        });
      }
      return summaries;
    } catch {
      return [];
    }
  }

  async getNamespacesSummary(): Promise<NamespaceSummary[]> {
    const db = this.requireDb();
    try {
      const rows = await db.sql.query<{ namespace: string; count: number }>(
        "SELECT namespace, COUNT(*) as count FROM _lilia_kv GROUP BY namespace ORDER BY namespace ASC"
      );
      return rows.map(r => ({
        namespace: r.namespace,
        count: Number(r.count),
      }));
    } catch {
      return [];
    }
  }

  async getGraphsSummary(): Promise<GraphSummary[]> {
    const db = this.requireDb();
    try {
      const nodeRows = await db.sql.query<{ graph: string; count: number }>(
        "SELECT graph, COUNT(*) as count FROM _lilia_graph_nodes GROUP BY graph"
      );
      const edgeRows = await db.sql.query<{ graph: string; count: number }>(
        "SELECT graph, COUNT(*) as count FROM _lilia_graph_edges GROUP BY graph"
      );
      const map = new Map<string, { nodes: number; edges: number }>();
      for (const r of nodeRows) {
        map.set(r.graph, { nodes: Number(r.count), edges: 0 });
      }
      for (const r of edgeRows) {
        const item = map.get(r.graph) || { nodes: 0, edges: 0 };
        item.edges = Number(r.count);
        map.set(r.graph, item);
      }
      return Array.from(map.entries()).map(([graph, counts]) => ({
        graph,
        nodeCount: counts.nodes,
        edgeCount: counts.edges,
      }));
    } catch {
      return [];
    }
  }

  async getFtsSummary(): Promise<FtsSummary[]> {
    const db = this.requireDb();
    try {
      const rows = await db.sql.query<{ collection: string; count: number }>(
        "SELECT collection, COUNT(*) as count FROM _lilia_fts GROUP BY collection ORDER BY collection ASC"
      );
      return rows.map(r => ({
        collection: r.collection,
        count: Number(r.count),
      }));
    } catch {
      return [];
    }
  }

  // JSON operations
  async jsonScan<T = JsonValue>(space: string, after?: string, limit = 50): Promise<JsonEntry<T>[]> {
    const db = this.requireDb();
    return db.json.scan<T>(space, { after, limit });
  }

  async jsonGet<T = JsonValue>(space: string, id: string): Promise<JsonEntry<T> | undefined> {
    const db = this.requireDb();
    return db.json.get<T>(space, id);
  }

  async jsonPut<T = JsonValue>(
    space: string,
    id: string,
    value: T,
    options?: { ifVersion?: bigint }
  ): Promise<MutationResult> {
    const db = this.requireDb();
    return db.json.put<T>(space, id, value, options);
  }

  async jsonDelete(space: string, id: string, options?: { ifVersion?: bigint }): Promise<MutationResult> {
    const db = this.requireDb();
    return db.json.delete(space, id, options);
  }

  async jsonCreateIndex(space: string, name: string, path: string): Promise<void> {
    const db = this.requireDb();
    await db.json.createIndex(space, name, path);
  }

  async jsonDropIndex(space: string, indexName: string): Promise<boolean> {
    const db = this.requireDb();
    return db.json.dropIndex(space, indexName);
  }

  async jsonListIndexes(space: string): Promise<JsonIndexInfo[]> {
    const db = this.requireDb();
    return db.json.listIndexes(space);
  }

  // KV operations
  async kvScan(namespace: string, after?: string | Uint8Array, limit = 50): Promise<KvEntry[]> {
    const db = this.requireDb();
    return db.kv.scan(namespace, { after, limit });
  }

  async kvGet(namespace: string, key: string | Uint8Array): Promise<KvEntry | undefined> {
    const db = this.requireDb();
    return db.kv.get(namespace, key);
  }

  async kvSet(
    namespace: string,
    key: string | Uint8Array,
    value: string | Uint8Array,
    options?: { ifVersion?: bigint; expiresAtMs?: number }
  ): Promise<MutationResult> {
    const db = this.requireDb();
    return db.kv.set(namespace, key, value, options);
  }

  async kvDelete(
    namespace: string,
    key: string | Uint8Array,
    options?: { ifVersion?: bigint }
  ): Promise<MutationResult> {
    const db = this.requireDb();
    return db.kv.delete(namespace, key, options);
  }

  // Graph operations
  async getGraphNodes(graph: string, limit = 100): Promise<GraphNode[]> {
    const db = this.requireDb();
    const rows = await db.sql.query<{ graph: string; id: string; kind: string; metadata: string }>(
      "SELECT graph, id, kind, metadata FROM _lilia_graph_nodes WHERE graph = ? LIMIT ?",
      [graph, limit]
    );
    return rows.map(r => ({
      graph: r.graph,
      id: r.id,
      kind: r.kind,
      metadata: typeof r.metadata === "string" ? JSON.parse(r.metadata) : r.metadata,
    }));
  }

  async getGraphEdges(graph: string, limit = 100): Promise<GraphEdge[]> {
    const db = this.requireDb();
    const rows = await db.sql.query<{ graph: string; source: string; target: string; kind: string }>(
      "SELECT graph, source, target, kind FROM _lilia_graph_edges WHERE graph = ? LIMIT ?",
      [graph, limit]
    );
    return rows.map(r => ({
      graph: r.graph,
      source: r.source,
      target: r.target,
      kind: r.kind,
    }));
  }

  async graphNeighbors(
    graph: string,
    node: string,
    direction: GraphDirection = "outgoing",
    kind?: string
  ): Promise<string[]> {
    const db = this.requireDb();
    return db.graph.neighbors(graph, node, { direction, kind });
  }

  async graphReachability(
    graph: string,
    startNodes: string[],
    direction: GraphDirection = "outgoing"
  ): Promise<string[]> {
    const db = this.requireDb();
    return db.graph.reachability(graph, startNodes, { direction });
  }

  // FTS operations
  async ftsSearch(collection: string, query: string, limit = 20): Promise<FtsHit[]> {
    const db = this.requireDb();
    return db.fts.search(collection, query, { limit });
  }

  // Commands & Executions
  async sqlQuery<T = Record<string, unknown>>(
    sql: string,
    params: JsonValue[] = []
  ): Promise<SqlQueryResult<T>> {
    const db = this.requireDb();
    return db.sql.raw<T>(sql, params);
  }

  async batch(operations: readonly BatchOperation[]): Promise<MutationResult[]> {
    const db = this.requireDb();
    return db.batch(operations);
  }
}
