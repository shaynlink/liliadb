#!/usr/bin/env bash
set -euo pipefail

# LiliaDB Custom Extension Lifecycle Demo Script
# Demonstrates: compilation, keygen, pack, sign, verify, trust, install, and CLI usage.

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${DIR}/../.." && pwd)"
CLI="${ROOT_DIR}/target/release/lilia"

if [[ ! -x "${CLI}" ]]; then
  CLI="${ROOT_DIR}/target/debug/lilia"
fi

if [[ ! -x "${CLI}" ]]; then
  echo "Error: lilia CLI binary not found. Please run 'cargo build -p lilia-cli' first."
  exit 1
fi

TEMP_DIR="$(mktemp -d -t lilia-extension-demo-XXXXXX)"
trap 'rm -rf "${TEMP_DIR}"' EXIT

echo "=== [1] Compiling lilia-plugin-custom-extension ==="
cargo build --release -p lilia-plugin-custom-extension

LIB_EXT="so"
if [[ "$OSTYPE" == "darwin"* ]]; then
  LIB_EXT="dylib"
elif [[ "$OSTYPE" == "msys" || "$OSTYPE" == "win32" ]]; then
  LIB_EXT="dll"
fi

COMPILED_LIB="${ROOT_DIR}/target/release/liblilia_plugin_custom_extension.${LIB_EXT}"
if [[ ! -f "${COMPILED_LIB}" ]]; then
  # Fallback to target/debug if release wasn't built
  COMPILED_LIB="${ROOT_DIR}/target/debug/liblilia_plugin_custom_extension.${LIB_EXT}"
fi

echo "Compiled plugin library: ${COMPILED_LIB}"

KEYS_DIR="${TEMP_DIR}/keys"
DIST_DIR="${TEMP_DIR}/dist/custom-pkg"
DB_PATH="${TEMP_DIR}/demo.lilia"

echo -e "\n=== [2] Generating author Ed25519 signing keypair ==="
"${CLI}" plugin keygen --out-dir "${KEYS_DIR}" --name demo-author

echo -e "\n=== [3] Packaging plugin artifact into distribution package ==="
"${CLI}" plugin pack "${COMPILED_LIB}" \
  --name lilia-plugin-custom-extension \
  --version 0.1.0 \
  --capability codec \
  --out-dir "${DIST_DIR}"

echo -e "\n=== [4] Cryptographically signing package manifest ==="
"${CLI}" plugin sign "${DIST_DIR}" --key "${KEYS_DIR}/demo-author.key"

echo -e "\n=== [5] Registering author public key in local trust store ==="
PUBKEY=$(cat "${KEYS_DIR}/demo-author.pub")
"${CLI}" plugin trust add "${PUBKEY}" --yes

echo -e "\n=== [6] Verifying signed package integrity against trusted keys ==="
"${CLI}" plugin verify "${DIST_DIR}" --output json

echo -e "\n=== [7] Writing confidential KV pair through LiliaDB with the custom plugin ==="
"${CLI}" --database "${DB_PATH}" --plugin "${COMPILED_LIB}" \
  kv set secret_vault master_key "top_secret_confidential_payload_42"

echo -e "\n=== [8] Reading transparently through LiliaDB engine ==="
"${CLI}" --database "${DB_PATH}" --plugin "${COMPILED_LIB}" \
  kv get secret_vault master_key

echo -e "\n=== [9] Inspecting SQLite raw database on disk ==="
if command -v sqlite3 &>/dev/null; then
  echo "Raw bytes stored in _lilia_kv:"
  sqlite3 "${DB_PATH}" "SELECT hex(value) FROM _lilia_kv WHERE namespace = 'secret_vault' AND key = 'master_key';"
  echo "(Notice that cleartext is not present in the stored bytes!)"
else
  echo "(sqlite3 CLI not found, skipping direct raw table inspection)"
fi

echo -e "\n=== Lifecycle demonstration completed successfully! ==="
