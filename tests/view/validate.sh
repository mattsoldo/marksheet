#!/usr/bin/env bash
set -euo pipefail

fixture_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

FIXTURE_ROOT="$fixture_root" python3 - <<'PY'
import json
import os
import re
from pathlib import Path

root = Path(os.environ["FIXTURE_ROOT"])
repository = root.parent.parent
manifest = json.loads((root / "manifest.json").read_text(encoding="utf-8"))

assert manifest["version"] == 1
assert manifest["protocol"] == "marksheet-view-conformance@1"
assert manifest["cases"]

kinds = {
    "workbook_view", "layer_projection", "sparse_viewport", "worker_protocol",
    "diagnostic_source", "external_change",
}
coordinate = re.compile(r"^[A-Z]+[1-9][0-9]*$")
case_ids = set()
host_only = {"max_rendered_grid_cells", "max_coordinate_probes", "writes"}
standard_layers = {
    "authored", "virtual", "calculated", "presentation", "geometry", "source_links",
}

def source_path(name):
    path = (root / name).resolve()
    assert path.is_file(), f"missing source fixture: {name}"
    assert repository in path.parents, f"source escapes repository: {name}"
    return path

def check_coordinate(value):
    assert isinstance(value, str) and coordinate.fullmatch(value), f"invalid coordinate: {value!r}"

def check_range(value):
    assert isinstance(value, str), f"range must be a string: {value!r}"
    parts = value.split(":")
    assert len(parts) in {1, 2}, f"invalid range: {value!r}"
    for part in parts:
        check_coordinate(part)

def coordinate_parts(value):
    check_coordinate(value)
    letters = value.rstrip("0123456789")
    row = int(value[len(letters):])
    column = 0
    for letter in letters:
        column = column * 26 + ord(letter) - ord("A") + 1
    return column, row

def check_in_range(value, requested):
    start, *rest = requested.split(":")
    end = rest[0] if rest else start
    column, row = coordinate_parts(value)
    start_column, start_row = coordinate_parts(start)
    end_column, end_row = coordinate_parts(end)
    assert start_column <= column <= end_column and start_row <= row <= end_row, (
        f"coordinate {value!r} lies outside requested range {requested!r}"
    )

def check_unsupported(value, allowed):
    assert isinstance(value, list), f"unsupported_assertions must be a list: {value!r}"
    assert len(value) == len(set(value)), f"duplicate unsupported assertion: {value!r}"
    assert set(value) <= allowed <= host_only, f"invalid unsupported assertion: {value!r}"

for entry in manifest["cases"]:
    assert set(entry) == {"id", "kind", "fixture"}, entry
    case_id = entry["id"]
    assert re.fullmatch(r"[a-z][a-z0-9_]*", case_id), case_id
    assert case_id not in case_ids, f"duplicate fixture id: {case_id}"
    case_ids.add(case_id)
    assert entry["kind"] in kinds, entry

    fixture_path = root / entry["fixture"]
    assert fixture_path.is_file(), f"missing fixture: {fixture_path.name}"
    fixture = json.loads(fixture_path.read_text(encoding="utf-8"))
    assert fixture["protocol"] == manifest["protocol"], fixture_path.name
    operations = fixture.get("operations")
    assert isinstance(operations, list) and operations, fixture_path.name

    if "source" in fixture:
        source_path(fixture["source"])
    for operation in operations:
        assert isinstance(operation, dict) and isinstance(operation.get("op"), str), operation
        op = operation["op"]
        if "range" in operation:
            check_range(operation["range"])
        if "expect_authored_coordinates" in operation:
            assert operation["expect_authored_coordinates"]
            for item in operation["expect_authored_coordinates"]:
                check_coordinate(item)
        for item in operation.get("expect_absent_coordinates", []):
            check_coordinate(item)
        if operation["op"] == "visible_region":
            assert operation.get("sheet") and operation.get("expect_layers")
            assert set(operation) <= {
                "op", "sheet", "range", "expect_layers", "expect_cells",
                "expect_authored_coordinates", "expect_absent_coordinates", "budget",
                "unsupported_assertions",
            }, operation
            expected_layers = operation["expect_layers"]
            assert isinstance(expected_layers, list), expected_layers
            assert len(expected_layers) == len(standard_layers), expected_layers
            assert set(expected_layers) == standard_layers, expected_layers
            for item in operation.get("expect_cells", {}):
                check_in_range(item, operation["range"])
            for item in operation.get("expect_authored_coordinates", []):
                check_in_range(item, operation["range"])
            for item in operation.get("expect_absent_coordinates", []):
                check_in_range(item, operation["range"])
            budget = operation.get("budget")
            if budget is not None:
                assert set(budget) == {"max_returned_cells", "max_rendered_grid_cells", "max_coordinate_probes"}
                assert all(isinstance(value, int) and value > 0 for value in budget.values())
                assert set(operation.get("unsupported_assertions", [])) == {
                    "max_rendered_grid_cells", "max_coordinate_probes"
                }, operation
            else:
                check_unsupported(operation.get("unsupported_assertions", []), set())
        if operation["op"] == "request":
            assert operation["worker_protocol"] == "marksheet-worker@1"
            assert isinstance(operation["request_id"], str) and operation["request_id"]
            assert isinstance(operation["revision"], int) and operation["revision"] >= 0
            if "source" in operation:
                source_path(operation["source"])
        if operation["op"] == "reply":
            assert isinstance(operation["request_id"], str) and operation["request_id"]
            assert isinstance(operation["revision"], int) and operation["revision"] >= 0
        if operation["op"] == "simulate_external_replace":
            source_path(operation["current_source"])
        if op == "edit_and_save":
            expect = operation["expect"]
            assert expect["outcome"] in {"saved", "conflict"}
            unsupported = expect.get("unsupported_assertions", [])
            if expect["outcome"] == "conflict":
                assert expect.get("writes") == 0
                assert set(unsupported) == {"writes"}, expect
            else:
                check_unsupported(unsupported, set())

budget = json.loads((root / "budget_open.json").read_text(encoding="utf-8"))
save = budget["operations"][-1]["expect"]
assert source_path(save["after_source"]).read_bytes().count(b"Tax rate,0.25") == 1
assert source_path("../../examples/budget.ms").read_bytes().count(b"Tax rate,0.2") == 1

print(f"validated {len(manifest['cases'])} browser-session fixtures")
PY
