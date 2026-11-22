// Operations to the module: in this thread, or in the worker that runs the database.

import {
  bytesOf,
  encodeWire,
  frameJsonLength,
  LiliaError,
  nativeError,
  readFrame,
  unwrap,
  type Operation,
  type ResponseValue,
  type Transport,
  type WireResult,
} from "@liliadb/client";
import type { Endpoint, Reply, Request } from "./messages.js";
import type { ModuleDatabase, ModuleSource } from "./module.js";

/** A request without its id, which `WorkerTransport.request` gives it. */
type Outgoing = { [K in Request["type"]]: Omit<Extract<Request, { type: K }>, "id"> }[Request["type"]];

const decoder = new TextDecoder();

/** The response a JS frame holds; its error, thrown. */
function response(frame: Uint8Array): ResponseValue {
  const json = frameJsonLength(frame);
  const { value, segment } = readFrame<WireResult>(decoder.decode(frame.subarray(4, 4 + json)), frame.subarray(4 + json));
  return unwrap(bytesOf(value, segment));
}

/** A database whose module runs in this thread. */
export class ModuleTransport implements Transport {
  constructor(private readonly database: ModuleDatabase) {}

  async execute(operation: Operation): Promise<ResponseValue> {
    let frame: Uint8Array;
    try {
      frame = await this.database.execute(encodeWire(operation));
    } catch (error) {
      throw nativeError(error);
    }
    return response(frame);
  }

  executeSync(operation: Operation): ResponseValue {
    let frame: Uint8Array;
    try {
      frame = this.database.executeSync(encodeWire(operation));
    } catch (error) {
      throw nativeError(error);
    }
    return response(frame);
  }

  async close(): Promise<void> {
    try {
      this.database.close();
    } catch (error) {
      throw nativeError(error);
    } finally {
      this.database.free();
    }
  }
}

/** A database a worker runs (see `serve.ts`), reached through `endpoint`. */
export class WorkerTransport implements Transport {
  private nextId = 1;
  private readonly pending = new Map<number, PromiseWithResolvers<Reply>>();
  private readonly onMessage = (event: MessageEvent) => {
    const reply = event.data as Reply;
    this.pending.get(reply.id)?.resolve(reply);
    this.pending.delete(reply.id);
  };

  /** `release` ends the worker once the database is closed. */
  private constructor(
    private readonly endpoint: Endpoint,
    private readonly release: () => void
  ) {
    endpoint.addEventListener("message", this.onMessage);
    endpoint.start?.();
  }

  /** Opens the database `options` (the module's JSON) describe in the worker of `endpoint`. */
  static async open(endpoint: Endpoint, options: string, wasm: ModuleSource | undefined, release: () => void): Promise<WorkerTransport> {
    const transport = new WorkerTransport(endpoint, release);
    try {
      await transport.request({ type: "open", options, wasm });
    } catch (error) {
      transport.detach();
      throw error;
    }
    return transport;
  }

  async execute(operation: Operation): Promise<ResponseValue> {
    // A copy of its own buffer (a Node `Buffer`'s `slice` would be a view of the encoder's):
    // the encoder reuses its buffer, and this one moves to the worker.
    const request = new Uint8Array(encodeWire(operation)).buffer;
    const reply = await this.request({ type: "execute", request }, [request]);
    if (!("frame" in reply) || !reply.frame) {
      throw new LiliaError({ code: "PROTOCOL", message: "the worker answered without a frame", retryable: false });
    }
    return response(new Uint8Array(reply.frame));
  }

  async close(): Promise<void> {
    try {
      await this.request({ type: "close" });
    } finally {
      this.detach();
    }
  }

  private detach(): void {
    this.endpoint.removeEventListener("message", this.onMessage);
    for (const waiting of this.pending.values()) {
      waiting.reject(new LiliaError({ code: "CLOSED", message: "the database's worker ended", retryable: false }));
    }
    this.pending.clear();
    this.release();
  }

  private async request(message: Outgoing, transfer: Transferable[] = []): Promise<Reply> {
    const id = this.nextId++;
    const waiting = Promise.withResolvers<Reply>();
    this.pending.set(id, waiting);
    this.endpoint.postMessage({ ...message, id }, transfer);
    const reply = await waiting.promise;
    if ("error" in reply) throw nativeError(new Error(reply.error));
    return reply;
  }
}
