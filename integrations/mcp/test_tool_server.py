#!/usr/bin/env python3

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SERVER = Path(__file__).with_name("marksheet_tool_server.py")
MARKSHEET = os.environ.get("MARKSHEET_BIN", str(ROOT / "target/debug/marksheet"))


class ToolServerTests(unittest.TestCase):
    def test_structured_queries_edits_and_workspace_boundary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory)
            workbook = workspace / "budget.ms"
            original = (ROOT / "examples/budget.ms").read_bytes()
            workbook.write_bytes(original)
            requests = [
                {"id": 1, "tool": "inspect", "arguments": {"path": "budget.ms"}},
                {
                    "id": 2,
                    "tool": "get",
                    "arguments": {"path": "budget.ms", "target": "tax_rate"},
                },
                {
                    "id": 3,
                    "tool": "set",
                    "arguments": {
                        "path": "budget.ms",
                        "target": "tax_rate",
                        "value_or_formula": "0.25",
                    },
                },
                {
                    "id": 4,
                    "tool": "append_table_row",
                    "arguments": {
                        "path": "budget.ms",
                        "table": "costs",
                        "values": ["Transport", "50", "2", ""],
                    },
                },
                {
                    "id": 5,
                    "tool": "calculate",
                    "arguments": {
                        "path": "budget.ms",
                        "targets": ["tax_rate", "inputs!D5"],
                    },
                },
                {
                    "id": 6,
                    "tool": "get",
                    "arguments": {"path": "../outside.ms", "target": "s!A1"},
                },
                {
                    "id": 7,
                    "tool": "format",
                    "arguments": {"path": "budget.ms", "check_only": True},
                },
                {
                    "id": 8,
                    "tool": "semantic_diff",
                    "arguments": {
                        "old_path": "budget.ms",
                        "new_path": "budget.ms",
                    },
                },
            ]
            completed = subprocess.run(
                [
                    "python3",
                    str(SERVER),
                    "--workspace",
                    str(workspace),
                    "--marksheet",
                    MARKSHEET,
                ],
                input="".join(json.dumps(request) + "\n" for request in requests),
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=True,
            )
            self.assertEqual(completed.stderr, "")
            responses = [json.loads(line) for line in completed.stdout.splitlines()]
            self.assertEqual([response["id"] for response in responses], list(range(1, 9)))
            self.assertTrue(responses[0]["ok"])
            self.assertEqual(responses[0]["result"]["version"], "marksheet-inspect@1")
            self.assertEqual(
                responses[1]["result"]["cells"][0]["calculated"]["value"], 0.2
            )
            self.assertEqual(
                responses[2]["result"]["patches"][0]["replacement"], "0.25"
            )
            self.assertEqual(
                responses[3]["result"]["operation"], "append_table_row"
            )
            self.assertEqual(
                responses[4]["result"]["targets"][1]["cells"][0]["calculated"][
                    "value"
                ],
                100.0,
            )
            self.assertEqual(
                responses[5]["error"]["kind"], "path_outside_workspace"
            )
            self.assertFalse(responses[6]["result"]["changed"])
            self.assertEqual(responses[6]["result"]["diagnostics"], [])
            self.assertTrue(responses[7]["result"]["equivalent"])
            self.assertNotEqual(workbook.read_bytes(), original)

    def test_malformed_request_is_correlated_when_possible(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            completed = subprocess.run(
                [
                    "python3",
                    str(SERVER),
                    "--workspace",
                    directory,
                    "--marksheet",
                    MARKSHEET,
                ],
                input='{"id":"bad","tool":"get","arguments":[]}\n',
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=True,
            )
            response = json.loads(completed.stdout)
            self.assertEqual(response["id"], "bad")
            self.assertEqual(response["error"]["kind"], "invalid_request")

    def test_integer_request_ids_are_browser_safe(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            requests = (
                '{"id":9007199254740992,"tool":"unknown","arguments":{}}\n'
                '{"id":9007199254740991,"tool":"unknown","arguments":{}}\n'
            )
            completed = subprocess.run(
                [
                    "python3",
                    str(SERVER),
                    "--workspace",
                    directory,
                    "--marksheet",
                    MARKSHEET,
                ],
                input=requests,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=True,
            )
            responses = [json.loads(line) for line in completed.stdout.splitlines()]
            self.assertIsNone(responses[0]["id"])
            self.assertEqual(responses[0]["error"]["kind"], "invalid_request")
            self.assertEqual(responses[1]["id"], 9007199254740991)
            self.assertEqual(responses[1]["error"]["kind"], "unknown_tool")

    def test_request_limits_are_bounded_and_do_not_stop_the_stream(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            oversized = b"{" + (b" " * (8 * 1024 * 1024)) + b"\n"
            following = b'{"id":"next","tool":"unknown","arguments":{}}\n'
            completed = subprocess.run(
                [
                    "python3",
                    str(SERVER),
                    "--workspace",
                    directory,
                    "--marksheet",
                    MARKSHEET,
                ],
                input=oversized + following,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=True,
            )
            responses = [json.loads(line) for line in completed.stdout.splitlines()]
            self.assertEqual(responses[0]["error"]["kind"], "request_limit")
            self.assertEqual(responses[1]["id"], "next")
            self.assertEqual(responses[1]["error"]["kind"], "unknown_tool")

    def test_pathological_json_does_not_stop_the_stream(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            huge_integer = b'{"id":' + (b"9" * 5000) + b',"tool":"x","arguments":{}}\n'
            deep_array = (b"[" * 20000) + (b"]" * 20000) + b"\n"
            following = b'{"id":"next","tool":"unknown","arguments":{}}\n'
            completed = subprocess.run(
                [
                    "python3",
                    str(SERVER),
                    "--workspace",
                    directory,
                    "--marksheet",
                    MARKSHEET,
                ],
                input=huge_integer + deep_array + following,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=5,
                check=True,
            )
            responses = [json.loads(line) for line in completed.stdout.splitlines()]
            self.assertEqual([item["error"]["kind"] for item in responses[:2]], [
                "invalid_json",
                "invalid_json",
            ])
            self.assertEqual(responses[2]["id"], "next")
            self.assertEqual(responses[2]["error"]["kind"], "unknown_tool")

    def test_calculation_target_count_is_bounded_before_cli_work(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            workbook = Path(directory) / "budget.ms"
            workbook.write_bytes((ROOT / "examples/budget.ms").read_bytes())
            request = {
                "id": "many",
                "tool": "calculate",
                "arguments": {
                    "path": "budget.ms",
                    "targets": ["tax_rate"] * 257,
                },
            }
            completed = subprocess.run(
                [
                    "python3",
                    str(SERVER),
                    "--workspace",
                    directory,
                    "--marksheet",
                    MARKSHEET,
                ],
                input=json.dumps(request) + "\n",
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=True,
            )
            response = json.loads(completed.stdout)
            self.assertEqual(response["id"], "many")
            self.assertEqual(response["error"]["kind"], "request_limit")

    def test_special_files_are_refused_without_blocking(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fifo = Path(directory) / "workbook.ms"
            os.mkfifo(fifo)
            request = {"id": "fifo", "tool": "inspect", "arguments": {"path": "workbook.ms"}}
            completed = subprocess.run(
                [
                    "python3",
                    str(SERVER),
                    "--workspace",
                    directory,
                    "--marksheet",
                    MARKSHEET,
                ],
                input=json.dumps(request) + "\n",
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=5,
                check=True,
            )
            response = json.loads(completed.stdout)
            self.assertEqual(response["error"]["kind"], "path_error")

    def test_committed_invalid_append_is_not_reported_as_a_refusal(self) -> None:
        source = b'''#!marksheet 0.1
@use assertions@1
@extension assertions@1 "checks"
assert summary!A1 = 1700
@end
@sheet inputs "Inputs"
@table costs A1 csv
Item,Cost
Rent,1500
Utilities,200
@end
@sheet summary "Summary"
@block A1 csv
=SUM(costs[Cost])
@end
'''
        with tempfile.TemporaryDirectory() as directory:
            workbook = Path(directory) / "workbook.ms"
            workbook.write_bytes(source)
            request = {
                "id": "append",
                "tool": "append_table_row",
                "arguments": {
                    "path": "workbook.ms",
                    "table": "costs",
                    "values": ["Transport", "50"],
                },
            }
            completed = subprocess.run(
                [
                    "python3",
                    str(SERVER),
                    "--workspace",
                    directory,
                    "--marksheet",
                    MARKSHEET,
                ],
                input=json.dumps(request) + "\n",
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=True,
            )
            response = json.loads(completed.stdout)
            self.assertTrue(response["ok"])
            self.assertEqual(response["status"], "committed_invalid")
            self.assertEqual(response["exit_code"], 1)
            self.assertTrue(response["result"]["changed"])
            self.assertFalse(response["result"]["valid"])
            self.assertEqual(response["result"]["diagnostics"][0]["code"], "MS3201")
            self.assertEqual(workbook.read_text().count("Transport,50"), 1)


if __name__ == "__main__":
    unittest.main()
