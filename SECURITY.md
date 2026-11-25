# Security policy

## Supported versions

Only the latest `0.1.x` release is supported while LiliaDB is pre-1.0.

## Reporting

Do not open a public issue for a suspected vulnerability. Use GitHub private vulnerability
reporting for `shaynlink/lilia` and include affected version, reproduction, impact, and suggested
mitigation when available.

## Security model

### 1. Data at Rest
- **No built-in encryption**: the engine stores records unencrypted. Put a database on an
  encrypted volume (FileVault, BitLocker, LUKS, the mobile platforms' data protection) when its
  data needs encryption at rest.
- **Integrity**: every block of the engine's files carries a checksum, verified on read; a
  mismatch fails the read with `CORRUPT` instead of returning damaged data.
  `lilia doctor --deep` (`integrityCheck`) reads every stored entry back.
- **Tamper evidence**: a database opened with the ledger records every write in a Merkle
  Mountain Range; `lilia ledger prove-*` export proofs that a record or a commit is in it, which
  anyone holding a trusted root can verify, and `lilia ledger verify-db` checks the whole ledger
  against the live records.
- **Access control**: row-level security policies and masking rules apply to every read and
  write of a session, SQL included, for the caller's security context.

### 2. Filesystem & DACL Hardening (Unix & Windows Parity)
- **Unix**: All database files, IPC socket paths, and authentication token files are created with mode `0600`; directories receive mode `0700`.
- **Windows**: All database files, IPC named pipes and token paths receive a protected, restrictive DACL (`O:{sid}D:P(A;OICI;FA;;;{sid})` for directories, `O:{sid}D:P(A;;FA;;;{sid})` for files) granting access exclusively to the current process user SID. Inheritance from parent volumes is explicitly disabled (`SE_DACL_PROTECTED`), and existing broader ACLs or unauthorized account permissions are rejected on startup.

### 3. IPC & Daemon Authentication
- **Local Loopback Only**: The daemon binds strictly to local Unix domain sockets, local Windows named pipes (`\\.\pipe\...`), or local loopback HTTP (`127.0.0.1`). Remote pipes and network-exposed IPC endpoints are strictly rejected.
- **Per-Session Bearer Tokens**: Daemon interactions require a cryptographically random token generated at daemon initialization. Token files are created with private DACLs and verified on every IPC and HTTP request.

### 4. Memory, Log, and Crash Hygiene
- **Zero Sensitive Data Logging**: Secret keys, authentication tokens, and user database payloads are strictly excluded from debug logs, error messages, and metrics.
- **Request Tracing**: All errors are assigned random request UUIDs (`request_id`) for diagnostics without exposing sensitive contextual payloads.
- **Physical Crash Resilience**: Recovery replays the journal up to its last complete, checksummed record: a torn tail is discarded, and with `durability: "durable"` every acknowledged commit survives a power cut.

Before production use, keep dependencies current, back up by copying the database directory
while it is closed, and never place a database on an untrusted network filesystem.
