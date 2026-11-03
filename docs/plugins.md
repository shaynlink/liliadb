# Plugins & WebAssembly Sandboxing

LiliaDB provides an extensible plugin system supporting two execution runtimes:
1. **Native Dynamic Libraries (C-ABI)**: High performance, direct host memory access. Trusted, privileged code with Ed25519 signature verification.
2. **WebAssembly Sandboxed Modules (WASI via Wasmtime)**: Hardened, fault-isolated execution with linear memory boundaries, strict memory quotas, CPU fuel metering, and process-crash immunity.

---

## Architecture: Native C-ABI vs. WebAssembly Sandbox

```
                      +---------------------------------------+
                      |       LiliaDB Daemon / CLI Engine     |
                      +---------------------------------------+
                                          |
                   -------------------------------------------------
                   |                                               |
                   v                                               v
        [ Native Plugin Loader ]                        [ Wasmtime WASI Sandbox ]
        - dlopen / LoadLibrary                          - 32-bit Isolated Linear Memory
        - Direct host memory                            - Configurable Memory Quota (64 MB)
        - Native C-ABI v1.0                             - CPU Fuel Metering (10M Units)
        - Cryptographic Ed25519                         - Fault-Isolated Instance Pool
        - Highly trusted code                           - Crash-Proof (No Segfaults)
```

| Dimension | Native C-ABI (`.dylib` / `.so` / `.dll`) | WebAssembly Sandbox (`.wasm`) |
| :--- | :--- | :--- |
| **Runtime** | OS dynamic loader (`libloading`) | `wasmtime` 43.0 + `wasmtime-wasi` (p1) |
| **Isolation** | Shared address space | Strict linear memory sandbox |
| **Fault Tolerance** | Host crash on panic / segfault | Traps caught safely; instance evicted |
| **Resource Limits** | Unbounded OS process limits | `--plugin-memory-limit-mb`, `--plugin-fuel-limit` |
| **Portability** | OS and arch-specific binary | Universal cross-platform bytecode |
| **Target Triples** | `x86_64-*`, `aarch64-*` | `wasm32-wasip1`, `wasm32-wasi`, `wasm32-unknown-none` |

---

## WebAssembly Sandboxing Runtime

The WebAssembly sandbox isolates untrusted or third-party plugins using Wasmtime:

### 1. Memory Isolation & Out-of-Bounds Protection
Every WASM plugin executes within its own isolated 32-bit linear memory space. Out-of-bounds pointer reads or writes trigger a WebAssembly trap that is mapped to `LiliaError::Storage` or `LiliaError::Corrupt`, completely preventing host memory corruption or segmentation faults.

### 2. Linear Memory Quotas
Memory allocation inside the sandbox is strictly bounded by `wasmtime::ResourceLimiter`:
- Default quota: **64 MB** per plugin instance.
- Configurable via `--plugin-memory-limit-mb <MB>` on `lilia-daemon`.
- Any attempt by the guest plugin to grow memory beyond this quota fails cleanly with a memory allocation error without starving the host daemon.

### 3. CPU Fuel Metering (Anti-DoS)
To prevent infinite loops, algorithmic denial of service, or rogue computation:
- Default execution budget: **10,000,000 fuel units** per plugin invocation.
- Configurable via `--plugin-fuel-limit <UNITS>` on `lilia-daemon`.
- When fuel is exhausted, the call aborts immediately with a `LiliaError::Timeout` trap.

### 4. Instance Pooling & Fault Eviction
Plugins are executed via a thread-safe `WasmInstancePool`:
- Hot instances are reused across requests for near-zero startup overhead.
- If a guest plugin instance panics, traps, or corrupts its internal state, it is immediately discarded and replaced with a fresh instance on the next request. Sibling connections and the daemon core remain fully unaffected.

---

## Daemon Security Policies

The daemon provides flags to configure plugin runtime behavior and enforce security constraints:

```sh
lilia-daemon \
  --database ./app.lilia \
  --plugin ./dist/my-wasm-plugin \
  --require-sandboxed-plugins \
  --plugin-memory-limit-mb 128 \
  --plugin-fuel-limit 20000000
```

