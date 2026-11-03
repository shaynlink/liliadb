#!/usr/bin/env node
import { execSync } from "node:child_process";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { existsSync } from "node:fs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const rootDir = resolve(__dirname, "..");

const env = {
  ...process.env,
  SQLITE_WASM_RS_FLAGS: "-DSQLITE_ENABLE_FTS5",
};

// On macOS, Apple Clang does not support wasm32-unknown-unknown target.
// Fallback to Homebrew LLVM clang if available and not explicitly configured.
if (!env.CC_wasm32_unknown_unknown) {
  if (existsSync("/opt/homebrew/opt/llvm/bin/clang")) {
    env.CC_wasm32_unknown_unknown = "/opt/homebrew/opt/llvm/bin/clang";
    env.AR_wasm32_unknown_unknown = "/opt/homebrew/opt/llvm/bin/llvm-ar";
  } else if (existsSync("/usr/local/opt/llvm/bin/clang")) {
    env.CC_wasm32_unknown_unknown = "/usr/local/opt/llvm/bin/clang";
    env.AR_wasm32_unknown_unknown = "/usr/local/opt/llvm/bin/llvm-ar";
  }
}

console.log("🔨 [1/3] Building Rust lilia-wasm cdylib (wasm32-unknown-unknown)...");
execSync(
  'cargo build --target wasm32-unknown-unknown --release -p lilia-wasm',
  {
    cwd: rootDir,
    stdio: "inherit",
    env,
  }
);

console.log("📦 [2/3] Generating WebAssembly JS bindings with wasm-bindgen...");
execSync(
  "mkdir -p packages/wasm/pkg && wasm-bindgen --target web --out-dir packages/wasm/pkg target/wasm32-unknown-unknown/release/lilia_wasm.wasm",
  {
    cwd: rootDir,
    stdio: "inherit",
  }
);

console.log("🚀 [3/3] Compiling TypeScript @liliadb/wasm...");
execSync("pnpm --filter @liliadb/wasm build", {
  cwd: rootDir,
  stdio: "inherit",
});

console.log("✅ @liliadb/wasm build completed successfully!");
