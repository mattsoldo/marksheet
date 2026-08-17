#!/usr/bin/env python3
"""Local JSON-lines tool adapter for the Marksheet CLI.

The transport is deliberately dependency-free and protocol-neutral: each input
line is one request object and each output line is one response object. Coding
harness packages can bridge this process to their preferred tool protocol
without duplicating workbook semantics.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import selectors
import stat
import subprocess
import sys
import time
from typing import Any

VERSION = "marksheet-tools@1"
MAX_REQUEST_BYTES = 8 * 1024 * 1024
MAX_RESPONSE_BYTES = 32 * 1024 * 1024
MAX_WORKBOOK_BYTES = 32 * 1024 * 1024
MAX_ARGUMENT_BYTES = 1024 * 1024
MAX_CALCULATION_TARGETS = 32
MAX_CALCULATION_CELLS = 100_000
MAX_PROCESS_SECONDS = 30.0
MAX_SAFE_INTEGER = 9_007_199_254_740_991


class ToolError(Exception):
    """A bounded, request-scoped tool refusal."""

    def __init__(self, kind: str, message: str) -> None:
        super().__init__(message)
        self.kind = kind
        self.message = message


class ToolServer:
    def __init__(self, workspace: Path, marksheet: str) -> None:
        self.workspace = workspace.resolve(strict=True)
        if not self.workspace.is_dir():
            raise ToolError("invalid_workspace", "workspace must be a directory")
        self.marksheet = marksheet

    def dispatch(self, request: dict[str, Any]) -> dict[str, Any]:
        self._expect_keys(request, {"id", "tool", "arguments"})
        request_id = request.get("id")
        if not isinstance(request_id, (str, int)) or isinstance(request_id, bool):
            raise ToolError("invalid_request", "id must be a string or integer")
        if isinstance(request_id, int) and abs(request_id) > MAX_SAFE_INTEGER:
            raise ToolError("invalid_request", "integer id exceeds the portable safe range")
        tool = request.get("tool")
        arguments = request.get("arguments", {})
        if not isinstance(tool, str) or not tool:
            raise ToolError("invalid_request", "tool must be a non-empty string")
        if not isinstance(arguments, dict):
            raise ToolError("invalid_request", "arguments must be an object")

        result = self._run_tool(tool, arguments)
        return {"version": VERSION, "id": request_id, **result}

    def _run_tool(self, tool: str, arguments: dict[str, Any]) -> dict[str, Any]:
        if tool == "check":
            self._expect_keys(arguments, {"path"})
            path = self._path(arguments, "path")
            return self._invoke(["check", "--format", "json", os.fspath(path)])
        if tool == "inspect":
            self._expect_keys(arguments, {"path"})
            path = self._path(arguments, "path")
            return self._invoke(["inspect", os.fspath(path)])
        if tool == "get":
            self._expect_keys(arguments, {"path", "target"}, {"calculated"})
            path = self._path(arguments, "path")
            target = self._string(arguments, "target")
            calculated = arguments.get("calculated", True)
            if not isinstance(calculated, bool):
                raise ToolError("invalid_arguments", "calculated must be boolean")
            return self._invoke(
                [
                    "get",
                    os.fspath(path),
                    target,
                    "--calculated",
                    str(calculated).lower(),
                ]
            )
        if tool == "set":
            self._expect_keys(arguments, {"path", "target", "value_or_formula"})
            path = self._path(arguments, "path")
            target = self._string(arguments, "target")
            value = self._string(arguments, "value_or_formula", allow_empty=True)
            return self._invoke(["set", os.fspath(path), target, value])
        if tool == "append_table_row":
            self._expect_keys(arguments, {"path", "table", "values"})
            path = self._path(arguments, "path")
            table = self._string(arguments, "table")
            values = arguments.get("values")
            if not isinstance(values, list) or not all(
                isinstance(value, str) for value in values
            ):
                raise ToolError("invalid_arguments", "values must be an array of strings")
            self._check_argument_bytes(values)
            command = ["append-table-row", os.fspath(path), table]
            for value in values:
                command.extend(["--value", value])
            return self._invoke(command)
        if tool == "calculate":
            self._expect_keys(arguments, {"path", "targets"})
            path = self._path(arguments, "path")
            targets = arguments.get("targets")
            if isinstance(targets, str):
                targets = [targets]
            if (
                not isinstance(targets, list)
                or not targets
                or not all(isinstance(target, str) and target for target in targets)
            ):
                raise ToolError(
                    "invalid_arguments", "targets must be a string or non-empty string array"
                )
            if len(targets) > MAX_CALCULATION_TARGETS:
                raise ToolError(
                    "request_limit", "calculate accepts at most 32 targets per request"
                )
            self._check_argument_bytes(targets)
            results = []
            exit_code = 0
            stderr = []
            result_bytes = 0
            result_cells = 0
            for target in targets:
                response = self._invoke(
                    ["get", os.fspath(path), target, "--calculated", "true"]
                )
                exit_code = max(exit_code, response["exit_code"])
                result = response.get("result")
                encoded = json.dumps(
                    result, separators=(",", ":"), ensure_ascii=False
                ).encode()
                result_bytes += len(encoded)
                if isinstance(result, dict) and isinstance(result.get("cells"), list):
                    result_cells += len(result["cells"])
                if result_bytes > MAX_RESPONSE_BYTES or result_cells > MAX_CALCULATION_CELLS:
                    raise ToolError(
                        "response_limit",
                        "calculation aggregate exceeds 32 MiB or 100000 cells",
                    )
                results.append(result)
                if response.get("stderr"):
                    stderr.append(response["stderr"])
            return self._response(exit_code, {"targets": results}, "\n".join(stderr))
        if tool == "format":
            self._expect_keys(arguments, {"path"}, {"check_only"})
            path = self._path(arguments, "path")
            check_only = arguments.get("check_only", True)
            if not isinstance(check_only, bool):
                raise ToolError("invalid_arguments", "check_only must be boolean")
            command = ["fmt", "--format", "json"]
            if check_only:
                command.append("--check")
            command.append(os.fspath(path))
            return self._invoke(command)
        if tool == "convert":
            self._expect_keys(arguments, {"path", "target_format"}, {"options"})
            path = self._path(arguments, "path")
            target_format = self._string(arguments, "target_format")
            if target_format not in {"marksheet", "xlsx", "csv"}:
                raise ToolError(
                    "invalid_arguments", "target_format must be marksheet, xlsx, or csv"
                )
            options = arguments.get("options", {})
            if not isinstance(options, dict):
                raise ToolError("invalid_arguments", "options must be an object")
            self._expect_keys(
                options,
                set(),
                {"output", "sheet", "label", "range", "table", "anchor"},
            )
            command = ["convert", "--to", target_format]
            for name in ("output", "sheet", "label", "range", "table", "anchor"):
                if name not in options:
                    continue
                value = options[name]
                if not isinstance(value, str) or not value:
                    raise ToolError("invalid_arguments", f"options.{name} must be a string")
                if name == "output":
                    value = os.fspath(self._resolved_path(value, must_exist=False))
                command.extend([f"--{name}", value])
            command.append(os.fspath(path))
            return self._invoke(command)
        if tool == "semantic_diff":
            self._expect_keys(arguments, {"old_path", "new_path"})
            old_path = self._path(arguments, "old_path")
            new_path = self._path(arguments, "new_path")
            return self._invoke(
                ["diff", "--format", "json", os.fspath(old_path), os.fspath(new_path)]
            )
        raise ToolError("unknown_tool", f"unknown tool {tool!r}")

    def _invoke(self, arguments: list[str]) -> dict[str, Any]:
        self._check_argument_bytes(arguments)
        try:
            return_code, stdout, stderr_bytes = self._run_bounded(arguments)
        except OSError as error:
            raise ToolError("process_error", f"cannot execute marksheet: {error}") from error
        stderr = stderr_bytes.decode("utf-8", errors="replace")
        result: Any = None
        if stdout:
            try:
                result = json.loads(stdout)
            except (UnicodeDecodeError, json.JSONDecodeError):
                result = {"text": stdout.decode("utf-8", errors="replace")}
        return self._response(return_code, result, stderr)

    def _run_bounded(self, arguments: list[str]) -> tuple[int, bytes, bytes]:
        process = subprocess.Popen(
            [self.marksheet, *arguments],
            cwd=self.workspace,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        assert process.stdout is not None
        assert process.stderr is not None
        stdout_fd = process.stdout.fileno()
        stderr_fd = process.stderr.fileno()
        output = {stdout_fd: bytearray(), stderr_fd: bytearray()}
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ)
        selector.register(process.stderr, selectors.EVENT_READ)
        deadline = time.monotonic() + MAX_PROCESS_SECONDS
        try:
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    process.kill()
                    process.wait()
                    raise ToolError("process_timeout", "marksheet exceeded 30 seconds")
                for key, _ in selector.select(timeout=remaining):
                    chunk = os.read(key.fd, 64 * 1024)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    output[key.fd].extend(chunk)
                    if sum(map(len, output.values())) > MAX_RESPONSE_BYTES:
                        process.kill()
                        process.wait()
                        raise ToolError("response_limit", "tool response exceeds 32 MiB")
            return_code = process.wait()
        finally:
            selector.close()
            process.stdout.close()
            process.stderr.close()
            if process.poll() is None:
                process.kill()
                process.wait()
        return return_code, bytes(output[stdout_fd]), bytes(output[stderr_fd])

    @staticmethod
    def _response(exit_code: int, result: Any, stderr: str) -> dict[str, Any]:
        committed_invalid = (
            exit_code == 1
            and isinstance(result, dict)
            and result.get("status") == "committed_invalid"
            and result.get("changed") is True
        )
        if committed_invalid:
            status = "committed_invalid"
        elif exit_code == 0:
            status = "ok"
        elif exit_code == 1:
            status = "rejected"
        else:
            status = "error"
        return {
            "ok": exit_code == 0 or committed_invalid,
            "status": status,
            "exit_code": exit_code,
            "result": result,
            "stderr": stderr,
        }

    def _path(self, arguments: dict[str, Any], name: str) -> Path:
        path = self._resolved_path(self._string(arguments, name), must_exist=True)
        try:
            metadata = path.stat()
        except OSError as error:
            raise ToolError("path_error", f"cannot inspect path {path}: {error}") from error
        if not stat.S_ISREG(metadata.st_mode):
            raise ToolError("path_error", "input path must be a regular file")
        if metadata.st_size > MAX_WORKBOOK_BYTES:
            raise ToolError("resource_limit", "input file exceeds 32 MiB")
        return path

    def _resolved_path(self, value: str, *, must_exist: bool) -> Path:
        candidate = Path(value)
        if not candidate.is_absolute():
            candidate = self.workspace / candidate
        resolved = candidate.resolve(strict=False)
        try:
            resolved.relative_to(self.workspace)
        except ValueError as error:
            raise ToolError(
                "path_outside_workspace",
                f"path {value!r} is outside the configured workspace",
            ) from error
        try:
            if must_exist:
                resolved = candidate.resolve(strict=True)
            else:
                parent = candidate.parent.resolve(strict=True)
                resolved = parent / candidate.name
        except OSError as error:
            raise ToolError("path_error", f"cannot resolve path {value!r}: {error}") from error
        try:
            resolved.relative_to(self.workspace)
        except ValueError as error:
            raise ToolError(
                "path_outside_workspace",
                f"path {value!r} resolved outside the configured workspace",
            ) from error
        return resolved

    @staticmethod
    def _string(
        arguments: dict[str, Any], name: str, *, allow_empty: bool = False
    ) -> str:
        value = arguments.get(name)
        if not isinstance(value, str) or (not value and not allow_empty):
            qualifier = "a string" if allow_empty else "a non-empty string"
            raise ToolError("invalid_arguments", f"{name} must be {qualifier}")
        return value

    @staticmethod
    def _check_argument_bytes(values: list[str]) -> None:
        total = 0
        for value in values:
            total += len(value.encode("utf-8"))
            if total > MAX_ARGUMENT_BYTES:
                raise ToolError(
                    "request_limit", "command arguments exceed the 1 MiB limit"
                )

    @staticmethod
    def _expect_keys(
        value: dict[str, Any], required: set[str], optional: set[str] | None = None
    ) -> None:
        optional = optional or set()
        missing = required - value.keys()
        unexpected = value.keys() - required - optional
        if missing:
            raise ToolError(
                "invalid_arguments", f"missing fields: {', '.join(sorted(missing))}"
            )
        if unexpected:
            raise ToolError(
                "invalid_arguments",
                f"unexpected fields: {', '.join(sorted(unexpected))}",
            )


def error_response(request_id: Any, error: ToolError) -> dict[str, Any]:
    return {
        "version": VERSION,
        "id": request_id,
        "ok": False,
        "status": "error",
        "exit_code": 2,
        "result": None,
        "stderr": "",
        "error": {"kind": error.kind, "message": error.message},
    }


def safe_request_id(request: Any) -> Any:
    if not isinstance(request, dict):
        return None
    request_id = request.get("id")
    if isinstance(request_id, str):
        return request_id
    if (
        isinstance(request_id, int)
        and not isinstance(request_id, bool)
        and abs(request_id) <= MAX_SAFE_INTEGER
    ):
        return request_id
    return None


def serve(server: ToolServer) -> int:
    while True:
        raw_line = sys.stdin.buffer.readline(MAX_REQUEST_BYTES + 1)
        if not raw_line:
            break
        request: Any = None
        if len(raw_line) > MAX_REQUEST_BYTES:
            while raw_line and not raw_line.endswith(b"\n"):
                raw_line = sys.stdin.buffer.readline(MAX_REQUEST_BYTES + 1)
            response = error_response(
                None, ToolError("request_limit", "request exceeds 8 MiB")
            )
        else:
            try:
                request = json.loads(raw_line)
                if not isinstance(request, dict):
                    raise ToolError("invalid_request", "request must be an object")
                response = server.dispatch(request)
            except (
                UnicodeDecodeError,
                json.JSONDecodeError,
                ValueError,
                RecursionError,
            ) as error:
                response = error_response(
                    None, ToolError("invalid_json", f"invalid JSON request: {error}")
                )
            except ToolError as error:
                response = error_response(safe_request_id(request), error)
        encoded = json.dumps(response, separators=(",", ":"), ensure_ascii=False).encode()
        if len(encoded) > MAX_RESPONSE_BYTES:
            encoded = json.dumps(
                error_response(
                    response.get("id"),
                    ToolError("response_limit", "tool response exceeds 32 MiB"),
                ),
                separators=(",", ":"),
            ).encode()
        sys.stdout.buffer.write(encoded + b"\n")
        sys.stdout.buffer.flush()
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", required=True, type=Path)
    parser.add_argument(
        "--marksheet", default=os.environ.get("MARKSHEET_BIN", "marksheet")
    )
    options = parser.parse_args()
    try:
        server = ToolServer(options.workspace, options.marksheet)
    except ToolError as error:
        print(error.message, file=sys.stderr)
        return 2
    return serve(server)


if __name__ == "__main__":
    raise SystemExit(main())
