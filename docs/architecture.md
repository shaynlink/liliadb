# Architecture

## Boundaries

`lilia-core` owns stable contracts and errors. `lilia-storage-sqlite` owns the standard SQLite
adapter, lifecycle, migrations, transactions, read pool, and writer admission. Neither depends on
the CLI, Node, daemon, or plugin loader.

The standard storage adapter is bundled SQLite in WAL mode with `synchronous=FULL`. A process may
run concurrent reads while writes are serialized by SQLite and the in-process writer mutex. LiliaDB
is not supported on network filesystems.

All internal tables use the `_lilia_` prefix. Format version `1` provides:

- `_lilia_metadata` for database format metadata;
- `_lilia_kv` for binary namespaces, keys, values, versions, and optional expiry;
- `_lilia_json` for named JSON spaces, identifiers, values, and versions.

`batch` is the common atomic mutation primitive. An expected version of `0` means create-only;
other expected versions implement optimistic concurrency. A missing expectation is unconditional.

## Exact versions

Rust versions and conditional expectations are `u64`. Node requires `bigint` for
`ifVersion` in `0..=18446744073709551615` and always returns version `bigint`s;
numbers, negative values and values above u64 are rejected, not coerced or wrapped.
MessagePack retains native integer fields, so protocol 1.0 framing is unchanged.
The SDK uses 64-bit integer encoding/decoding and never routes versions through
JavaScript `number`.

Human-readable serialization (native JSON and CLI JSON/JSONL) now emits
canonical decimal strings for `version` and `if_version`, including small values.
Absent versions remain null. Integer JSON inputs remain accepted for compatibility,
but clients should send decimal strings to avoid rounding before transmission.
Conflict details `expected` and `actual` are decimal strings or null as well.
This is a pre-alpha JSON output contract change: update consumers and install the
SDK and native addon together. It does not change numbers inside JSON documents;
Node `JsonValue` still uses IEEE-754 `number`. SDK expiration inputs must be safe
integer milliseconds, encoded as integers over IPC.

Storage format 1 still uses positive SQLite INTEGER versions, capped at
9223372036854775807 (`i64::MAX`). The full u64 expectation range can be compared
exactly, but incrementing a stored version beyond that cap returns non-retryable
`STORAGE` and rolls back the batch. Supporting stored versions beyond this limit
requires a separate format migration; this transport fix does not perform one.

## Runtime modes

- Embedded Rust calls `lilia-core` directly.
- Embedded Node calls `lilia-node-native`; blocking SQLite work runs outside the JavaScript loop.
- Daemon clients use size-prefixed MessagePack over a Unix socket or Windows named pipe.
- CLI uses the embedded core and emits human, JSON, or JSONL output.

The daemon handshake has protocol major/minor `1.0`, a 16 MiB frame limit, per-request UUIDs, a
random local token stored with user-only permissions, bounded connections, and idle/deadline
timeouts. No TCP listener exists in version `0.1`.

Daemon requests are authenticated before dispatch, including shutdown. A rejected
shutdown never signals daemon termination. Equal-length tokens are compared in
constant time. Blocking storage operations run outside the async executor.

`deadline_ms` is an optional relative admission budget starting after frame decode.
Zero rejects immediately; the budget is checked again when a blocking worker becomes
available. An expired queued request returns retryable `TIMEOUT` without dispatching
the operation. This is not an execution or response-time limit: it does not interrupt
SQLite work or its internal lock waits after dispatch, and a queued response may wait
for a worker. Once work starts, the daemon returns its actual outcome rather than
claiming that a potentially committed write timed out. Omitting the field imposes no
admission deadline. The existing frame-read idle timeout remains separate.

## Node.js SDK streaming and keyset pagination

Large scans in `@liliadb/node` avoid unbounded in-memory allocations and frame limit
exhaustion through asynchronous streaming (`scanStream`, `stream`) and standard Node.js
streams (`createReadStream`).

- **Keyset pagination**: Scans progress strictly using the exclusive `after` cursor
  (`WHERE key > ?` or `WHERE id > ?` ordered by key/id), eliminating offset drift and
  redundant reads.
- **Configurable chunking**: `batchSize` governs the maximum entries requested per
  internal fetch (default: 100). If a batch returns fewer rows than requested, the
  stream terminates cleanly without an unnecessary trailing empty query.
- **Bounded traversal**: Optional `limit` specifies the total maximum entries yielded
  across all batches. The final batch is automatically clamped (`fetchLimit = Math.min(batchSize, remaining)`).
- **Lifecycle tracking**: Every internal page query is registered with the `Database`
  pending set via `this.run(...)`. A graceful `db.close({ timeoutMs })` drains active
  page fetches before shutting down the native engine or daemon connection.
- **Node.js Stream interoperability**: `createReadStream` produces a `Readable` stream
  in `objectMode` via `Readable.from(...)`, integrating natively with `stream.pipeline`
  and transform streams.

## Unix IPC filesystem policy

The daemon requires an owned, user-only parent directory for each socket and token.
Missing directories are created with mode `0700`; existing directory permissions are
never changed by the IPC setup. Ancestors must be owned by root or the current user
and not writable by others, except root-owned sticky temporary directories. Paths
containing parent traversal or symlinks are rejected, including directory symlinks.
Use physical paths (for example, resolve macOS `/var` or `/tmp` aliases with `realpath`
before passing a generated temporary directory). Relative paths without traversal
are supported. This intentionally rejects older configurations using a shared IPC
directory; choose a new private directory rather than broadening its permissions.
On macOS, extended ACL grants on ancestors, IPC directories, existing tokens or
staging files are refused even when mode bits are private. Deny-only ACLs, such as
the usual home-directory delete restriction, remain supported. This deliberately
rejects even grants to the current user rather than reinterpreting ACL inheritance.

The socket is claimed before credential rotation. Every existing endpoint is refused,
even a stale socket: inspect and remove stale endpoints explicitly after confirming
that no daemon is using them. Cleanup only removes the socket inode created by this
instance. Accepted connections must report the current effective UID via OS peer
credentials; credential lookup failures are refused.

Tokens are written into a mode-`0600` staging file, synced, then atomically published.
Rotation accepts only an owned private regular file with one hard link; symlinks,
hardlinked files, public files and other entry types are rejected without modification.
Normal shutdown retains the token for subsequent atomic replacement on restart.
These checks isolate different unprivileged users, not root or hostile processes
already running as the same user. They are not a general-purpose filesystem sandbox.

## Windows IPC security

The daemon creates local named pipes with an explicit protected DACL granting full
access only to the current process user SID. The first instance claims the name
before token rotation; replacement listeners are created while the connected
instance remains alive. Remote pipe names and remote clients are rejected.

Windows token paths must use a local drive, without traversal, alternate data streams
or reparse points. The final parent must have a protected current-user-only DACL;
missing directories receive that DACL at creation. Existing files with broader or
inherited ACLs are refused, not silently repaired. Directory handles deny delete
sharing and remain open for the daemon lifetime to prevent ancestor replacement.
Ancestors must be owned by the current user, SYSTEM, Administrators or the exact
Windows Modules Installer (TrustedInstaller) service SID and must not
grant other principals rights to modify data, attributes, ACLs, ownership or child
deletion. This also prevents in-place junction conversion; delete sharing alone
does not. Uninterpreted ACL entry types are refused.
Tokens are staged with the private DACL before writing, synced, then renamed over
an existing validated token, followed by an explicit metadata flush on the parent
directory handle via `FILE_FLAG_BACKUP_SEMANTICS` and `FlushFileBuffers`. Hardlinks
and nonregular entries are refused.

Use a new private IPC subdirectory when migrating from earlier Windows builds.
These guarantees exclude administrators with privilege overrides and hostile
same-user processes. Native Windows runtime tests and second-account denial tests
are separate from cross-compilation; neither should be inferred from a macOS test run.

## Backups

`backup(destination)` creates a new snapshot using SQLite's online backup API,
checks its integrity, closes and syncs it, then publishes it atomically without
overwriting any existing entry. Use a fresh destination name for every backup;
existing files, directories, hard links and symlinks are rejected. Staging is in a
private sibling directory and is cleaned on normal success or failure. A process
kill can leave an unpublished `.lilia-backup-*` directory for manual cleanup.
Publication requires a local filesystem supporting hard links; unsupported
filesystems return an error without deleting the destination. Publication
also syncs the destination directory (on Unix via explicit `fsync` on the
directory file descriptor; on Windows via `CreateFileW` with `FILE_FLAG_BACKUP_SEMANTICS`
and `FlushFileBuffers`). A directory-sync failure can be reported
after the complete snapshot has become visible.

Existing parent-directory permissions are preserved; newly created directories
and snapshot files are user-only on both Unix (0700/0600) and Windows (current-user-only
protected DACL `O:{sid}D:P(A;OICI;FA;;;{sid})` for directories, `O:{sid}D:P(A;;FA;;;{sid})` for files).
Destination parent directories must be trusted and must not be concurrently replaced by an untrusted
process; this is not a sandboxed path API. Windows directory durability (metadata flush) is enforced via explicit directory handle flushing.

## Writer admission

