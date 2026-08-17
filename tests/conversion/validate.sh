#!/usr/bin/env bash
set -euo pipefail

fixture_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

FIXTURE_ROOT="$fixture_root" python3 - <<'PY'
import json
import os
import re
from pathlib import Path

root = Path(os.environ["FIXTURE_ROOT"]).resolve()
manifest = json.loads((root / "manifest.json").read_text(encoding="utf-8"))

assert set(manifest) == {"version", "protocol", "cases"}
assert manifest["version"] == 1
assert manifest["protocol"] == "marksheet-conversion-conformance@1"
assert isinstance(manifest["cases"], list) and manifest["cases"]

identifier = re.compile(r"^[a-z][a-z0-9_]*$")
coordinate = re.compile(r"^[A-Z]+[1-9][0-9]*$")
range_pattern = re.compile(r"^[A-Z]+[1-9][0-9]*(?::[A-Z]+[1-9][0-9]*)?$")
feature_pattern = re.compile(r"^[a-z][a-z0-9_.-]*$")
path_pattern = re.compile(r"^[a-z0-9_./-]+\.(?:ms|csv)$")
fixture_pattern = re.compile(r"^[a-z0-9_./-]+\.json$")
kinds = {"marksheet_to_xlsx", "xlsx_to_marksheet", "marksheet_to_csv", "csv_to_marksheet"}
outcomes = {"exact", "approximated", "omitted", "unsupported"}
fidelities = {"lossless", "lossy", "unsupported"}

def read_json(path):
    return json.loads(path.read_text(encoding="utf-8"))

def safe_path(value):
    assert isinstance(value, str) and ".." not in Path(value).parts, value
    path = (root / value).resolve()
    assert root in path.parents, value
    assert path.is_file(), f"missing fixture input: {value}"
    return path

def check_location(value):
    assert isinstance(value, dict) and value, value
    assert set(value) <= {"sheet", "cell", "range", "table", "source"}, value
    if "sheet" in value:
        assert identifier.fullmatch(value["sheet"])
    if "cell" in value:
        assert coordinate.fullmatch(value["cell"])
    if "range" in value:
        assert range_pattern.fullmatch(value["range"])
    if "table" in value:
        assert identifier.fullmatch(value["table"])
    if "source" in value:
        assert isinstance(value["source"], str) and value["source"]

def check_source(source):
    assert set(source) in ({"format", "version", "path"}, {"format", "version", "fixture"}), source
    assert source["format"] in {"marksheet", "xlsx", "csv"}
    assert isinstance(source["version"], str) and source["version"]
    if "path" in source:
        assert path_pattern.fullmatch(source["path"]), source
        path = safe_path(source["path"])
        if source["format"] == "marksheet":
            assert path.suffix == ".ms" and path.read_bytes().startswith(b"#!marksheet 0.1\n")
        elif source["format"] == "csv":
            assert path.suffix == ".csv"
    else:
        assert source["format"] == "xlsx"
        assert fixture_pattern.fullmatch(source["fixture"]), source
        xlsx = read_json(safe_path(source["fixture"]))
        assert set(xlsx) <= {"fixture", "sheets", "parts", "features", "declared_uncompressed_bytes", "generated"}
        assert xlsx["fixture"] == "marksheet-xlsx-source@1"
        assert xlsx.get("generated") is True
        assert all(isinstance(value, str) and value for value in xlsx.get("parts", []))

def check_selection(request):
    selection = request.get("selection")
    if selection is None:
        return None
    assert isinstance(selection, dict)
    if set(selection) == {"table"}:
        assert identifier.fullmatch(selection["table"])
        return "table"
    assert set(selection) == {"sheet", "range"}
    assert identifier.fullmatch(selection["sheet"])
    assert range_pattern.fullmatch(selection["range"])
    return "range"

def check_import_target(request):
    target = request.get("import_target")
    if target is None:
        return False
    assert set(target) <= {"sheet", "label", "anchor", "table", "range"}
    assert {"sheet", "label"} <= set(target)
    assert identifier.fullmatch(target["sheet"])
    assert isinstance(target["label"], str) and target["label"]
    has_table = "table" in target or "anchor" in target
    has_range = "range" in target
    assert has_table != has_range
    if has_table:
        assert {"anchor", "table"} <= set(target)
        assert coordinate.fullmatch(target["anchor"])
        assert identifier.fullmatch(target["table"])
    if has_range:
        assert range_pattern.fullmatch(target["range"])
    return True

