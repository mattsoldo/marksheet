#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from unittest import mock


HERE = Path(__file__).resolve().parent


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


live = load_module("marksheet_harness_live", HERE / "live.py")
runner = load_module("marksheet_harness_run", HERE / "run.py")


def result(harness: str, verified_at: str, passed: bool = True) -> dict[str, object]:
    return {
        "harness": harness,
        "client": f"{harness}-client",
        "tasks": 7,
        "passed": passed,
        "verified_at": verified_at,
    }


class LiveRecordTests(unittest.TestCase):
    def test_partial_rerun_preserves_the_other_harness_freshness(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            marksheet = temporary / "marksheet"
            marksheet.write_bytes(b"")
            record = temporary / "live-results.json"
            record.write_text(
                json.dumps(
                    {
                        "version": "marksheet-live-harness@1",
                        "corpus": "marksheet-harness-corpus@1",
                        "results": [
                            result("codex", "2025-01-01"),
                            result("claude-code", "2024-01-01"),
                        ],
                    }
                )
            )
            refreshed = result("codex", "2026-08-17")
            arguments = [
                "live.py",
                "--harness",
                "codex",
                "--marksheet",
                str(marksheet),
                "--record",
                str(record),
            ]
            with mock.patch.object(sys, "argv", arguments), mock.patch.object(
                live, "run_harness", return_value=(refreshed, None)
            ), redirect_stdout(io.StringIO()):
                self.assertEqual(live.main(), 0)

            recorded = json.loads(record.read_text())
            self.assertEqual(recorded["results"][0]["verified_at"], "2026-08-17")
            self.assertEqual(recorded["results"][1]["verified_at"], "2024-01-01")

    def test_failed_attempt_is_written_before_nonzero_exit(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            marksheet = temporary / "marksheet"
            marksheet.write_bytes(b"")
            record = temporary / "live-results.json"
            failed = result("codex", "2026-08-17", passed=False)
            arguments = [
                "live.py",
                "--harness",
                "codex",
                "--marksheet",
                str(marksheet),
                "--record",
                str(record),
            ]
            with mock.patch.object(sys, "argv", arguments), mock.patch.object(
                live, "run_harness", return_value=(failed, "acceptance failed")
            ), redirect_stdout(io.StringIO()):
                self.assertEqual(live.main(), 1)

            recorded = json.loads(record.read_text())
            self.assertIs(recorded["results"][0]["passed"], False)
            self.assertEqual(recorded["results"][0]["verified_at"], "2026-08-17")

    def test_workspace_validation_failure_becomes_a_result(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            marksheet = Path(directory) / "marksheet"
            marksheet.write_bytes(b"")
            completed = subprocess.CompletedProcess(["codex"], 0, "", "")
            with mock.patch.object(
                live, "client_version", return_value="codex-test"
            ), mock.patch.object(
                live, "invoke_agent", return_value=completed
            ), mock.patch.object(
                live,
                "validate_workspace",
                side_effect=AssertionError("bad workbook"),
            ):
                recorded, error = live.run_harness("codex", marksheet)

            self.assertIs(recorded["passed"], False)
            self.assertEqual(recorded["client"], "codex-test")
            self.assertEqual(error, "bad workbook")

    def test_nonzero_client_exit_becomes_a_result(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            marksheet = Path(directory) / "marksheet"
            marksheet.write_bytes(b"")
            completed = subprocess.CompletedProcess(
                ["codex"], 17, "agent output", "agent error"
            )
            with mock.patch.object(
                live, "client_version", return_value="codex-test"
            ), mock.patch.object(
                live, "invoke_agent", return_value=completed
            ), mock.patch.object(live, "validate_workspace") as validate:
                recorded, error = live.run_harness("codex", marksheet)

            self.assertIs(recorded["passed"], False)
            self.assertIn("exited 17", error)
            self.assertIn("agent error", error)
            validate.assert_not_called()

    def test_freshness_is_checked_per_harness(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            (temporary / "live-results.json").write_text(
                json.dumps(
                    {
                        "version": "marksheet-live-harness@1",
                        "corpus": "marksheet-harness-corpus@1",
                        "results": [
                            result("codex", "2026-08-17"),
                            result("claude-code", "2020-01-01"),
                        ],
                    }
                )
            )
            manifest = {
                "version": "marksheet-harness-corpus@1",
                "harnesses": ["codex", "claude-code"],
            }
            with mock.patch.object(runner, "HERE", temporary), mock.patch.object(
                runner, "utc_today", return_value=runner.datetime.date(2026, 8, 17)
            ):
                runner.validate_live_record(manifest, require_fresh=False)
                with self.assertRaisesRegex(AssertionError, "claude-code"):
                    runner.validate_live_record(manifest, require_fresh=True)


if __name__ == "__main__":
    unittest.main()