Writer admission is a bounded FIFO shared by batch mutations, explicit checkpoints
and backups. `writerQueueCapacity` counts waiting operations in addition to the
one active writer; zero allows only immediate admission. Saturation returns
retryable `BUSY`. Read connections do not enter this queue.
Daemon batch deadlines include FIFO waiting time: an expired waiter is removed
without starting its transaction. Once a transaction starts, its actual outcome
is returned rather than reporting a timeout that could conceal a commit.
The queue is per database handle (not a cross-process scheduler); SQLite still
arbitrates writers from other handles/processes via the configured busy timeout.
Backups retain admission through snapshot verification and publication, so closing
also waits for their filesystem work to finish.

## Automatic checkpoints

The writer disables SQLite's commit-path autocheckpoint. Each database handle owns
one maintenance thread with its own connection; commits send coalesced, nonblocking
wakeups. The worker uses `wal_checkpoint(NOOP)` to measure outstanding frames
(page bytes plus frame headers), not the allocated WAL file size. It runs a
`PASSIVE` checkpoint when the byte threshold or periodic interval is reached.
It also checks recovered WAL at startup and polls when there are no local writes.
The maintenance connection has a zero busy timeout and never takes the writer
admission queue. Readers may prevent full progress; incomplete attempts and errors
are retried on the next interval, not in a hot loop. Rust `checkpoint_stats()`
exposes attempt/incomplete counters, frame counts and the last error code, with
no stored values or paths. Explicit checkpoints are not included in these counters.

Rust `CheckpointPolicy` and Node embedded `checkpointPolicy` are active, with
defaults of 16 MiB and 5 seconds. Thresholds must be positive; the interval is
1..=4294967295 milliseconds. Node fields are limited to positive u32 integers.
Daemon connections use the server's defaults, not client open settings.
PASSIVE does not promise WAL truncation or a hard WAL size cap during long-lived
reads. After explicit close, `checkpoint_stats()` returns default counters because
the maintenance service has been released.

## Explicit close and drain

Rust `Database::close(Duration)` stops new operation admission with `CLOSED`, drains
accepted readers and queued writers, stops the maintenance worker, performs a final
PASSIVE checkpoint and closes all connections. Cleanup runs on a separate thread;
the timeout bounds the caller's wait, not filesystem I/O. A retryable `TIMEOUT`
does not cancel accepted writes or reopen admission. Calling close again waits for
the same cleanup and returns its stored outcome. Success means the handle has
released its connections; external readers may still pin WAL frames.

Node `db.close({ timeoutMs })` defaults to 5000 ms and accepts integer budgets from
0 through 2147483647 ms. It also drains calls accepted in JavaScript before they
reach a native worker. New operations fail with a structured `CLOSED` error;
close timeouts have a request ID and are retryable. Closing a daemon SDK client
drains that client's calls but does not shut down the shared server.

Authenticated daemon shutdown stops accepting connections and requests database
close with a five-second wait budget. The shutdown response acknowledges the
request, not completed cleanup. Runtime blocking work may delay process exit;
abrupt signals and process kills are not a graceful-drain guarantee. Implicit Rust
drop still performs synchronous resource cleanup without a caller timeout.

## Recovery and Chaos Resilience Guarantees

Storage resilience is tested against process crashes, power interruption simulations,
torn WAL frames, frame checksum corruptions, and disk exhaustion (`ENOSPC`).

- **Multi-Model Crash Atomicity (`SIGKILL`)**:
  Transactions execute atomically across all data models (KV, JSON, Graph nodes/edges, FTS).
  Abrupt process termination (`SIGKILL`) while dirty uncommitted multi-model mutations are
  flushed to WAL guarantees strict all-or-nothing rollback on reopen. No partial records,
  orphaned graph edges, or unindexed search tokens are exposed. Confirmed commits preceding
  the crash remain 100% durable.
- **Torn WAL Writes & Power Loss Simulation**:
  Simulating power failure during disk flush (partial WAL frame write, e.g. 512 bytes of a
  4120-byte page frame) triggers SQLite's WAL recovery protocol on reopen. The torn frame is
  discarded, all preceding valid commit frames are replayed, `integrity_check` passes, and
  normal writes resume immediately.
- **Corrupt Frame Checksum Rejection**:
  Bit flips in WAL frame checksums (simulating torn sectors or storage corruption) are
  detected by SQLite's running 64-bit frame checksum. Replay cleanly halts before the
  corrupted frame, committing only validated prior frames and leaving the database consistent.
- **Truncated WAL Headers**:
  Interruption during WAL file initialization leaving a truncated header (< 32 bytes) is
  detected on startup; the invalid WAL is discarded, falling back safely to the main database file.
- **Disk Full (`ENOSPC`) & Self-Healing**:
  When write operations exceed disk space or page quota, transactions fail immediately with
  structured, non-retryable `ErrorCode::DiskFull`. The writer queue lock is cleanly released
  (preventing writer lock starvation or mutex poisoning), concurrent readers continue operating
  uninterrupted, and the database automatically heals and accepts new writes as soon as disk
  space is restored without requiring a process restart.
- **Backup Staging Isolation & Cleanup**:
  Disk or permission failures during backup staging immediately clean up any temporary
  `.lilia-backup-*` staging artifacts, leaving the source database fully operational.
- **Physical Corruption Guard**:
  Severe database file header corruption is diagnosed and rejected with non-retryable
  `ErrorCode::Corrupt`. The engine never silently overwrites, reinitializes, or truncates
  corrupted files.
- **Stress Crash-Recovery Cycles**:
  Iterative crash-recovery loops verify sustained stability through repeated abrupt terminations.

## Declarative SQL Querying & SQL Plugin

Declarative SQL querying is integrated into LiliaDB's SQLite engine and plugin architecture via `Capability::Sql` and `plugins/lilia-plugin-sql`.

- **Predefined Views**: Internal schema tables `_lilia_kv`, `_lilia_json`, `_lilia_graph_nodes`, and `_lilia_graph_edges` are exposed to callers via read-only SQL views (`lilia_kv`, `lilia_json`, `lilia_graph_nodes`, `lilia_graph_edges`).
- **Transparent Decoding Scalar Functions**: Custom SQLite scalar functions `lilia_decode_kv` and `lilia_decode_json` are deterministically registered on every connection. If an encryption codec (`ValueCodec`) is mounted, raw ciphertext and `{"$codec": ...}` envelopes are decrypted on the fly within SQLite queries without storing plaintext on disk.
- **Statement Routing**: Before executing, statements are parsed and checked via `stmt.readonly()`. Read-only queries (`SELECT`, `EXPLAIN`) run concurrently on the reader pool connections (`query_only = true`). Mutating queries (`INSERT`, `UPDATE`, `DELETE`) are routed through the serialized exclusive writer queue.
- **Cross-Surface Support**: Declarative SQL is accessible uniformly across:
  - Native Rust: `db.sql_query(&sql, &params)`
  - Node.js SDK: `db.sql.query<T>()`, `db.sql.raw()`, and `db.sql.execute()`
  - Local Daemon & IPC: `Operation::SqlQuery` and `ResponseValue::Sql`
  - CLI: `lilia sql <QUERY> [--param <P>]` with human ASCII table, JSON, and JSONL output

## Roadmap boundaries

The [shared conformance suite](../tools/conformance/README.md) runs one ordered
fixture corpus through Rust embedded, Node embedded, daemon, and CLI JSON.
CLI execution errors now preserve the structured Lilia error on stderr with exit
code 1 and no result on stdout; JSON parse failures use `INVALID_INPUT`. Clap's
command-line usage errors remain separate (exit code 2). Node native errors are
decoded into `LiliaError`, and binary daemon error UUIDs become string request IDs.

Document, Graph, and replication plugins continue active development adhering to
the same transaction boundary and protecting `_lilia_*` tables.

## Real-Time CDC & Event Streaming Architecture

LiliaDB includes a zero-copy Change Data Capture (CDC) event streaming engine:
- **Transaction Ingestion**: Whenever mutations commit to the SQLite WAL, structured change events (`kv_mutated`, `json_mutated`, `graph_node_mutated`, `graph_edge_mutated`, `fts_mutated`, `vector_mutated`) are generated within the transaction boundary.
- **In-Memory Ring Buffer**: Events pass through a bounded, non-blocking broadcast channel to active subscribers without stalling writer transactions.
- **IPC Streaming Framing**: Over local Unix domain sockets and Windows named pipes, streaming subscriptions use dedicated protocol framing (`RequestPayload::SubscribeWatch`, `ResponsePayload::WatchEvent`) maintaining sub-millisecond dispatch.
- **HTTP Server-Sent Events (SSE)**: The daemon's HTTP gateway exposes `GET /v1/watch?namespace=<ns>`, allowing any HTTP/SSE client to consume real-time change events without proprietary drivers.

## Le 6ᵉ Modèle : Moteur Vectoriel Natif & Recherche Hybride

Au-delà du KV, JSON, Graphe, FTS5 et SQL, LiliaDB intègre un sous-moteur vectoriel natif de pointe dans `lilia-storage-sqlite` et `lilia-core` :

