// Operations to the JSI binding: MessagePack requests, JS frames back.

import {
  bytesOf,
  encodeWire,
  nativeError,
  readFrame,
  unwrap,
  type Operation,
  type ResponseValue,
  type Transport,
  type WireResult,
} from "@liliadb/client";
import type { NativeDatabase } from "./native.js";

/** The response a frame holds; its error, thrown. */
function response([json, tail]: [string, ArrayBuffer]): ResponseValue {
  const { value, segment } = readFrame<WireResult>(json, new Uint8Array(tail));
  return unwrap(bytesOf(value, segment));
}

export class JsiTransport implements Transport {
  /** `done` hears of each operation that succeeded. */
  constructor(
    private readonly database: NativeDatabase,
    private readonly done: (operation: Operation) => void,
  ) {}

  async execute(operation: Operation): Promise<ResponseValue> {
    let frame: [string, ArrayBuffer];
    try {
      frame = await this.database.execute(encodeWire(operation));
    } catch (error) {
      throw nativeError(error);
    }
    const result = response(frame);
    this.done(operation);
    return result;
  }

  executeSync(operation: Operation): ResponseValue {
    let frame: [string, ArrayBuffer];
    try {
      frame = this.database.executeSync(encodeWire(operation));
    } catch (error) {
      throw nativeError(error);
    }
    const result = response(frame);
    this.done(operation);
    return result;
  }

  async close(timeoutMs: number): Promise<void> {
    try {
      await this.database.close(timeoutMs);
    } catch (error) {
      throw nativeError(error);
    }
  }
}
