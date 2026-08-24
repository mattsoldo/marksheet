#!/usr/bin/env bash
# Launch the self-contained Marksheet viewer for Zed's project task runner.
set -euo pipefail

project_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$project_root/viewer"

npm run build
npm run preview -- --host 127.0.0.1 --port 4173 &
viewer_pid=$!
trap 'kill "$viewer_pid" 2>/dev/null || true' EXIT INT TERM

for _ in $(seq 1 30); do
  if curl --fail --silent --output /dev/null http://127.0.0.1:4173/; then
    open http://127.0.0.1:4173/
    wait "$viewer_pid"
    exit 0
  fi
  sleep 0.2
done

echo "Marksheet viewer did not become available at http://127.0.0.1:4173/" >&2
exit 1
