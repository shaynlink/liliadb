# LiliaDB Custom Extension Guide & Lifecycle

This directory contains a complete, runnable custom extension plugin for LiliaDB (`lilia-plugin-custom-extension`).

It demonstrates:
- How to author a custom LiliaDB extension in Rust conforming to the **C-ABI v1.0** specification.
- How to implement and expose pluggable storage capabilities (such as the `Codec` capability for transparent on-disk encryption/obfuscation).
- How to target both **Native dynamic libraries** (`.dylib` / `.so` / `.dll`) and **WebAssembly WASI** (`wasm32-wasip1`).
- The complete cryptographic distribution lifecycle:
  1. Compiling the extension (`cargo build --release`).
  2. Generating author Ed25519 signing keys (`lilia plugin keygen`).
  3. Packaging the artifact with its canonical manifest (`lilia plugin pack`).
  4. Cryptographically signing the manifest (`lilia plugin sign`).
  5. Verifying package integrity and cryptographic signatures (`lilia plugin verify`).
  6. Adding author keys to the local trust store (`lilia plugin trust add`).
  7. Installing the package into the local plugin registry (`lilia plugin install`).
  8. Using the extension seamlessly with the standalone CLI, background Daemon, and embedded database.

---

## Architecture: Native C-ABI vs. WebAssembly Sandbox

LiliaDB plugins can be deployed in two modes:

| Dimension | Native C-ABI (`.dylib` / `.so` / `.dll`) | WebAssembly Sandbox (`.wasm`) |
| :--- | :--- | :--- |
| **Runtime** | Operating system dynamic loader (`dlopen` / `LoadLibrary`) | Hardened Wasmtime / WASI sandbox |
| **Isolation** | Shared host address space | Strict 32-bit linear memory boundary |
| **Fault Tolerance** | Host crash on panic / segfault | Traps caught safely; instance evicted without crashing host |
| **Resource Limits** | Unbounded OS process limits | Linear memory quota (`--plugin-memory-limit-mb`) & CPU fuel metering (`--plugin-fuel-limit`) |
| **Compilation** | `cargo build --release` | `cargo build --target wasm32-wasip1 --release` |

---

## Extension Code Structure

### 1. Declaring Capabilities (`lilia_plugin_v1`)

Every LiliaDB plugin exports a root C-ABI function named `lilia_plugin_v1()` that returns a `PluginDescriptorV1`:

```rust
use lilia_plugin_api::{Capability, CodecDescriptorV1, PluginDescriptorV1};

static PLUGIN_NAME: &[u8] = b"lilia-custom-codec\0";
static CODEC_NAME: &[u8] = b"xor-rotate-codec\0";

static CODEC_DESCRIPTOR: CodecDescriptorV1 = CodecDescriptorV1 {
    name: CODEC_NAME.as_ptr().cast(),
    init: Some(codec_init),
    encode: Some(codec_encode),
    decode: Some(codec_decode),
};

#[no_mangle]
pub extern "C" fn lilia_plugin_v1() -> PluginDescriptorV1 {
    PluginDescriptorV1::with_codec(
        PLUGIN_NAME.as_ptr().cast(),
        Capability::Codec.bit(),
        &raw const CODEC_DESCRIPTOR,
    )
}
```

### 2. Implementing the Codec Hooks

The `Codec` capability provides three hooks:
- `codec_init`: Optional initialization with a custom encryption key.
- `codec_encode`: Transforms/encrypts plaintext before it is committed to SQLite or WAL pages.
- `codec_decode`: Inverts/decrypts ciphertext when read back by the engine.

```rust
#[no_mangle]
pub unsafe extern "C" fn codec_encode(
    input: *const u8,
    input_len: usize,
    out: *mut u8,
    out_cap: usize,
    out_len: *mut usize,
) -> i32 {
    // Encrypt input into out buffer...
    // Return 0 on success, 1 if out_cap is insufficient, or negative error code.
}

#[no_mangle]
pub unsafe extern "C" fn codec_decode(
    input: *const u8,
    input_len: usize,
    out: *mut u8,
    out_cap: usize,
    out_len: *mut usize,
) -> i32 {
    // Decrypt input into out buffer...
}
```

