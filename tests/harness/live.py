#!/usr/bin/env python3
"""Run the seven-task corpus through real Codex and Claude Code clients.

This is an explicit release acceptance test rather than a hermetic CI test: it
requires authenticated local clients and invokes hosted coding-agent models.
All agent writes are confined to a fresh temporary workspace, and the result
is independently checked with the built `marksheet` binary.
"""

from __future__ import annotations

import argparse
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
DEFAULT_MARKSHEET = ROOT / "target/debug/marksheet"
HARNESSES = ("codex", "claude-code")


PROMPT = """Use the installed Marksheet skill and the local `.tools/marksheet`
executable to complete all seven tasks below. Work only in this directory; do
not use the network or git. Check every workbook after each material change.

1. Create `workbook.ms` from scratch. It must use Draft 0.1, locale en-US,
   timezone UTC, and portable-a1@1. Add an `inputs` sheet with a `costs` table
   at A1 whose headers are Item, Cost, Quantity, Subtotal and whose rows are
   Rent/1500/1/blank and Utilities/200/1/blank. Define a calculated-column fill
   for Subtotal. Put text `Tax rate` and number 0.2 at E2 and F2 in a small
   block, and define workbook name `tax_rate = inputs!F2`. Add a `summary`
   sheet with an A1:B2 block whose first row is Metric,Value and second row is
   After tax plus a formula that totals the Subtotal column after tax.
2. Add a `notes` sheet with a two-column A1:B2 block containing headers Note,
   Score and row `Reviewed by automation`, blank. Add coordinate fill B2:B2
   with formula `=1`; this intentionally makes XLSX conversion lossy.
3. Use `append-table-row` to append Transport, 50, 2, blank to `costs`.
4. Use `set` to change named input `tax_rate` to 0.25.
5. `invalid.ms` contains malformed CSV. Diagnose and repair it in place so
   `marksheet check` succeeds while preserving the intended two-column data;
   the missing Oranges quantity is 3.
6. Diagnose `formula-error.ms` with `get`. Write `diagnosis.json` containing
   exactly these keys: `code` (MS2103), `value` (#NAME?), and a short
   `explanation` string.
7. Convert `workbook.ms` to `workbook.xlsx` and capture the JSON report in
   `conversion-report.json`. Preserve the honest lossy/MS4102 report.

Do not stop until all requested files exist and `.tools/marksheet check` passes
for both `workbook.ms` and repaired `invalid.ms`.
"""


def run(command: list[str], *, cwd: Path, timeout: int = 900) -> subprocess.CompletedProcess[str]:
    environment = os.environ.copy()
    environment["NO_COLOR"] = "1"
    environment["MARKSHEET_BIN"] = str(cwd / ".tools/marksheet")
    return subprocess.run(
        command,
        cwd=cwd,
        env=environment,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )


def client_version(harness: str) -> str:
    command = ["codex", "--version"] if harness == "codex" else ["claude", "--version"]
    completed = subprocess.run(
        command,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=10,
        check=True,
    )
    return completed.stdout.strip()


def invoke_agent(harness: str, workspace: Path) -> subprocess.CompletedProcess[str]:
    if harness == "codex":
        return run(
            [
                "codex",
                "exec",
                "--ephemeral",
                "--ignore-user-config",
                "--skip-git-repo-check",
                "--ignore-rules",
                "--approve-for-me",
                "--cd",
                str(workspace),
                PROMPT,
            ],
            cwd=workspace,
        )
    return run(
        [
            "claude",
            "--print",
            "--no-session-persistence",
            "--no-chrome",
            "--setting-sources",
            "project",
            "--permission-mode",
            "acceptEdits",
            "--allowedTools",
            "Bash,Read,Write,Edit,Glob,Grep",
            "--max-budget-usd",
            "3",
            PROMPT,
        ],
        cwd=workspace,
    )


def cli(workspace: Path, *arguments: str) -> dict[str, Any]:
    completed = run([str(workspace / ".tools/marksheet"), *arguments], cwd=workspace, timeout=30)
    if completed.returncode not in (0, 1):
        raise AssertionError(f"marksheet {' '.join(arguments)} failed: {completed.stderr}")
    if not completed.stdout:
        raise AssertionError(f"marksheet {' '.join(arguments)} returned no JSON")
    value = json.loads(completed.stdout)
    if not isinstance(value, dict):
        raise AssertionError("automation response must be an object")
    return value


