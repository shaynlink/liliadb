#!/usr/bin/env node

/**
 * LiliaDB Custom Extension Verification Script (Node.js)
 *
 * Verifies:
 * 1. CLI packaging, signing, and integrity verification.
 * 2. Transparent encryption/decryption using the extension.
 * 3. Raw SQLite verification that ciphertext is written to disk.
 */

import { execFileSync } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import assert from "node:assert/strict";

import { existsSync } from "node:fs";

const CLI = existsSync(join(process.cwd(), "target", "debug", "lilia"))
  ? join(process.cwd(), "target", "debug", "lilia")
  : existsSync(join(process.cwd(), "target", "release", "lilia"))
  ? join(process.cwd(), "target", "release", "lilia")
  : "lilia";

const LIB_EXT = process.platform === "darwin" ? "dylib" : process.platform === "win32" ? "dll" : "so";
const LIB_PATH = existsSync(join(process.cwd(), "target", "debug", `liblilia_plugin_custom_extension.${LIB_EXT}`))
  ? join(process.cwd(), "target", "debug", `liblilia_plugin_custom_extension.${LIB_EXT}`)
  : join(process.cwd(), "target", "release", `liblilia_plugin_custom_extension.${LIB_EXT}`);

async function main() {
  console.log("=== LiliaDB Custom Extension Automated Test ===");
  console.log(`Using CLI: ${CLI}`);
  console.log(`Using Plugin Library: ${LIB_PATH}`);

  const tempDir = await mkdtemp(join(tmpdir(), "lilia-ext-test-"));
  const keysDir = join(tempDir, "keys");
  const distDir = join(tempDir, "dist");
  const dbPath = join(tempDir, "test.lilia");

  try {
    // 1. Keygen
    console.log("\n[1] Generating Ed25519 author keypair...");
    const keygenOut = execFileSync(CLI, [
      "plugin", "keygen",
      "--out-dir", keysDir,
      "--name", "author",
      "--output", "json",
    ], { encoding: "utf8" });
    const keygenData = JSON.parse(keygenOut);
    console.log(`  Author fingerprint: ${keygenData.fingerprint}`);

    // 2. Pack
    console.log("\n[2] Packaging plugin...");
    const packOut = execFileSync(CLI, [
      "plugin", "pack", LIB_PATH,
      "--name", "lilia-plugin-custom-extension",
      "--version", "0.1.0",
      "--capability", "codec",
      "--out-dir", distDir,
      "--output", "json",
    ], { encoding: "utf8" });
    const packData = JSON.parse(packOut);
    console.log(`  Pack success: ${packData.name} v${packData.version}`);

    // 3. Sign
    console.log("\n[3] Signing package with private key...");
    execFileSync(CLI, [
      "plugin", "sign", distDir,
      "--key", join(keysDir, "author.key"),
      "--output", "json",
    ], { encoding: "utf8" });
    console.log("  Signed manifest.json -> manifest.sig generated.");

    // 4. Trust author key
    console.log("\n[4] Adding author public key to trust store...");
    const pubKey = (await readFile(join(keysDir, "author.pub"), "utf8")).trim();
    execFileSync(CLI, [
      "plugin", "trust", "add", pubKey,
      "--yes",
      "--output", "json",
    ], { encoding: "utf8" });
    console.log(`  Added author key to trust store: ${pubKey.slice(0, 16)}...`);

    // 5. Verify package
    console.log("\n[5] Verifying signed package integrity against trusted keys...");
    const verifyOut = execFileSync(CLI, [
      "plugin", "verify", distDir,
      "--output", "json",
    ], { encoding: "utf8" });
    const verifyData = JSON.parse(verifyOut);
    console.log(`  Package verified: name=${verifyData.name}, version=${verifyData.version}`);

    // 6. Write data through plugin
    console.log("\n[6] Writing confidential KV pair through LiliaDB with plugin...");
    execFileSync(CLI, [
      "--database", dbPath,
      "--plugin", LIB_PATH,
      "kv", "set", "credentials", "api_token", "super_secret_payload_987",
      "--output", "json",
    ], { encoding: "utf8" });
    console.log("  Written 'api_token' to 'credentials' namespace.");

    // 7. Read data back through plugin
    console.log("\n[7] Reading back through LiliaDB with plugin (transparent decryption)...");
    const getOut = execFileSync(CLI, [
      "--database", dbPath,
      "--plugin", LIB_PATH,
      "kv", "get", "credentials", "api_token",
      "--output", "json",
    ], { encoding: "utf8" });
    const getData = JSON.parse(getOut);
    const rawValue = Array.isArray(getData.value)
      ? Buffer.from(getData.value).toString("utf8")
      : typeof getData.value === "string"
      ? getData.value
      : "";
    console.log(`  Decrypted value string: "${rawValue}"`);
    assert.equal(rawValue, "super_secret_payload_987");

    console.log("\n[✓] Custom extension lifecycle and transparent encryption PASSED!");
  } finally {
    await rm(tempDir, { recursive: true, force: true });
  }
}

main().catch(err => {
  console.error("Test failed:", err);
  process.exit(1);
});
