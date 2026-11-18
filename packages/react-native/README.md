# `@liliadb/react-native`

> Local-first multimodel embedded database for **React Native** and **Expo**.
> Key/Value, JSON documents, Graph, Full-Text Search (FTS5) and SQL in a single
> SQLite ACID transaction boundary, with optional AES-256-GCM encryption backed
> by the iOS Keychain / Android Keystore.

The module exposes the raw `LiliaDatabase` engine, an automatic synchronous
C++ JSI fast path (`db.sync`), a full set of reactive React hooks, and 20
idiomatic mobile components (lists, forms, search, graph explorer, storage and
security) so you can build offline-first screens with zero boilerplate.

---

## Features

- **Multimodel**: KV, JSON, Graph, FTS5 and declarative SQL share one database file.
- **Zero-FOUC reads**: synchronous JSI access (`db.sync`) hydrates values on the
  very first render, with an async fallback when JSI is unavailable.
- **Live reactivity**: table/namespace/space/document subscriptions drive hooks
  and components without polling.
- **Hardware encryption**: `useSecureStorage` generates and stores a 256-bit
  AES-GCM key in the Keychain / Keystore; passphrases and raw keys are supported too.
- **Mobile-aware sandboxing**: picks the right storage directory per platform and
  avoids Google Drive / iCloud backup pitfalls by default.
- **Resource profiles**: tune SQLite cache/WAL/mmap for phones, wearables or servers.
- **React lifecycle safe**: automatic WAL flush on background, Fast Refresh
  singleton to avoid `SQLITE_BUSY`, and provider-managed open/close.
- **Expo & bare React Native**: autolinking + an Expo config plugin, with every
  optional integration (`@shopify/flash-list`, React Navigation, haptics,
  biometrics) loaded lazily.

---

## Requirements

| Requirement | Version |
| --- | --- |
| `react` | `>=18.0.0` |
| `react-native` | `>=0.71.0` |
| iOS | `>=14.0` |
| Android | `minSdkVersion 21` |

The native core ships as a Swift Package / XCFramework on Apple platforms and as
the `android-sdk` Kotlin/JNI module on Android.

---

## Installation

```sh
pnpm add @liliadb/react-native
# or
npm install @liliadb/react-native
# or
yarn add @liliadb/react-native
```

### Bare React Native

Autolinking picks up `react-native.config.js`, the podspec and the Android
package automatically.

```sh
# iOS
cd ios && pod install

# Android: the module is linked by the React Native Gradle plugin.
```

The iOS podspec links `Lilia.xcframework` from `release/apple/` and requires the
`packages/android-sdk` Gradle project on Android. Ensure those build artifacts
are present (they are produced by the mobile build tooling under `tools/mobile`).

### Expo

Add the config plugin to `app.json` / `app.config.js`:

```json
{
  "expo": {
    "plugins": ["@liliadb/react-native"]
  }
}
```

Then run a prebuild and reinstall native dependencies:

```sh
npx expo prebuild --clean
```

> The config plugin is resolved from `app.plugin.js`. It is a no-op verification
> step today; native linking is handled by Expo autolinking and prebuild.

### Optional integrations

These packages are **not** required. They are loaded dynamically and each
feature degrades gracefully when absent:

| Package | Enables |
| --- | --- |
| `@shopify/flash-list` | `LiliaFlashList` uses FlashList instead of FlatList |
| `@react-navigation/native` | `refreshOnFocus` hook option |
| `expo-haptics` | Haptic feedback in `LiliaOptimisticButton` |
| `expo-local-authentication` | `LiliaBiometricLock` |

---

## Quick start

