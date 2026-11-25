# Security policy

## Supported versions

LiliaDB is in alpha. Security fixes go to `main` and ship in the next release; earlier alphas are
not patched. Upgrade to the latest release to get them.

## Reporting a vulnerability

Do not open a public issue, pull request or discussion for a vulnerability.

Report it privately through GitHub:
[Security → Report a vulnerability](https://github.com/shaynlink/liliadb/security/advisories/new).
Only the maintainers see the report.

Include what you can of:

- the affected component (engine, daemon, a binding, the CLI, `lilia-zk`) and version or commit;
- the platform, and the options the database was opened with;
- the steps, input or file that reproduces it, and what an attacker gains;
- a fix or a mitigation, if you have one.

The maintainers confirm the report, work on a fix in a private fork, and publish a GitHub security
advisory (with a CVE when one applies) when the release that fixes it is out. Reporters are
credited in the advisory unless they ask not to be.

## Security model

What LiliaDB protects against, and what it leaves to the platform.

- **Embedded use** (Node, Bun, Deno, React Native, Flutter, Swift, Kotlin, the browser): the
  database runs with the privileges of the application. Its files are protected by the file
  system's permissions only.
- **Encryption at rest**: LiliaDB does not encrypt its files. Rely on the platform: FileVault,
  BitLocker, LUKS, iOS Data Protection, Android file-based encryption, or the browser's storage
  isolation.
- **Integrity**: CRC32C checksums on every block detect accidental corruption (a torn write, a
  failing drive), not deliberate modification by someone who can write the files.
- **The daemon** (`lilia-daemon`) isolates the users of one machine from each other: user-only
  sockets, named pipes and token files, peer credential checks, a token compared in constant time
  on every request. It does not isolate a user from root, nor from other processes of the same
  user. The HTTP gateway binds loopback; `--development` allows other addresses and is not for
  production. See [the daemon's security](docs/architecture.md#daemon).
- **Untrusted requests**: frames are limited to 16 MiB and results are bounded. A request that
  makes the engine panic, read out of bounds, or exhaust memory or time beyond these limits is a
  vulnerability.
- **`lilia-zk`** (zero-knowledge proofs) has not been audited. Do not rely on its proofs where a
  forged proof would be costly.

## Verification

- **Fuzzing** (`fuzz/`, nightly in CI): the decoders of the daemon's requests (`rkyv` and
  `MessagePack`), sequences of operations run on a database, and databases whose files were
  damaged, opened and read in full. Run a target with
  `cargo +nightly fuzz run <target>` from `fuzz/` after `node fuzz/seeds.mjs`.
- **Power loss**: the engine's crash tests cut the power at random moments over an in-memory file
  system that keeps what a real drive would, and check that every acknowledged commit survives.

## Supply chain

`cargo deny check` (see [deny.toml](deny.toml)) refuses Rust dependencies with a known
vulnerability, an unsound API, an unknown source or a license outside the allowed list; CI runs it
on every pull request. Dependabot opens security updates for every ecosystem of the repository.
