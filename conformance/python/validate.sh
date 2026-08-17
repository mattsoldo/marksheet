#!/usr/bin/env bash
# Validate the stdlib-only independent projection consumer and checked outputs.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

python3 -m unittest discover -s conformance/python -p 'test_*.py' -v
python3 conformance/python/generate_projections.py --check
