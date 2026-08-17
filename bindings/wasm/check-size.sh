#!/usr/bin/env bash
set -euo pipefail

binding_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
artifact="$binding_dir/target/wasm32-unknown-unknown/release/marksheet_wasm.wasm"
limit_bytes=$((2 * 1024 * 1024))

cargo build --manifest-path "$binding_dir/Cargo.toml" \
  --target wasm32-unknown-unknown --release

actual_bytes=$(wc -c < "$artifact")
if (( actual_bytes > limit_bytes )); then
  echo "Wasm artifact is ${actual_bytes} bytes; budget is ${limit_bytes} bytes" >&2
  exit 1
fi

echo "Wasm artifact is ${actual_bytes} bytes (budget ${limit_bytes})"
