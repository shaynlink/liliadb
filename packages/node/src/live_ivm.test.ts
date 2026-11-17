import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Database } from "./index.js";
import type { LiveQueryDiff } from "./types.js";

async function withDb(fn: (db: Database) => Promise<void>) {
  const dir = await mkdtemp(join(tmpdir(), "lilia-live-ivm-"));
  const dbPath = join(dir, "test.db");
  const db = await Database.open({ path: dbPath });
  try {
    await fn(db);
  } finally {
    await db.close();
    await rm(dir, { recursive: true, force: true });
  }
}

test("Live Queries: differential updates { added, updated, removed } on SQL table mutations", async () => {
  await withDb(async (db) => {
    // 1. Create table
    await db.sql.raw(`
      CREATE TABLE tasks (
        id TEXT PRIMARY KEY,
        title TEXT NOT NULL,
        status TEXT NOT NULL,
        priority INTEGER NOT NULL
      );
    `);

    // Insert 2 initial rows: 1 matching, 1 not matching
    await db.sql.raw("INSERT INTO tasks VALUES ('T1', 'Write tests', 'pending', 1);");
    await db.sql.raw("INSERT INTO tasks VALUES ('T2', 'Deploy app', 'done', 2);");

    const diffs: LiveQueryDiff[] = [];
    let resolveNextDiff: ((diff: LiveQueryDiff) => void) | null = null;

    const waitForDiff = (): Promise<LiveQueryDiff> => {
      return new Promise((resolve) => {
        resolveNextDiff = resolve;
      });
    };

    // 2. Subscribe to live query
    const sub = await db.liveQuery(
      "SELECT id, title, status, priority FROM tasks WHERE status = 'pending';",
      (diff) => {
        diffs.push(diff);
        if (resolveNextDiff) {
          const fn = resolveNextDiff;
          resolveNextDiff = null;
          fn(diff);
        }
      }
    );

    // Initial snapshot should be emitted synchronously/immediately
    assert.strictEqual(diffs.length, 1);
    const initial = diffs[0]!;
    assert.strictEqual(initial.isInitial, true);
    assert.strictEqual(initial.added.length, 1);
    assert.strictEqual((initial.added[0] as any).id, "T1");

    // 3. INSERT matching row T3 (status = 'pending') -> emits added: [T3]
    let nextPromise = waitForDiff();
    await db.sql.raw("INSERT INTO tasks VALUES ('T3', 'Review PR', 'pending', 3);");
    let d = await nextPromise;
    assert.strictEqual(d.isInitial, false);
    assert.strictEqual(d.added.length, 1);
    assert.strictEqual((d.added[0] as any).id, "T3");
    assert.strictEqual(d.updated.length, 0);
    assert.strictEqual(d.removed.length, 0);

    // 4. INSERT non-matching row T4 (status = 'draft') -> no diff emitted
    await db.sql.raw("INSERT INTO tasks VALUES ('T4', 'Draft RFC', 'draft', 1);");
    await new Promise((r) => setTimeout(r, 50));
    assert.strictEqual(diffs.length, 2);

    // 5. UPDATE T1 priority within pending status -> emits updated: [T1]
    nextPromise = waitForDiff();
    await db.sql.raw("UPDATE tasks SET priority = 10, title = 'Write tests urgent' WHERE id = 'T1';");
    d = await nextPromise;
    assert.strictEqual(d.added.length, 0);
    assert.strictEqual(d.updated.length, 1);
    assert.strictEqual((d.updated[0] as any).id, "T1");
    assert.strictEqual((d.updated[0] as any).priority, 10);
    assert.strictEqual((d.updated[0] as any).title, "Write tests urgent");
    assert.strictEqual(d.removed.length, 0);

    // 6. UPDATE T1 status to 'done' -> transitions out of view -> emits removed: [T1]
    nextPromise = waitForDiff();
    await db.sql.raw("UPDATE tasks SET status = 'done' WHERE id = 'T1';");
    d = await nextPromise;
    assert.strictEqual(d.added.length, 0);
    assert.strictEqual(d.updated.length, 0);
    assert.strictEqual(d.removed.length, 1);
    assert.strictEqual((d.removed[0] as any).id, "T1");

    // 7. UPDATE T4 status to 'pending' -> transitions into view -> emits added: [T4]
    nextPromise = waitForDiff();
    await db.sql.raw("UPDATE tasks SET status = 'pending' WHERE id = 'T4';");
    d = await nextPromise;
    assert.strictEqual(d.added.length, 1);
    assert.strictEqual((d.added[0] as any).id, "T4");
    assert.strictEqual(d.updated.length, 0);
    assert.strictEqual(d.removed.length, 0);

    // 8. DELETE T3 -> emits removed: [T3]
    nextPromise = waitForDiff();
    await db.sql.raw("DELETE FROM tasks WHERE id = 'T3';");
    d = await nextPromise;
    assert.strictEqual(d.added.length, 0);
    assert.strictEqual(d.updated.length, 0);
    assert.strictEqual(d.removed.length, 1);
    assert.strictEqual((d.removed[0] as any).id, "T3");

    // 9. Unsubscribe and verify no more events are received
    sub.unsubscribe();
    await db.sql.raw("INSERT INTO tasks VALUES ('T5', 'Ignored after unsub', 'pending', 5);");
    await new Promise((r) => setTimeout(r, 50));
    assert.strictEqual(diffs.length, 6);
  });
});