case_ids = set()
for entry in manifest["cases"]:
    assert set(entry) == {"id", "kind", "fixture"}, entry
    assert identifier.fullmatch(entry["id"]) and entry["id"] not in case_ids
    case_ids.add(entry["id"])
    assert entry["kind"] in kinds
    assert re.fullmatch(r"[a-z][a-z0-9_]*\.json", entry["fixture"])
    fixture = read_json(safe_path(entry["fixture"]))
    assert set(fixture) == {"protocol", "request", "expect"}, entry["fixture"]
    assert fixture["protocol"] == "marksheet-conversion-fixture@1"

    request = fixture["request"]
    assert set(request) <= {"source", "destination", "selection", "import_target", "limits"}
    assert {"source", "destination"} <= set(request)
    check_source(request["source"])
    assert request["destination"] in {"marksheet", "xlsx", "csv"}
    selection_kind = check_selection(request)
    has_import_target = check_import_target(request)
    if "limits" in request:
        assert request["limits"] and all(isinstance(value, int) and value > 0 for value in request["limits"].values())

    if entry["kind"] == "marksheet_to_xlsx":
        assert request["source"]["format"] == "marksheet" and request["destination"] == "xlsx"
    elif entry["kind"] == "xlsx_to_marksheet":
        assert request["source"]["format"] == "xlsx" and request["destination"] == "marksheet"
    else:
        if entry["kind"] == "marksheet_to_csv":
            assert request["source"]["format"] == "marksheet" and request["destination"] == "csv"
        else:
            assert request["source"]["format"] == "csv" and request["destination"] == "marksheet"

    expect = fixture["expect"]
    assert set(expect) <= {"fidelity", "artifact", "outcomes", "diagnostics"}
    assert {"fidelity", "outcomes", "diagnostics"} <= set(expect)
    assert expect["fidelity"] in fidelities
    assert isinstance(expect["outcomes"], list) and expect["outcomes"]
    assert isinstance(expect["diagnostics"], list)
    if "artifact" in expect:
        assert re.fullmatch(r"generated/[a-z0-9_./-]+", expect["artifact"])
    outcome_keys = set()
    for outcome in expect["outcomes"]:
        assert set(outcome) <= {"feature", "outcome", "formula", "location", "detail"}
        assert {"feature", "outcome"} <= set(outcome)
        assert feature_pattern.fullmatch(outcome["feature"])
        assert outcome["outcome"] in outcomes
        if "formula" in outcome:
            assert outcome["formula"] in {"preserved", "translated", "replaced"}
        if "location" in outcome:
            check_location(outcome["location"])
        if "detail" in outcome:
            assert isinstance(outcome["detail"], str) and outcome["detail"]
        key = (outcome["feature"], json.dumps(outcome.get("location"), sort_keys=True))
        assert key not in outcome_keys, f"duplicate conversion outcome: {key}"
        outcome_keys.add(key)
    diagnostic_keys = set()
    for diagnostic in expect["diagnostics"]:
        assert set(diagnostic) <= {"code", "severity", "location"}
        assert set(diagnostic) >= {"code", "severity"}
        assert re.fullmatch(r"MS[0-9]{4}", diagnostic["code"])
        assert diagnostic["severity"] in {"warning", "error"}
        if "location" in diagnostic:
            check_location(diagnostic["location"])
        key = (diagnostic["code"], json.dumps(diagnostic.get("location"), sort_keys=True))
        assert key not in diagnostic_keys, f"duplicate conversion diagnostic: {key}"
        diagnostic_keys.add(key)

    statuses = {outcome["outcome"] for outcome in expect["outcomes"]}
    severities = {diagnostic["severity"] for diagnostic in expect["diagnostics"]}
    if expect["fidelity"] == "lossless":
        assert statuses == {"exact"} and not expect["diagnostics"] and "artifact" in expect
    elif expect["fidelity"] == "lossy":
        assert statuses & {"approximated", "omitted"} and "unsupported" not in statuses
        assert "error" not in severities and "artifact" in expect
    else:
        assert "unsupported" in statuses and "error" in severities and "artifact" not in expect
    if request["destination"] == "csv":
        if expect["fidelity"] == "unsupported":
            assert selection_kind is None and {outcome["feature"] for outcome in expect["outcomes"]} == {"csv_selection"}
        else:
            assert selection_kind in {"table", "range"}
    if request["source"]["format"] == "csv" and request["destination"] == "marksheet":
        if expect["fidelity"] == "unsupported":
            assert not has_import_target and {outcome["feature"] for outcome in expect["outcomes"]} == {"csv_import_target"}
        else:
            assert has_import_target

print(f"validated {len(manifest['cases'])} conversion fixtures")
PY