```tsx
import React from 'react';
import { Text, View } from 'react-native';
import { LiliaProvider, useKv, useSqlQuery } from '@liliadb/react-native';

function SettingsScreen() {
  const [theme, setTheme] = useKv<string>('app_settings', 'theme', {
    defaultValue: 'light',
  });

  return (
    <View>
      <Text>Theme: {theme}</Text>
      <Text onPress={() => setTheme(theme === 'light' ? 'dark' : 'light')}>
        Toggle
      </Text>
    </View>
  );
}

function Inbox() {
  const { data, isLoading } = useSqlQuery<{ id: string; title: string }>(
    'SELECT id, title FROM messages ORDER BY id DESC',
    [],
    { tables: ['messages'] },
  );

  if (isLoading) return <Text>Loading…</Text>;
  return data.map((row) => <Text key={row.id}>{row.title}</Text>);
}

export default function App() {
  return (
    <LiliaProvider options={{ name: 'app', useSecureStorage: true }}>
      <SettingsScreen />
      <Inbox />
    </LiliaProvider>
  );
}
```

---

## Opening a database

### `LiliaDatabase.open(options)`

```ts
import { LiliaDatabase } from '@liliadb/react-native';

const db = await LiliaDatabase.open({
  name: 'app',
  profile: 'mobile',
  useSecureStorage: true,
});
```

| Option | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `string` | `'app_data'` | Logical name; resolves the optimal sandbox path per OS. |
| `path` | `string` | — | Explicit absolute path (alternative to `name`). |
| `profile` | `'mobile' \| 'constrained' \| 'server'` | `'mobile'` | Cache/WAL/mmap tuning (see below). |
| `useSecureStorage` | `boolean` | `false` | Generate/retrieve a 256-bit AES-GCM key from Keychain / Keystore. |
| `encryptionKey` | `Uint8Array \| string` | — | Manual 32-byte key or 64-character hex string. |
| `passphrase` | `string` | — | Derive the key with PBKDF2-HMAC-SHA256. |
| `autoFlushOnBackground` | `boolean` | `true` | Flush the WAL when the app leaves the foreground. |
| `ios` | `IosDatabaseConfig` | — | iOS sandbox tuning. |
| `android` | `AndroidDatabaseConfig` | — | Android sandbox tuning. |
| `dev` | `{ enableFastRefreshSingleton?: boolean }` | `true` | Reuse open instances across Fast Refresh cycles. |

#### Resource profiles

| Profile | Cache | mmap | Target |
| --- | --- | --- | --- |
| `mobile` | 8 MB | WAL optimized | Phones, tablets, Android TV |
| `constrained` | 2 MB | disabled | watchOS, Wear OS, tvOS |
| `server` | 64 MB | 256 MB | Desktop / server workloads |

#### iOS sandbox (`ios`)

| Option | Type | Default | Description |
| --- | --- | --- | --- |
| `directory` | `'applicationSupport' \| 'documents' \| 'caches'` | `'applicationSupport'` | Persistent (not purgeable) location by default. |
| `excludeFromCloudBackup` | `boolean` | `true` | Exclude large databases from iCloud backup. |
| `groupContainerIdentifier` | `string` | — | App Group container (WidgetKit, App Clips, watchOS). |

#### Android sandbox (`android`)

| Option | Type | Default | Description |
| --- | --- | --- | --- |
| `directory` | `'noBackup' \| 'files' \| 'cache'` | `'noBackup'` | `noBackup` avoids consuming the 25 MB Google Drive backup quota. |

### Sandbox path resolution

`resolveDatabasePath(options)` is exported if you need to inspect the resolved
location before opening:

```ts
import { resolveDatabasePath } from '@liliadb/react-native';

resolveDatabasePath({ name: 'app' });
// iOS:     ios://applicationSupport/app.lilia
// Android: android://noBackup/app.lilia

resolveDatabasePath({
  name: 'widget',
  ios: { groupContainerIdentifier: 'group.com.example.app' },
});
// group://group.com.example.app/widget.lilia
```

### Fast Refresh singleton

In development, reopening the same path reuses the live instance (unless it was
closed) to avoid `SQLITE_BUSY: database is locked` during hot reloads. Disable
with `dev: { enableFastRefreshSingleton: false }`.

---

## Providers and context

### `<LiliaProvider>`

Two mutually exclusive modes:

