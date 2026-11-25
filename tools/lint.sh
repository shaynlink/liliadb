#!/usr/bin/env bash
# The rules of docs/v0.2-storage-paradigm.md §5.2 that a machine checks. CI runs this script on
# every platform; run it before pushing.
#
#   tools/lint.sh
#
# The workspace lints (Cargo.toml) deny `unsafe_code` and warn on every `#[allow]` without a
# `reason`; clippy's warnings are errors here. The regression-benchmark rule of §5.2 needs the
# reference machine and is not checked by this script.
set -euo pipefail
cd "$(dirname "$0")/.."

failed=0
fail() {
  echo "error: $*" >&2
  failed=1
}

cargo fmt --all -- --check

# Every target, tests included: no warning, and every exception says why.
cargo clippy --workspace --all-targets -- -D warnings

# Outside tests: no `unwrap`/`expect`, and no `let _ =` discarding a value that must be used.
cargo clippy --workspace --lib --bins -- -D warnings \
  -D clippy::unwrap_used -D clippy::expect_used -D clippy::let_underscore_must_use

# No source file longer than 600 lines.
while read -r lines file; do
  fail "$file has $lines lines (at most 600)"
done < <(git ls-files -z --cached --others --exclude-standard -- 'crates/*.rs' 'apps/*.rs' | xargs -0 wc -l |
  awk '$2 != "total" && $1 > 600 { print $1, $2 }')

# `unsafe` only in the platform layer and the native bindings: elsewhere the workspace's
# `unsafe_code = "deny"` may not be lifted.
unsafe_crates='^crates/(lilia-io|lilia-ffi|lilia-node-native|lilia-wasm)/'
while read -r file; do
  fail "$file lifts unsafe_code outside lilia-io and the native bindings"
done < <(git grep -l --untracked -E 'unsafe_code' -- 'crates/**' 'apps/**' | grep -Ev "$unsafe_crates" || true)

exit "$failed"
