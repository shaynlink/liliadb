import { Database } from "@liliadb/wasm";

let db: Database | null = null;

const outputEl = document.getElementById("output") as HTMLPreElement;
const statusEl = document.getElementById("db-status") as HTMLSpanElement;
const btnOpenOpfs = document.getElementById("btn-open-opfs") as HTMLButtonElement;
const btnOpenMemory = document.getElementById("btn-open-memory") as HTMLButtonElement;
const btnClose = document.getElementById("btn-close") as HTMLButtonElement;
const btnAddDoc = document.getElementById("btn-add-doc") as HTMLButtonElement;
const btnSearch = document.getElementById("btn-search") as HTMLButtonElement;
const btnRunSql = document.getElementById("btn-run-sql") as HTMLButtonElement;

const docTitleInput = document.getElementById("doc-title") as HTMLInputElement;
const docContentInput = document.getElementById("doc-content") as HTMLTextAreaElement;
const searchQueryInput = document.getElementById("search-query") as HTMLInputElement;
const sqlInput = document.getElementById("sql-input") as HTMLTextAreaElement;

function log(msg: string, data?: unknown) {
  const line = data ? `${msg}\n${JSON.stringify(data, null, 2)}` : msg;
  outputEl.textContent = `${new Date().toLocaleTimeString()} - ${line}\n\n${outputEl.textContent}`;
}

function updateButtons(connected: boolean) {
  btnOpenOpfs.disabled = connected;
  btnOpenMemory.disabled = connected;
  btnClose.disabled = !connected;
  btnAddDoc.disabled = !connected;
  btnSearch.disabled = !connected;
  btnRunSql.disabled = !connected;
  statusEl.textContent = connected ? "Connected" : "Not Connected";
  statusEl.style.background = connected ? "#065f46" : "#7f1d1d";
  statusEl.style.color = connected ? "#6ee7b7" : "#fca5a5";
}

btnOpenOpfs.addEventListener("click", async () => {
  try {
    log("Opening database in OPFS (mydb.lilia)...");
    db = await Database.open({
      path: "mydb.lilia",
      vfs: "opfs-sahpool",
    });
    const ok = await db.integrityCheck();
    log("Database opened successfully in OPFS! Integrity check:", ok);
    updateButtons(true);
  } catch (err: any) {
    log("Failed to open OPFS database:", err?.message ?? err);
  }
});

btnOpenMemory.addEventListener("click", async () => {
  try {
    log("Opening in-memory database (:memory:)...");
    db = await Database.open({
      path: ":memory:",
      vfs: "memory",
    });
    const ok = await db.integrityCheck();
    log("In-memory database opened successfully! Integrity check:", ok);
    updateButtons(true);
  } catch (err: any) {
    log("Failed to open memory database:", err?.message ?? err);
  }
});

btnClose.addEventListener("click", async () => {
  if (!db) return;
  try {
    log("Closing database...");
    await db.close();
    db = null;
    log("Database closed.");
    updateButtons(false);
  } catch (err: any) {
    log("Error closing database:", err?.message ?? err);
  }
});

btnAddDoc.addEventListener("click", async () => {
  if (!db) return;
  const title = docTitleInput.value.trim();
  const content = docContentInput.value.trim();
  if (!title || !content) {
    alert("Please enter both title and content");
    return;
  }
  const id = `doc-${Date.now()}`;
  try {
    // Atomic batch: store structured JSON + index FTS5 full-text
    const results = await db.batch([
      {
        model: "json_put",
        space: "articles",
        id,
        value: { title, createdAt: Date.now() },
      },
      {
        model: "fts_index",
        collection: "articles",
        docId: id,
        content: `${title} ${content}`,
      },
    ]);
    log(`Document ${id} stored and indexed atomically:`, results);
    docTitleInput.value = "";
    docContentInput.value = "";
  } catch (err: any) {
    log("Failed to add document:", err?.message ?? err);
  }
});

btnSearch.addEventListener("click", async () => {
  if (!db) return;
  const q = searchQueryInput.value.trim();
  if (!q) return;
  try {
    const hits = await db.fts.search("articles", q);
    log(`FTS search results for "${q}":`, hits);
  } catch (err: any) {
    log("FTS search error:", err?.message ?? err);
  }
});

btnRunSql.addEventListener("click", async () => {
  if (!db) return;
  const sql = sqlInput.value.trim();
  if (!sql) return;
  try {
    const rows = await db.sql.query(sql);
    log(`SQL Results:`, rows);
  } catch (err: any) {
    log("SQL error:", err?.message ?? err);
  }
});
