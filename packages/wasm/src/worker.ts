import { DirectWasmClient } from "./client.js";
import type { WasmOpenOptions } from "./types.js";

let client: DirectWasmClient | null = null;

self.onmessage = async (event: MessageEvent) => {
  const { id, type, payload } = event.data as {
    id: number;
    type: string;
    payload: any;
  };

  try {
    let result: any;
    switch (type) {
      case "open":
        client = await DirectWasmClient.open(payload as WasmOpenOptions);
        result = true;
        break;

      case "kvGet":
        if (!client) throw new Error("Database not opened");
        result = await client.kvGet(payload.namespace, payload.key);
        break;

      case "kvScan":
        if (!client) throw new Error("Database not opened");
        result = await client.kvScan(payload.namespace, payload.after, payload.limit);
        break;

      case "jsonGet":
        if (!client) throw new Error("Database not opened");
        result = await client.jsonGet(payload.space, payload.id);
        break;

      case "jsonScan":
        if (!client) throw new Error("Database not opened");
        result = await client.jsonScan(payload.space, payload.after, payload.limit);
        break;

      case "jsonCreateIndex":
        if (!client) throw new Error("Database not opened");
        result = await client.jsonCreateIndex(payload.space, payload.name, payload.path);
        break;

      case "jsonDropIndex":
        if (!client) throw new Error("Database not opened");
        result = await client.jsonDropIndex(payload.space, payload.name);
        break;

      case "jsonListIndexes":
        if (!client) throw new Error("Database not opened");
        result = await client.jsonListIndexes(payload.space);
        break;

      case "jsonScanByIndex":
        if (!client) throw new Error("Database not opened");
        result = await client.jsonScanByIndex(
          payload.space,
          payload.indexName,
          payload.value,
          payload.after,
          payload.limit
        );
        break;

      case "jsonFind":
        if (!client) throw new Error("Database not opened");
        result = await client.jsonFind(
          payload.space,
          payload.query,
          payload.after,
          payload.limit
        );
        break;

      case "batch":
        if (!client) throw new Error("Database not opened");
        result = await client.batch(payload.operations);
        break;

      case "graphNeighbors":
        if (!client) throw new Error("Database not opened");
        result = await client.graphNeighbors(
          payload.graph,
          payload.node,
          payload.direction,
          payload.kind
        );
        break;

      case "graphReachability":
        if (!client) throw new Error("Database not opened");
        result = await client.graphReachability(
          payload.graph,
          payload.startNodes,
          payload.direction
        );
        break;

      case "ftsSearch":
        if (!client) throw new Error("Database not opened");
        result = await client.ftsSearch(payload.collection, payload.query, payload.limit);
        break;

      case "sqlQuery":
        if (!client) throw new Error("Database not opened");
        result = await client.sqlQuery(payload.sql, payload.params);
        break;

      case "integrityCheck":
        if (!client) throw new Error("Database not opened");
        result = await client.integrityCheck();
        break;

      case "close":
        if (client) {
          await client.close(payload.timeoutMs);
          client = null;
        }
        result = true;
        break;

      default:
        throw new Error(`Unknown worker message type: ${type}`);
    }

    self.postMessage({ id, success: true, result });
  } catch (err: any) {
    self.postMessage({
      id,
      success: false,
      error: {
        name: err?.name ?? "Error",
        message: err?.message ?? String(err),
        code: err?.code,
        retryable: err?.retryable,
      },
    });
  }
};
