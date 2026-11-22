# @liliadb/react-native Changelog

All notable changes to the `@liliadb/react-native` package will be documented in this file.

---

## [Unreleased] - v0.2 (lilia-db)

### Changed
- The package runs on the native `lilia-db` engine instead of SQLite, through a JSI binding
  over the engine's C ABI: a TurboModule (New Architecture, React Native 0.76 and later)
  installs it. Asynchronous calls run on native worker threads; `db.sync` runs on the
  JavaScript thread. Results reach JavaScript as one JSON text and an `ArrayBuffer` over the
  engine's own memory.
- `LiliaDatabase` has the API of `@liliadb/client`, shared with `@liliadb/node`: every model
  (key/value, JSON, graph, full-text, vector, hybrid, time series, geo, CRDT, SQL), batches,
  undo/redo and checks, with `bigint` versions and `Uint8Array` keys and values.
- Writes notify the topics they change (`topics.kv(namespace, key)`,
  `topics.json(space, id)`, `topics.table(name)`...); `db.subscribe(topics, listener)` and
  the hooks follow them, so mutations need no invalidation.
- `useKv` and `useDocument` read their first value synchronously; `useKv` stores JSON by
  default; `useSqlQuery` runs again after writes to its `tables`, or any write.
- Open options: `durability`, `profile`, `ledger`, `historyRetentionMs`, `undoDepth`,
  `checkpointOnBackground` (was `autoFlushOnBackground`), `dev.reuseAcrossFastRefresh` (was
  `dev.enableFastRefreshSingleton`). A database is a directory, `<name>.lilia`.
- `useDatabaseMaintenance` returns `stats` (`StorageStats`), `checkpoint` and
  `integrityCheck`; `LiliaStorageBar` shows the bytes on disk and in memtables.
- `useGraphNeighbors` and `LiliaGraphExplorer` take `kind` and the directions `outgoing`,
  `incoming`, `both`.
- The package is an ES module; React Native 0.76 and React 19 are the minimum.

### Fixed
- `LiliaInfiniteList` no longer reads its first page again on every render when no `filter`
  is given, and a write reads again the pages shown instead of only the first.

### Removed
- Retired with SQLite: encryption (`useSecureStorage`, `encryptionKey`, `passphrase`, the
  Keychain and Keystore helpers), backups (`LiliaBackupShareButton`), `vacuum`, WAL
  checkpoint modes and `getFileSize`, `sql.mutate` (SQL is read-only).
- The bridge module and its per-call methods, `notifyChange` and the hooks' `invalidate*`
  options, `refreshOnFocus`, the in-memory KV cache, and the no-op Expo config plugin
  (development builds link the module by autolinking).
