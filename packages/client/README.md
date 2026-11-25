# @liliadb/client

The LiliaDB calls, written once for every JavaScript runtime. Applications install a runtime
package, which depends on this one and re-exports it:

- [`@liliadb/node`](../node): Node.js, Bun and Deno, embedded or through a daemon;
- [`@liliadb/react-native`](../react-native): iOS and Android through JSI.

What it holds:

- `Database`: every model (`kv`, `json`, `graph`, `fts`, `vector`, `hybrid`, `timeseries`,
  `geo`, `crdt`, `sql`), `batch`, `undo`/`redo`, checks, `close` (which waits for the calls
  made) and `sync`, the same calls run on the caller's thread when the transport can;
- the protocol: each call is one `Operation`, encoded in MessagePack (`encodeWire`); its
  result comes back as a JS frame, one JSON text and the raw bytes it refers to (`readFrame`,
  `bytesOf`, `unwrap`);
- `Transport`, which a runtime package implements: `execute`, optionally `executeSync`, and
  `close`;
- the public types, `LiliaError`, and the `geo`, `quantization` and `timeseries` helpers.

It uses no API of a particular runtime: `TextEncoder`, typed arrays, `AbortSignal` and
`BigInt` are all it needs.
