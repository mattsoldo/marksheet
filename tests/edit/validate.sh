#!/usr/bin/env bash
set -euo pipefail

fixture_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

FIXTURE_ROOT="$fixture_root" python3 - <<'PY'
import json
import os
from pathlib import Path

root = Path(os.environ["FIXTURE_ROOT"])
manifest = json.loads((root / "manifest.json").read_text(encoding="utf-8"))
assert manifest["version"] == 1
assert manifest["cases"]

known_kinds = {"commit", "reject", "no_op", "rebase_conflict", "semantic_equivalence"}
seen_ids = set()

def source(name):
    path = root / name
    assert path.is_file(), f"missing fixture source: {name}"
    return path.read_bytes()

def apply_patches(original, patches):
    last_end = 0
    for patch in patches:
        assert set(patch) >= {"start", "end", "replacement"}, patch
        start, end = patch["start"], patch["end"]
        assert isinstance(start, int) and isinstance(end, int)
        assert 0 <= start <= end <= len(original), patch
        assert start >= last_end, f"patches overlap or are unsorted: {patches}"
        last_end = end
    result = original
    for patch in reversed(patches):
        start, end = patch["start"], patch["end"]
        result = result[:start] + patch["replacement"].encode("utf-8") + result[end:]
    return result

for entry in manifest["cases"]:
    case_id = entry["id"]
    assert case_id not in seen_ids, f"duplicate fixture id: {case_id}"
    seen_ids.add(case_id)
    assert entry["kind"] in known_kinds, entry
    fixture = json.loads((root / entry["fixture"]).read_text(encoding="utf-8"))

    if entry["kind"] == "semantic_equivalence":
        left, right = source(fixture["left"]), source(fixture["right"])
        assert left != right, "equivalence fixture needs distinct source spellings"
        assert fixture["expected"]["equivalent"] is True
        continue

    before = source(fixture["before"])
    patches = fixture["patches"]
    if entry["kind"] in {"reject", "no_op"}:
        assert patches == [], f"{case_id} must not patch source"
    if entry["kind"] == "reject":
        assert fixture["expected"]["outcome"] == "rejected"
        continue
    if entry["kind"] == "rebase_conflict":
        current = source(fixture["current"])
        assert any(
            current[p["start"]:p["end"]].decode("utf-8") != p["expected"]
            for p in patches
        ), "conflict fixture must change at least one precondition span"
        continue

    after = source(fixture["after"])
    assert apply_patches(before, patches) == after, f"patch plan mismatch: {case_id}"
    inverse = fixture.get("inversePatches")
    if inverse is not None:
        assert apply_patches(after, inverse) == before, f"inverse mismatch: {case_id}"

print(f"validated {len(manifest['cases'])} editing fixtures")
PY
