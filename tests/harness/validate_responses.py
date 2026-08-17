#!/usr/bin/env python3
"""Validate representative runtime envelopes against the published schema."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
from typing import Any

from jsonschema import Draft202012Validator


ROOT = Path(__file__).resolve().parents[2]
MARKSHEET = os.environ.get("MARKSHEET_BIN", str(ROOT / "target/debug/marksheet"))
SCHEMA = json.loads((ROOT / "integrations/mcp/response-schema.json").read_text())
VALIDATOR = Draft202012Validator(SCHEMA)


def invoke(*arguments: str) -> dict[str, Any]:
    completed = subprocess.run(
        [MARKSHEET, *arguments],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    assert completed.returncode in (0, 1), completed.stderr
    value = json.loads(completed.stdout)
    VALIDATOR.validate(value)
    return value


def validate_tool_response(workspace: Path) -> None:
    requests = (
        '{"id":"inspect","tool":"inspect","arguments":{"path":"workbook.ms"}}\n'
        '{"id":"bad","tool":"unknown","arguments":{}}\n'
    )
    completed = subprocess.run(
        [
            "python3",
            str(ROOT / "integrations/mcp/marksheet_tool_server.py"),
            "--workspace",
            str(workspace),
            "--marksheet",
            MARKSHEET,
        ],
        input=requests,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
    )
    assert completed.stderr == "", completed.stderr
    for line in completed.stdout.splitlines():
        VALIDATOR.validate(json.loads(line))


def main() -> int:
    Draft202012Validator.check_schema(SCHEMA)
    with tempfile.TemporaryDirectory(prefix="marksheet-response-schema-") as directory:
        workspace = Path(directory)
        workbook = workspace / "workbook.ms"
        shutil.copy2(ROOT / "examples/budget.ms", workbook)
        invoke("check", "--format", "json", str(workbook))
        invoke("inspect", str(workbook))
        invoke("get", str(workbook), "tax_rate")
        invoke("set", str(workbook), "tax_rate", "0.25")
        refusal = invoke("append-table-row", str(workbook), "Not-A-Table", "--value", "x")
        assert refusal["status"] == "invalid" and refusal["changed"] is False
        invoke("fmt", "--check", "--format", "json", str(workbook))
        validate_tool_response(workspace)

        assertions = workspace / "assertions.ms"
        shutil.copy2(ROOT / "tests/extensions/assertions_success.ms", assertions)
        committed = invoke("set", str(assertions), "inputs!A2", "1")
        assert committed["status"] == "committed_invalid" and committed["changed"] is True
        repeated = invoke("set", str(assertions), "inputs!A2", "1")
        assert repeated["status"] == "invalid" and repeated["changed"] is False
    print("automation response schema: runtime samples valid")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
