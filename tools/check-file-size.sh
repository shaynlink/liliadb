#!/usr/bin/env bash
# Keeps source files small enough to be read, edited and reasoned about
# (by humans and LLM agents) without paging through thousands of lines.
#
# Policy (line counts of tracked source files):
#   - above SOFT_LIMIT: warning, the file should be split along a module seam
#   - above HARD_LIMIT: error when the file is new or grew compared to its base
#     version; files already above the limit may stay as they are or shrink
#     (ratchet), so legacy files never block unrelated work
#
# Usage:
#   check-file-size.sh staged          check the git index against HEAD (pre-commit)
#   check-file-size.sh range <base> [tip]
#                                      check <tip> (default HEAD) against <base> (pre-push)
#
# Environment:
#   LILIA_SOFT_LINES  warning threshold (default 400)
#   LILIA_HARD_LINES  blocking threshold (default 800)
#   SKIP_FILE_SIZE=1  bypass the check

set -euo pipefail

SOFT_LIMIT="${LILIA_SOFT_LINES:-400}"
HARD_LIMIT="${LILIA_HARD_LINES:-800}"

if [ "${SKIP_FILE_SIZE:-0}" = "1" ]; then
  exit 0
fi

mode="${1:-}"
case "$mode" in
  staged)
    new_prefix=":"
    old_prefix="HEAD:"
    list_cmd=(git diff --cached --name-only -z --diff-filter=ACM)
    ;;
  range)
    base="${2:?usage: check-file-size.sh range <base> [tip]}"
    tip="${3:-HEAD}"
    new_prefix="${tip}:"
    old_prefix="${base}:"
    list_cmd=(git diff --name-only -z --diff-filter=ACM "$base" "$tip")
    ;;
  *)
    echo "usage: check-file-size.sh staged | range <base> [tip]" >&2
    exit 2
    ;;
esac

is_source() {
  case "$1" in
    node_modules/* | */node_modules/* | target/* | */target/* | */generated/* | */vendor/*) return 1 ;;
    # UniFFI bindings, generated from lilia-ffi by `uniffi-bindgen` (tools/mobile/build-*.sh)
    packages/apple-spm/Sources/Lilia/Lilia.swift | packages/android-sdk/src/main/java/uniffi/*) return 1 ;;
    *.rs | *.ts | *.tsx | *.js | *.mjs | *.dart | *.swift | *.kt | *.c | *.h) return 0 ;;
    *) return 1 ;;
  esac
}

line_count() {
  if git cat-file -e "$1" 2>/dev/null; then
    git show "$1" | wc -l | tr -d ' '
  else
    echo 0
  fi
}

errors=0
warnings=0

while IFS= read -r -d '' file; do
  is_source "$file" || continue

  new_lines=$(line_count "${new_prefix}${file}")
  [ "$new_lines" -gt "$SOFT_LIMIT" ] || continue

  old_lines=$(line_count "${old_prefix}${file}")

  if [ "$new_lines" -gt "$HARD_LIMIT" ] && [ "$new_lines" -gt "$old_lines" ]; then
    echo "error: $file has $new_lines lines (hard limit $HARD_LIMIT, was $old_lines): split it into smaller modules" >&2
    errors=$((errors + 1))
  else
    echo "warning: $file has $new_lines lines (soft limit $SOFT_LIMIT): consider splitting it" >&2
    warnings=$((warnings + 1))
  fi
done < <("${list_cmd[@]}")

if [ "$errors" -gt 0 ]; then
  echo "file-size check failed: $errors file(s) over $HARD_LIMIT lines and growing (bypass once with SKIP_FILE_SIZE=1)" >&2
  exit 1
fi
