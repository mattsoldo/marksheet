#!/usr/bin/env bash
set -euo pipefail

fixture_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

FIXTURE_ROOT="$fixture_root" python3 - <<'PY'
import base64
import json
import os
import re
from pathlib import Path

root = Path(os.environ["FIXTURE_ROOT"]).resolve()
manifest = json.loads((root / "manifest.json").read_text(encoding="utf-8"))
assert set(manifest) == {"version", "protocol", "cases"}
assert manifest["version"] == 1
assert manifest["protocol"] == "marksheet-extension-conformance@1"
assert manifest["cases"]

identifier = re.compile(r"^[a-z][a-z0-9_]*$")
extension_id = re.compile(r"^[a-z][a-z0-9_]*@[1-9][0-9]*$")
assertion = re.compile(r'^assert ([A-Za-z][A-Za-z0-9_]*!)?[A-Z]+[1-9][0-9]* (?:=|!=|<|<=|>|>=) (?:blank|true|false|#(?:DIV/0!|N/A|NAME\?|NUM!|REF!|VALUE!|CIRC!)|-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?|[0-9]{4}-[0-9]{2}-[0-9]{2}(?:T[^ ]+)?|"(?:[^"\\]|\\.)*")$')
cases = {"registry", "assertions", "availability", "lossless"}

def read_fixture(name):
    assert re.fullmatch(r"[a-z][a-z0-9_]*\.json", name)
    path = (root / name).resolve()
    assert root in path.parents and path.is_file()
    return json.loads(path.read_text(encoding="utf-8"))

def source_bytes(name):
    path = (root / name).resolve()
    assert root in path.parents and path.is_file(), f"missing source {name}"
    return path.read_bytes()

def extension_instances(source):
    declarations = {}
    instances = []
    current_sheet = None
    lines = source.decode("utf-8").splitlines()
    index = 0
    while index < len(lines):
        line = lines[index]
        if line.startswith("@sheet "):
            current_sheet = line.split()[1]
        match = re.fullmatch(r"@(use|require) ([a-z][a-z0-9_]*@[1-9][0-9]*)", line)
        if match:
            assert match.group(2) not in declarations, f"duplicate source declaration {match.group(2)}"
            declarations[match.group(2)] = (match.group(1), index + 1)
        match = re.fullmatch(r'@extension ([a-z][a-z0-9_]*@[1-9][0-9]*) "([^"\\]*)"', line)
        if match:
            capability, name = match.groups()
            directive_line = index + 1
            payload_start = index + 1
            payload = []
            index += 1
            while index < len(lines) and lines[index] != "@end":
                payload.append((index + 1, lines[index]))
                index += 1
            assert index < len(lines), f"unterminated extension {capability}:{name}"
            instances.append({"capability": capability, "name": name, "scope": current_sheet, "payload": payload, "line": payload_start, "directive_line": directive_line})
        index += 1
    return declarations, instances

