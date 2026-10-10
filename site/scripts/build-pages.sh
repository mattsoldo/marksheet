#!/usr/bin/env bash
# Assembles the GitHub Pages site: the landing page at the root and the full
# viewer beneath app/. Both use relative asset URLs, so the result works at a
# domain root or under a project path such as /marksheet/.
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
site_dir=$(cd -- "$script_dir/.." && pwd)
viewer_dir=$(cd -- "$site_dir/../viewer" && pwd)

(cd -- "$site_dir" && npm run build)
(cd -- "$viewer_dir" && MARKSHEET_VIEWER_BASE=./ npm run build)

rm -rf -- "$site_dir/dist/app"
cp -R -- "$viewer_dir/dist" "$site_dir/dist/app"

test -s "$site_dir/dist/index.html"
test -s "$site_dir/dist/marksheet-wasm/pkg/marksheet_wasm_bg.wasm"
test -s "$site_dir/dist/app/index.html"
test -s "$site_dir/dist/app/marksheet-wasm/pkg/marksheet_wasm_bg.wasm"
if grep -q 'src="/assets/' "$site_dir/dist/app/index.html"; then
  echo "viewer was built with an absolute base; expected relative asset URLs" >&2
  exit 1
fi
echo "Pages site assembled in $site_dir/dist"
