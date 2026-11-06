#!/usr/bin/env node

/**
 * LiliaDB Knowledge Graph & Second Brain Software Executable
 *
 * Runs as:
 * - Interactive REPL console (when invoked without arguments)
 * - CLI tool (when invoked with subcommands: list, add, view, search, stats...)
 * - Automated Demo Runner (when invoked with --demo)
 */

import { KnowledgeStorage } from "../src/storage.mjs";
import { handleCliCommand } from "../src/cli.mjs";
import { runInteractiveConsole, seedDemoArticles } from "../src/interactive.mjs";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

async function main() {
  const args = process.argv.slice(2);
  const isDemo = args.includes("--demo");

  // Determine database path
  let tempDir = null;
  let dbPath = "./knowledge_brain.lilia";

  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--db" && args[i + 1]) {
      dbPath = args[i + 1];
      args.splice(i, 2);
      break;
    }
  }

  if (isDemo) {
    tempDir = await mkdtemp(join(tmpdir(), "liliadb-brain-demo-"));
    dbPath = join(tempDir, "demo_brain.lilia");
  }

  const storage = await KnowledgeStorage.open(dbPath);

  try {
    if (isDemo) {
      console.log("==================================================================");
      console.log("🌸 LiliaDB Second Brain & Knowledge Graph Software (Automated Demo)");
      console.log("==================================================================");

      console.log(`[1] Seeding knowledge articles with automatic [[wikilinks]] extraction...`);
      await seedDemoArticles(storage);

      console.log(`\n[2] Listing all indexed notes:`);
      await handleCliCommand(storage, "list", []);

      console.log(`\n[3] Searching full-text index with FTS5 BM25:`);
      await handleCliCommand(storage, "search", ["ACID", "durability"]);

      console.log(`\n[4] Querying backlinks pointing to [note_sqlite]:`);
      await handleCliCommand(storage, "backlinks", ["note_sqlite"]);

      console.log(`\n[5] Calculating concept reachability from [note_architecture]:`);
      await handleCliCommand(storage, "reachability", ["note_architecture"]);

      console.log(`\n[6] Testing collaborative lock (KV with TTL):`);
      await handleCliCommand(storage, "lock", ["note_architecture", "Alice"]);
      await handleCliCommand(storage, "unlock", ["note_architecture"]);

      console.log(`\n[7] Displaying SQL Analytics Dashboard:`);
      await handleCliCommand(storage, "stats", []);

      console.log(`\n[8] Verifying storage integrity...`);
      const ok = await storage.integrityCheck();
      console.log(`  Storage integrity: ${ok ? "PASS (Healthy WAL)" : "FAIL"}`);
      console.log(`\n[✓] Automated software demo completed successfully!`);
    } else if (args.length > 0) {
      const command = args[0];
      const cmdArgs = args.slice(1);
      await handleCliCommand(storage, command, cmdArgs);
    } else {
      await runInteractiveConsole(storage);
    }
  } finally {
    await storage.close();
    if (tempDir) {
      await rm(tempDir, { recursive: true, force: true });
    }
  }
}

main().catch((err) => {
  console.error("Execution error:", err);
  process.exit(1);
});