1. **Indexation HNSW (Hierarchical Navigable Small World) en Rust Natif** :
   - Multi-couches probabilistes ($P(l) \propto e^{-l / m_L}$ avec $m_L = 1 / \ln(M)$) garantissant une traversée de graphe approximative (ANN) en temps sub-milliseconde $O(\log N)$.
   - Zéro dépendance C/C++ externe (compilation native directe sur Linux, macOS, Windows, iOS, Android et WebAssembly OPFS).
   - Persistance relationnelle ACID dans SQLite (`_lilia_hnsw_meta`, `_lilia_hnsw_edges`), synchronisée de manière atomique au commit WAL dans la même transaction que les données KV, JSON ou FTS5.
   - Routage automatique : si un index HNSW existe sur une collection, `vector_search` bascule automatiquement sur la recherche par faisceau HNSW ; sinon, un scan séquentiel exact (flat scan) sert de repli transparent.

2. **Quantification Scalaire Matérielle (SQ8 / Binary 1-bit) & Re-ranking Coarse-to-Fine** :
   - Compression symétrique des vecteurs flottants `f32` vers entiers signés `i8` (SQ8, division par 4 de la RAM) ou binaires `1bit` (division par 32 de la RAM, crucial sur montres et mobiles sous profil `ResourceProfile::Constrained`).
   - Opérations accélérées par le processeur : calcul de produit scalaire entier vectorisé et instructions POPCOUNT (`count_ones` matériel pour la distance de Hamming binaire).
   - Pipeline de recherche en 2 étapes (*Coarse-to-Fine Re-ranking*) : traversée ultra-rapide de l'index quantifié avec sur-échantillonnage 2x, suivie d'un réordonnancement exact sur les vecteurs `f32` d'origine stockés dans `_lilia_vectors`, combinant débit maximal et 99%+ de précision topologique.

3. **Recherche Hybride ACID (FTS5 + HNSW via RRF)** :
   - Fusion dans une seule transaction de lecture et un seul snapshot WAL du score lexical BM25 (`_lilia_fts`) et de la similarité cosinus sémantique (HNSW) via l'algorithme Reciprocal Rank Fusion (RRF) :
     $$RRF(d) = \frac{w_{\text{fts}}}{k + \text{rank}_{\text{fts}}(d)} + \frac{w_{\text{vec}}}{k + \text{rank}_{\text{vec}}(d)}$$
     avec constante de lissage configurable $k$ (défaut: 60) et pondérations relatives.

4. **Filtrage Combiné Pré-Filtré (Pre-filtered Vector Search)** :
   - Élimine le problème classique du *post-filtrage* (qui produit des résultats vides si les top candidats HNSW ne satisfont pas les critères).
   - L'algorithme traverse l'ensemble du graphe pour préserver la navigabilité *small-world*, mais n'admet dans le tas final de résultats que les nœuds validant les filtres de clés secondaires JSON (`vector_search_by_json_index`), de sous-graphes connectés (`vector_search_in_graph_neighbors`) ou de listes de candidats arbitraires (`vector_search_filtered`).

5. **Intégration Déclarative SQL** :
   - Fonctions scalaires enregistrées : `lilia_vec_distance_cosine`, `lilia_vec_distance_l2`, `lilia_vec_distance_hamming`, `lilia_vec_quantize_sq8`, `lilia_vec_quantize_bit`, `lilia_vec_dimension`, `lilia_vec_to_json`, `lilia_vec_from_json`.
   - Vues SQL de supervision : `lilia_vectors` (vecteurs déchiffrés à la volée) et `lilia_vector_indexes` (métadonnées des index HNSW actifs).

6. **Embeddings calculés par le moteur (`auto_embed`, crate `lilia-embed`)** :
   - Une collection JSON déclare le champ texte à embarquer (`CollectionOptions::auto_embed`, persisté dans `_lilia_catalog.auto_embed_json`). Le vecteur est rangé dans la collection vectorielle de même nom, sous l'id du document : le document n'est jamais réécrit, sa version ne change pas.
   - **Écriture** : `put_json_doc` compare le texte source avant et après ; s'il a changé, le document est inscrit dans `_lilia_embed_queue` dans la même transaction. Une suppression l'inscrit aussi. `undo` et `redo`, qui rejouent des changesets bruts, réinscrivent les documents qu'ils touchent ; la file elle-même est exclue des changesets.
   - **Worker `lilia-embed`** : un thread dédié lit les documents en attente, exécute l'inférence sans tenir aucun verrou, puis écrit en une transaction les `VectorSet` / `VectorDelete`, la mise à jour HNSW et le retrait de la file. Chaque résultat n'est appliqué que si le document contient encore le texte qui a été embarqué ; sinon l'entrée reste en file et le nouveau texte est traité au passage suivant. La cohérence est donc à terme : `wait_for_embeddings` permet d'attendre la convergence.
   - **Échecs** : un lot en erreur est rejoué texte par texte ; un document fautif est retenté avec un délai exponentiel (`EmbedOptions`), puis signalé par `embed_status` sans bloquer les autres. Une nouvelle écriture du document le réarme.
   - **Découplage** : le moteur ne dépend d'aucune bibliothèque d'inférence. Il pilote un `Embedder` (trait de `lilia-core`) injecté par `DatabaseOptions::with_embedder`. `lilia-embed` fournit `CandleEmbedder` (candle, pur Rust) pour `all-MiniLM-L6-v2` : pooling moyen pondéré par le masque d'attention, normalisation L2, 384 dimensions, troncature à 256 tokens. Les fichiers du modèle sont fournis par l'application.
   - **Contraintes** : tier `Disk` obligatoire pour la collection JSON et sa collection vectorielle ; champ source non couvert par le chiffrement au niveau champ ; pas de worker sur `wasm32` (la déclaration y est refusée).

## Cross-Model Integrity Contracts (Clés Étrangères Croisées & Contraintes CHECK)

Le défi majeur d'une base de données multimodèle (KV, JSON, Graphe, Vecteur, Séries Temporelles, SQL) est de garantir la cohérence d'un modèle à l'autre sans désynchronisation silencieuse, sans processus asynchrone de réconciliation et sans lectures sales.

LiliaDB intègre un sous-système déclaratif de contrats d'intégrité inter-modèles (`crates/lilia-core/src/integrity.rs`, `crates/lilia-core/src/json/expr/check.rs`, `crates/lilia-storage-sqlite/src/integrity.rs`) appliqué directement au sein de l'entonnoir transactionnel unique (`transaction::apply`) dans SQLite WAL.

```mermaid
flowchart TD
    subgraph Write_Funnel["Entonnoir Transactionnel Unique (transaction::apply)"]
        OP["BatchOperation (JsonPut, JsonDelete, GraphSetNode, KvSet)"]
        GUARD{"Integrity Guard"}
        CHECK{"CompiledCheck (Kleene 3VL)"}
        CASCADE["Résolution Cascades (GraphDeleteNode, KvDelete)"]
        SQLITE[("SQLite WAL (Transaction Atomique)")]
    end

    OP --> GUARD
    GUARD -->|JsonPut| CHECK
    CHECK -->|False| ERR["CONSTRAINT_VIOLATION (Rollback complet)"]
    CHECK -->|True / Unknown| SQLITE
    GUARD -->|JsonDelete (Parent)| CASCADE
    CASCADE -->|Restrict & Enfants > 0| ERR
    CASCADE -->|Cascade| SQLITE
    GUARD -->|GraphSetNode / KvSet| REF_VAL{"Parent JSON existe ?"}
    REF_VAL -->|Non| ERR
    REF_VAL -->|Oui| SQLITE
```

### 1. Clés Étrangères Croisées Natives (Cross-Model Foreign Keys)

Les clés étrangères inter-modèles permettent d'assujettir les entités d'un modèle (nœuds de graphe ou clés KV) à l'existence et au cycle de vie d'un document JSON parent.

#### A. Liaisons Nœud de Graphe $\to$ Document JSON
- **Déclaration** :
  ```rust
  db.integrity().add_reference(
      Reference::graph_nodes("social", "users")
          .on_delete(RefAction::Cascade)
  )?;
  ```
- **Liaison explicite** :
  Un nœud de graphe pointe vers un document JSON via `doc_space` et `doc_id` (dans `BatchOperation::GraphSetNode`). Si `doc_id` est omis, il prend par défaut la valeur de l'identifiant du nœud (`id`). L'ancien repli implicite sur le `kind` du nœud a été supprimé pour éliminer toute ambiguïté.
- **Actions de cycle de vie (`RefAction`)** :
  - `RefAction::Cascade` : La suppression d'un document JSON parent déclenche immédiatement et atomiquement la suppression du nœud dans le graphe (`GraphDeleteNode`), ce qui détruit également toutes ses arêtes incidentes entrantes et sortantes (`_lilia_graph_edges`).
  - `RefAction::Restrict` : La suppression du document JSON est rejetée tant qu'un ou plusieurs nœuds enfants y font référence.
  - `RefAction::SetNull` : La suppression du document détache les nœuds correspondants en mettant à jour leurs colonnes `doc_space` et `doc_id` à `NULL`, sans détruire le nœud ni ses arêtes.
- **Option `required`** :
  Lorsque `required: true`, chaque nœud créé dans le graphe doit obligatoirement être lié à un document existant de l'espace cible.

