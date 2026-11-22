// How operations reach a database: in process through the native module, or over the local
// socket of a daemon. Both take operations in MessagePack and answer in JS frames (see
// `@liliadb/client`).

import { createRequire } from "node:module";
import { copyFileSync, existsSync, statSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { connect, type Socket } from "node:net";
import { resolve } from "node:path";
import {
  LiliaError,
  abortable,
  bytesOf,
  encodeWire,
  expect,
  getUuid,
  readFrame,
  unwrap,
  type Operation,
  type ResponseValue,
  type Transport,
  type WireResult,
} from "@liliadb/client";

export type { Transport } from "@liliadb/client";

/** The protocol version this package speaks. */
export const PROTOCOL_MAJOR = 2;
export const PROTOCOL_MINOR = 0;
/** The preface of a JavaScript client's connection to the daemon. */
const JS_PREFACE = Buffer.from("LJS1", "ascii");
const MAX_FRAME_BYTES = 16 * 1024 * 1024;
/** Initial size of the buffer synchronous calls write their results into; it grows to fit. */
const SYNC_BUFFER_BYTES = 64 * 1024;

/** The value of the JS frame `frame` (from its start), its JSON decoded by Node. */
function nodeFrame<T>(frame: Buffer): { value: T; segment: (index: number) => Uint8Array } {
  const json = frame.readUInt32LE(0);
  return readFrame<T>(frame.toString("utf8", 4, 4 + json), frame.subarray(4 + json));
}

/** The result a JS frame holds, its byte strings in place. */
function frameResult(frame: Buffer): WireResult {
  const { value, segment } = nodeFrame<WireResult>(frame);
  return bytesOf(value, segment);
}

/** A native error whose message is the JSON of a protocol error, as a `LiliaError`. */
function nativeError(error: unknown): unknown {
  if (!(error instanceof Error)) return error;
  try {
    const shape = JSON.parse(error.message) as { code?: unknown; message?: unknown };
    if (typeof shape.code === "string" && typeof shape.message === "string") {
      return new LiliaError(shape as ConstructorParameters<typeof LiliaError>[0]);
    }
  } catch {
    return error;
  }
  return error;
}

interface NativeDatabaseHandle {
  execute(request: Uint8Array): Promise<Buffer>;
  /** Writes the frame into `out` and returns its length, or returns it in a new buffer. */
  executeSync(request: Uint8Array, out: Buffer): number | Buffer;
  close(timeoutMs?: number): Promise<void>;
}

export interface NativeOpenOptions {
  path: string;
  durability?: string;
  profile?: string;
  ledger?: boolean;
  historyRetentionMs?: number;
  undoDepth?: number;
}

export interface NativeModule {
  open(options: NativeOpenOptions): Promise<NativeDatabaseHandle>;
}

const require = createRequire(import.meta.url);
let loaded: NativeModule | undefined;

function nativePackagePlatform(): string {
  if (process.platform === "darwin") return `darwin-${process.arch}`;
  if (process.platform === "linux") return `linux-${process.arch}-gnu`;
  if (process.platform === "win32") return "win32-x64-msvc";
  return `${process.platform}-${process.arch}`;
}

/** `path` (relative to the working directory) as an addon path: a shared library loads as an addon only under a `.node` name. */
function addonPath(path: string): string {
  const library = resolve(path);
  if (library.endsWith(".node")) return library;
  const addon = `${library}.node`;
  if (!existsSync(addon) || statSync(library).mtimeMs > statSync(addon).mtimeMs) {
    copyFileSync(library, addon);
  }
  return addon;
}

/**
 * The native module: `LILIA_NATIVE_PATH`, else the platform package, else a build of this
 * checkout (release, then debug).
 */
export function loadNative(): NativeModule {
  if (loaded) return loaded;
  const explicit = process.env.LILIA_NATIVE_PATH;
  if (explicit) {
    loaded = require(addonPath(explicit)) as NativeModule;
    return loaded;
  }
  const platform = nativePackagePlatform();
  try {
    loaded = require(`@liliadb/node-${platform}`) as NativeModule;
    return loaded;
  } catch (error) {
    const library = process.platform === "win32" ? "lilia_node_native.dll"
      : process.platform === "darwin" ? "liblilia_node_native.dylib"
      : "liblilia_node_native.so";
    for (const profile of ["release", "debug"]) {
      const candidate = new URL(`../../../target/${profile}/${library}`, import.meta.url).pathname;
      if (existsSync(candidate)) {
        loaded = require(addonPath(candidate)) as NativeModule;
        return loaded;
      }
    }
    throw new Error(`No LiliaDB native build for ${platform}; set LILIA_NATIVE_PATH`, { cause: error });
  }
}

/** The native module, if one loads. */
export function tryLoadNative(): NativeModule | undefined {
  try {
    return loadNative();
  } catch {
    return undefined;
  }
}

/** A database opened in this process by the native module. */
export class NativeTransport implements Transport {
  /** Where synchronous calls write their results, reused from call to call. */
  private out = Buffer.allocUnsafe(SYNC_BUFFER_BYTES);

  private constructor(private readonly handle: NativeDatabaseHandle) {}

  static async open(options: NativeOpenOptions, signal?: AbortSignal): Promise<NativeTransport> {
    signal?.throwIfAborted();
    try {
      return new NativeTransport(await abortable(loadNative().open(options), signal));
    } catch (error) {
      throw nativeError(error);
    }
  }

  async execute(operation: Operation): Promise<ResponseValue> {
    let frame: Buffer;
    try {
      frame = await this.handle.execute(encodeWire(operation));
    } catch (error) {
      throw nativeError(error);
    }
    return unwrap(frameResult(frame));
  }

  executeSync(operation: Operation): ResponseValue {
    let written: number | Buffer;
    try {
      written = this.handle.executeSync(encodeWire(operation), this.out);
    } catch (error) {
      throw nativeError(error);
    }
    if (typeof written === "number") return unwrap(frameResult(this.out));
    this.out = Buffer.allocUnsafe(written.length * 2);
    return unwrap(frameResult(written));
  }

  async close(timeoutMs: number): Promise<void> {
    try {
      await this.handle.close(timeoutMs);
    } catch (error) {
      throw nativeError(error);
    }
  }
}

interface WireResponse {
  id: string;
  result: WireResult;
}

/** A database served by a daemon over its local socket (a named pipe on Windows). */
export class DaemonTransport implements Transport {
  private socket?: Socket;
  private connecting?: Promise<Socket>;
  private buffer = Buffer.alloc(0);
  private readonly pending = new Map<string, PromiseWithResolvers<WireResult>>();
  private closed = false;

  private constructor(
    private readonly endpoint: string,
    private readonly token: string
  ) {}

  /** Connects to the daemon of `endpoint` with the token of `tokenFile`, checking its protocol. */
  static async open(endpoint: string, tokenFile: string, signal?: AbortSignal): Promise<DaemonTransport> {
    signal?.throwIfAborted();
    const token = (await readFile(tokenFile, "utf8")).trim();
    const transport = new DaemonTransport(endpoint, token);
    try {
      const handshake = expect(
        await abortable(transport.execute({ type: "handshake", major: PROTOCOL_MAJOR, minor: PROTOCOL_MINOR }), signal),
        "handshake"
      );
      if (handshake.major !== PROTOCOL_MAJOR) {
        throw new LiliaError({
          code: "UNSUPPORTED",
          message: `the daemon speaks protocol ${handshake.major}, this package ${PROTOCOL_MAJOR}`,
          retryable: false,
        });
      }
    } catch (error) {
      await transport.close();
      throw error;
    }
    return transport;
  }

  async execute(operation: Operation): Promise<ResponseValue> {
    if (this.closed) {
      throw new LiliaError({ code: "CLOSED", message: "the daemon connection is closed", retryable: false });
    }
    const socket = await this.connection();
    const id = getUuid();
    const payload = encodeWire({ id, token: this.token, operation });
    if (payload.length > MAX_FRAME_BYTES) {
      throw new LiliaError({ code: "INVALID_INPUT", message: "the request exceeds the frame limit", retryable: false });
    }
    const header = Buffer.alloc(4);
    header.writeUInt32BE(payload.length);
    const response = Promise.withResolvers<WireResult>();
    this.pending.set(id, response);
    socket.write(Buffer.concat([header, payload]));
    try {
      return unwrap(await response.promise);
    } finally {
      this.pending.delete(id);
    }
  }

  async close(): Promise<void> {
    this.closed = true;
    const socket = this.socket;
    this.socket = undefined;
    if (!socket || socket.destroyed) return;
    const ended = Promise.withResolvers<void>();
    socket.once("close", () => ended.resolve());
    socket.end();
    await ended.promise;
  }

  /** The open connection; a new one when the daemon dropped the last (idle clients are). */
  private connection(): Promise<Socket> {
    if (this.socket && !this.socket.destroyed) return Promise.resolve(this.socket);
    this.connecting ??= new Promise<Socket>((resolve, reject) => {
      const socket = connect(this.endpoint);
      socket.once("error", reject);
      socket.once("connect", () => {
        socket.off("error", reject);
        socket.on("data", (chunk: Buffer) => this.receive(chunk));
        socket.on("error", (error) => this.fail(error));
        socket.on("close", () => this.fail(new LiliaError({
          code: "CLOSED",
          message: "the daemon closed the connection",
          retryable: true,
        })));
        socket.write(JS_PREFACE);
        this.socket = socket;
        this.buffer = Buffer.alloc(0);
        resolve(socket);
      });
    }).finally(() => {
      this.connecting = undefined;
    });
    return this.connecting;
  }

  private receive(chunk: Buffer): void {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    while (this.buffer.length >= 4) {
      const length = this.buffer.readUInt32BE(0);
      if (length === 0 || length > MAX_FRAME_BYTES) {
        this.fail(new LiliaError({ code: "PROTOCOL", message: "invalid frame length from the daemon", retryable: false }));
        this.socket?.destroy();
        return;
      }
      if (this.buffer.length < 4 + length) return;
      const { value: response, segment } = nodeFrame<WireResponse>(this.buffer.subarray(4, 4 + length));
      this.buffer = this.buffer.subarray(4 + length);
      this.pending.get(response.id)?.resolve(bytesOf(response.result, segment));
    }
  }

  /** Fails every request waiting on the connection. */
  private fail(error: unknown): void {
    for (const waiting of this.pending.values()) waiting.reject(error);
    this.pending.clear();
  }
}
