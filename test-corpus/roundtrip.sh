#!/usr/bin/env bash
# Round-trip stability check over the corpus.
#
#   xlsx --> A.ms --> B.xlsx --> B.ms      and then asserts   A.ms == B.ms
#
# Why not compare the original .xlsx against B.xlsx? Marksheet is deliberately
# a subset of XLSX: themes, charts, drawings, docProps, most styling, macros
# and pivot tables have no representation, and every corpus import reports
# `fidelity: "lossy"` for exactly that reason. Byte-identity against the
# original is therefore the wrong bar -- it would only ever be met by a format
# that is a superset of XLSX, which Marksheet does not claim to be.
#
# What *must* hold is that the surviving subset is a fixed point: whatever
# made it into Marksheet on the first import has to survive an export and
# re-import unchanged. A mismatch is a real defect -- a value mangled on each
# pass, a formula rewritten differently, a style or identifier that churns --
# and this catches those without demanding lossless XLSX fidelity.
#
# Files that cannot import at all are skipped, not failed: several corpus
# entries are deliberately defective (corrupted packages, fuzzer crash inputs,
# a zip bomb, an encrypted workbook), and refusing them is correct behavior.
# ./verify.sh is the report that covers those.
#
# Usage:  ./test-corpus/roundtrip.sh [path-to-marksheet-binary]
# Exits non-zero if any importable file fails to round-trip.
set -uo pipefail
cd "$(dirname "$0")/.."

BIN="${1:-./target/debug/marksheet}"
if [ ! -x "$BIN" ]; then
  echo "no marksheet binary at $BIN -- run: cargo build -p marksheet-cli" >&2
  exit 2
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

stable=0
unstable=0
skipped=0
failures=()

for f in test-corpus/xlsx/* test-corpus/real-world/xlsx/*/*; do
  [ -e "$f" ] || continue
  name="$(basename "$f")"

  # Pass 1: xlsx -> A.ms
  if ! timeout 180 "$BIN" convert --to marksheet --output "$TMP/a.ms" "$f" >/dev/null 2>&1; then
    skipped=$((skipped + 1))
    continue
  fi
  # Pass 2: A.ms -> B.xlsx
  if ! timeout 180 "$BIN" convert --to xlsx --output "$TMP/b.xlsx" "$TMP/a.ms" >/dev/null 2>&1; then
    unstable=$((unstable + 1))
    failures+=("$name: export to xlsx failed")
    rm -f "$TMP/a.ms" "$TMP/b.xlsx"
    continue
  fi
  # Pass 3: B.xlsx -> B.ms
  if ! timeout 180 "$BIN" convert --to marksheet --output "$TMP/b.ms" "$TMP/b.xlsx" >/dev/null 2>&1; then
    unstable=$((unstable + 1))
    failures+=("$name: re-import of exported xlsx failed")
    rm -f "$TMP/a.ms" "$TMP/b.xlsx" "$TMP/b.ms"
    continue
  fi

  if cmp -s "$TMP/a.ms" "$TMP/b.ms"; then
    stable=$((stable + 1))
  else
    unstable=$((unstable + 1))
    failures+=("$name: .ms differs after xlsx round-trip ($(diff <(head -200 "$TMP/a.ms") <(head -200 "$TMP/b.ms") | head -4 | tr '\n' ' '))")
  fi
  rm -f "$TMP/a.ms" "$TMP/b.xlsx" "$TMP/b.ms"
done

echo "round-trip stable:   $stable"
echo "round-trip unstable: $unstable"
echo "skipped (no import): $skipped"
if [ ${#failures[@]} -gt 0 ]; then
  echo
  echo "failures:"
  printf '  %s\n' "${failures[@]}"
  exit 1
fi
