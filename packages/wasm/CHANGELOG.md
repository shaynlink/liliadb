# @liliadb/wasm Changelog

All notable changes to the `@liliadb/wasm` package will be documented in this file.

---

## [Unreleased] - v0.2 (lilia-db)

### Changed
- The package runs on the native `lilia-db` engine compiled to WebAssembly instead of SQLite.
  On OPFS a database is a pool of slot files in one directory (`path`), which grows as the
  database does; one tab at a time opens it. `storage: "memory"` keeps it in memory.
- The calls are those of `@liliadb/client`, shared with `@liliadb/node` and
  `@liliadb/react-native`: every model (key/value, JSON, graph, full-text, vector, hybrid,
  time series, geo, CRDT, SQL), batches, undo/redo and checks.
- Each call is one MessagePack operation answered by a JS frame; through the package's worker,
  requests and frames cross by transfer. In the calling thread (a dedicated worker, or a
  database in memory), `db.sync` runs the calls synchronously.
- Open options: `path`, `storage`, `slots`, `worker` (`true`, `false`, a `Worker` or a script
  URL), `wasm` (the module), `durability`, `profile` (`mobile` by default), `ledger`,
  `historyRetentionMs`, `undoDepth`. A worker of the app's own serves a database with
  `serve(self)`.
- SQL is DataFusion and read-only: parameters are `$1`, `$2`...; JSON is read with
  `value->>'field'` and `json_get_*`; every JSON space is also a table.
- The module (`pkg/`) is built by `tools/wasm/build.sh` with a size-optimized profile; it is
  no longer kept in the repository.

### Removed
- Retired with SQLite: encryption (`encryptionKey`), the SQLite VFS options (`vfs`,
  `opfs-sahpool`, `:memory:` paths), `busyTimeoutMs`, `writerQueueCapacity`, `plugins`,
  `sql.execute`, and `ensureWasmInitialized` (the `wasm` option loads the module).
- Outside the browser the module's bytes are passed with `wasm`; it is not read from disk by
  itself.
