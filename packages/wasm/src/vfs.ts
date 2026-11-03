import type { WasmVfs } from "./types.js";

export function isBrowser(): boolean {
  return typeof window !== "undefined" && typeof window.document !== "undefined";
}

export function isDedicatedWorker(): boolean {
  return (
    typeof self !== "undefined" &&
    typeof (self as unknown as { WorkerGlobalScope?: unknown }).WorkerGlobalScope !== "undefined" &&
    typeof (self as unknown as { importScripts?: unknown }).importScripts === "function"
  );
}

export function isOpfsAvailable(): boolean {
  if (typeof navigator !== "undefined" && navigator.storage && typeof navigator.storage.getDirectory === "function") {
    // In dedicated worker, sync access handle is required for SQLite SAHPool
    if (isDedicatedWorker()) {
      return typeof (globalThis as unknown as { FileSystemSyncAccessHandle?: unknown }).FileSystemSyncAccessHandle !== "undefined";
    }
    return true;
  }
  return false;
}

export function resolveVfs(path: string, explicitVfs?: WasmVfs): string {
  if (explicitVfs === "memory") return "memory";
  if (explicitVfs === "opfs" || explicitVfs === "opfs-sahpool") return "opfs-sahpool";

  if (path === ":memory:" || path.includes(":memory:")) {
    return "memory";
  }

  const vfsMatch = path.match(/[?&]vfs=([^&]+)/);
  if (vfsMatch && vfsMatch[1]) {
    return vfsMatch[1];
  }

  if (isOpfsAvailable()) {
    return "opfs-sahpool";
  }

  return "memory";
}
