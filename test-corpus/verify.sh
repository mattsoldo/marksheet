#!/usr/bin/env bash
# Runs every corpus file (synthetic + real-world) through
# `marksheet convert --to marksheet` and prints a one-line-per-file result.
#
# Usage:  ./test-corpus/verify.sh [path-to-marksheet-binary]
# Exits 0 always -- this is a report, not a pass/fail gate, because several
# corpus files are *expected* to fail (deliberately corrupt, encrypted, or
# known-gap regression fixtures). Compare output against the tables in
# README.md and real-world/README.md.
set -uo pipefail
cd "$(dirname "$0")/.."

BIN="${1:-./target/debug/marksheet}"
if [ ! -x "$BIN" ]; then
  echo "no marksheet binary at $BIN -- run: cargo build -p marksheet-cli" >&2
  exit 2
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

run_set() {
  local label="$1"; shift
  local ok=0 fail=0
  echo "=== $label ==="
  for f in "$@"; do
    [ -e "$f" ] || continue
    local name; name="$(basename "$f")"
    local report="$TMP/report.json"
    timeout 240 "$BIN" convert --to marksheet --output "$TMP/out.ms" "$f" \
      > "$report" 2> "$TMP/err.txt"
    local code=$?
    if [ $code -eq 0 ]; then
      ok=$((ok + 1))
      printf 'OK        %-62s %s\n' "$name" \
        "$(python3 -c "import json;print(json.load(open('$report')).get('fidelity','?'))" 2>/dev/null)"
    elif [ $code -eq 124 ]; then
      fail=$((fail + 1))
      printf 'TIMEOUT   %-62s (240s)\n' "$name"
    else
      fail=$((fail + 1))
      local detail
      detail="$(python3 -c "
import json
try:
    r = json.load(open('$report'))
    print(r.get('outcomes',[{}])[0].get('detail','?'))
except Exception:
    print(open('$TMP/err.txt').read().strip().splitlines()[-1][:90] if open('$TMP/err.txt').read().strip() else '?')
" 2>/dev/null)"
      printf 'FAIL(%d)   %-62s %s\n' "$code" "$name" "$detail"
    fi
  done
  echo "--- $label: $ok ok, $fail fail ---"
  echo
}

run_set "synthetic" test-corpus/xlsx/*
run_set "real-world" test-corpus/real-world/xlsx/*/*
