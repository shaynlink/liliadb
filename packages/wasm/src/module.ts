// The WebAssembly module (crates/lilia-wasm, built into pkg/ by tools/wasm/build.sh).

import init, { Database as ModuleDatabase } from "../pkg/lilia_wasm.js";

export { ModuleDatabase };

/** Where the module comes from: its URL, a fetch `Response`, its bytes, or a compiled module. */
export type ModuleSource = URL | string | Response | BufferSource | WebAssembly.Module;

let loading: Promise<void> | undefined;

/**
 * Loads the module, once per thread: from `source`, else from `pkg/` next to this package
 * (which needs a `fetch` of `file:` URLs outside the browser: give the bytes there).
 */
export function loadModule(source?: ModuleSource): Promise<void> {
  loading ??= init({
    module_or_path: source ?? new URL("../pkg/lilia_wasm_bg.wasm", import.meta.url),
  }).then(
    () => undefined,
    (error: unknown) => {
      loading = undefined;
      throw error;
    },
  );
  return loading;
}