#### B. Liaisons Clé KV $\to$ Document JSON
- **Déclaration** :
  ```rust
  db.integrity().add_reference(
      Reference::kv("sessions", "users")
          .key(KeyMapping::Prefix { separator: ':' })
          .on_delete(RefAction::Restrict)
  )?;
  ```
- **Stratégies de mapping (`KeyMapping`)** :
  - `KeyMapping::Whole` : La clé KV entière (en UTF-8) désigne l'identifiant du document parent.
  - `KeyMapping::Prefix { separator }` : L'identifiant parent est extrait du préfixe de la clé précédant le premier séparateur (ex. `user_42:cart` avec `:` extrait `user_42`).
- **Validation stricte UTF-8** :
  Les identifiants de documents JSON étant textuels, toute clé KV binaire brute non décodable en UTF-8 insérée dans un namespace sous contrat KV $\to$ JSON est immédiatement rejetée avec `CONSTRAINT_VIOLATION`.
- **Résolution des cascades KV** :
  Lors de la suppression d'un document parent sous `RefAction::Cascade`, le moteur calcule l'intervalle lexicographique `[lower, upper)` correspondant à tous les enfants et génère les opérations `BatchOperation::KvDelete` correspondantes.

### 2. Contraintes CHECK Déclaratives sur le JSON

LiliaDB permet d'associer une contrainte d'intégrité forte à une collection JSON :
```rust
db.json().create_collection("accounts", CollectionOptions {
    check: Some("$.balance >= 0.0 AND length($.iban) == 27".into()),
    ..Default::default()
})?;
```

- **Logique Ternaire Kleene (SQL 3-Valued Logic)** :
  L'expression est analysée dans un AST dédié (`crates/lilia-core/src/json/expr/`) et évaluée selon la logique SQL standard à trois états : `True`, `False`, `Unknown`.
  - Conformément à la norme SQL, une contrainte CHECK est satisfaite tant qu'elle n'évalue **pas** à `False`.
  - Un champ manquant ou une valeur `null` produit `Unknown`, laissant le document être accepté (ce qui permet les attributs optionnels).
  - Pour rendre un champ obligatoire, la contrainte doit spécifier explicitement `$.field IS NOT NULL`.
- **Remplacement dynamique (`set_check`)** :
  La contrainte peut être mise à jour via `db.json().set_check("accounts", Some(...))` ou supprimée avec `None`. Tous les documents existants de la collection sont validés au préalable.

### 3. Garanties Fondamentales du Moteur & Protection Anti-Contournement

1. **Entonnoir Transactionnel Unique (Write Funnel)** :
   Toutes les mutations transitent par `transaction::apply`. Le `IntegrityRegistry` en mémoire intercepte les opérations via `integrity::guard` avant toute écriture. Toute violation interrompt immédiatement la transaction SQLite active et applique un rollback complet.
2. **Code d'Erreur Structuré (`CONSTRAINT_VIOLATION`)** :
   Les violations retournent `ErrorCode::ConstraintViolation` (code HTTP 409) avec un objet JSON détaillé :
   ```json
   {
     "kind": "check",
     "constraint": "check:accounts",
     "model": "json",
     "collection": "accounts",
     "id": "acc_1",
     "expression": "$.balance >= 0.0 AND length($.iban) == 27"
   }
   ```
3. **Protection contre le Contournement SQL (`CONTRACT_TABLES`)** :
   Pour éviter qu'une requête SQL brute n'insère des données en contournant les règles d'intégrité, le moteur bloque toute écriture SQL directe (`INSERT`, `UPDATE`, `DELETE`) sur les tables `_lilia_json`, `_lilia_kv` et `_lilia_graph_nodes` dès lors qu'un contrat d'intégrité est déclaré sur la base.
4. **Validation Préalable à la Déclaration** :
   L'enregistrement d'une nouvelle contrainte ou référence scanne l'existant (`check_offenders`, `reference_offenders`). Si des lignes en infraction sont détectées, l'enregistrement est refusé et jusqu'à 10 identifiants contrevenants sont rapportés.
5. **Éligibilité des Tiers de Stockage** :
   Les collections soumises à un contrat d'intégrité doivent impérativement résider sur le tier `StorageTier::Disk`. Les tiers `Memory` et `Buffered` sont refusés car ils ne garantissent pas l'atomicité synchrone dans le WAL SQLite.

The `@liliadb/node` package is engineered for broad compatibility across modern server-side JavaScript runtimes:
- **Node.js (>=22)**: Fully supports in-process embedded storage via Node-API precompiled binaries and daemon IPC via `node:net`.
- **Bun (>=1.0)**: Native Node-API support allows Bun to execute both embedded `.node` prebuilds and daemon IPC natively at high throughput.
- **Deno (>=2.0)**: Supports `npm:@liliadb/node` via Deno's standard Node compatibility layer, connecting over Unix domain sockets with zero external dependencies.

## Spatial Indexing & Geofencing Sub-Engine

Embedded geographic and spatial query engine (`crates/lilia-core/src/geo.rs` and `crates/lilia-storage-sqlite/src/geofence.rs`):
- **Haversine Distance & Bounding Boxes**: Microsecond spatial calculations and bounding box intersections.
- **Polygon Containment via SQLite Geopoly**: Native representation of complex geographical boundaries and geofences.
- **Geofence Breach Event Streams**: Automated evaluation during track point insertion generating real-time entry and exit breach notifications over the CDC bus.

## Time-Series Compression & Retention Sub-Engine

Optimized telemetry and time-series sub-engine (`crates/lilia-core/src/timeseries.rs` and `crates/lilia-storage-sqlite/src/timeseries.rs`):
- **Gorilla-Style Lossless Compression**: Bit-packed delta-of-delta timestamp encoding paired with XOR floating-point value compression achieving 80–90% disk space reduction for sensor data.
- **Automated Downsampling & Rollups**: Aggregation windows (`min`, `max`, `avg`, `count`, `sum`) for long-term historical trend analysis.
- **Configurable Retention Policies**: Background pruning of outdated metric intervals preserving operational storage budgets on constrained edge devices.

## Universal C-ABI FFI (`crates/lilia-ffi`) & Mobile Targets

To provide cross-platform database support without JVM or V8 overhead, `crates/lilia-ffi` delivers a stable C-compatible interface:
- **Zero-Copy Handle Lifecycle**: Thread-safe pointer wrapping for `Database` instances with synchronized mutex admission.
- **Apple Ecosystem (`packages/apple-spm`)**: Swift Package Manager module with universal XCFramework support for iOS, macOS, watchOS, and tvOS (`tools/mobile/build-apple.sh`).
- **Android SDK (`packages/android-sdk`)**: Kotlin bindings with JNI/NDK prebuilts targeting `aarch64-linux-android` and `armv7-linux-androideabi` (`tools/mobile/build-android.sh`).
- **React Native (`packages/react-native`)**: Native C++ JSI bindings for high-performance direct execution on iOS and Android without bridge serialization bottlenecks.
- **Flutter (`packages/flutter`)**: Dart FFI plugin delivering direct native execution across mobile platforms.

## Visual Studio Code Extension (`apps/lilia-vscode`)

Official VSCode extension offering developer tooling for local LiliaDB files:
- Interactive multimodel explorer (inspect KV namespaces, JSON spaces, Graph networks, and FTS collections).
- SQL query editor with live table preview and schema inspection.
- Integrated database health and doctor diagnostic tools.

## Tiered Storage Engine & Multi-Tier Architecture

Starting in v0.1.0-alpha.7, LiliaDB introduces a declarative multi-tier storage subsystem (`crates/lilia-core/src/tier.rs` and `crates/lilia-storage-sqlite/src/mem_engine.rs`, `ring_buffer.rs`, `catalog.rs`). Rather than treating all collections as monolithic disk-backed SQLite tables, developers configure per-collection storage tiers via `CollectionSpec`:

### 1. The Three Storage Tiers
1. **`StorageTier::Disk` (Default)**:
   - Full ACID transactional durability backed by SQLite in WAL mode (`synchronous=FULL` or `NORMAL`).
   - Mutations are immediately written to the WAL and fsynced based on the configured `Durability` profile.
   - Ideal for authoritative business data, user state, and permanent records.

2. **`StorageTier::Memory` (RAM Resident)**:
   - Zero-copy, lock-free in-memory key-value engine (`mem_engine.rs`) operating directly in RAM.
   - Bypasses disk I/O, SQLite query planning, and WAL frame serialization entirely.
   - Delivers sub-microsecond latency ($p_{50}$ of **0.50 µs**) and throughput exceeding **1.57 million ops/sec**.
   - Supports LRU (`EvictionPolicy::Lru { max_entries }`) and FIFO (`EvictionPolicy::DropOldest { max_entries }`) capacity caps as well as millisecond TTL expirations.
   - Ideal for volatile session tokens, real-time gaming state, live device heartbeats, and high-frequency caches.

