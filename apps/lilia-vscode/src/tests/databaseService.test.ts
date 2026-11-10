import { test, describe, before, after } from "node:test";
import * as assert from "node:assert";
import * as fs from "node:fs";
import * as path from "node:path";
import { LiliaDatabaseService } from "../connection/LiliaDatabaseService.js";

describe("LiliaDatabaseService Tests", () => {
  let repoRoot = process.cwd();
  while (!fs.existsSync(path.join(repoRoot, "knowledge_brain.lilia")) && path.dirname(repoRoot) !== repoRoot) {
    repoRoot = path.dirname(repoRoot);
  }
  const rootDb = path.resolve(repoRoot, "knowledge_brain.lilia");
  const tempDb = path.resolve(repoRoot, "temp_vscode_test.lilia");

  before(() => {
    if (fs.existsSync(rootDb)) {
      fs.copyFileSync(rootDb, tempDb);
    }
  });

  after(async () => {
    // Cleanup temp database and WAL files
    for (const file of [tempDb, `${tempDb}-wal`, `${tempDb}-shm`]) {
      if (fs.existsSync(file)) {
        try {
          fs.unlinkSync(file);
        } catch {
          // ignore
        }
      }
    }
  });

  test("can open real database in embedded mode and read metadata", async () => {
    const service = new LiliaDatabaseService();
    const info = await service.open({
      path: tempDb,
      mode: "embedded",
    });

    assert.strictEqual(info.connected, true);
    assert.strictEqual(info.mode, "embedded");
    assert.strictEqual(info.databaseName, "temp_vscode_test.lilia");

    const meta = await service.getMetadata();
    assert.strictEqual(meta["format_version"], "1");

    await service.close();
  });

  test("can inspect spaces, documents, and FTS in embedded mode", async () => {
    const service = new LiliaDatabaseService();
    await service.open({
      path: tempDb,
      mode: "embedded",
    });

    // Spaces summary
    const spaces = await service.getSpacesSummary();
    assert.ok(spaces.length >= 1);
    const notesSpace = spaces.find(s => s.space === "notes");
    assert.ok(notesSpace);
    assert.ok(notesSpace.count >= 1);

    // Scan documents
    const docs = await service.jsonScan("notes", undefined, 10);
    assert.ok(docs.length >= 1);
    const note1 = docs.find(d => d.id === "1");
    assert.ok(note1);
    assert.strictEqual((note1.value as { title?: string }).title, "One piece");

    // FTS search
    const ftsResults = await service.ftsSearch("notes_fts", "piece");
    assert.ok(ftsResults.length >= 1);
    assert.strictEqual(ftsResults[0]?.docId, "1");
    assert.ok(ftsResults[0]?.snippet.includes("<b>piece</b>"));

    await service.close();
  });

  test("can execute SQL queries", async () => {
    const service = new LiliaDatabaseService();
    await service.open({
      path: tempDb,
      mode: "embedded",
    });

    const res = await service.sqlQuery("SELECT space, id FROM _lilia_json WHERE space = ? ORDER BY id ASC", ["notes"]);
    assert.ok(res.records.length >= 1);
    assert.strictEqual((res.records[0] as { id?: string }).id, "1");

    await service.close();
  });

  test("can modify records: JSON put/delete, KV set/delete, and batch", async () => {
    const service = new LiliaDatabaseService();
    await service.open({
      path: tempDb,
      mode: "embedded",
    });

    // 1. JSON Put
    const putRes = await service.jsonPut("notes", "test_doc_99", { title: "VSCode Extension Test", active: true });
    assert.ok(putRes.version !== undefined);

    const doc = await service.jsonGet("notes", "test_doc_99");
    assert.ok(doc);
    assert.strictEqual((doc?.value as { title?: string }).title, "VSCode Extension Test");

    // 2. KV Set
    const kvRes = await service.kvSet("config", "theme", "dark");
    assert.ok(kvRes.version !== undefined);

    const kvEntry = await service.kvGet("config", "theme");
    assert.ok(kvEntry);
    assert.strictEqual(Buffer.from(kvEntry.value).toString("utf8"), "dark");

    // 3. Batch operation
    const batchRes = await service.batch([
      { model: "kv_set", namespace: "config", key: Buffer.from("lang"), value: Buffer.from("fr") },
      { model: "json_put", space: "notes", id: "test_doc_100", value: { title: "Batch Doc" } },
    ]);
    assert.strictEqual(batchRes.length, 2);

    // 4. Integrity check
    const ok = await service.integrityCheck();
    assert.strictEqual(ok, true);

    // 5. Deletions
    await service.jsonDelete("notes", "test_doc_99");
    const deletedDoc = await service.jsonGet("notes", "test_doc_99");
    assert.strictEqual(deletedDoc, undefined);

    await service.kvDelete("config", "theme");
    const deletedKv = await service.kvGet("config", "theme");
    assert.strictEqual(deletedKv, undefined);

    await service.close();
  });
});
