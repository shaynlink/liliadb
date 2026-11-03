import { Database } from "@liliadb/wasm";

console.log("🦕 Starting LiliaDB on Deno / Deno Deploy runtime...");

const db = await Database.open({
  path: ":memory:",
  vfs: "memory",
});

console.log("Database initialized in memory.");

// 1. JSON and Secondary Index
await db.json.put("users", "u-1", { name: "Grace Hopper", role: "admin" });
await db.json.put("users", "u-2", { name: "Ada Lovelace", role: "admin" });
await db.json.createIndex("users", "role_idx", "$.role");

const admins = await db.json.findByIndex("users", "role_idx", "admin");
console.log(`Found ${admins.length} admins via secondary index:`);
for (const admin of admins) {
  console.log(` - ${(admin.value as any).name} (${admin.id})`);
}

// 2. FTS5 Full-Text Search
await db.fts.index("docs", "doc-1", "Deno Deploy runs JavaScript and WebAssembly at the Edge");
const hits = await db.fts.search("docs", "WebAssembly");
console.log("FTS search hit:", hits[0]?.snippet);

// 3. SQL Query
const sql = await db.sql.query("SELECT sqlite_version() as version, 'deno' as runtime");
console.log("SQL query result:", sql[0]);

const ok = await db.integrityCheck();
console.log("Integrity check:", ok ? "PASSED" : "FAILED");

await db.close();
console.log("Database closed successfully.");
