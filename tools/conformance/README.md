# Shared conformance

Run `pnpm nx run lilia-conformance:test` from the workspace root. This builds the
Rust binaries/addon and TypeScript SDK, stages the local addon, runs the Rust
fixture test (the protocol's executor), then runs the same fixtures through Node
embedded, the daemon via the Node SDK, and CLI JSON subprocesses.
Missing binaries are failures, never skips. Tests use fresh temporary databases
per surface; servers are terminated and awaited before temporary files are removed.

`fixtures.json` is the single expected-result oracle. Its ordered steps cover
missing records, recursive JSON, binary KV values, optimistic updates and conflicts,
multimodel rollback, past expiration, exclusive-cursor pagination, namespace/space
isolation, conditional deletion, empty batches and invalid names/batches. Both runners
validate error codes, retryability and request IDs. JS adapters additionally check public
`Uint8Array`/`bigint` types before normalizing them to the JSON oracle. Error text,
random request IDs and transport envelopes are not compared verbatim.

A version is the engine's sequence number of a write, so the oracle names versions instead
of stating them: `"$kv_a1"` binds the version a step returns, later steps compare with it or
send it as an `if_version`, and two names never bind the same version (an update or a delete
yields a new one). `oracle.mjs` and the Rust runner
(`crates/lilia-protocol/tests/conformance.rs`, through `lilia_protocol::execute`) apply the
same rules.

CLI invalid JSON has additional transport-specific assertions. This is functional model
conformance, not a substitute for storage fault-injection tests.

The shared fixtures deliberately use text-compatible KV keys (the CLI takes text keys by
default) and an already-expired timestamp rather than timing-dependent sleeps. Versions in the
JSON oracle are decimal strings; their exact `u64` round trips through MessagePack, rkyv and
JS frames are tested by `crates/lilia-protocol/tests/versions.rs` and the `@liliadb/client`
codec test.

CI executes this target on five native platforms with Node 24, and Linux x64 with
Node 22/24/26. Nx caching is disabled for this end-to-end target so native artifacts
and child processes are exercised on every invocation.
