import { test } from "node:test";
import assert from "node:assert/strict";
import React from "react";
import {
  LiliaFlashList,
  LiliaSectionList,
  LiliaInfiniteList,
  LiliaKvSwitch,
  LiliaKvInput,
  LiliaDocumentView,
  LiliaForm,
  LiliaCountBadge,
  LiliaErrorBoundary,
  LiliaSuspense,
  LiliaOptimisticButton,
  LiliaSearchBar,
  LiliaHighlightSnippet,
  LiliaGraphExplorer,
  LiliaGraphBreadcrumbs,
  LiliaBiometricLock,
  LiliaSecureField,
  LiliaStorageBar,
  LiliaDevInspector,
} from "./index.js";
import { splitSnippet } from "./components/fts/LiliaHighlightSnippet.js";

test("All 19 Built-in Components are exported and valid React components", () => {
  const components = [
    { name: "LiliaFlashList", comp: LiliaFlashList },
    { name: "LiliaSectionList", comp: LiliaSectionList },
    { name: "LiliaInfiniteList", comp: LiliaInfiniteList },
    { name: "LiliaKvSwitch", comp: LiliaKvSwitch },
    { name: "LiliaKvInput", comp: LiliaKvInput },
    { name: "LiliaDocumentView", comp: LiliaDocumentView },
    { name: "LiliaForm", comp: LiliaForm },
    { name: "LiliaCountBadge", comp: LiliaCountBadge },
    { name: "LiliaErrorBoundary", comp: LiliaErrorBoundary },
    { name: "LiliaSuspense", comp: LiliaSuspense },
    { name: "LiliaOptimisticButton", comp: LiliaOptimisticButton },
    { name: "LiliaSearchBar", comp: LiliaSearchBar },
    { name: "LiliaHighlightSnippet", comp: LiliaHighlightSnippet },
    { name: "LiliaGraphExplorer", comp: LiliaGraphExplorer },
    { name: "LiliaGraphBreadcrumbs", comp: LiliaGraphBreadcrumbs },
    { name: "LiliaBiometricLock", comp: LiliaBiometricLock },
    { name: "LiliaSecureField", comp: LiliaSecureField },
    { name: "LiliaStorageBar", comp: LiliaStorageBar },
    { name: "LiliaDevInspector", comp: LiliaDevInspector },
  ];

  assert.equal(components.length, 19);
  for (const { name, comp } of components) {
    assert.equal(
      typeof comp === "function",
      true,
      `Expected ${name} to be a function/class component`,
    );
  }
});

test("LiliaErrorBoundary static getDerivedStateFromError catches storage errors", () => {
  const err = new Error("Disk I/O failure or database locked");
  const state = LiliaErrorBoundary.getDerivedStateFromError(err);
  assert.equal(state.hasError, true);
  assert.equal(state.error?.message, "Disk I/O failure or database locked");
});

test("LiliaSuspense conditional gateway behavior", () => {
  const fallbackEl = React.createElement("span", null, "Skeleton loading...");
  const childEl = React.createElement("div", null, "Active Screen");

  // When loading, renders fallback
  const loadingSuspense = LiliaSuspense({
    isLoading: true,
    fallback: fallbackEl,
    children: childEl,
  });
  assert.deepEqual((loadingSuspense.props as { children: unknown }).children, fallbackEl);

  // When ready, renders children
  const readySuspense = LiliaSuspense({
    isLoading: false,
    isReady: true,
    fallback: fallbackEl,
    children: childEl,
  });
  assert.deepEqual((readySuspense.props as { children: unknown }).children, childEl);
});

test("LiliaGraphBreadcrumbs renders path segments and custom separators", () => {
  const crumbs = LiliaGraphBreadcrumbs({
    path: ["Root", "Categories", "Electronics"],
    separator: ">",
  });
  assert.ok(crumbs);
  assert.ok((crumbs.props as { contentContainerStyle?: unknown }).contentContainerStyle);
});

test("LiliaHighlightSnippet parses engine markers without a backtracking regex", () => {
  assert.deepEqual(splitSnippet("Found in <b>LiliaDB</b> today"), [
    { text: "Found in ", isHighlight: false, offset: 0 },
    { text: "LiliaDB", isHighlight: true, offset: 9 },
    { text: " today", isHighlight: false, offset: 23 },
  ]);
  assert.deepEqual(splitSnippet("Unclosed <b>marker"), [
    { text: "Unclosed ", isHighlight: false, offset: 0 },
    { text: "<b>marker", isHighlight: false, offset: 9 },
  ]);
});

test("React element creation succeeds for all 19 components", () => {
  // Verifies that React.createElement works with all props for every component without syntax or typing errors
  const elements = [
    React.createElement(LiliaFlashList, { estimatedItemSize: 50, renderItem: () => null }),
    React.createElement(LiliaSectionList, {
      sql: "SELECT * FROM items",
      groupBy: () => "A",
      renderItem: () => null,
    }),
    React.createElement(LiliaInfiniteList, { space: "users", renderItem: () => null }),
    React.createElement(LiliaKvSwitch, { namespace: "prefs", storageKey: "dark_mode" }),
    React.createElement(LiliaKvInput, { namespace: "notes", storageKey: "draft" }),
    // A render prop: `createElement` types its children arguments as nodes, not functions.
    // oxlint-disable-next-line react/no-children-prop
    React.createElement(LiliaDocumentView, { space: "settings", id: "app", children: () => null }),
    // A render prop: `createElement` types its children arguments as nodes, not functions.
    // oxlint-disable-next-line react/no-children-prop
    React.createElement(LiliaForm, { space: "users", id: "u1", children: () => null }),
    React.createElement(LiliaCountBadge, { count: 42 }),
    React.createElement(LiliaErrorBoundary, null, React.createElement("div", null)),
    React.createElement(LiliaSuspense, {
      fallback: React.createElement("div", null),
      // A required prop: `createElement` checks the props without its children arguments.
      // oxlint-disable-next-line react/no-children-prop
      children: React.createElement("div", null),
    }),
    React.createElement(LiliaOptimisticButton, { onPress: () => {}, title: "Submit" }),
    React.createElement(LiliaSearchBar, { collection: "articles" }),
    React.createElement(LiliaHighlightSnippet, { snippet: "Found in <b>LiliaDB</b>" }),
    React.createElement(LiliaGraphExplorer, { graph: "social", rootNode: "user:1" }),
    React.createElement(LiliaGraphBreadcrumbs, { path: ["Home", "Profile"] }),
    React.createElement(LiliaBiometricLock, null, React.createElement("div", null)),
    React.createElement(LiliaSecureField, { value: "api-secret-123" }),
    React.createElement(LiliaStorageBar, null),
    React.createElement(LiliaDevInspector, { enabled: false }),
  ];

  assert.equal(elements.length, 19);
  for (const el of elements) {
    assert.ok(React.isValidElement(el));
  }
});