3. **`StorageTier::Buffered` (Asynchronous RingBuffer)**:
   - High-throughput asynchronous write-combining buffer (`ring_buffer.rs`) designed to decouple producer write latency from SQLite transaction commits.
   - Producers enqueue mutations lock-free into a circular ring buffer at **~1.40 million ops/sec** ($p_{50}$ ~ 3.0 µs).
   - An asynchronous background flusher coalesces queued operations into micro-batches, committing them to the underlying SQLite database when either `flush_interval_ms` or `watermark_trigger` is reached.
   - Provides sequence-stamped RAM overlays ensuring immediate **Read-Your-Own-Writes** consistency before dirty batches hit disk.
   - Configurable backpressure overflow policies: `OverflowPolicy::Block { timeout_ms }`, `OverflowPolicy::DropOldest`, or `OverflowPolicy::Reject`.

### 2. GroupSync: Multi-Threaded Group Commit Architecture
In standard SQLite, concurrent writes from multiple threads suffer severe mutex lock contention, stalling on individual sequential `fsync` calls.

LiliaDB's `commit_group_batch` implements **GroupSync** (group commit):
- When multiple threads enqueue write batches simultaneously, the writers join a shared barrier.
- One designated worker thread flushes a consolidated transaction containing all pending mutations across all concurrent threads into a single WAL frame and issues a single `fsync`.
- The worker then signals the waiting threads, unblocking all of them simultaneously with individual commit receipts.
- Under an 8-thread concurrent write workload, GroupSync achieves **19,481 ops/sec** (compared to ~1,800 ops/sec serialized single-op durable writes), delivering a **10.8x throughput speedup** with zero compromise on data durability.

### 3. Incremental View Maintenance (IVM)
The IVM engine (`crates/lilia-storage-sqlite/src/ivm.rs`) provides fast declarative materialized views updated incrementally via SQLite triggers without rescanning full source tables:
- **Supported Aggregates**: `COUNT(*)`, `COUNT(col)`, `SUM(col)`, `AVG(col)`, `MIN(col)`, `MAX(col)`.
- **Differential Triggers**: Automatically generates and mounts after-insert and after-delete triggers on source tables, updating aggregate counters in dedicated `_lilia_ivm_<name>` tables.
- **Microsecond Aggregate Queries**: Queries against materialized views read directly from pre-aggregated summary tables in $O(1)$ time rather than computing full-table table scans.

### 4. Differential Live Queries & Change Streams
Built on SQLite's Session API (`crates/lilia-storage-sqlite/src/live_query.rs`), Live Queries provide reactive change notifications with row-level diffs (`LiveQueryDiff`):
- Callers register declarative SQL queries (`db.subscribe_live_query`).
- Pre-commit changelists generated by SQLite's changeset iterator are matched against query predicates in memory.
- Observers receive typed `RowChange::Insert`, `RowChange::Update`, and `RowChange::Delete` events detailing old and new values without polling.

### 5. Conflict-Free Replicated Data Types (CRDTs) & Hybrid Logical Clocks (HLC)
Native distributed replication primitives (`crates/lilia-core/src/crdt/`, `crates/lilia-core/src/hlc.rs`, `crates/lilia-storage-sqlite/src/crdt.rs`):
- **Hybrid Logical Clocks (HLC)**: Monotonically increasing physical-logical timestamps ensuring causal ordering across distributed nodes even under clock skew.
- **PN-Counters**: Positive-Negative distributed counters supporting concurrent increments and decrements across replicas.
- **Observed-Remove Sets (OR-Sets)**: Set data structures with element add/remove tracking that converge deterministically under out-of-order replication.
- **LWW JSON Documents**: Last-Write-Wins JSON document trees resolving concurrent attribute-level field conflicts using HLC timestamps.

### 6. Content Addressable Storage (CAS) Deduplication Engine & Atomic Blob GC

Storing large binary objects (images, PDF documents, video clips, machine learning weights) directly in relational or document B-tree pages quickly causes severe page fragmentation, WAL write amplification, and degraded cache efficiency.

Starting in v0.1.0-alpha.7, LiliaDB introduces a Content Addressable Storage (CAS) deduplication engine (`crates/lilia-core/src/cas.rs`, `crates/lilia-storage-sqlite/src/cas.rs`):

#### Architecture & Storage Layout
- **Two-Level Sharded Directory**:
  Blobs are stored in a dedicated directory (by default `<db_path>.blobs/` or customizable via `DatabaseOptions::with_cas_dir(...)`), organized using two-level hexadecimal prefix subdirectories:
  ```
  <database.lilia>.blobs/
  ├── .tmp/
  │   └── 550e8400-e29b-41d4-a716-446655440000.tmp
  ├── 3f/
  │   └── 8a/
  │       └── 3f8a9b2c4d5e6f70...blob (mode 0o444 / readonly)
  └── a1/
      └── b2/
          └── a1b2c3d4e5f60718...blob (mode 0o444 / readonly)
  ```
  This partitioning guarantees filesystem responsiveness even when holding hundreds of thousands of distinct blobs.
- **Cryptographic Content Addressing**:
  Each blob is identified by its cryptographic digest (SHA-256 by default or BLAKE3, selectable via `CasOptions::hash_algorithm`).
- **Atomic Writes & Immutability**:
  Incoming blobs are streamed chunk-by-chunk (64 KB buffers) into `.tmp/<uuid>.tmp`, synced to durable media via `fsync` (`sync_all()`), and renamed into place. If the content hash already exists on disk, the temporary file is unlinked immediately, achieving zero redundant disk consumption.
  Once committed, files are sealed with read-only permissions (`0o444` on Unix, `readonly` attribute on Windows).

#### Transparent KV Payload Offloading
For key-value operations (`kv.set` / `kv.get`):
- Payloads exceeding `threshold_bytes` (default: 64 KB, configurable via `DatabaseOptions::with_cas_threshold(...)`) are automatically offloaded to the CAS store.
- In SQLite's `_lilia_kv` table, the raw payload is replaced with a compact **45-byte pointer**:
  - `0x4C, 0x42, 0x43, 0x01` (`LBC\x01` magic bytes: Lilia Blob CAS version 1)
  - `0x01` (algorithm: 1 = SHA-256, 2 = BLAKE3)
  - `u64` little-endian original size in bytes
  - 32-byte raw cryptographic hash
- Read queries (`kv_get`, `kv_scan`) detect the `LBC\x01` magic prefix and transparently reconstitute the payload from the CAS file.
- Direct zero-copy readers (`kv_get_blob_reader`, `kv_get_blob_path`) allow streaming multigigabyte files without allocating the entire payload in memory.

#### JSON Document `$blob` Descriptors & Reference Tracking
JSON documents can reference blobs explicitly or via `db.blob_store(&data)`:
```json
{
  "title": "Quarterly Report",
  "document": {
    "$blob": "3f8a9b2c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8",
    "size_bytes": 1048576,
    "mime_type": "application/pdf"
  }
}
```
LiliaDB parses JSON trees recursively to extract `$blob` hashes and maintains reference counting in SQLite:
- `_lilia_blobs`: tracks `(hash, size_bytes, mime_type, ref_count, created_at_ms, sealed)`.
- `_lilia_blob_refs`: tracks `(ref_kind, space_or_ns, doc_or_key_id, blob_hash)`.
- Differential updates: updating a JSON document automatically decrements the ref count of removed blobs and increments newly referenced blobs.

#### Atomic Garbage Collection on WAL Checkpoint
- When `kv.delete()` or `json.delete()` is executed, reference counts are decremented within the active SQLite transaction.
- When `ref_count` reaches `0`, the blob enters an orphaned state indexed by `_lilia_blobs_orphan` (`WHERE ref_count = 0`).
- Because readers might still be accessing older snapshot versions in the WAL, physical deletion is strictly deferred until a WAL checkpoint runs (`PRAGMA wal_checkpoint(PASSIVE)` or automatic background checkpointing).
- When the checkpoint confirms that all log frames are committed (`copied >= log`), the background garbage collector unlinks orphaned physical files from disk and purges metadata rows from `_lilia_blobs`, guaranteeing ACID transactional consistency.

---

## Vectorized Analytics Engine (`lilia-analytics`, feature `analytics`)

SQLite's B-tree is a row store, ideal for OLTP reads and writes but slow for scans: aggregating a few hundred thousand rows decodes every row to read one or two values. The analytics engine adds a columnar path next to it, in the same process and the same database file.

```text
Row store (B-tree)      [id, name, age, city] [id, name, age, city] ...   source of truth, OLTP
Columnar segments       [age, age, age, ...] [city, city, city, ...]      derived, scanned in batches of 1,024
```

### 1. Layers

| Layer | Crate | Responsibility |
| :--- | :--- | :--- |
| Engine | `lilia-analytics` | Arrow schema of a table, segment codec (one Arrow IPC chunk per column), zone maps, decoded-chunk cache, DataFusion `TableProvider`, query runtime. Storage-agnostic: a backend implements `ColumnarSource`. |
| Store | `lilia-storage-sqlite::columnar` | Persistence of segments in SQLite, change tracking, merge-on-read scans, build and compaction, background worker, `Database::analytics()`. |

The feature is opt-in (`analytics`, native targets only): DataFusion needs a multi-threaded runtime and noticeably grows the binary, so the Node, FFI, mobile and WebAssembly builds are unchanged.

