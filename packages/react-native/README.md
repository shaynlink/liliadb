# `@liliadb/react-native`

> Local-first multimodel database for **React Native**: key/value, JSON documents,
> graph, full-text search, vectors, time series, geo, CRDTs and read-only SQL, on the
> native LiliaDB engine, called straight from JavaScript through JSI.

`LiliaDatabase` has every call of the LiliaDB clients (the same API as
`@liliadb/node`, from `@liliadb/client`), asynchronous on native worker threads or
synchronous through `db.sync`. On top of it: React hooks that read synchronously on
their first render and follow the writes that change what they show, and 19
idiomatic mobile components.

---

## How it works

- A **TurboModule** (`LiliaReactNative`) installs a JSI binding,
  `globalThis.__liliadb`, when React Native creates it.
- The binding calls the engine's **C ABI** (`lilia_c_open`, `lilia_c_execute`,
  `lilia_c_close`): each call is one MessagePack operation, answered by a JS frame,
  one JSON text (parsed by Hermes' native `JSON.parse`) and the raw bytes it refers
  to, handed to JavaScript as an `ArrayBuffer` over the engine's own memory.
- Asynchronous calls run on a pool of native threads and settle their promise back
  on the JavaScript thread; `db.sync` runs on the JavaScript thread itself.
- Each successful write notifies the **topics** it changed (a namespace and key, a
  space and document, a graph, the SQL tables...), which the hooks subscribe to.

---

## Requirements

| Requirement | Version |
| --- | --- |
| `react` | `>=19.0.0` |
| `react-native` | `>=0.76.0`, New Architecture |
| iOS | React Native's minimum |
| Android | `minSdkVersion 24` |

The engine's library ships as `Lilia.xcframework` on iOS and as
`liblilia_ffi.so` per ABI on Android, built by `tools/mobile/build-apple.sh` and
`tools/mobile/build-android.sh` into `release/`.

---

## Installation

```sh
pnpm add @liliadb/react-native
# or
npm install @liliadb/react-native
```

React Native's autolinking finds the podspec and the Android package; codegen
generates the module's spec from `src/NativeLiliaReactNative.ts`.

```sh
cd ios && pod install
```

With Expo, a development build (`npx expo prebuild`, then `npx expo run:ios` or
`run:android`) links it the same way; Expo Go cannot load native modules.

### Optional integrations

These packages are **not** required; each feature degrades gracefully without them:

| Package | Enables |
| --- | --- |
| `@shopify/flash-list` | `LiliaFlashList` uses FlashList instead of FlatList |
| `expo-haptics` | Haptic feedback in `LiliaOptimisticButton` |
| `expo-local-authentication` | `LiliaBiometricLock` |

---

## Quick start

```tsx
import { LiliaProvider, useKv, useDocument, useSqlQuery } from '@liliadb/react-native';

export function App() {
  return (
    <LiliaProvider options={{ name: 'app' }} fallback={<Splash />}>
      <Settings />
    </LiliaProvider>
  );
}

function Settings() {
  const [theme, setTheme] = useKv('settings', 'theme', { defaultValue: 'light' });
  const [profile, updateProfile] = useDocument<{ name: string }>('users', 'me');
  const { data: users } = useSqlQuery<{ id: string }>(
    "SELECT id FROM lilia_json WHERE space = $1",
    ['users'],
    { tables: ['lilia_json'] }
  );

  return (
    <View>
      <Switch value={theme === 'dark'} onValueChange={(dark) => setTheme(dark ? 'dark' : 'light')} />
      <TextInput value={profile?.name ?? ''} onChangeText={(name) => updateProfile({ name })} />
      <Text>{users.length} users</Text>
    </View>
  );
}
```

---

## Opening a database

### `LiliaDatabase.open(options)`

```ts
const db = await LiliaDatabase.open({
  name: 'app',                 // lives in app.lilia, in the platform's directory
  durability: 'balanced',      // 'durable' (default) | 'balanced' | 'performance'
  profile: 'mobile',           // 'mobile' (default here) | 'constrained' | 'server'
  historyRetentionMs: 86_400_000,
  undoDepth: 50,
});
```

| Option | Description |
| --- | --- |
| `name` | The database's name: it lives in `<name>.lilia`. Default `app_data`. |
| `path` | The database's directory, instead of `name`. |
| `durability` | `durable`: every acknowledged commit survives power loss; `balanced`: the journal stays consistent, the latest commits may be lost; `performance`: no sync on commit. |
| `profile` | Memory and cache budget: `mobile`, `constrained` (watches, TVs), `server`. |
| `ledger` | Keeps a tamper-evident ledger of every record write. |
| `historyRetentionMs` | How long old versions stay readable; a week by default, 0 keeps none. |
| `undoDepth` | Commits `undo` can undo; needs a history retention. |
| `checkpointOnBackground` | Checkpoints when the app goes to the background (default `true`). |
| `ios` | `directory` (`applicationSupport`, `documents`, `caches`), `excludeFromCloudBackup` (default `true`), `groupContainerIdentifier`. |
| `android` | `directory` (`noBackup`, `files`, `cache`). |
| `dev.reuseAcrossFastRefresh` | In development, `open` returns the database already open at the path, so Fast Refresh does not open it twice (default `true`). |

### Where databases live

`resolveDatabasePath(options)` gives the directory `open` uses:

- **iOS**: Application Support by default (persistent, not shown in Files),
  excluded from iCloud backups; `groupContainerIdentifier` puts it in an App Group
  container, shared with widgets and extensions.
- **Android**: the no-backup files directory by default, left out of Auto Backup
  and its 25 MB quota.

### Background checkpoints

A checkpoint writes the memtables to sorted runs. With `checkpointOnBackground`,
the database checkpoints when the app goes to the background, where the system may
end it: the commits are then in the database's files whatever the durability, and
the next open has less journal to replay.

---

## Providers and context

### `<LiliaProvider>`

```tsx
// Opens the database while mounted, closes it on unmount.
<LiliaProvider options={{ name: 'app' }} fallback={<Splash />} errorFallback={(e) => <Crash error={e} />}>
  <App />
</LiliaProvider>

// Or gives an open database to the tree.
<LiliaProvider database={db}>
  <App />
</LiliaProvider>
```

Without `errorFallback`, an open failure is thrown to the nearest error boundary.

### Accessing the database

```tsx
const db = useDatabase();      // throws outside a provider that has opened it
const same = useLilia();       // alias
<LiliaConsumer>{(db) => <Inspector db={db} />}</LiliaConsumer>
```

---

## Database API

`LiliaDatabase` is a `Database` of `@liliadb/client`: the API of `@liliadb/node`,
documented in its README, without the Node streams.

```ts
await db.kv.set('settings', 'theme', 'dark');
const entry = await db.kv.get('settings', 'theme');      // { key, value: Uint8Array, version: bigint, ... }
await db.json.put('users', 'u1', { name: 'Ada', age: 36 });
const adults = await db.json.find('users', { age: 36 });  // field paths or index names
await db.graph.addEdge('social', 'u1', 'u2', 'follows');
await db.fts.index('notes', 'n1', 'local first storage');
const rows = await db.sql.query('SELECT id FROM lilia_json WHERE space = $1', ['users']);
for await (const document of db.json.scanStream('users')) { /* a page at a time */ }
```

Also `vector`, `hybrid`, `timeseries`, `geo`, `crdt`, `batch`, `undo`/`redo`,
`quickCheck`, `integrityCheck`, `checkpoint` and `storageStats`.

### Synchronous API (`db.sync`)

The same calls, run on the JavaScript thread without a promise. Use them for small
reads a render needs at once; long queries belong to the asynchronous API, which
keeps the JavaScript thread free.

```ts
const theme = db.sync.kv.get('settings', 'theme');
db.sync.json.put('drafts', 'd1', { text: 'Hello' });
```

### Change topics

Every successful write, asynchronous or synchronous, notifies the topics it
changed; `db.subscribe(topics, listener)` returns the unsubscribe.

```ts
import { topics } from '@liliadb/react-native';

const unsubscribe = db.subscribe(
  [topics.kv('settings', 'theme'), topics.table('lilia_json')],
  () => refresh()
);
db.subscribe('*', () => console.log('something changed'));
```

| Topic | Notified by |
| --- | --- |
| `topics.kv(namespace, key?)` | `kv.set`, `kv.delete` |
| `topics.json(space, id?)` | `json.put`, `json.delete` |
| `topics.graph(graph)` | node and edge writes |
| `topics.fts(collection)`, `topics.vector(collection)` | index writes |
| `topics.timeseries(series?)`, `topics.geofences()`, `topics.track(id)` | appends, policies, retention, fences, tracks |
| `topics.crdt(space, id?)` | CRDT writes and merges |
| `topics.table(name)` | writes to an SQL table: `lilia_kv`, `lilia_json`, a JSON space, `lilia_graph_nodes`, `lilia_graph_edges`, `lilia_timeseries` |
| `'*'` | every write |

`undo` and `redo` notify every listener.

### Lifecycle

```ts
await db.checkpoint();
const stats = await db.storageStats();   // levels, memtable bytes, versions
await db.close();                        // waits for the calls made; later calls fail with CLOSED
```

---

## Hooks

Each hook takes the database of the nearest `<LiliaProvider>`, or `options.db`.

### `useLiliaDatabase(options)`

Opens a database for the component's lifetime: `{ db, isLoading, error }`.

### `useKv` / `useKvState`

```tsx
const [theme, setTheme, meta] = useKv<'light' | 'dark'>('settings', 'theme', { defaultValue: 'light' });
await setTheme((previous) => (previous === 'light' ? 'dark' : 'light'));
await meta.remove();
const [count, setCount] = useKvState('counters', 'taps', 0);
```

Values are stored as text through `serialize` / `deserialize`, JSON by default.
The first render reads the value synchronously; later writes of the key, by this
hook or anything else, show at once.

### `useDocument` / `useJson`

```tsx
const [user, update, { remove, isLoading }] = useDocument<User>('users', 'u1');
await update({ age: 37 });                  // merged into the document
await update((previous) => ({ ...previous!, tags: [] }));
```

### `useGraphNeighbors`

```tsx
const { neighbors, isLoading } = useGraphNeighbors('social', 'u1', { direction: 'outgoing', kind: 'follows' });
```

### `useFts`

```tsx
const { hits, isSearching } = useFts('notes', query, { limit: 20, debounceMs: 200 });
```

### `useSqlQuery`

```tsx
const { data, columns, isLoading, refetch } = useSqlQuery<{ id: string; name: string }>(
  "SELECT id, value->>'name' AS name FROM users ORDER BY id",
  [],
  { tables: ['users'] }
);
```

The query runs again after writes to `tables`, or after any write when omitted;
`enabled: false` holds it.

### `useMutation`

```tsx
const { mutate, isPending, error } = useMutation(
  (title: string, db) => db.json.put('todos', `todo-${Date.now()}`, { title, done: false }),
  { onSuccess: () => setTitle('') }
);
```

The hooks that read what a mutation writes run again by themselves.

### `useDatabaseMaintenance`

```tsx
const { stats, checkpoint, integrityCheck, refresh } = useDatabaseMaintenance();
```

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
  sql="SELECT id, value->>'title' AS title FROM messages ORDER BY id DESC"
  tables={['messages']}
  renderItem={({ item }) => <Text>{item.title}</Text>}
  loadingComponent={<ActivityIndicator />}
  emptyComponent={<Text>No messages</Text>}
/>
```

`LiliaFlatList` props: `sql`, `sqlParams`, `tables` (the tables whose writes run
the query again; any write when omitted), `data`, `renderItem`,
`loadingComponent`, `emptyComponent`, `errorComponent`, `db`, plus FlatList props.
The default key extractor looks for `id`, `rowid`, then `key`.

`LiliaFlashList` additionally requires `estimatedItemSize`.

`LiliaSectionList` requires `sql` and `groupBy: (item) => string`; optional
`sortSections`, `renderSectionHeader`, plus SectionList props.

`LiliaInfiniteList` props: `space`, `pageSize` (default 20), `filter` (a
`json.find` query), `renderItem` over `JsonEntry<T>`, `loadingComponent`,
`emptyComponent`, `errorComponent`, `db`, and `onEndReachedThreshold` (default
0.5). A write to the space reads again the documents the list shows.

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

`LiliaHighlightSnippet` parses the `<b>…</b>` tags the engine wraps matches in,
and styles the matched terms.

### Graph

| Component | Props |
| --- | --- |
| `LiliaGraphExplorer` | `graph`, `rootNode`, `direction` (`outgoing`), `kind`, `renderNode(nodeId, isCurrent)`, `onNodePress`, `containerStyle`, `db` |
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
| `LiliaStorageBar` | `showCheckpointButton`, `barHeight`, `style`, `db`, `onMaintenanceComplete` |

`LiliaStorageBar` shows the bytes of the database's sorted runs (on disk) and of
its memtables (in memory until a checkpoint), kept current as the app writes,
with a Checkpoint action.

### DevTools

| Component | Props |
| --- | --- |
| `LiliaDevInspector` | `enabled` (defaults to `__DEV__`), `position`, `db` |

Floating inspector with an on-device SQL console, live storage statistics, and a
key/value reader and writer.

---

## Types

The package exports the types of `@liliadb/client` (`KvEntry`, `JsonEntry<T>`,
`GraphNode`, `FtsHit`, `SqlQueryResult<T>`, `StorageStats`, `LiliaError`...), and
its own:

- Opening: `OpenDatabaseOptions`, `IosDatabaseConfig`, `AndroidDatabaseConfig`.
- Hooks: `UseKvOptions`, `UseKvMeta`, `UseKvReturn`, `UseDocumentOptions`,
  `UseDocumentMeta`, `UseDocumentReturn`, `UseSqlQueryOptions`,
  `UseSqlQueryReturn`, `UseMutationOptions`, `UseMutationReturn`, `UseFtsOptions`,
  `UseFtsReturn`, `UseGraphNeighborsOptions`, `UseGraphNeighborsReturn`,
  `UseDatabaseMaintenanceReturn`.
- Components: `LiliaProviderProps`, `LiliaFlatListProps` and the props of each
  component.

---

## Troubleshooting

**`the LiliaDB native module is not linked`**
Run `pod install` on iOS, rebuild the Android app, and check that
`release/apple/Lilia.xcframework` and `release/android/jniLibs` exist.

**`the LiliaDB JSI binding is not installed`**
The app must run the New Architecture, with JavaScript on the device (not in a
remote debugger).

**Opening fails after Fast Refresh**
Two opens of one path conflict. In development `open` reuses the database already
open at the path; in production, open each path once (a `<LiliaProvider>` at the
root).

**`no database: render this inside a <LiliaProvider>`**
Wrap the tree in `<LiliaProvider>` or pass `db` to the hook.

**Biometrics unlock immediately**
`expo-local-authentication` is missing or reports no enrolled hardware. The
component unlocks rather than locking users out.

---

## License

Apache-2.0