```tsx
// 1. Provide an already-open instance
<LiliaProvider database={db}>
  <App />
</LiliaProvider>

// 2. Let the provider open and own the lifecycle
<LiliaProvider
  options={{ name: 'app', useSecureStorage: true }}
  fallback={<SplashScreen />}
  errorFallback={(error) => <ErrorScreen error={error} />}
>
  <App />
</LiliaProvider>
```

In auto-managed mode the provider closes the database when it unmounts.

### Accessing the instance

```tsx
import { useDatabase, useLilia, LiliaConsumer } from '@liliadb/react-native';

// Hook — throws if there is no provider and no explicit instance.
const db = useDatabase();

// Alias
const same = useLilia();

// Render-prop
<LiliaConsumer>
  {(db) => <Text>{db.path}</Text>}
</LiliaConsumer>;
```

Every hook/component also accepts an explicit `db` prop or option to work
outside a provider.

---

## Database API

`LiliaDatabase` exposes two parallel surfaces: an **async** API (thread pool,
non-blocking) and a **synchronous** JSI API (`db.sync`, sub-millisecond). Results
are identical; pick async for normal work and `sync` for first-paint reads.

### Async multimodel API

```ts
const db = await LiliaDatabase.open({ name: 'app' });

// Key/Value
const version = await db.kv.set('cache', 'token', 'xyz123', Date.now() + 3_600_000);
const entry = await db.kv.get('cache', 'token');
// entry: { namespace, key: Uint8Array, value: Uint8Array, version, expiresAtMs? }
await db.kv.delete('cache', 'token');

// JSON documents
await db.json.put('users', 'ada', { name: 'Ada', role: 'admin' });
const doc = await db.json.get<{ name: string }>('users', 'ada');
// doc: { space, id, value, version } | null
const page = await db.json.find('users', { role: 'admin' }, /* afterId */ undefined, 50);
await db.json.delete('users', 'ada');

// Graph
await db.graph.setNode('social', 'ada', 'User', { name: 'Ada' });
await db.graph.addEdge('social', 'ada', 'grace', 'FOLLOWS');
const node = await db.graph.getNode('social', 'ada');
const neighbors = await db.graph.neighbors('social', 'ada', 'out', 'FOLLOWS');
const reachable = await db.graph.reachability('social', ['ada'], 'out');

// Full-text search
const hits = await db.fts.search('articles', 'local-first database', 20);
// hits: { collection, docId, snippet, rank }[]

// SQL
const { columns, rows, rowsAffected } = await db.sql.query<{ id: string }>(
  'SELECT id FROM lilia_json WHERE space = ?1',
  ['users'],
);
await db.sql.mutate('UPDATE messages SET read = 1', [], ['messages']);
```

| Object | Methods |
| --- | --- |
| `db.kv` | `get`, `set`, `delete` |
| `db.json` | `get`, `put`, `delete`, `find` |
| `db.graph` | `setNode`, `getNode`, `addEdge`, `neighbors`, `reachability` |
| `db.fts` | `search` |
| `db.sql` | `query`, `mutate` |
| `db.maintenance` | `vacuum`, `checkpoint`, `getFileSize`, `backup` |

> `db.json.find(space, query, afterId?, limit = 50)` performs cursor pagination
> using `afterId`. `sql.mutate(sql, params, tablesToInvalidate)` emits table
> change events after the write so hooks refresh automatically.

### Synchronous JSI API (`db.sync`)

```ts
db.sync.kv.set('auth', 'jwt', 'token-abc');
const token = db.sync.kv.get('auth', 'jwt'); // string | null
db.sync.kv.peek('auth', 'jwt');             // cache-only read
db.sync.kv.delete('auth', 'jwt');

db.sync.json.put('users', 'u1', { name: 'Grace' });
const doc = db.sync.json.get<{ name: string }>('users', 'u1');
const found = db.sync.json.find('users', { name: 'Grace' });

db.sync.graph.setNode('social', 'alice', 'user', { bio: 'Engineer' });
db.sync.graph.addEdge('social', 'alice', 'bob', 'knows');
const neighbors = db.sync.graph.neighbors('social', 'alice', 'out');
db.sync.graph.deleteNode('social', 'alice');

const hits = db.sync.fts.search('docs', 'quantum');
const result = db.sync.sql.query<{ id: string }>('SELECT id FROM lilia_json');
db.sync.maintenance.vacuum();
db.sync.maintenance.checkpoint();
const size = db.sync.maintenance.getFileSize();
```