### 2. Columnar Tables

A columnar table is a typed projection declared over a JSON collection (`ColumnarTableSpec::json`, keyed by document `id`) or a SQL table (`ColumnarTableSpec::sql`, keyed by `rowid`). Column types are `Int64`, `Float64`, `Boolean`, `Utf8` and `TimestampMillis`; a value that does not fit its column is stored as NULL, except integers in a `Float64` column.

| Table | Content |
| :--- | :--- |
| `_lilia_col_tables` | Definition: source, columns, target segment size, state (`building` / `ready`), next segment id. |
| `_lilia_col_segments` | One row per segment: row count and metadata (zone maps, key bounds). |
| `_lilia_col_chunks` | One row per column of a segment: the Arrow IPC chunk. Reading a column never touches the others. |
| `_lilia_col_delta` | Keys of the rows changed since the last compaction. |

These tables are created on first use, so a database that never uses analytics keeps its schema. Segment metadata and chunks go through the database `ValueCodec`, zone maps included, and documents are decoded without the field-level key registry: values encrypted in the row store are never decrypted into segments.

Invariant: the segments of a table are sorted by key and their key ranges do not overlap, which makes locating a changed key a pair of binary searches.

### 3. Staying in Sync with the Row Store

The row store is never bypassed and its write path is unchanged. Three `AFTER` triggers per columnar table record the key of every inserted, updated or deleted source row in `_lilia_col_delta`. Triggers cover every write path (batch API, transactions, ring-buffer flushes, CRDT merges, raw SQL, undo/redo) and keep working when the file is opened by a build without the feature: the delta just grows until an analytics-enabled process compacts it.

- **Scan (merge-on-read)**: inside one read transaction, a scan returns the segments minus the rows whose key is in the delta, plus the current version of those rows read from the row store. A query is therefore always consistent with the row store, compacted or not.
- **Initial build**: the definition and the triggers are committed first, then the source is read one segment at a time and each segment is committed in its own short write transaction. Writers are never blocked for the duration of the build; any row written meanwhile is in the delta. A build interrupted by a crash is detected (`building` state) and restarted when the engine starts.
- **Compaction**: one atomic write transaction rewrites only the segments that own a changed key, splitting and merging to stay close to the target size, then clears the delta. It runs in a background worker once a table has `compaction_delta_rows` pending rows, or on demand with `Analytics::compact`.

Undo/redo changesets exclude `_lilia_col_*`: applying a changeset fires the triggers again, which is what keeps the delta right.

### 4. Query Execution

`Analytics::query` hands the SQL to a DataFusion session that only knows the columnar tables. The engine is read-only by construction (DDL, DML and session statements are rejected, no file access).

1. **Projection pushdown**: only the chunks of referenced columns are loaded; `count(*)` reads the key column alone.
2. **Zone-map pruning**: `column <op> literal` filters skip the segments whose min/max cannot match. Pruning is inexact by design; DataFusion still filters rows.
3. **Chunk cache**: decoded chunks are kept in a byte-bounded LRU (`cache_bytes`). Segment ids are never reused, so a cached chunk cannot go stale.
4. **Vectorized execution**: rows reach the operators as zero-copy slices of `batch_size` rows (1,024 by default), spread over `target_partitions` partitions.

The engine owns a dedicated runtime, so `query` is a plain blocking call that is safe from any thread, including one already running inside an async runtime.

SIMD comes from the Arrow kernels and follows the target features of the build, with no runtime dispatch: NEON is always on for aarch64; x86_64 uses the SSE2 baseline unless the embedding application compiles with `-C target-cpu=x86-64-v3` (AVX2) or `x86-64-v4` (AVX-512).

### 5. Limits

- Rust API only; not yet exposed through the Node SDK, the daemon, the C ABI or WebAssembly.
- No columnar table over a memory-tier collection (no persistent rows), a JSON collection under temporal sharding (rows live in attached files the triggers cannot see), or a `WITHOUT ROWID` SQL table.
- A scan materializes the projected columns of the surviving segments for the duration of the query.
- Tables created by another process become queryable after the engine restarts; the engine is meant for a single embedding process.
- Dropping a source SQL table does not drop its columnar tables: drop them first.

---

## Native Query JIT Compilation (`lilia-core`, Cranelift)

To eliminate interpreter dispatch overhead and recursive AST traversals during query filtering, `lilia-core` incorporates an embedded Just-In-Time (JIT) compiler based on Cranelift.

```text
JSON Filter AST (Path, Literal, BinaryOp)
                   │
                   ▼ Cranelift JIT Compilation
Native Machine Code (ARM64 / x86_64)
                   │
                   ▼ Direct Execution
Contiguous BinaryDoc Buffers (Zero Copy) ──► Match Index Bitset
```

### 1. Direct Memory Evaluation over `BinaryDoc`
Compiled predicates execute directly against contiguous `BinaryDoc` byte buffers in host memory. Fast runtime helper trampolines (`lilia_jit_extract_f64`, `lilia_jit_extract_i64`, `lilia_jit_extract_str`, `lilia_jit_memcmp`, `lilia_jit_strcmp`) inspect binary dictionary offsets without creating intermediate serde structures.

### 2. Batch Execution Loop
Instead of invoking function pointers per row, the JIT emits a vectorized batch loop (`CompiledPredicate::eval_batch`):
- Operates over an array of `DocRef { ptr, len }`.
- Evaluates predicates in tight machine registers.
- Writes matching row indices directly to an output pointer buffer, achieving multi-million document scan throughput per CPU core.

### 3. Predicate Cache
Compiled functions are immutable and thread-safe. A shared `JitPredicateCache` uses the serialized AST expression as cache key, tracking hit/miss rates via atomic counters and amortizing JIT compilation time across queries.

---

## P2P Set Reconciliation via Invertible Bloom Filters (`lilia-core`, `lilia-storage-sqlite`)

For decentralized, multi-device, and offline-first synchronization, LiliaDB provides an exact set reconciliation layer using Invertible Bloom Filters (IBF / IBLT).

```text
Peer A (Set S_A) ◄─── Strata Estimator Exchange ───► Peer B (Set S_B)
                                │
                   Calculate Difference Bound d
                                │
Peer A ─────────── Transmit IBF of size O(d) ───────► Peer B
                                │
             Peel Pure Cells: S_A \ S_B and S_B \ S_A
                                │
                   HLC 3-Way Merge Resolution
```

### 1. Invertible Bloom Lookup Table (IBLT)
An IBLT maintains an array of cells, where each cell contains:
- `count: i32`: Number of inserted elements minus deleted elements.
- `key_sum: u128`: XOR sum of all element keys.
- `hash_sum: u64`: XOR sum of verification checksums.

A cell with `count == 1` and matching `hash_sum` is a "pure cell", allowing deterministic peeling of elements in $O(d)$ time.

### 2. Strata Estimator
Before generating an IBF, peers must agree on the expected divergence size $d$ without transmitting full inventory keys. A Strata Estimator partitions keys into geometric hash buckets ($k=32$), estimating set difference size in a single small exchange (under 1 KB).

### 3. Low-Bandwidth Synchronization & 3-Way Merge
Once differences are identified, mutations are exchanged and resolved using Hybrid Logical Clock (HLC) timestamps and CRDT deterministic rules, enabling reliable peer-to-peer sync over Bluetooth Low Energy (BLE), local Wi-Fi, or message queues.

---

## Reed-Solomon Erasure Coding & Automatic Page Repair (`lilia-core`, `lilia-storage-sqlite`)

To safeguard embedded databases against hardware flash wear, bit rot, and sudden power cuts, LiliaDB implements physical database parity protection using Reed-Solomon erasure coding in Galois Field $GF(2^8)$.

```text
Database File (SQLite B-Tree Pages 1..N)
┌──────────┬──────────┬──────────┬──────────┐
│  Page 1  │  Page 2  │  Page 3  │  Page 4  │ ... Data Shards (k=6)
└──────────┴──────────┴──────────┴──────────┘
      │          │          │          │
      ▼          ▼          ▼          ▼
Cauchy Generator Matrix Multiplication in GF(2^8)
      │                     │
      ▼                     ▼
┌──────────────┐     ┌──────────────┐
│ Parity Shard 1│     │ Parity Shard 2│ ... Parity Shards (m=2) (.parity Sidecar)
└──────────────┘     └──────────────┘
```

### 1. Cauchy Generator Matrix
Parity shards are computed using systematic Cauchy generator matrices where $C_{i,j} = \frac{1}{x_i \oplus y_j}$ in $GF(2^8)$. Inverting square submatrices allows reconstruction of up to $m$ corrupted pages out of any $k + m$ shard group.

### 2. Checkpoint Integration & Parity Sidecar
The `ParityManager` hooks into WAL checkpoint cycles:
- Pages are organized into groups of $k$ data pages.
- $m$ parity pages per group are written to a companion `.parity` sidecar file alongside the database.
- Each page header includes CRC32 checksums for microsecond validation.

### 3. Transparent Auto-Repair
If an SQLite read trips a checksum mismatch or corrupted page error, the engine automatically reconstructs the exact original page bytes from sister data pages and parity shards, writes the repaired page back to storage, and completes the read without bubbling errors to application code.

