#!/usr/bin/env bash
# Builds lilia-wasm for the browser into packages/wasm/pkg: the module (lilia_wasm_bg.wasm) and
# its wasm-bindgen glue (lilia_wasm.js, `--target web`).
#
# Needs the wasm32-unknown-unknown target, the wasm-bindgen CLI of the crate's wasm-bindgen
# version, and a C compiler that targets wasm32 (for the zstd that DataFusion links): Homebrew's
# LLVM by default on macOS, or CC_wasm32_unknown_unknown / AR_wasm32_unknown_unknown.
#
#   tools/wasm/build.sh            # the `wasm` profile: optimized for size
#   tools/wasm/build.sh --debug    # debug, faster to build

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="${ROOT}/packages/wasm/pkg"
PROFILE=wasm
CARGO_PROFILE=(--profile wasm)
if [ "${1:-}" = "--debug" ]; then
  PROFILE=debug
  CARGO_PROFILE=()
fi

if [ -z "${CC_wasm32_unknown_unknown:-}" ]; then
  LLVM="$(brew --prefix llvm 2>/dev/null || true)"
  if [ -z "${LLVM}" ] || [ ! -x "${LLVM}/bin/clang" ]; then
    echo "error: no C compiler for wasm32: set CC_wasm32_unknown_unknown (and AR_wasm32_unknown_unknown)" >&2
    exit 1
  fi
  export CC_wasm32_unknown_unknown="${LLVM}/bin/clang"
  export AR_wasm32_unknown_unknown="${LLVM}/bin/llvm-ar"
fi

CRATE_BINDGEN="$(cargo metadata --format-version 1 --manifest-path "${ROOT}/Cargo.toml" \
  | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const p=JSON.parse(s).packages.find(p=>p.name==="wasm-bindgen");console.log(p?p.version:"")})')"
CLI_BINDGEN="$(wasm-bindgen --version | awk '{print $2}')"
if [ "${CRATE_BINDGEN}" != "${CLI_BINDGEN}" ]; then
  echo "error: the crate uses wasm-bindgen ${CRATE_BINDGEN}, the CLI is ${CLI_BINDGEN}: cargo install wasm-bindgen-cli --version ${CRATE_BINDGEN}" >&2
  exit 1
fi

cargo build -p lilia-wasm --target wasm32-unknown-unknown "${CARGO_PROFILE[@]}" --manifest-path "${ROOT}/Cargo.toml"
rm -rf "${OUT}"
wasm-bindgen --target web --out-dir "${OUT}" \
  "${ROOT}/target/wasm32-unknown-unknown/${PROFILE}/lilia_wasm.wasm"
echo "==> ${OUT}: $(du -h "${OUT}/lilia_wasm_bg.wasm" | cut -f1) of WebAssembly, $(gzip -9c "${OUT}/lilia_wasm_bg.wasm" | wc -c | awk '{printf "%.1fM", $1 / 1048576}') gzipped"
