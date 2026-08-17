#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
dist_dir="$script_dir/../dist/marksheet-wasm"

for artifact in \
  "$dist_dir/web/worker.js" \
  "$dist_dir/web/protocol.js" \
  "$dist_dir/pkg/marksheet_wasm.js" \
  "$dist_dir/pkg/marksheet_wasm_bg.wasm"
do
  if [[ ! -s "$artifact" ]]; then
    echo "viewer build is missing required Wasm asset: $artifact" >&2
    exit 1
  fi
done

echo "Viewer build contains the worker host, protocol, JavaScript glue, and Wasm module."