seen = set()
for entry in manifest["cases"]:
    assert set(entry) == {"id", "kind", "fixture"}
    assert identifier.fullmatch(entry["id"]) and entry["id"] not in seen
    seen.add(entry["id"])
    assert entry["kind"] in cases
    fixture = read_fixture(entry["fixture"])
    allowed = {"protocol", "registry", "source", "limits", "expect"}
    if entry["kind"] == "lossless":
        allowed |= {"original_source_base64", "lossless_output_base64", "canonical_output_base64"}
    assert set(fixture) <= allowed and {"protocol", "registry", "source", "expect"} <= set(fixture)
    assert fixture["protocol"] == "marksheet-extension-fixture@1"
    assert isinstance(fixture["registry"], list) and all(extension_id.fullmatch(value) for value in fixture["registry"])
    expect = fixture["expect"]
    report_fields = {
        "capabilities_complete", "calculation_complete", "rendering_complete",
        "validation_complete", "valid", "diagnostics", "opaque_instances",
        "instance_outcomes",
    }
    assert set(expect) <= report_fields | {"registry_error"}

    if entry["kind"] == "registry":
        assert len(fixture["registry"]) != len(set(fixture["registry"]))
        assert expect == {"registry_error": "duplicate_exact_id"}
        continue

    assert set(expect) == report_fields
    assert all(isinstance(expect[field], bool) for field in {
        "capabilities_complete", "calculation_complete", "rendering_complete",
        "validation_complete", "valid",
    })
    assert isinstance(expect["diagnostics"], list)
    assert all(re.fullmatch(r"[a-z][a-z0-9_]*@[1-9][0-9]*:.+", value) for value in expect["opaque_instances"])
    assert isinstance(expect["instance_outcomes"], list)
    for outcome in expect["instance_outcomes"]:
        assert set(outcome) == {"capability", "name", "scope", "outcome"}
        assert extension_id.fullmatch(outcome["capability"])
        assert isinstance(outcome["name"], str) and outcome["name"]
        assert outcome["scope"] == "workbook" or re.fullmatch(r"sheet:[a-z][a-z0-9_]*", outcome["scope"])
        assert outcome["outcome"] in {
            "processed", "skipped_unavailable", "skipped_undeclared",
            "rejected_duplicate", "rejected_by_limit",
        }

    if entry["kind"] == "lossless":
        assert re.fullmatch(r"[a-z][a-z0-9_.-]*\.inline", fixture["source"])
        original = base64.b64decode(fixture["original_source_base64"], validate=True)
        lossless = base64.b64decode(fixture["lossless_output_base64"], validate=True)
        canonical = base64.b64decode(fixture["canonical_output_base64"], validate=True)
        assert b"\r\n" in original and lossless == original
        assert b"\r\n" not in canonical and canonical == original.replace(b"\r\n", b"\n")
        declarations, instances = extension_instances(original)
    else:
        assert re.fullmatch(r"[a-z][a-z0-9_.-]*\.ms", fixture["source"])
        declarations, instances = extension_instances(source_bytes(fixture["source"]))

    expected_opaque = [f"{item['capability']}:{item['name']}" for item in instances]
    assert expect["opaque_instances"] == expected_opaque
    actual_outcome_instances = [f"{item['capability']}:{item['name']}" for item in expect["instance_outcomes"]]
    assert actual_outcome_instances == expected_opaque
    expected_scopes = ["workbook" if item["scope"] is None else f"sheet:{item['scope']}" for item in instances]
    assert [item["scope"] for item in expect["instance_outcomes"]] == expected_scopes
    expected_availability = []
    for capability, (mode, line) in declarations.items():
        if capability not in fixture["registry"]:
            expected_availability.append(("MS3101" if mode == "require" else "MS3102", "error" if mode == "require" else "warning", line))
    for item in instances:
        if item["capability"] not in declarations:
            expected_availability.append(("MS3103", "warning", item["directive_line"]))
    actual_codes = [(item["code"], item["severity"], item.get("line")) for item in expect["diagnostics"]]
    for diagnostic in expect["diagnostics"]:
        assert set(diagnostic) == {"code", "severity", "line"}
        assert re.fullmatch(r"MS[0-9]{4}", diagnostic["code"])
        assert diagnostic["severity"] in {"warning", "error"}
        assert isinstance(diagnostic["line"], int) and diagnostic["line"] > 0
    for availability in expected_availability:
        assert availability in actual_codes, f"missing availability diagnostic {availability}"
    required_missing = any(mode == "require" and capability not in fixture["registry"] for capability, (mode, _) in declarations.items())
    assert expect["capabilities_complete"] is (not required_missing)
    assert expect["calculation_complete"] is (not required_missing)
    assert expect["rendering_complete"] is (not required_missing)
    assert expect["validation_complete"] is not any(code in {"MS3111", "MS3203"} for code, _, _ in actual_codes)
    assert expect["valid"] is not any(severity == "error" for _, severity, _ in actual_codes)

    for item in instances:
        if item["capability"] != "assertions@1" or item["capability"] not in fixture["registry"]:
            continue
        non_comment = [(line, text) for line, text in item["payload"] if text and not text.startswith("#")]
        limits = fixture.get("limits", {})
        if limits:
            assert set(limits) <= {"max_payload_bytes", "max_lines", "max_targets", "max_diagnostics"}
            assert all(isinstance(value, int) and value > 0 for value in limits.values())
            max_lines = limits.get("max_lines")
            if max_lines is not None and len(non_comment) > max_lines:
                trigger_line = non_comment[max_lines][0]
                assert ("MS3203", "error", trigger_line) in actual_codes
        for line, text in non_comment:
            valid = assertion.fullmatch(text) is not None
            if item["scope"] is None and valid:
                valid = "!" in text.split(" ", 2)[1]
            if item["scope"] is not None and valid:
                valid = "!" not in text.split(" ", 2)[1]
            if not valid:
                assert ("MS3202", "error", line) in actual_codes, f"missing malformed diagnostic for {line}: {text}"

print(f"validated {len(manifest['cases'])} extension fixtures")
PY
