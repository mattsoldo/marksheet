#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
viewer_dir=$(cd -- "$script_dir/.." && pwd)
repository_dir=$(cd -- "$viewer_dir/.." && pwd)
binding_dir="$repository_dir/bindings/wasm"
asset_root="$viewer_dir/public/marksheet-wasm"
artifact="$binding_dir/target/wasm32-unknown-unknown/release/marksheet_wasm.wasm"

if ! command -v wasm-bindgen >/dev/null 2>&1; then
  echo "wasm-bindgen is required; see viewer/README.md" >&2
  exit 1
fi

cargo build --manifest-path "$binding_dir/Cargo.toml" \
  --target wasm32-unknown-unknown --release

# This exact directory is ignored and contains generated build inputs only.
rm -rf -- "$asset_root"
mkdir -p -- "$asset_root/pkg" "$asset_root/web"

wasm-bindgen --target web --out-dir "$asset_root/pkg" "$artifact"
cp -- "$binding_dir/web/worker.js" "$asset_root/web/worker.js"
cp -- "$binding_dir/web/protocol.js" "$asset_root/web/protocol.js"

test -f "$asset_root/pkg/marksheet_wasm.js"
test -f "$asset_root/pkg/marksheet_wasm_bg.wasm"
test -f "$asset_root/web/worker.js"
test -f "$asset_root/web/protocol.js"