test("Incremental Materialized Views (IVM): O(1) group-by and global aggregates", async () => {
  await withDb(async (db) => {
    // 1. Create source table
    await db.sql.raw(`
      CREATE TABLE orders (
        id TEXT PRIMARY KEY,
        status TEXT NOT NULL,
        amount INTEGER NOT NULL
      );
    `);

    // 2. Define incremental materialized view with GROUP BY
    await db.createMaterializedView(
      "order_stats",
      "SELECT status, count(*) AS total_orders, sum(amount) AS total_revenue FROM orders GROUP BY status;"
    );

    // Verify catalog listing
    const views = await db.listMaterializedViews();
    assert.strictEqual(views.length, 1);
    const view0 = views[0]!;
    assert.strictEqual(view0.name, "order_stats");
    assert.strictEqual(view0.sourceTable, "orders");
    assert.strictEqual(view0.groupBy, "status");

    // 3. Insert rows into orders
    await db.sql.raw("INSERT INTO orders VALUES ('O1', 'paid', 100);");
    await db.sql.raw("INSERT INTO orders VALUES ('O2', 'paid', 250);");
    await db.sql.raw("INSERT INTO orders VALUES ('O3', 'refunded', 50);");

    // 4. Query the materialized view directly
    interface OrderStatsRow {
      status: string;
      total_orders: number;
      total_revenue: number;
    }
    let stats = await db.sql.query<OrderStatsRow>(
      "SELECT * FROM order_stats ORDER BY status ASC;"
    );
    assert.strictEqual(stats.length, 2);
    assert.deepStrictEqual(stats[0]!, { status: "paid", total_orders: 2, total_revenue: 350 });
    assert.deepStrictEqual(stats[1]!, { status: "refunded", total_orders: 1, total_revenue: 50 });

    // 5. UPDATE an order's amount: O(1) trigger update
    await db.sql.raw("UPDATE orders SET amount = 300 WHERE id = 'O2';");
    stats = await db.sql.query<OrderStatsRow>(
      "SELECT * FROM order_stats WHERE status = 'paid';"
    );
    assert.strictEqual(stats.length, 1);
    assert.strictEqual(stats[0]!.total_orders, 2);
    assert.strictEqual(stats[0]!.total_revenue, 400);

    // 6. DELETE an order: O(1) trigger update
    await db.sql.raw("DELETE FROM orders WHERE id = 'O3';");
    stats = await db.sql.query<OrderStatsRow>(
      "SELECT * FROM order_stats WHERE status = 'refunded';"
    );
    assert.strictEqual(stats.length, 0);

    // 7. Global aggregate view (no GROUP BY) created via native SQL syntax
    await db.sql.raw(
      "CREATE MATERIALIZED VIEW global_stats AS SELECT count(*) AS count, sum(amount) AS revenue, avg(amount) AS avg_rev FROM orders;"
    );

    const globalViews = await db.listMaterializedViews();
    assert.strictEqual(globalViews.length, 2);

    interface GlobalStatsRow {
      count: number;
      revenue: number;
      avg_rev: number;
    }
    const globalStats = await db.sql.query<GlobalStatsRow>(
      "SELECT * FROM global_stats;"
    );
    assert.strictEqual(globalStats.length, 1);
    assert.strictEqual(globalStats[0]!.count, 2);
    assert.strictEqual(globalStats[0]!.revenue, 400);
    assert.strictEqual(globalStats[0]!.avg_rev, 200);

    // 8. Drop view via DDL syntax
    await db.sql.raw("DROP MATERIALIZED VIEW order_stats;");
    const afterDrop = await db.listMaterializedViews();
    assert.strictEqual(afterDrop.length, 1);
    assert.strictEqual(afterDrop[0]!.name, "global_stats");
  });
});