- `--require-sandboxed-plugins`: **Fail-closed security policy**. Rejects any native dynamic library before `dlopen` is attempted. Only WebAssembly (`.wasm`) plugins are allowed to load.
- `--plugin-memory-limit-mb <MB>`: Maximum linear memory permitted for sandboxed plugins (default: 64).
- `--plugin-fuel-limit <UNITS>`: Execution fuel limit per invocation (default: 10,000,000).

---

## Packaging & Distribution

A LiliaDB plugin package contains:
- `manifest.json`: Canonical JSON descriptor specifying name, version, ABI, capabilities, target, and SHA-256 digest.
- `manifest.sig`: Ed25519 signature covering `manifest.json`.
- `library`: The compiled binary (native dynamic library or `.wasm` module).

### Supported Targets

**Native:**
- `aarch64-apple-darwin`
- `x86_64-apple-darwin`
- `aarch64-unknown-linux-gnu`
- `x86_64-unknown-linux-gnu`
- `x86_64-pc-windows-msvc`

**WebAssembly (Sandboxed):**
- `wasm32-wasip1` (Recommended)
- `wasm32-wasi`
- `wasm32-unknown-none`

### CLI Packaging Workflow

```sh
# 1. Generate an Ed25519 signing keypair
lilia plugin keygen --out-dir ./keys --name my-author

# 2. Package a compiled WebAssembly module
lilia plugin pack ./target/wasm32-wasip1/release/my_plugin.wasm \
  --name lilia-plugin-custom \
  --version 0.1.0 \
  --target wasm32-wasip1 \
  --capability codec \
  --out-dir ./dist/lilia-plugin-custom-package

# 3. Cryptographically sign the package with your private key
lilia plugin sign ./dist/lilia-plugin-custom-package --key ./keys/my-author.key

# 4. Verify package integrity and signature against trusted keys
lilia plugin verify ./dist/lilia-plugin-custom-package

# 5. Install into the local plugin store
lilia plugin install ./dist/lilia-plugin-custom-package

# 6. List installed plugins (displays target and sandbox compatibility)
lilia plugin list
```

---

## Guest WebAssembly ABI Specification

WebAssembly plugins export standard memory management functions along with their respective capability hooks:

### Memory Allocation
```c
// Allocate memory inside guest linear memory
void* lilia_alloc(uint32_t len);

// Free previously allocated memory
void lilia_dealloc(void* ptr, uint32_t len);
```

### Codec Plugin Exports (`--capability codec`)
```c
// Encrypt plaintext using key, returning 0 on success
uint32_t codec_encode(
    const uint8_t* key_ptr, uint32_t key_len,
    const uint8_t* pt_ptr,  uint32_t pt_len,
    uint8_t** out_ptr_buf,  uint32_t* out_len_buf
);

// Decrypt ciphertext using key, returning 0 on success
uint32_t codec_decode(
    const uint8_t* key_ptr, uint32_t key_len,
    const uint8_t* ct_ptr,  uint32_t ct_len,
    uint8_t** out_ptr_buf,  uint32_t* out_len_buf
);
```

### SQL Plugin Exports (`--capability sql`)
```c
// Validate SQL query before execution, returning 0 on success
uint32_t sql_validate_query(
    const uint8_t* sql_ptr, uint32_t sql_len,
    uint8_t** out_err_ptr_buf, uint32_t* out_err_len_buf
);
```

---

## Trust & Signatures

Official release packages are signed with the embedded LiliaDB official release key. Third-party keys live in the per-user trust store:
- macOS: `~/Library/Application Support/LiliaDB/plugins/.trust`
- Linux: `$XDG_DATA_HOME/liliadb/plugins/.trust`, or `~/.local/share/liliadb/plugins/.trust`
- Windows: `%LOCALAPPDATA%\LiliaDB\plugins\.trust`

Manage trusted keys using:
```sh
lilia plugin trust add <base64-public-key> --yes
lilia plugin trust list
```

Unsigned packages fail closed by default; the CLI requires `--allow-unsigned --development`, and the daemon equivalently requires `--allow-unsigned-plugins --development`.
