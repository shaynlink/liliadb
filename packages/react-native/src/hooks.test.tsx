import assert from "node:assert/strict";
import test from "node:test";
import { act } from "react";
import { create, type ReactTestRenderer } from "react-test-renderer";
import { LiliaDatabase, LiliaProvider, useDocument, useFts, useKv, useSqlQuery } from "./index.js";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let opened = 0;
const open = () => LiliaDatabase.open({ name: `hooks-${++opened}`, durability: "performance" });

/** Renders `use` inside a provider of `db`; `latest()` is what it returned last. */
async function render<T>(
  db: LiliaDatabase,
  use: () => T,
): Promise<{ latest: () => T; renders: T[]; renderer: ReactTestRenderer }> {
  const renders: T[] = [];
  function Probe(): null {
    renders.push(use());
    return null;
  }
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(
      <LiliaProvider database={db}>
        <Probe />
      </LiliaProvider>,
    );
  });
  return { latest: () => renders[renders.length - 1]!, renders, renderer };
}

/** Lets the reads the hooks started finish, until `check` holds. */
async function until(check: () => boolean): Promise<void> {
  for (let i = 0; i < 200 && !check(); i++) {
    await act(() => new Promise((resolve) => setTimeout(resolve, 5)));
  }
  assert.ok(check(), "the hook never showed the expected state");
}

test("useKv shows the stored value on its first render and follows writes", async () => {
  const db = await open();
  await db.kv.set("settings", "theme", JSON.stringify("dark"));
  const view = await render(db, () =>
    useKv<string>("settings", "theme", { defaultValue: "light" }),
  );
  assert.equal(view.renders[0]![0], "dark", "read synchronously, before any effect");
  assert.equal(view.renders[0]!.isLoading, false);

  await act(() => view.latest()[1]("blue"));
  assert.equal(view.latest().value, "blue");
  assert.equal(
    JSON.parse(new TextDecoder().decode((await db.kv.get("settings", "theme"))!.value)),
    "blue",
  );

  await db.kv.set("settings", "theme", JSON.stringify("green"));
  await until(() => view.latest().value === "green");

  await act(() => view.latest().remove());
  assert.equal(view.latest().value, "light");
  await act(async () => view.renderer.unmount());
  await db.close();
});

test("useDocument merges partial updates and follows other writers", async () => {
  const db = await open();
  await db.json.put("users", "u1", { name: "Ada", age: 36 });
  const view = await render(db, () => useDocument<{ name: string; age: number }>("users", "u1"));
  assert.deepEqual(view.renders[0]!.data, { name: "Ada", age: 36 });

  await act(() => view.latest().update({ age: 37 }));
  assert.deepEqual((await db.json.get("users", "u1"))?.value, { name: "Ada", age: 37 });

  await db.json.put("users", "u1", { name: "Grace", age: 85 });
  await until(() => view.latest().data?.name === "Grace");
  await act(async () => view.renderer.unmount());
  await db.close();
});

test("useSqlQuery runs again after writes to its tables", async () => {
  const db = await open();
  await db.json.put("users", "u1", { name: "Ada" });
  const view = await render(db, () =>
    useSqlQuery<{ id: string }>(
      "SELECT id FROM lilia_json WHERE space = $1 ORDER BY id",
      ["users"],
      { tables: ["lilia_json"] },
    ),
  );
  await until(() => view.latest().data.length === 1);
  await db.json.put("users", "u2", { name: "Grace" });
  await until(
    () =>
      view
        .latest()
        .data.map((row) => row.id)
        .join() === "u1,u2",
  );

  const reads = view.renders.length;
  await db.kv.set("other", "k", "v");
  await act(() => new Promise((resolve) => setTimeout(resolve, 20)));
  assert.equal(view.renders.length, reads, "a write to another table runs nothing");
  await act(async () => view.renderer.unmount());
  await db.close();
});

test("useFts searches once the query settles", async () => {
  const db = await open();
  await db.fts.index("notes", "n1", "local first storage");
  const view = await render(db, () => useFts("notes", "storage", { debounceMs: 0 }));
  await until(() => view.latest().hits[0]?.docId === "n1" && !view.latest().isSearching);
  await act(async () => view.renderer.unmount());
  await db.close();
});