test("Live Queries on JSON collections: differential updates with json_extract predicates", async () => {
  await withDb(async (db) => {
    const diffs: LiveQueryDiff[] = [];
    let resolveNextDiff: ((diff: LiveQueryDiff) => void) | null = null;

    const waitForDiff = (): Promise<LiveQueryDiff> => {
      return new Promise((resolve) => {
        resolveNextDiff = resolve;
      });
    };

    // 1. Subscribe to live query on JSON space 'tickets' with priority >= 3
    const sub = await db.liveQuery(
      "SELECT id, value FROM lilia_json WHERE space = 'tickets' AND json_extract(value, '$.priority') >= 3;",
      (diff) => {
        diffs.push(diff);
        if (resolveNextDiff) {
          const fn = resolveNextDiff;
          resolveNextDiff = null;
          fn(diff);
        }
      }
    );

    // Initial snapshot: empty
    assert.strictEqual(diffs.length, 1);
    assert.strictEqual(diffs[0]!.isInitial, true);
    assert.strictEqual(diffs[0]!.added.length, 0);

    // 2. Put low-priority ticket (priority 1) -> not matching -> no diff
    await db.json.put("tickets", "TICK-1", { title: "Minor typo", priority: 1 });
    await new Promise((r) => setTimeout(r, 50));
    assert.strictEqual(diffs.length, 1);

    // 3. Put high-priority ticket (priority 5) -> matching -> emits added: [TICK-2]
    let nextPromise = waitForDiff();
    await db.json.put("tickets", "TICK-2", { title: "Production outage", priority: 5 });
    let d = await nextPromise;
    assert.strictEqual(d.isInitial, false);
    assert.strictEqual(d.added.length, 1);
    assert.strictEqual((d.added[0] as any).id, "TICK-2");
    assert.strictEqual(d.updated.length, 0);
    assert.strictEqual(d.removed.length, 0);

    // 4. Update TICK-1 to priority 4 -> transitions into view -> emits added: [TICK-1]
    nextPromise = waitForDiff();
    await db.json.put("tickets", "TICK-1", { title: "Minor typo elevated", priority: 4 });
    d = await nextPromise;
    assert.strictEqual(d.added.length, 1);
    assert.strictEqual((d.added[0] as any).id, "TICK-1");

    // 5. Update TICK-2 title keeping priority 5 -> remains in view -> emits updated: [TICK-2]
    nextPromise = waitForDiff();
    await db.json.put("tickets", "TICK-2", { title: "Production outage resolved", priority: 5 });
    d = await nextPromise;
    assert.strictEqual(d.added.length, 0);
    assert.strictEqual(d.updated.length, 1);
    assert.strictEqual((d.updated[0] as any).id, "TICK-2");

    // 6. Delete TICK-2 -> emits removed: [TICK-2]
    nextPromise = waitForDiff();
    await db.json.delete("tickets", "TICK-2");
    d = await nextPromise;
    assert.strictEqual(d.added.length, 0);
    assert.strictEqual(d.updated.length, 0);
    assert.strictEqual(d.removed.length, 1);
    assert.strictEqual((d.removed[0] as any).id, "TICK-2");

    sub.unsubscribe();
  });
});

test("Live Queries on Incremental Materialized Views", async () => {
  await withDb(async (db) => {
    await db.sql.raw(`
      CREATE TABLE metric_events (
        id TEXT PRIMARY KEY,
        service TEXT NOT NULL,
        errors INTEGER NOT NULL
      );
    `);

    // Create IVM
    await db.createMaterializedView(
      "service_errors",
      "SELECT service, count(*) AS total_events, sum(errors) AS total_errors FROM metric_events GROUP BY service;"
    );

    const diffs: LiveQueryDiff[] = [];
    let resolveNextDiff: ((diff: LiveQueryDiff) => void) | null = null;
    const waitForDiff = (): Promise<LiveQueryDiff> => {
      return new Promise((resolve) => {
        resolveNextDiff = resolve;
      });
    };

    // Subscribe to live query on the materialized view
    const sub = await db.liveQuery(
      "SELECT service, total_events, total_errors FROM service_errors;",
      (diff) => {
        diffs.push(diff);
        if (resolveNextDiff) {
          const fn = resolveNextDiff;
          resolveNextDiff = null;
          fn(diff);
        }
      }
    );

    assert.strictEqual(diffs.length, 1);
    assert.strictEqual(diffs[0]!.isInitial, true);
    assert.strictEqual(diffs[0]!.added.length, 0);

    // Insert into source table -> triggers update IVM backing table -> emits live query diff
    const nextPromise = waitForDiff();
    await db.sql.raw("INSERT INTO metric_events VALUES ('E1', 'auth', 5);");
    const d = await nextPromise;
    assert.strictEqual(d.added.length, 1);
    assert.strictEqual((d.added[0] as any).service, "auth");
    assert.strictEqual((d.added[0] as any).total_events, 1);
    assert.strictEqual((d.added[0] as any).total_errors, 5);

    sub.unsubscribe();
  });
});