If no JSI host object is available, `db.sync.kv` falls back to an in-memory
cache and the graph/JSON/SQL helpers return empty results — always prefer the
async API for correctness outside the embedded JSI runtime.

### Reactivity

```ts
const unsubscribe = db.subscribe('table:messages', () => {
  // re-fetch or invalidate
});

db.notifyChange({ type: 'table', table: 'messages' });
unsubscribe();
```

Supported subscription patterns:

| Pattern | Fires on |
| --- | --- |
| `*` | every event |
| `kv:<namespace>` | any key in the namespace |
| `kv:<namespace>:<key>` | a specific key |
| `json:<space>` | any document in the space |
| `json:<space>:<id>` | a specific document |
| `table:<table>` | an explicit table invalidation |
| `tag:<tag>` | a custom event |

Change events are expressed as:

```ts
type DatabaseChangeEvent =
  | { type: 'kv'; namespace: string; key: string; op: 'set' | 'delete' }
  | { type: 'json'; space: string; id: string; op: 'put' | 'delete' }
  | { type: 'graph'; graph: string; op: 'node' | 'edge' }
  | { type: 'fts'; collection: string }
  | { type: 'table'; table: string }
  | { type: 'custom'; tag: string };
```

### Lifecycle & maintenance

```ts
await db.prepareForBackground(); // flush WAL before the OS suspends the app
await db.maintenance.checkpoint('truncate');
await db.maintenance.vacuum();
const metrics = await db.maintenance.getFileSize();
// { mainBytes, walBytes, shmBytes, totalBytes }
await db.maintenance.backup(`${db.path}.backup.db`);
await db.close(5_000); // idempotent, bounded drain
```

`autoFlushOnBackground` (default `true`) calls `prepareForBackground()` whenever
the app transitions to `background` or `inactive`. Checkpoint modes are
`'passive' | 'full' | 'restart' | 'truncate'`.

---

## Hooks

### `useLiliaDatabase(options)`

Opens a database from options, closes it on unmount, and returns
`{ db, isLoading, error }`.

```tsx
const { db, isLoading, error } = useLiliaDatabase({ name: 'reports' });
```

### `useKv` / `useKvState`

Live Key/Value value with synchronous first read. Accepts two overloads:

```tsx
useKv<T>(namespace, key, options?);
useKv<T>(db, namespace, key, options?);
```

It returns a tuple that also carries named metadata:

```tsx
const [theme, setTheme, meta] = useKv<string>('app_settings', 'theme', {
  defaultValue: 'light',
  refreshOnFocus: true,
});

meta.value;     // current value
meta.isLoading; // true while hydrating
meta.error;     // Error | null
meta.remove();  // delete the key
meta.reload();  // force an async re-read
```

`setValue` supports updater functions and an optional TTL:

```ts
await setTheme((prev) => (prev === 'light' ? 'dark' : 'light'));
await setTheme('dark', Date.now() + 86_400_000);
```

Options: `{ defaultValue, serialize, deserialize, refreshOnFocus, db }`.
Values are JSON (de)serialized by default; strings are stored verbatim unless a
custom `serialize`/`deserialize` is provided.

For a simple `[value, setValue]` pair:

```tsx
const [draft, setDraft] = useKvState('notes', 'draft', '');
```

### `useDocument` / `useJson`

Live JSON document with optimistic updates. `useJson` is an alias.

```tsx
useDocument<T>(space, id, options?);
useDocument<T>(db, space, id, options?);

const [user, update, meta] = useDocument<{ name: string; active: boolean }>(
  'users',
  'ada',
  { defaultValue: { name: '', active: false }, refreshOnFocus: true },
);

await update({ active: true });              // shallow merge
await update((prev) => ({ ...prev!, name: 'Ada Lovelace' }));
meta.remove();
meta.reload();
```