def validate_workspace(workspace: Path) -> None:
    check = cli(workspace, "check", "--format", "json", "workbook.ms")
    assert check["status"] == "ok", check
    inspect = cli(workspace, "inspect", "workbook.ms")
    assert inspect["status"] == "ok", inspect
    sheets = {sheet["id"]: sheet for sheet in inspect["workbook"]["sheets"]}
    assert set(sheets) == {"inputs", "summary", "notes"}, inspect
    table = sheets["inputs"]["tables"][0]
    assert table["id"] == "costs"
    assert table["range"] == "A1:D4", table
    assert table["data_range"] == "A2:D4", table
    assert table["headers"] == ["Item", "Cost", "Quantity", "Subtotal"], table

    costs = cli(workspace, "get", "workbook.ms", "inputs!A1:D4")["cells"]
    authored = [cell["authored"] for cell in costs]
    assert [value["value"] for value in authored[:4]] == [
        "Item",
        "Cost",
        "Quantity",
        "Subtotal",
    ]
    assert [authored[index]["value"] for index in (4, 5, 6)] == ["Rent", 1500.0, 1.0]
    assert [authored[index]["value"] for index in (8, 9, 10)] == [
        "Utilities",
        200.0,
        1.0,
    ]
    assert [authored[index]["value"] for index in (12, 13, 14)] == [
        "Transport",
        50.0,
        2.0,
    ]
    assert [costs[index]["calculated"]["value"] for index in (7, 11, 15)] == [
        1500.0,
        200.0,
        100.0,
    ]

    tax = cli(workspace, "get", "workbook.ms", "tax_rate")
    assert tax["cells"][0]["calculated"]["value"] == 0.25, tax
    subtotal = cli(workspace, "get", "workbook.ms", "inputs!D4")
    assert subtotal["cells"][0]["calculated"]["value"] == 100.0, subtotal
    summary = cli(workspace, "get", "workbook.ms", "summary!A1:B2")
    assert summary["cells"][2]["authored"]["value"] == "After tax", summary
    assert summary["cells"][3]["calculated"]["value"] == 1350.0, summary
    notes = cli(workspace, "get", "workbook.ms", "notes!A1:B2")
    assert [notes["cells"][index]["authored"]["value"] for index in (0, 1, 2)] == [
        "Note",
        "Score",
        "Reviewed by automation",
    ]
    assert notes["cells"][3]["virtual_formula"] == "=1", notes
    assert notes["cells"][3]["calculated"]["value"] == 1.0, notes

    repaired = cli(workspace, "check", "--format", "json", "invalid.ms")
    assert repaired["status"] == "ok", repaired
    repaired_diff = cli(
        workspace,
        "diff",
        "--format",
        "json",
        str(HERE / "invalid_csv.expected.ms"),
        "invalid.ms",
    )
    assert repaired_diff["equivalent"] is True, repaired_diff
    diagnosis = json.loads((workspace / "diagnosis.json").read_text())
    assert set(diagnosis) == {"code", "value", "explanation"}, diagnosis
    assert diagnosis["code"] == "MS2103", diagnosis
    assert diagnosis["value"] == "#NAME?", diagnosis
    assert isinstance(diagnosis["explanation"], str) and diagnosis["explanation"], diagnosis

    report = json.loads((workspace / "conversion-report.json").read_text())
    assert report["schema"] == "marksheet-conversion@1", report
    assert report["fidelity"] == "lossy", report
    assert any(item["outcome"] == "approximated" for item in report["outcomes"]), report
    assert any(item["code"] == "MS4102" for item in report["diagnostics"]), report
    assert any(
        item["feature"] == "portable_formulas"
        and item["outcome"] == "approximated"
        and {
            "kind": "range",
            "sheet": "notes",
            "range": "B2",
        }
        in item["locations"]
        for item in report["outcomes"]
    ), report
    assert (workspace / "workbook.xlsx").is_file()


def run_harness(harness: str, marksheet: Path) -> dict[str, Any]:
    manifest = json.loads(
        (ROOT / "integrations/harnesses" / harness / "harness.json").read_text()
    )
    with tempfile.TemporaryDirectory(prefix=f"marksheet-live-{harness}-") as directory:
        workspace = Path(directory)
        skill_source = (ROOT / "integrations/harnesses" / harness / manifest["skill_source"]).resolve()
        skill_target = workspace / manifest["project_install_path"]
        shutil.copytree(skill_source, skill_target)
        tools = workspace / ".tools"
        tools.mkdir()
        shutil.copy2(marksheet, tools / "marksheet")
        (tools / "marksheet").chmod(0o755)
        shutil.copy2(HERE / "invalid_csv.start.ms", workspace / "invalid.ms")
        shutil.copy2(HERE / "formula_error.ms", workspace / "formula-error.ms")

        completed = invoke_agent(harness, workspace)
        transcript = workspace / "agent-transcript.txt"
        transcript.write_text(completed.stdout + "\n--- stderr ---\n" + completed.stderr)
        if completed.returncode != 0:
            raise AssertionError(
                f"{harness} exited {completed.returncode}; transcript:\n{transcript.read_text()}"
            )
        validate_workspace(workspace)
    return {
        "harness": harness,
        "client": client_version(harness),
        "tasks": 7,
        "passed": True,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--harness", choices=(*HARNESSES, "all"), default="all")
    parser.add_argument("--marksheet", type=Path, default=DEFAULT_MARKSHEET)
    parser.add_argument("--record", type=Path)
    options = parser.parse_args()
    marksheet = options.marksheet.resolve(strict=True)
    harnesses = HARNESSES if options.harness == "all" else (options.harness,)
    results = []
    for harness in harnesses:
        result = run_harness(harness, marksheet)
        results.append(result)
        print(f"{harness}: 7 live tasks passed ({result['client']})")
    record = {
        "version": "marksheet-live-harness@1",
        # UTC so the recording clock matches run.py's freshness check.
        "verified_at": datetime.datetime.now(datetime.timezone.utc)
        .date()
        .isoformat(),
        "corpus": "marksheet-harness-corpus@1",
        "results": results,
    }
    if options.record:
        if options.record.exists():
            previous = json.loads(options.record.read_text())
            assert previous["version"] == record["version"]
            merged = {
                item["harness"]: item
                for item in (*previous["results"], *record["results"])
            }
            record["results"] = [merged[name] for name in HARNESSES if name in merged]
        options.record.write_text(json.dumps(record, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
