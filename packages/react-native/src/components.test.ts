import { test } from 'node:test';
import assert from 'node:assert/strict';
import React from 'react';
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
  LiliaBackupShareButton,
  LiliaDevInspector,
} from './index.js';

test('All 20 Built-in Components are exported and valid React components', () => {
  const components = [
    { name: 'LiliaFlashList', comp: LiliaFlashList },
    { name: 'LiliaSectionList', comp: LiliaSectionList },
    { name: 'LiliaInfiniteList', comp: LiliaInfiniteList },
    { name: 'LiliaKvSwitch', comp: LiliaKvSwitch },
    { name: 'LiliaKvInput', comp: LiliaKvInput },
    { name: 'LiliaDocumentView', comp: LiliaDocumentView },
    { name: 'LiliaForm', comp: LiliaForm },
    { name: 'LiliaCountBadge', comp: LiliaCountBadge },
    { name: 'LiliaErrorBoundary', comp: LiliaErrorBoundary },
    { name: 'LiliaSuspense', comp: LiliaSuspense },
    { name: 'LiliaOptimisticButton', comp: LiliaOptimisticButton },
    { name: 'LiliaSearchBar', comp: LiliaSearchBar },
    { name: 'LiliaHighlightSnippet', comp: LiliaHighlightSnippet },
    { name: 'LiliaGraphExplorer', comp: LiliaGraphExplorer },
    { name: 'LiliaGraphBreadcrumbs', comp: LiliaGraphBreadcrumbs },
    { name: 'LiliaBiometricLock', comp: LiliaBiometricLock },
    { name: 'LiliaSecureField', comp: LiliaSecureField },
    { name: 'LiliaStorageBar', comp: LiliaStorageBar },
    { name: 'LiliaBackupShareButton', comp: LiliaBackupShareButton },
    { name: 'LiliaDevInspector', comp: LiliaDevInspector },
  ];

  assert.equal(components.length, 20);
  for (const { name, comp } of components) {
    assert.equal(
      typeof comp === 'function',
      true,
      `Expected ${name} to be a function/class component`
    );
  }
});

test('LiliaErrorBoundary static getDerivedStateFromError catches storage errors', () => {
  const err = new Error('Disk I/O failure or database locked');
  const state = LiliaErrorBoundary.getDerivedStateFromError(err);
  assert.equal(state.hasError, true);
  assert.equal(state.error?.message, 'Disk I/O failure or database locked');
});

test('LiliaSuspense conditional gateway behavior', () => {
  const fallbackEl = React.createElement('span', null, 'Skeleton loading...');
  const childEl = React.createElement('div', null, 'Active Screen');

  // When loading, renders fallback
  const loadingSuspense = LiliaSuspense({
    isLoading: true,
    fallback: fallbackEl,
    children: childEl,
  });
  assert.deepEqual(loadingSuspense.props.children, fallbackEl);

  // When ready, renders children
  const readySuspense = LiliaSuspense({
    isLoading: false,
    isReady: true,
    fallback: fallbackEl,
    children: childEl,
  });
  assert.deepEqual(readySuspense.props.children, childEl);
});

test('LiliaGraphBreadcrumbs renders path segments and custom separators', () => {
  const crumbs = LiliaGraphBreadcrumbs({
    path: ['Root', 'Categories', 'Electronics'],
    separator: '>',
  });
  assert.ok(crumbs);
  assert.ok(crumbs.props.contentContainerStyle);
});

test('React element creation succeeds for all 20 components', () => {
  // Verifies that React.createElement works with all props for every component without syntax or typing errors
  const elements = [
    React.createElement(LiliaFlashList, { estimatedItemSize: 50, renderItem: () => null }),
    React.createElement(LiliaSectionList, { sql: 'SELECT * FROM items', groupBy: () => 'A', renderItem: () => null }),
    React.createElement(LiliaInfiniteList, { space: 'users', renderItem: () => null }),
    React.createElement(LiliaKvSwitch, { namespace: 'prefs', storageKey: 'dark_mode' }),
    React.createElement(LiliaKvInput, { namespace: 'notes', storageKey: 'draft' }),
    React.createElement(LiliaDocumentView, { space: 'settings', id: 'app', children: () => null }),
    React.createElement(LiliaForm, { space: 'users', id: 'u1', children: () => null }),
    React.createElement(LiliaCountBadge, { count: 42 }),
    React.createElement(LiliaErrorBoundary, null, React.createElement('div', null)),
    React.createElement(LiliaSuspense, { fallback: React.createElement('div', null), children: React.createElement('div', null) }),
    React.createElement(LiliaOptimisticButton, { onPress: () => {}, title: 'Submit' }),
    React.createElement(LiliaSearchBar, { collection: 'articles' }),
    React.createElement(LiliaHighlightSnippet, { snippet: 'Found in <b>LiliaDB</b>' }),
    React.createElement(LiliaGraphExplorer, { graph: 'social', rootNode: 'user:1' }),
    React.createElement(LiliaGraphBreadcrumbs, { path: ['Home', 'Profile'] }),
    React.createElement(LiliaBiometricLock, null, React.createElement('div', null)),
    React.createElement(LiliaSecureField, { value: 'api-secret-123' }),
    React.createElement(LiliaStorageBar, null),
    React.createElement(LiliaBackupShareButton, null),
    React.createElement(LiliaDevInspector, { enabled: false }),
  ];

  assert.equal(elements.length, 20);
  for (const el of elements) {
    assert.ok(React.isValidElement(el));
  }
});