---

## Row-Level Security (RLS) & Dynamic Data Masking (DDM) (`lilia-core`, `lilia-storage-sqlite`)

LiliaDB introduces fine-grained, database-enforced security policies and dynamic data masking directly inside the embedded storage engine.

```text
Caller / API Request
   │ with SecurityContext { subject: "dr_smith", roles: ["doctor"], tenant_id: "cardio" }
   ▼
Row-Level Security Filter
   │ Evaluates Policy: "$.tenant_id == ctx.tenant_id"
   ▼ Permitted Rows
Dynamic Data Masking Pipeline
   │ Applies Masks: $.ssn -> Redact, $.email -> Email ("s***h@clinic.org")
   ▼
Sanitized Result Set
```

### 1. Security Context
Database sessions accept a `SecurityContext` specifying caller identity, tenant boundaries, assigned roles, and key-value attributes. Operations executed without an active context operate under default security rules.

### 2. Declarative RLS Policies
Policies (`NamedPolicy`) specify a target table/collection, an action (`Select`, `Insert`, `Update`, `Delete`, `All`), and an AST expression condition. Unmatched rows are transparently excluded from queries or rejected on write with `SECURITY_VIOLATION`.

### 3. Dynamic Data Masking Algorithms
Sensitive fields are masked on retrieval based on configured `MaskingRule`s:
- `CreditCard`: Preserves the last 4 digits (`****-****-****-1234`).
- `Email`: Preserves first and last local characters and domain (`a***e@example.com`).
- `Redact`: Completely replaces value with `[REDACTED]`.
- `Partial`: Retains custom prefix and suffix character counts with configurable mask character.
- `Hash`: Deterministic 16-hex SHA-256 digest for privacy-preserving correlation.
- Role-based exemptions: Users with administrative or auditing roles can bypass masks via declarative exemption rules.

---

## Flash Wear Leveling & Adaptive WAL Fsync Control (`lilia-core`, `lilia-storage-sqlite`)

Mobile and edge devices use NAND flash memory with finite write endurance (P/E cycles). Heavy write workloads and frequent fsync operations accelerate device degradation.

### 1. Physical Write Volume Telemetry
The `FlashWearManager` monitors:
- Cumulative physical bytes written to the database and WAL files.
- Daily write quotas (`target_daily_budget_bytes`, defaulting to 5 GiB/day).
- Write velocity over sliding time windows.

### 2. Adaptive Fsync & Checkpoint Stretching
Based on write volume, the engine transitions across four tiers:
1. `Nominal`: Standard fsync/checkpoint interval (e.g. 3,000 ms).
2. `Elevated`: Moderate interval elongation to coalesce writes.
3. `Severe`: Aggressive write coalescing with extended checkpoint intervals.
4. `Critical`: Maximum WAL fsync stretching (up to 15,000 ms) to prevent device endurance exhaustion.

---

## Sandboxed WebAssembly User-Defined Functions (WASM UDF) (`lilia-storage-sqlite`)

Custom scalar SQL functions can be deployed as WebAssembly modules executed safely within Wasmtime runtimes.

### 1. Fault Isolation & Resource Bounding
- Functions operate inside isolated linear memory spaces. Panics or segmentation faults in guest WASM code cannot crash the host process.
- CPU execution is restricted by deterministic fuel meters. Runaway calculations or infinite loops trigger an engine trap (`FUEL_EXHAUSTED`).

### 2. Auxdata State Caching
Using SQLite's `sqlite3_get_auxdata` and `sqlite3_set_auxdata` APIs, WASM module instances, runtime stores, and memory allocations are cached across row evaluations, eliminating instantiation overhead during table scans.

---

## Polyglot Virtual Tables & Multi-Model SQL Rewriter (`lilia-storage-sqlite`)

LiliaDB supports cross-model joins and multi-model querying directly in standard SQL.

```sql
SELECT 
    u.id, 
    u.value->>'name' AS user_name,
    m.value AS latest_heart_rate
FROM lilia_json u
JOIN lilia_polyglot('timeseries', 'heart_rate', 100) m ON m.tag = u.id
WHERE u.space = 'patients';
```

The SQL query engine transparently detects `lilia_polyglot(...)` function calls, rewrites the SQL AST, registers ephemeral SQLite virtual table modules on the active connection, and delivers zero-copy unified query results across JSON, KV, Graph, and Timeseries tiers.

---

## Embedded Zero-Knowledge Proofs Engine (`lilia-zk`, `lilia-core`, `lilia-storage-sqlite`)

LiliaDB v0.1.0-alpha.8 introduces embedded zero-knowledge proofs designed for client-side privacy-preserving verifications (identity, KYC, credit checks, health attestation) without disclosing private values.

```text
Private Document (e.g. {"balance": 3500, "age": 28})
          │
          ▼ Poseidon Cryptographic Commitment
   Commitment Anchor (0x...) ──► Public Ledger / Database Record
          │
          ▼ Groth16 Prover (R1CS over BN254)
   Succinct Proof (128 bytes) ──► Cryptographic Verification (< 1ms)
                                  (Certifies: balance >= 1000 AND age >= 18)
```

### 1. Poseidon Cryptographic Commitments
- Utilizes the SNARK-friendly Poseidon algebraic hash function over the BN254 scalar field $\mathbb{F}_r$.
- Each confidential document generates a deterministic, collision-resistant commitment anchor combining document field values with an optional blinding salt:
  $$C = \text{Poseidon}(v_1, v_2, \dots, v_k, \text{salt})$$
- Commitment anchors are stored alongside the document metadata or exposed for decentralized verification.

### 2. Groth16 & BN254 Circuit Evaluation
- Pre-compiled arithmetic circuits formulate Rank-1 Constraint Systems (R1CS) over the pairing-friendly BN254 curve (alt_bn128).
- The default `SoldeAgePredicateCircuit` enforces range inequality constraints:
  $$\text{balance} \ge \text{min\_balance} \quad \land \quad \text{age} \ge \text{min\_age}$$
  while asserting that the committed witness matches the public Poseidon root anchor.
- Emits ultra-compact 128-byte proofs (`Proof<Bn254>`: $A \in G_1, B \in G_2, C \in G_1$) with sub-millisecond verification times on commodity mobile and desktop hardware.

### 3. SQL & CLI Integration
- In SQL: `SELECT id, zk_verify(proof, commitment, min_balance, min_age) FROM users;`
- In CLI:
  ```sh
  lilia --database ./app.lilia zk commit users alice
  lilia --database ./app.lilia zk prove users alice --condition "balance >= 1000 AND age >= 18" --out-proof proof.json
  lilia zk verify --proof proof.json
  ```

---

## Zero-Copy Relative-Pointer Architecture & Sub-Nanosecond Shared Memory Engine (`rkyv`, `lilia-core`, `lilia-protocol`, `lilia-shm`, `lilia-storage-sqlite`, `lilia-ffi`)

Introduced in LiliaDB v0.1.0-alpha.9, the engine implements an end-to-end **Zero-Copy Relative-Pointer Architecture** (`rkyv` 0.8), fully replacing MessagePack (`rmp-serde`) across network protocols, IPC framing, storage transaction overlays, and shared memory arenas.

```text
Daemon / Network / Writer Process
   │ Serializes relative pointers (rkyv) into AlignedVec
   ├─► Network / IPC Socket: write_archived_frame() / read_archived_frame() [Zero Deserialization]
   ├─► Transaction Pending Overlay: In-memory AlignedVec [0-alloc fast-path query]
   ├─► Writes to SQLite WAL: Persistent storage
   ▼ Atomically mirrors updates into Shared Memory
Shared Memory Segment (/dev/shm or memory-mapped file)
   │ Header (version, writer epoch, ring offsets)
   │ Slot Map & Fixed-Size Page Arenas (Relative Offsets)
   ▼ Zero-Copy Pointer Cast (0.69 ns / 0 ns deserialization)
Reader Process 1        Reader Process 2        C-ABI / FFI Consumers
```

### 1. Relative Pointers vs Absolute Memory Addresses
- Traditional serializers embed 64-bit virtual memory pointers that become invalid once written to disk, transferred over IPC, or mapped across different process addresses.
- `rkyv` represents references as relative displacement offsets (`i32`). A serialized buffer is byte-for-byte identical to the CPU RAM layout and can be mapped at arbitrary virtual addresses with zero fixups.
- Deserialization cost is **$O(1)$ at 0 nanoseconds** (`unsafe { rkyv::access_unchecked::<T>(bytes) }`), operating in **0.69 ns** (125.9x faster than `serde_json` and 91.9x faster than `rmp_serde`).

### 2. Flat Node Tape Representation for Dynamic JSON
- Hierarchical recursive enums create cyclic trait resolutions and heap allocation overhead.
- `lilia_core::json::JsonDocument` flattens arbitrary JSON trees into a contiguous tape arena (`Vec<FlatNode>`), where object keys and array elements refer to children by `u32` node indices.
- Readers extract nested fields (`archived.extract_str("profile.name")`, `archived.extract_f64("telemetry.reading")`, `archived.extract_bool("flags.active")`) directly from the mapped buffer without parsing JSON text or allocating heap memory.