Options: `{ defaultValue, refreshOnFocus, db }`.

### `useGraphNeighbors`

```tsx
const { neighbors, isLoading, error, reload } = useGraphNeighbors(
  'social',
  'ada',
  { direction: 'out', kindFilter: 'FOLLOWS', refreshOnFocus: true },
);
```

Options: `{ direction: 'in' | 'out' | 'both', kindFilter, refreshOnFocus, db }`.

### `useFts`

Debounced full-text search.

```tsx
const { hits, isSearching, error } = useFts('articles', query, {
  limit: 20,
  debounceMs: 200,
});
```

Options: `{ limit (default 20), debounceMs (default 200), refreshOnFocus, db }`.
Empty/whitespace queries short-circuit to an empty result set.

### `useSqlQuery`

Live SQL with optional table subscriptions.

```tsx
const { data, columns, isLoading, isRefetching, error, refetch } =
  useSqlQuery<{ id: string; title: string }>(
    'SELECT id, title FROM messages WHERE read = ?1',
    [0],
    { tables: ['messages'], deps: [filter] },
  );
```

Second overload binds an explicit database:

```ts
useSqlQuery<T>(db, sql, params?, deps?);
```

Options: `{ tables, refreshOnFocus, enabled, deps, db }`. `data` and `rows` are
the same array. Tables in `tables` are observed via `table:<name>` events.

### `useMutation`

TanStack-Query-style mutations with automatic invalidation.

```tsx
const { mutate, mutateAsync, isPending, error, data, reset } = useMutation(
  async (vars: { id: string; title: string }, db) => {
    await db.json.put('messages', vars.id, vars);
    return vars.id;
  },
  {
    invalidateTables: ['messages'],
    invalidateSpaces: ['messages'],
    invalidateKeys: [{ namespace: 'counters', key: 'messages' }],
    onSuccess: (id) => console.log('saved', id),
    onError: (err) => console.warn(err),
    onSettled: () => console.log('done'),
  },
);
```

Options: `{ onSuccess, onError, onSettled, invalidateTables, invalidateKeys, invalidateSpaces, db }`.

### `useDatabaseMaintenance`

```tsx
const { metrics, isLoading, error, vacuum, checkpoint, refresh } =
  useDatabaseMaintenance();

await checkpoint('truncate');
await vacuum();
```

Returns `{ metrics, isLoading, error, vacuum, checkpoint, refresh }` where
`metrics: StorageMetrics | null`.

---

## Built-in components

All components accept a `db` prop/option and otherwise resolve the database from
`<LiliaProvider>`. They render `loadingComponent`, `emptyComponent` and
`errorComponent` when supplied.

### Lists

| Component | Description |
| --- | --- |
| `LiliaFlatList` | FlatList bound to an observed SQL query with pull-to-refresh. |
| `LiliaFlashList` | FlashList-backed list; falls back to FlatList when not installed. |
| `LiliaSectionList` | Transforms flat SQL rows into grouped sections via `groupBy`. |
| `LiliaInfiniteList` | Cursor-paginated infinite scroll over a JSON space. |

```tsx
<LiliaFlatList
  sql="SELECT id, title FROM messages ORDER BY id DESC"
  tables={['messages']}
  renderItem={({ item }) => <Text>{item.title}</Text>}
  loadingComponent={<ActivityIndicator />}
  emptyComponent={<Text>No messages</Text>}
/>
```

`LiliaFlatList` props: `sql`, `sqlParams`, `tables`, `data`, `renderItem`,
`loadingComponent`, `emptyComponent`, `errorComponent`, `db`, plus FlatList props.
The default key extractor looks for `id`, `rowid`, then `key`.

`LiliaFlashList` additionally requires `estimatedItemSize`.

`LiliaSectionList` requires `sql` and `groupBy: (item) => string`; optional
`sortSections`, `renderSectionHeader`, plus SectionList props.

`LiliaInfiniteList` props: `space`, `pageSize` (default 20), `filter`, `renderItem`
over `JsonEntry<T>`, `loadingComponent`, `emptyComponent`, `errorComponent`, `db`,
and `onEndReachedThreshold` (default 0.5).

