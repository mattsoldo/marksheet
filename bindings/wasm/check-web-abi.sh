#!/usr/bin/env bash
# Regenerates the browser ABI declaration in a temporary directory and compares
# it with the tracked contract. This deliberately never writes generated Wasm
# or JavaScript artifacts into the source tree.
set -euo pipefail

binding_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
temp_dir=$(mktemp -d)
cleanup() {
  rm -rf -- "$temp_dir"
}
trap cleanup EXIT

cargo run --quiet --manifest-path "$binding_dir/Cargo.toml" --bin generate_protocol -- --check
cargo build --manifest-path "$binding_dir/Cargo.toml" --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir "$temp_dir" \
  "$binding_dir/target/wasm32-unknown-unknown/debug/marksheet_wasm.wasm"
diff -u "$binding_dir/marksheet_wasm.d.ts" "$temp_dir/marksheet_wasm.d.ts"
