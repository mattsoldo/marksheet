#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo_root"

python3 -m json.tool integrations/mcp/tool-schema.json >/dev/null
python3 -m json.tool integrations/mcp/response-schema.json >/dev/null
python3 -m json.tool tests/harness/manifest.json >/dev/null
python3 -m json.tool tests/harness/live-results.json >/dev/null
python3 tests/harness/validate_responses.py
python3 tests/harness/run.py