### Key/Value

| Component | Props |
| --- | --- |
| `LiliaKvSwitch` | `namespace`, `storageKey`, `defaultValue`, `onValueChange`, `db` + Switch props |
| `LiliaKvInput` | `namespace`, `storageKey`, `defaultValue`, `debounceMs` (300), `db` + TextInput props |

```tsx
<LiliaKvSwitch namespace="prefs" storageKey="dark_mode" defaultValue={false} />
<LiliaKvInput namespace="notes" storageKey="draft" debounceMs={300} placeholder="Draft…" />
```

### Documents & forms

| Component | Description |
| --- | --- |
| `LiliaDocumentView` | Render-prop view observing a single JSON document. |
| `LiliaForm` | Declarative form container with dirty tracking and atomic commit. |

```tsx
<LiliaDocumentView space="users" id="ada">
  {({ doc, update, remove }) =>
    doc ? (
      <View>
        <Text>{doc.name as string}</Text>
        <Button title="Activate" onPress={() => update({ active: true })} />
        <Button title="Delete" onPress={remove} />
      </View>
    ) : null
  }
</LiliaDocumentView>
```

`LiliaDocumentView` children receive `{ doc, isLoading, error, update, remove, reload }`.

```tsx
<LiliaForm
  space="users"
  id="ada"
  initialValues={{ name: 'Ada', active: false }}
  onSubmit={async (values) => validate(values)}
  onSuccess={() => navigation.goBack()}
>
  {({ values, setFieldValue, isDirty, isSubmitting, submit, reset }) => (
    <View>
      <TextInput value={values.name} onChangeText={(v) => setFieldValue('name', v)} />
      <Button title="Save" onPress={submit} disabled={!isDirty || isSubmitting} />
      <Button title="Reset" onPress={reset} />
    </View>
  )}
</LiliaForm>
```

`FormHelpers<T>`: `values`, `setFieldValue`, `setValues`, `isDirty`,
`isSubmitting`, `isLoading`, `error`, `submit`, `reset`.

### Feedback & state

| Component | Props |
| --- | --- |
| `LiliaCountBadge` | `sql`, `sqlParams`, `tables`, `count`, `maxDisplay` (99), `hideZero`, `badgeStyle`, `textStyle`, `db` |
| `LiliaErrorBoundary` | `fallback(error, retry)`, `onError(error, info)`, `children` |
| `LiliaSuspense` | `fallback`, `isReady`, `isLoading`, `children` |
| `LiliaOptimisticButton` | `onPress`, `title`, `children`, `haptic`, `style`, `textStyle`, `disabled`, `loadingComponent` |

`LiliaCountBadge` reads the first column of the first SQL row (e.g.
`SELECT COUNT(*) AS total FROM messages`) or a static `count`.

### Full-text search

| Component | Props |
| --- | --- |
| `LiliaSearchBar` | `collection`, `onSearch`, `limit` (20), `debounceMs` (250), `placeholder`, `containerStyle`, `inputStyle`, `onClear`, `db` |
| `LiliaHighlightSnippet` | `snippet`, `highlightStyle`, `textStyle` + Text props |

```tsx
<LiliaSearchBar collection="articles" onSearch={setHits} />
<LiliaHighlightSnippet snippet={hit.snippet} />
```

`LiliaHighlightSnippet` parses SQLite FTS5 `<b>…</b>` tags and styles matched
terms.

### Graph

| Component | Props |
| --- | --- |
| `LiliaGraphExplorer` | `graph`, `rootNode`, `direction`, `kindFilter`, `renderNode(nodeId, isCurrent)`, `onNodePress`, `containerStyle`, `db` |
| `LiliaGraphBreadcrumbs` | `path`, `onNodePress(nodeId, index)`, `separator` (`›`), style props |

`LiliaGraphExplorer` keeps an interactive traversal history with back/reset
controls.

### Security