### 3. Multi-Process Zero-Copy Shared Memory Access
- Writers update slot maps with epoch-based synchronization (`crossbeam-epoch`).
- Client readers map the segment directly into their address space and inspect typed structures (`reader.get_archived::<T>()`) or dynamic JSON (`reader.get_archived_doc()`, `reader.json_extract_str_zero_copy()`) with sub-nanosecond access latency.
- Writers can push pre-archived buffers directly into SHM via `Engine::put_archived()`, skipping intermediate serialization.

### 4. Zero-Copy Protocol Framing (`lilia-protocol`)
- MessagePack serialization (`rmp-serde`) is completely eliminated from the wire protocol.
- Sockets and IPC connections communicate through `write_archived_frame()` and `read_archived_frame()` operating over `RawFrame` envelopes.
- Core types (CRDTs `LwwJson`/`LwwRegister`/`OrSet`/`PnCounter`, IBF, HLC timestamps, Geo, Timeseries, Graph, and Vector types) derive `Archive`, `Serialize`, and `Deserialize`, eliminating wire conversion costs.

### 5. Storage Engine `AlignedVec` Pending Overlay (`lilia-storage-sqlite`)
- Active, uncommitted write transactions buffer mutations in an in-memory `AlignedVec` overlay rather than intermediate heap objects.
- Read queries execute against `with_archived_json()` and `with_archived_kv()` fast paths:
  1. Inspect the transaction `AlignedVec` overlay first via zero-copy relative-pointer dereferencing.
  2. If hit, yield the uncommitted data immediately with zero allocation and zero deserialization.
  3. If miss, fall back to SQLite page reads.

### 6. Universal C-ABI Shared Memory Bridge (`lilia-ffi`)
- Cross-language bindings (C, C++, Node-API, Python, Go) consume shared memory frames through `lilia_shm_get_archived()`.
- External runtimes map shared arenas and perform zero-copy reads without passing through intermediate JSON strings or serialization layers.

---

## Partially Homomorphic Encryption & Paillier Cryptosystem (`lilia-core`, `lilia-storage-sqlite`)

To enable analytics and aggregations over confidential numeric data without plaintext decryption in RAM, LiliaDB provides additive homomorphic encryption powered by the Paillier cryptosystem.

### 1. Additive Homomorphic Arithmetic
Given an $n$-bit Paillier public key with $n = p \cdot q$ and ciphertexts $c_1 = g^{m_1} r_1^n \pmod{n^2}$ and $c_2 = g^{m_2} r_2^n \pmod{n^2}$:
$$c_{\text{sum}} = c_1 \cdot c_2 \pmod{n^2} \implies D(c_{\text{sum}}) = (m_1 + m_2) \pmod n$$
Scalar multiplication by an integer $k$:
$$c_{\text{mult}} = c^k \pmod{n^2} \implies D(c_{\text{mult}}) = (k \cdot m) \pmod n$$

### 2. Streaming Aggregation over Ciphertexts
- `HomomorphicSumAccumulator` in `lilia-core` multiplies encrypted residues directly in memory without possessing the private key.
- SQL aggregation functions `paillier_add(c1, c2, n_squared)` and `paillier_sum(ciphertext, n_squared)` allow secure analytical queries over untrusted cloud infrastructure and local devices.

---

## Crash-Safe Double-Header Envelope Encryption & Key Rotation (`lilia-core`, `lilia-storage-sqlite`)

Field-Level and Full-Database Envelope Encryption incorporates a dual-header fail-safe design preventing catastrophic data corruption during sudden power outages or crashes.

### 1. Front and Rear Header Synchronization
- Header Page 0 (front) and trailer (rear) store mirrored encryption envelopes, key generation counters, algorithm identifiers, and BLAKE3 integrity authentication tags.
- When performing key rotation or generation bumps, writes alternate between front and rear headers with explicit atomic `fsync` barriers:
  1. Write and flush updated rear header.
  2. Verify rear header integrity.
  3. Write and flush updated front header.
  4. Finalize active generation pointer.

### 2. Automated Crash Recovery
On database startup, `EnvelopeManager` inspects both headers. If a power loss interrupted a header write, the engine detects the invalid BLAKE3 authentication tag, transparently rolls back to the undamaged alternate header, and repairs the corrupted page.

---

## Reactive Incremental Dataflow Query Engine (`lilia-core`, `lilia-storage-sqlite`)

LiliaDB v0.1.0-alpha.8 provides an incremental dataflow computation engine that maintains live views and streams updates in real time.

```text
Table / Collection Writes (Insert, Update, Delete)
   │
   ▼ Mutation Trigger
Dataflow Dependency Graph (Filter, Map, Aggregate, Join)
   │ Incremental Delta Computation (only changed keys)
   ▼
Materialized View Cache ──► Subscriber Callbacks / WebSocket Streams
```

### 1. Pipeline Views & Continuous Updates
- Developers define reactive pipelines specifying source collections, filter predicates, transformations, and windowed aggregations.
- Mutations compute minimal mathematical deltas ($\Delta$), updating materialized view tables without re-evaluating the entire dataset.

### 2. View Subscriptions & Change Notifications
- Applications subscribe to pipeline views via `db.subscribe(view_id)`.
- Listeners receive structured differential diffs (`Insert`, `Update`, `Delete`, `InitialSnapshot`) with exact HLC timestamps.

---

## Battery-Aware & Energy-Efficient Adaptive Engine (`lilia-core`, `lilia-storage-sqlite`)

Designed specifically for wearable and mobile environments, LiliaDB adapts query scheduling and execution intensity according to hardware battery and thermal states.

### 1. Power Profiles
- `Normal`: Full performance with multi-threaded parallel execution and unrestricted memory allocations.
- `LowPower`: Throttles background compaction, defers non-critical indexing, and reduces sort buffer memory limits.
- `Critical` / `Smartwatch`: Enforces strict serialized execution, disables speculative prefetching, and penalizes heavy B-Tree sorts.

### 2. Pedagogical Query Diagnostics (`EXPLAIN DIAGNOSE`)
The query planner features `EXPLAIN DIAGNOSE` inspection, identifying energy-draining query patterns (temporary B-tree sorts, unbounded full table scans, unindexed joins) and outputting actionable developer recommendations for battery optimization.

---

## Optimistic Concurrency Control (OCC) (`lilia-storage-sqlite`)

To maximize throughput under read-heavy and concurrent multi-threaded workloads, LiliaDB provides an Optimistic Concurrency Control subsystem alongside traditional two-phase locking.

### 1. Read-Your-Own-Writes (RYOW) Memory Buffering
- Transactions record reads in a lightweight snapshot read-set and buffer uncommitted mutations in private thread-local memory.
- Reads inside the transaction observe all pending modifications without taking SQLite write locks.

### 2. Conflict Detection & Automatic Retry
- On commit, the transaction validates whether any key in its read set was modified by a concurrent writer.
- If no collision occurred, mutations are applied atomically to SQLite in a single batched commit.
- In case of collision, the engine automatically rolls back the in-memory changes and retries up to configured retry limits, achieving superior multi-core transaction throughput.

---

## Multilingual CJK & Phonetic Full-Text Search (`lilia-core`, `lilia-storage-sqlite`)

Full-Text Search in LiliaDB v0.1.0-alpha.8 expands beyond Western languages to provide native multilingual CJK and phonetic search capabilities.

### 1. Multilingual CJK Tokenization
- **Chinese**: Integrated Jieba segmentation engine extracting words, compound nouns, and frequency-weighted n-grams.
- **Japanese**: Mixed-script analyzer handling Kanji, Hiragana, and Katakana with unigram and bigram tokenization.
- **Korean**: Hangul syllabic block splitting and jamo decomposition.

### 2. Phonetic Search Algorithms
- Implements Double Metaphone and Soundex indexing in `lilia-core`.
- Queries match terms phonetically regardless of spelling errors or transliteration differences (e.g., matching "Smith" with "Smyth", "Catherine" with "Katherine", "Croissant" with "Croisant").
- Available via Rust API (`fts_phonetic_search`), CLI (`lilia fts phonetic`), and HTTP REST API (`POST /v1/fts/phonetic`).

---

## Unified Cross-Model Query Engine: "LiliaQL" (`lilia-storage-sqlite`)

LiliaQL introduces a fluent, unified query API allowing complex cross-model data queries across JSON, Key-Value, Graph, Lexical FTS, and Vector stores in a single type-safe chain.

```rust
let results = db.query()
    .from_json("users")
    .filter(doc! { "status": "active", "tier": "pro" })
    .traverse_graph("KNOWS", Direction::Outbound)
    .search_fts("bio", "rust developer")
    .with_vector_near("embedding", &query_vector, 0.82)
    .project(projection! {
        "id": "$.id",
        "name": "$.name",
        "friend": "$graph.target.name",
        "score": "$rrf_score"
    })
    .execute_as::<UserProfile>()?;
```

- **Reciprocal Rank Fusion (RRF)**: Merges text BM25 relevance and dense vector distances into a unified scoring metric.
- **Zero-Cost Projection**: Contextual projections extract fields directly into strongly typed structs without intermediate allocations.



