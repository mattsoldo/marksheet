#!/usr/bin/env python3

from __future__ import annotations

import datetime
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
MARKSHEET = os.environ.get("MARKSHEET_BIN", str(ROOT / "target/debug/marksheet"))


class Server:
    def __init__(self, workspace: Path, server_path: Path) -> None:
        self.process = subprocess.Popen(
            [
                "python3",
                str(server_path),
                "--workspace",
                str(workspace),
                "--marksheet",
                MARKSHEET,
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.next_id = 1

    def call(self, tool: str, arguments: dict[str, Any]) -> dict[str, Any]:
        assert self.process.stdin is not None
        assert self.process.stdout is not None
        request = {"id": self.next_id, "tool": tool, "arguments": arguments}
        self.next_id += 1
        self.process.stdin.write(json.dumps(request) + "\n")
        self.process.stdin.flush()
        response = json.loads(self.process.stdout.readline())
        assert response["id"] == request["id"], response
        return response

    def close(self) -> None:
        assert self.process.stdin is not None
        self.process.stdin.close()
        return_code = self.process.wait(timeout=10)
        assert self.process.stderr is not None
        stderr = self.process.stderr.read()
        assert return_code == 0, stderr
        assert stderr == "", stderr


def read_json(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text())
    assert isinstance(value, dict)
    return value


def validate_manifest() -> tuple[dict[str, Any], list[dict[str, Any]]]:
    manifest = read_json(HERE / "manifest.json")
    assert manifest["version"] == "marksheet-harness-corpus@1"
    assert manifest["harnesses"] == ["codex", "claude-code"]
    tasks = manifest["tasks"]
    assert [task["id"] for task in tasks] == [
        "create_workbook",
        "add_sheet",
        "append_table_row",
        "change_named_input",
        "repair_invalid_csv",
        "explain_formula_error",
        "honest_conversion",
    ]
    for task in tasks:
        assert set(task).issubset({"id", "kind", "expected", "start", "source", "tool"})
    referenced = {
        task[key]
        for task in tasks
        for key in ("expected", "start", "source")
        if key in task
    }
    assert all(Path(name).name == name and name.endswith(".ms") for name in referenced)
    assert referenced == {path.name for path in HERE.glob("*.ms")}
    assert all((HERE / name).is_file() for name in referenced)
    live = read_json(HERE / "live-results.json")
    assert set(live) == {"version", "verified_at", "corpus", "results"}
    assert live["version"] == "marksheet-live-harness@1"
    assert live["corpus"] == manifest["version"]
    assert datetime.date.fromisoformat(live["verified_at"]) <= datetime.date.today()
    assert [result["harness"] for result in live["results"]] == manifest["harnesses"]
    assert all(
        set(result) == {"harness", "client", "tasks", "passed"}
        and result["tasks"] == 7
        and result["passed"] is True
        and isinstance(result["client"], str)
        and result["client"]
        for result in live["results"]
    )
    return manifest, tasks


def validate_harness(name: str) -> dict[str, Any]:
    directory = ROOT / "integrations/harnesses" / name
    harness = read_json(directory / "harness.json")
    assert harness == {
        "version": "marksheet-harness@1",
        "harness": name,
        "skill_source": "../../skill",
        "project_install_path": (
            ".codex/skills/marksheet"
            if name == "codex"
            else ".claude/skills/marksheet"
        ),
        "tool_schema": "../../mcp/tool-schema.json",
        "response_schema": "../../mcp/response-schema.json",
        "tool_server": "../../mcp/marksheet_tool_server.py",
    }
    for key in ("skill_source", "tool_schema", "response_schema", "tool_server"):
        assert (directory / harness[key]).resolve().exists(), (name, key)
    skill = (directory / harness["skill_source"] / "SKILL.md").read_text()
    workflows = (
        directory / harness["skill_source"] / "references/workflows.md"
    ).read_text()
    assert "committed_invalid" in skill
    assert "Do not retry an append" in workflows
    return harness


def require_ok(response: dict[str, Any]) -> Any:
    assert response["ok"], response
    assert response["status"] == "ok", response
    return response["result"]


def run_harness(name: str, tasks: list[dict[str, Any]]) -> None:
    harness = validate_harness(name)
    harness_directory = ROOT / "integrations/harnesses" / name
    task = {item["id"]: item for item in tasks}
    assert task["append_table_row"]["tool"] == "append_table_row"
    assert task["change_named_input"]["tool"] == "set"
    assert task["honest_conversion"]["tool"] == "convert"
    with tempfile.TemporaryDirectory(prefix=f"marksheet-{name}-") as directory:
        workspace = Path(directory)
        canonical_skill = (harness_directory / harness["skill_source"]).resolve()
        installed_skill = workspace / harness["project_install_path"]
        shutil.copytree(canonical_skill, installed_skill)
        canonical_files = {
            path.relative_to(canonical_skill): path.read_bytes()
            for path in canonical_skill.rglob("*")
            if path.is_file()
        }
        installed_files = {
            path.relative_to(installed_skill): path.read_bytes()
            for path in installed_skill.rglob("*")
            if path.is_file()
        }
        assert installed_files == canonical_files
        workbook = workspace / "workbook.ms"
        server = Server(
            workspace, (harness_directory / harness["tool_server"]).resolve()
        )
        try:
            # Direct source authoring is the normal workflow for new structure.
            workbook.write_bytes(
                (HERE / task["create_workbook"]["expected"]).read_bytes()
            )
            result = require_ok(server.call("check", {"path": "workbook.ms"}))
            assert result["version"] == "marksheet-check@1"
            assert result["diagnostics"] == []
            structure = require_ok(server.call("inspect", {"path": "workbook.ms"}))
            assert [sheet["id"] for sheet in structure["workbook"]["sheets"]] == [
                "inputs",
                "summary",
            ]

            # Adding a sheet remains a readable direct-source edit.
            workbook.write_bytes((HERE / task["add_sheet"]["expected"]).read_bytes())
            structure = require_ok(server.call("inspect", {"path": "workbook.ms"}))
            assert [sheet["id"] for sheet in structure["workbook"]["sheets"]] == [
                "inputs",
                "summary",
                "notes",
            ]

            appended = require_ok(
                server.call(
                    "append_table_row",
                    {
                        "path": "workbook.ms",
                        "table": "costs",
                        "values": ["Transport", "50", "2", ""],
                    },
                )
            )
            assert appended["changed"] and len(appended["patches"]) == 1

            edited = require_ok(
                server.call(
                    "set",
                    {
                        "path": "workbook.ms",
                        "target": "tax_rate",
                        "value_or_formula": "0.25",
                    },
                )
            )
            assert edited["changed"] and len(edited["patches"]) == 1
            calculated = require_ok(
                server.call(
                    "calculate",
                    {"path": "workbook.ms", "targets": ["tax_rate", "inputs!D4"]},
                )
            )
            assert calculated["targets"][0]["cells"][0]["calculated"]["value"] == 0.25
            assert calculated["targets"][1]["cells"][0]["calculated"]["value"] == 100.0

            invalid = workspace / "invalid.ms"
            invalid.write_bytes(
                (HERE / task["repair_invalid_csv"]["start"]).read_bytes()
            )
            rejected = server.call("check", {"path": "invalid.ms"})
            assert not rejected["ok"] and rejected["exit_code"] == 1
            invalid.write_bytes(
                (HERE / task["repair_invalid_csv"]["expected"]).read_bytes()
            )
            require_ok(server.call("check", {"path": "invalid.ms"}))

            formula_error = workspace / "formula-error.ms"
            formula_error.write_bytes(
                (HERE / task["explain_formula_error"]["source"]).read_bytes()
            )
            diagnosis = server.call(
                "get", {"path": "formula-error.ms", "target": "data!A1"}
            )
            assert diagnosis["exit_code"] == 1
            assert diagnosis["result"]["cells"][0]["calculated"]["value"] == "#NAME?"
            assert diagnosis["result"]["diagnostics"][0]["code"] == "MS2103"

            conversion = require_ok(
                server.call(
                    "convert",
                    {
                        "path": "workbook.ms",
                        "target_format": "xlsx",
                        "options": {"output": "workbook.xlsx"},
                    },
                )
            )
            assert conversion["schema"] == "marksheet-conversion@1"
            assert conversion["fidelity"] == "lossy"
            assert any(
                outcome["outcome"] == "approximated"
                for outcome in conversion["outcomes"]
            )
            assert any(
                diagnostic["code"] == "MS4102"
                for diagnostic in conversion["diagnostics"]
            )
            assert (workspace / "workbook.xlsx").is_file()
        finally:
            server.close()


def main() -> int:
    manifest, tasks = validate_manifest()
    for harness in manifest["harnesses"]:
        run_harness(harness, tasks)
        print(f"{harness}: 7 tasks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
