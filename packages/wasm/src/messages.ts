// What a database's thread and the worker that runs it say to each other.

import type { ModuleSource } from "./module.js";

/** Both ends of the exchange: a `Worker` and the worker's global scope, or `MessagePort`s. */
export interface Endpoint {
  postMessage(message: unknown, transfer: Transferable[]): void;
  addEventListener(type: "message", listener: (event: MessageEvent) => void): void;
  removeEventListener(type: "message", listener: (event: MessageEvent) => void): void;
  /** Starts the delivery of messages, which a `MessagePort` needs. */
  start?(): void;
}

export type Request =
  | { id: number; type: "open"; options: string; wasm?: ModuleSource }
  | { id: number; type: "execute"; request: ArrayBuffer }
  | { id: number; type: "close" };

/** A reply: the JS frame of an `execute`, nothing for `open` and `close`, or the error thrown. */
export type Reply = { id: number; frame?: ArrayBuffer } | { id: number; error: string };
