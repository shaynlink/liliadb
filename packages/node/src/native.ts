// The Node-API module: `open` and the handle of a database, which runs MessagePack operations
// and answers in JS frames. `@liliadb/node/native` exports it for tools and tests that drive the
// engine below the client.

import { createRequire } from "node:module";
import { copyFileSync, existsSync, statSync } from "node:fs";
import { resolve } from "node:path";

export interface NativeDatabaseHandle {
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
    const library =
      process.platform === "win32"
        ? "lilia_node_native.dll"
        : process.platform === "darwin"
          ? "liblilia_node_native.dylib"
          : "liblilia_node_native.so";
    for (const profile of ["release", "debug"]) {
      const candidate = new URL(`../../../target/${profile}/${library}`, import.meta.url).pathname;
      if (existsSync(candidate)) {
        loaded = require(addonPath(candidate)) as NativeModule;
        return loaded;
      }
    }
    throw new Error(`No LiliaDB native build for ${platform}; set LILIA_NATIVE_PATH`, {
      cause: error,
    });
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