| Component | Props |
| --- | --- |
| `LiliaBiometricLock` | `promptMessage`, `fallbackComponent`, `autoPrompt` (true), `containerStyle`, `children` |
| `LiliaSecureField` | `value`, `maskChar` (`•`), `autoHideTimeoutMs` (8000), style props, `renderToggleIcon`, `onReveal` |

`LiliaBiometricLock` uses `expo-local-authentication` when available and unlocks
gracefully when biometrics are unavailable. `LiliaSecureField` masks values with
a one-tap reveal and auto-hide timer.

### Storage & maintenance

| Component | Props |
| --- | --- |
| `LiliaStorageBar` | `showVacuumButton`, `showCheckpointButton`, `barHeight`, `style`, `db`, `onMaintenanceComplete` |
| `LiliaBackupShareButton` | `title`, `fileName`, `buttonStyle`, `textStyle`, `onBackupComplete`, `onError`, `db` |

`LiliaStorageBar` visualizes main / WAL / SHM distribution and exposes Vacuum and
Checkpoint actions. `LiliaBackupShareButton` performs a hot consistent snapshot
and opens the native share sheet.

### DevTools

| Component | Props |
| --- | --- |
| `LiliaDevInspector` | `enabled` (defaults to `__DEV__`), `position`, `db` |

Floating inspector with an on-device SQL console (JSI-accelerated), live storage
telemetry, and KV peek/save.

---

## Types

The package exports all public types:

- Database: `OpenDatabaseOptions`, `ResourceProfile`, `IosDatabaseConfig`,
  `AndroidDatabaseConfig`, `StorageMetrics`, `CheckpointMode`.
- Models: `KvEntry`, `JsonEntry<T>`, `GraphNode<T>`, `GraphEdge`, `GraphDirection`,
  `FtsHit`, `SqlQueryResult<T>`.
- Reactivity: `DatabaseChangeEvent`.
- Hooks: `UseKvOptions`, `UseKvMeta`, `UseKvReturn`, `UseDocumentOptions`,
  `UseDocumentMeta`, `UseDocumentReturn`, `UseSqlQueryOptions`,
  `UseSqlQueryReturn`, `UseMutationOptions`, `UseMutationReturn`, `UseFtsOptions`,
  `UseGraphNeighborsOptions`, `UseDatabaseMaintenanceReturn`.
- Components: `LiliaProviderProps`, `LiliaFlatListProps`, `LiliaFlashListProps`,
  `LiliaSectionListProps`, `LiliaSectionData`, `LiliaInfiniteListProps`,
  `LiliaKvSwitchProps`, `LiliaKvInputProps`, `LiliaDocumentViewProps`,
  `LiliaFormProps`, `FormHelpers`, `LiliaCountBadgeProps`,
  `LiliaErrorBoundaryProps`, `LiliaSuspenseProps`, `LiliaOptimisticButtonProps`,
  `LiliaSearchBarProps`, `LiliaHighlightSnippetProps`, `LiliaGraphExplorerProps`,
  `LiliaGraphBreadcrumbsProps`, `LiliaBiometricLockProps`,
  `LiliaSecureFieldProps`, `LiliaStorageBarProps`, `LiliaBackupShareButtonProps`,
  `LiliaDevInspectorProps`.

---

## Troubleshooting

**`Native module not found`**
Ensure `pod install` ran on iOS and the Android module is linked (autolinking or
Expo prebuild). Verify the `Lilia.xcframework` / `android-sdk` artifacts exist.

**`SQLITE_BUSY: database is locked`**
Usually caused by two instances on the same path during Fast Refresh. The dev
singleton is enabled by default; avoid calling `LiliaDatabase.open` with the same
path from multiple providers in production, or disable the singleton.

**Hooks render with default values first**
This happens when the JSI host object is unavailable and there is no warm cache;
the async fetch fills in shortly after. First-paint zero-FOUC requires the JSI
runtime.

**`No LiliaDatabase instance found`**
Wrap the tree in `<LiliaProvider>` or pass an explicit `db` to the hook.

**Biometrics unlock immediately**
`expo-local-authentication` is missing or reports no enrolled hardware. The
component unlocks rather than locking users out.

---

## License

Apache-2.0