### 3. WebAssembly Memory Allocators

For WebAssembly targets, the guest module exports standard allocators so the host can transfer buffers across linear memory:

```rust
#[no_mangle]
pub extern "C" fn lilia_alloc(size: i32) -> i32 { ... }

#[no_mangle]
pub extern "C" fn lilia_dealloc(ptr: i32, size: i32) { ... }
```

---

## Step-by-Step Packaging & Usage Walkthrough

### 1. Compile the Library
```bash
# Build the native shared library (.dylib on macOS, .so on Linux, .dll on Windows)
cargo build --release -p lilia-plugin-custom-extension
```

### 2. Generate Author Signing Keypair
```bash
lilia plugin keygen --name demo-author --out-dir ./keys
# Created keys:
#   ./keys/demo-author.key (private Ed25519 key, mode 0600)
#   ./keys/demo-author.pub (public base64 key)
```

### 3. Package the Extension
```bash
# Determine platform library extension (.dylib, .so, or .dll)
LIB_EXT="dylib" # or "so" / "dll"

lilia plugin pack ./target/release/liblilia_plugin_custom_extension.${LIB_EXT} \
  --name lilia-plugin-custom-extension \
  --version 0.1.0 \
  --capability codec \
  --out-dir ./dist/custom-extension-pkg
```

This creates:
- `./dist/custom-extension-pkg/manifest.json` (canonical JSON metadata with SHA-256 digest)
- `./dist/custom-extension-pkg/liblilia_plugin_custom_extension.${LIB_EXT}`

### 4. Sign the Package
```bash
lilia plugin sign ./dist/custom-extension-pkg --key ./keys/demo-author.key
# Produces ./dist/custom-extension-pkg/manifest.sig
```

### 5. Verify Package Integrity
```bash
lilia plugin verify ./dist/custom-extension-pkg
```

### 6. Trust the Author Key and Install
```bash
# Read author public key and register it in the user's trust store
PUBKEY=$(cat ./keys/demo-author.pub)
lilia plugin trust add "${PUBKEY}" --yes

# Install into the local plugin store
lilia plugin install ./dist/custom-extension-pkg

# List installed plugins
lilia plugin list
```

---

## Using the Extension

### With the Lilia CLI (`lilia`)
Pass `--plugin` pointing to either the installed plugin name, the package directory, or the raw shared library:

```bash
# 1. Store confidential KV secret
lilia --database ./secure.lilia --plugin lilia-plugin-custom-extension \
  kv set credentials api_key "sk_live_very_secret_token_12345"

# 2. Read back transparently (decrypted in memory)
lilia --database ./secure.lilia --plugin lilia-plugin-custom-extension \
  kv get credentials api_key

# 3. Inspect raw SQLite on disk: notice cleartext is NOT visible!
sqlite3 ./secure.lilia "SELECT value FROM _lilia_kv WHERE namespace = 'credentials'"
```

### With the Background Daemon (`lilia-daemon`)
```bash
lilia-daemon \
  --database ./secure.lilia \
  --plugin lilia-plugin-custom-extension \
  --endpoint ./secure.lilia.sock \
  --token-file ./secure.lilia.token
```

### With Embedded Node.js / JavaScript
```javascript
import { Database } from "@liliadb/node";

// When opening the database, transparent encryption is activated via encryptionKey
// or connecting to a daemon loaded with the plugin.
const db = await Database.open({
  path: "./secure.lilia",
  encryptionKey: "my-custom-key-phrase",
});

await db.kv.set("credentials", new TextEncoder().encode("token"), new TextEncoder().encode("secret"));
const entry = await db.kv.get("credentials", new TextEncoder().encode("token"));
console.log(new TextDecoder().decode(entry.value)); // "secret"
await db.close();
```

---

## Automated Test Runner

You can execute the entire lifecycle automatically using the included Node.js test runner:

```bash
node examples/custom-extension/test-extension.mjs
```
