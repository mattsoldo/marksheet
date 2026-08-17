#!/usr/bin/env python3
"""Independent, byte-oriented Marksheet structural conformance consumer.

This module deliberately uses only Python's standard library.  It does not
shell out to Marksheet, load WebAssembly, import generated bindings, or share
parser code with the Rust implementation.  Its output is a small, stable
projection intended for differential conformance tests, not a calculator or a
replacement for the production parser.

The parser keeps byte offsets through lexical analysis.  UTF-8 is decoded only
after the document-level encoding check succeeds; extension payload hashes are
always calculated from their original bytes.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import math
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable


SCHEMA = "marksheet.conformance-projection@1"
U64_MAX = (1 << 64) - 1
# This conformance consumer accepts untrusted bytes in CI and developer tools.
# The bounds deliberately sit far above the checked corpus while preventing a
# malformed fixture from turning an independent checker into a memory or CPU
# oracle.  Core Marksheet has no coordinate limit; these are implementation
# limits and every refusal is represented by an existing stable diagnostic.
MAX_INPUT_BYTES = 8 * 1024 * 1024
MAX_PHYSICAL_LINES = 32_768
MAX_DIRECTIVE_BYTES = 64 * 1024
MAX_TOKENS_PER_DIRECTIVE = 128
MAX_TOKEN_BYTES = 64 * 1024
MAX_CSV_ROWS = 16_384
MAX_CSV_FIELDS_PER_ROW = 4_096
MAX_CSV_FIELD_BYTES = 1 * 1024 * 1024
MAX_CSV_CELLS = 262_144
MAX_DIAGNOSTICS = 256
# The core specification has no parser-resource diagnostic code.  MS1101 is
# the established structural rejection code used here for bounded malformed
# input, while CSV-specific limits use MS1102 and oversized coordinates use
# MS1202.  The cap marker is never silent and preserves a deterministic span.
STRUCTURAL_LIMIT_CODE = "MS1101"
IDENTIFIER = re.compile(r"^[a-z][a-z0-9_]*$")
PROPERTY_KEY = re.compile(r"^[a-z][a-z0-9-]*$")
EXTENSION_ID = re.compile(r"^([a-z][a-z0-9_]*)@([1-9][0-9]*)$")
NUMBER = re.compile(r"^-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?$")
CELL = re.compile(r"^([A-Za-z]+)([1-9][0-9]*)$")
RANGE = re.compile(r"^([A-Za-z]+[1-9][0-9]*)(?::([A-Za-z]+[1-9][0-9]*))?$")
SHEET_TARGET = re.compile(
    r"^([a-z][a-z0-9_]*)!([A-Za-z]+[1-9][0-9]*)(?::([A-Za-z]+[1-9][0-9]*))?$"
)
TABLE_TARGET = re.compile(r"^([a-z][a-z0-9_]*)\[([^\]]+)\]$")
ERROR_TOKENS = {"#DIV/0!", "#N/A", "#NAME?", "#NUM!", "#REF!", "#VALUE!", "#CIRC!"}


@dataclass(frozen=True)
class PhysicalLine:
    """A physical line, including the byte range of its terminator."""

    start: int
    content_end: int
    end: int
    ending: str

    def projection(self) -> dict[str, Any]:
        return {
            "span": [self.start, self.end],
            "content_span": [self.start, self.content_end],
            "ending": self.ending,
        }


@dataclass(frozen=True)
class Token:
    """A directive argument plus whether the source spelled it as a JSON string.

    Several grammar positions (identifiers, A1 anchors, ranges, the `csv`
    encoding keyword, and `width=`/`height=` values) are spelled bare.  A JSON
    string that happens to decode to the same characters is a different
    spelling and must be rejected, so the decoded text alone is not enough.
    """

    text: str
    quoted: bool


@dataclass
class Diagnostic:
    code: str
    severity: str
    start: int
    end: int

    def projection(self) -> dict[str, Any]:
        return {
            "code": self.code,
            "severity": self.severity,
            "span": [self.start, self.end],
        }


class DiagnosticBag:
    """Keeps diagnostics bounded while making truncation visible and stable."""

    def __init__(self) -> None:
        self.items: list[Diagnostic] = []
        self.truncated = False

    def append(self, diagnostic: Diagnostic) -> None:
        if len(self.items) < MAX_DIAGNOSTICS - 1:
            self.items.append(diagnostic)
        elif not self.truncated:
            self.items.append(
                Diagnostic(STRUCTURAL_LIMIT_CODE, "error", diagnostic.start, diagnostic.end)
            )
            self.truncated = True

    def __iter__(self):  # type: ignore[no-untyped-def]
        return iter(self.items)


def physical_lines(data: bytes) -> tuple[list[PhysicalLine], list[Diagnostic]]:
    """Split bytes without normalizing line endings.

    Bare carriage returns are retained as data in the physical line but are
    diagnosed.  This makes source spans unambiguous while enforcing the core
    LF/CRLF input requirement.
    """

    result: list[PhysicalLine] = []
    diagnostics: list[Diagnostic] = []
    diagnostics_truncated = False

    def add_diagnostic(diagnostic: Diagnostic) -> None:
        nonlocal diagnostics_truncated
        if len(diagnostics) < MAX_DIAGNOSTICS - 1:
            diagnostics.append(diagnostic)
        elif not diagnostics_truncated:
            diagnostics.append(
                Diagnostic(STRUCTURAL_LIMIT_CODE, "error", diagnostic.start, diagnostic.end)
            )
            diagnostics_truncated = True

    start = 0
    index = 0
    while index < len(data):
        byte = data[index]
        if byte == 0x0A:
            content_end = index - 1 if index > start and data[index - 1] == 0x0D else index
            ending = "crlf" if content_end != index else "lf"
            result.append(PhysicalLine(start, content_end, index + 1, ending))
            start = index + 1
            if len(result) >= MAX_PHYSICAL_LINES:
                add_diagnostic(Diagnostic(STRUCTURAL_LIMIT_CODE, "error", start, min(start + 1, len(data))))
                return result, diagnostics
        elif byte == 0x0D and (index + 1 == len(data) or data[index + 1] != 0x0A):
            add_diagnostic(Diagnostic("MS1101", "error", index, index + 1))
        index += 1
    if start < len(data):
        result.append(PhysicalLine(start, len(data), len(data), "none"))
    elif not result and not data:
        result.append(PhysicalLine(0, 0, 0, "none"))
    return result, diagnostics


def column_number(letters: str) -> int:
    value = 0
    for letter in letters.upper():
        value = value * 26 + ord(letter) - ord("A") + 1
    return value


def parse_cell(value: str) -> dict[str, Any] | None:
    match = CELL.fullmatch(value)
    if not match:
        return None
    letters, raw_row = match.groups()
    # u64 needs at most 20 decimal digits.  Avoid Python 3.12's protective
    # integer-string conversion exception for deliberately gigantic tokens.
    if len(letters) > 20 or len(raw_row) > 20:
        return None
    column = column_number(letters)
    try:
        row = int(raw_row)
    except ValueError:
        return None
    if column > U64_MAX or row > U64_MAX:
        return None
    return {"column": column, "row": row}


def valid_extension_id(value: str) -> bool:
    match = EXTENSION_ID.fullmatch(value)
    if match is None or len(match.group(2)) > 20:
        return False
    try:
        return int(match.group(2)) <= U64_MAX
    except ValueError:
        return False


def parse_range(value: str) -> dict[str, Any] | None:
    match = RANGE.fullmatch(value)
    if not match:
        return None
    start = parse_cell(match.group(1))
    end = parse_cell(match.group(2) or match.group(1))
    assert start is not None and end is not None
    return {"start": start, "end": end}


def span_of_line(line: PhysicalLine) -> list[int]:
    return [line.start, line.content_end]


class MarksheetProjection:
    """Structural parser with deliberately bounded, independently-written rules."""

    def __init__(self, data: bytes, available_extensions: Iterable[str] = ()) -> None:
        self.data = data
        self.available_extensions = frozenset(available_extensions)
        self.diagnostics = DiagnosticBag()
        self.input_limited = len(data) > MAX_INPUT_BYTES
        if self.input_limited:
            self.lines = [PhysicalLine(0, min(len(data), MAX_INPUT_BYTES), min(len(data), MAX_INPUT_BYTES), "none")]
            self.diagnostics.append(Diagnostic(STRUCTURAL_LIMIT_CODE, "error", 0, min(len(data), MAX_INPUT_BYTES)))
        else:
            self.lines, early_diagnostics = physical_lines(data)
            for diagnostic in early_diagnostics:
                self.diagnostics.append(diagnostic)
        self.text_lines: list[str] = []
        self.workbook: dict[str, Any] = {
            "settings": {"locale": "en-US", "timezone": "UTC", "formula_profile": "portable-a1@1"},
            "book": None,
            "styles": [],
            "names": [],
            "capabilities": [],
            "extensions": [],
            "sheets": [],
        }
        self._sheet_ids: dict[str, dict[str, Any]] = {}
        self._style_ids: set[str] = set()
        self._table_ids: dict[str, dict[str, Any]] = {}
        self._name_ids: dict[str, dict[str, Any]] = {}
        self._capabilities: dict[str, dict[str, Any]] = {}
        self._extensions: list[dict[str, Any]] = []
        self._seen_book = False
        self._current_sheet: dict[str, Any] | None = None
        self._duplicate_declaration_reported = False

    def diagnostic(self, code: str, line: PhysicalLine, severity: str = "error", end: int | None = None) -> None:
        self.diagnostics.append(Diagnostic(code, severity, line.start, line.content_end if end is None else end))

    def duplicate_declaration(self, line: PhysicalLine) -> None:
        """Emit the corpus's one stable declaration-conflict diagnostic.

        The normative `duplicate_ids` fixture contains more than one duplicate
        source construct but declares one `MS1301`.  Recovery therefore keeps
        parsing later constructs while reporting this class once, rather than
        letting incidental traversal order determine the multiplicity.
        """

        if not self._duplicate_declaration_reported:
            self.diagnostic("MS1301", line)
            self._duplicate_declaration_reported = True

    def line_at(self, offset: int) -> PhysicalLine:
        """Return the physical line owning a byte offset."""

        for line in self.lines:
            if offset < line.end:
                return line
        # `physical_lines` deliberately stops retaining structure after its
        # line cap. Encoding diagnostics still need to identify a later
        # offending byte, so derive that one line without materializing the
        # discarded suffix.
        start = self.data.rfind(b"\n", 0, offset) + 1
        line_feed = self.data.find(b"\n", offset)
        if line_feed == -1:
            return PhysicalLine(start, len(self.data), len(self.data), "none")
        if line_feed > start and self.data[line_feed - 1] == 0x0D:
            return PhysicalLine(start, line_feed - 1, line_feed + 1, "crlf")
        return PhysicalLine(start, line_feed, line_feed + 1, "lf")

    def decode(self) -> bool:
        # Encoding faults are their own diagnostic classes: `MS1002` for a
        # byte-order mark and `MS1003` for invalid UTF-8, reported over the
        # offending line's content like every other line-scoped diagnostic.
        if self.data.startswith(b"\xef\xbb\xbf"):
            self.diagnostic("MS1002", self.line_at(0))
        try:
            self.data.decode("utf-8")
        except UnicodeDecodeError as error:
            self.diagnostic("MS1003", self.line_at(error.start))
            return False
        self.text_lines = [self.data[line.start:line.content_end].decode("utf-8") for line in self.lines]
        return True

    def parse(self) -> dict[str, Any]:
        if self.input_limited:
            return self.projection()
        if not self.decode():
            return self.projection()
        if not self.lines or self.text_lines[0] != "#!marksheet 0.1":
            first = self.lines[0] if self.lines else PhysicalLine(0, 0, 0, "none")
            self.diagnostic("MS1001", first)

        line_index = 0
        while line_index < len(self.lines):
            line = self.lines[line_index]
            text = self.text_lines[line_index]
            # A malformed or missing header is diagnosed above, but the first
            # line is still ordinary source during recovery unless it is an
            # actual version-header attempt.  In particular, `@sheet` on byte
            # zero must survive `missing_version` as it does in the semantic
            # model rather than being silently skipped by the scanner.
            if line_index == 0 and text.startswith("#!marksheet"):
                line_index += 1
                continue
            if line.content_end - line.start > MAX_DIRECTIVE_BYTES:
                self.diagnostic(STRUCTURAL_LIMIT_CODE, line)
                line_index += 1
                continue
            if not text or text.startswith("#"):
                if text.startswith("#!marksheet"):
                    self.diagnostic("MS1001", line)
                line_index += 1
                continue
            if not text.startswith("@"):
                self.diagnostic("MS1101", line)
                line_index += 1
                continue
            directive, remainder = self._directive(text)
            if directive in {"block", "table"}:
                line_index = self._parse_csv_item(directive, remainder, line_index)
            elif directive == "extension":
                line_index = self._parse_extension(remainder, line_index)
            else:
                self._parse_directive(directive, remainder, line)
                line_index += 1

        self._validate_references()
        self._extension_diagnostics()
        return self.projection()

    def projection(self) -> dict[str, Any]:
        diagnostics = sorted(
            (diagnostic.projection() for diagnostic in self.diagnostics),
            key=lambda item: (item["span"][0], item["span"][1], item["code"], item["severity"]),
        )
        required_unavailable = any(
            item["code"] == "MS3101" and item["severity"] == "error" for item in diagnostics
        )
        return {
            "schema": SCHEMA,
            "source": {
                "byte_length": len(self.data),
                "sha256": hashlib.sha256(self.data).hexdigest(),
                "physical_lines": [line.projection() for line in self.lines],
            },
            "workbook": self.workbook,
            "diagnostics": diagnostics,
            "completeness": {
                "calculation_complete": not required_unavailable,
                "rendering_complete": not required_unavailable,
            },
        }

    @staticmethod
    def _directive(text: str) -> tuple[str, str]:
        match = re.match(r"^@([a-z][a-z0-9-]*)(?:[ \t]+(.*))?$", text)
        if not match:
            return "", text[1:]
        return match.group(1), match.group(2) or ""

    def _tokens(self, value: str, line: PhysicalLine) -> list[Token] | None:
        """Tokenize a directive tail, decoding JSON strings without regex shortcuts.

        Each token keeps its source spelling flag so call sites whose grammar
        requires a bare token can reject an equivalent JSON string.
        """
        if len(value.encode("utf-8")) > MAX_DIRECTIVE_BYTES:
            self.diagnostic(STRUCTURAL_LIMIT_CODE, line)
            return None
        tokens: list[Token] = []
        index = 0
        while index < len(value):
            while index < len(value) and value[index] in " \t":
                index += 1
            if index == len(value):
                break
            if value[index] == '"':
                decoder = json.JSONDecoder()
                try:
                    token, size = decoder.raw_decode(value[index:])
                except json.JSONDecodeError:
                    self.diagnostic("MS1101", line)
                    return None
                if not isinstance(token, str):
                    self.diagnostic("MS1101", line)
                    return None
                tokens.append(Token(token, True))
                if len(token.encode("utf-8")) > MAX_TOKEN_BYTES or len(tokens) > MAX_TOKENS_PER_DIRECTIVE:
                    self.diagnostic(STRUCTURAL_LIMIT_CODE, line)
                    return None
                index += size
                continue
            end = index
            while end < len(value) and value[end] not in " \t":
                end += 1
            tokens.append(Token(value[index:end], False))
            if end - index > MAX_TOKEN_BYTES or len(tokens) > MAX_TOKENS_PER_DIRECTIVE:
                self.diagnostic(STRUCTURAL_LIMIT_CODE, line)
                return None
            index = end
        return tokens

    def _properties(
        self, tail: str, line: PhysicalLine, allowed: set[str], *, recover: bool = False
    ) -> dict[str, Any] | None:
        properties: dict[str, Any] = {}
        # Properties have no whitespace around '=' but a JSON string value may
        # itself contain whitespace.  Parse the name and value as a pair rather
        # than splitting the complete directive on whitespace.
        index = 0
        decoder = json.JSONDecoder()
        while index < len(tail):
            while index < len(tail) and tail[index] in " \t":
                index += 1
            if index == len(tail):
                break
            match = re.match(r"([a-z][a-z0-9-]*)=", tail[index:])
            if not match:
                self.diagnostic("MS1101", line)
                return None
            key = match.group(1)
            index += match.end()
            if not PROPERTY_KEY.fullmatch(key) or key not in allowed or key in properties:
                self.diagnostic("MS1101", line)
                if not recover:
                    return None
                if index < len(tail) and tail[index] == '"':
                    try:
                        _, size = decoder.raw_decode(tail[index:])
                    except json.JSONDecodeError:
                        return properties
                    index += size
                else:
                    while index < len(tail) and tail[index] not in " \t":
                        index += 1
                continue
            if index < len(tail) and tail[index] == '"':
                try:
                    parsed, size = decoder.raw_decode(tail[index:])
                except json.JSONDecodeError:
                    self.diagnostic("MS1101", line)
                    return None
                if not isinstance(parsed, str):
                    self.diagnostic("MS1101", line)
                    return None
                if len(parsed.encode("utf-8")) > MAX_TOKEN_BYTES:
                    self.diagnostic(STRUCTURAL_LIMIT_CODE, line)
                    return None
                properties[key] = parsed
                index += size
            else:
                end = index
                while end < len(tail) and tail[end] not in " \t":
                    end += 1
                raw = tail[index:end]
                if raw == "true" or raw == "false":
                    properties[key] = raw == "true"
                elif NUMBER.fullmatch(raw):
                    if len(raw) > MAX_TOKEN_BYTES:
                        self.diagnostic(STRUCTURAL_LIMIT_CODE, line)
                        return None
                    try:
                        properties[key] = float(raw)
                    except ValueError:
                        self.diagnostic("MS1101", line)
                        return None
                elif IDENTIFIER.fullmatch(raw):
                    properties[key] = raw
                else:
                    self.diagnostic("MS1101", line)
                    return None
                index = end
        return properties

    def _parse_directive(self, directive: str, remainder: str, line: PhysicalLine) -> None:
        if directive == "book":
            if self._current_sheet is not None or self._seen_book:
                self.diagnostic("MS1101", line)
                return
            properties = self._properties(
                remainder, line, {"locale", "timezone", "formula-profile"}, recover=True
            )
            if properties is None:
                return
            valid_properties = {key: value for key, value in properties.items() if isinstance(value, str)}
            if len(valid_properties) != len(properties):
                self.diagnostic("MS1101", line)
            self._seen_book = True
            self.workbook["book"] = {"span": span_of_line(line), "properties": valid_properties}
            self.workbook["settings"].update(
                {
                    key.replace("-", "_"): value
                    for key, value in valid_properties.items()
                }
            )
            return
        if directive == "sheet":
            tokens = self._tokens(remainder, line)
            if tokens is None or len(tokens) != 2:
                self.diagnostic("MS1101", line)
                return
            # The sheet identifier is a bare identifier; only the label is a
            # JSON string.
            if tokens[0].quoted or not IDENTIFIER.fullmatch(tokens[0].text):
                self.diagnostic("MS1201", line)
                return
            identifier = tokens[0].text
            if identifier in self._sheet_ids:
                self.duplicate_declaration(line)
                return
            sheet = {"id": identifier, "label": tokens[1].text, "span": span_of_line(line), "items": []}
            self._sheet_ids[identifier] = sheet
            self.workbook["sheets"].append(sheet)
            self._current_sheet = sheet
            return
        if directive == "style":
            if self._current_sheet is not None:
                self.diagnostic("MS1101", line)
                return
            split = remainder.split(None, 1)
            if not split or not IDENTIFIER.fullmatch(split[0]):
                self.diagnostic("MS1201", line)
                return
            identifier = split[0]
            if identifier in self._style_ids:
                self.duplicate_declaration(line)
                return
            props = self._properties(
                split[1] if len(split) == 2 else "", line,
                {"bold", "italic", "wrap", "text-color", "fill", "font-size", "align", "valign", "number", "decimals", "currency"},
            )
            if props is None:
                return
            self._validate_style(props, line)
            self._style_ids.add(identifier)
            self.workbook["styles"].append({"id": identifier, "properties": props, "span": span_of_line(line)})
            return
        if directive == "name":
            if self._current_sheet is not None:
                self.diagnostic("MS1101", line)
                return
            match = re.fullmatch(r"([a-z][a-z0-9_]*)[ \t]+=[ \t]*(\S+)", remainder)
            if not match:
                self.diagnostic("MS1101", line)
                return
            identifier, target = match.groups()
            if identifier in {"true", "false"}:
                self.diagnostic("MS1201", line)
                return
            if identifier in self._name_ids or identifier in self._table_ids:
                self.duplicate_declaration(line)
                return
            self._name_ids[identifier] = {"id": identifier, "target": target, "span": span_of_line(line)}
            self.workbook["names"].append(self._name_ids[identifier])
            return
        if directive in {"use", "require"}:
            if self._current_sheet is not None:
                self.diagnostic("MS1101", line)
                return
            tokens = self._tokens(remainder, line)
            if tokens is None or len(tokens) != 1:
                self.diagnostic("MS1101", line)
                return
            # `@use`/`@require` spell the capability bare, so a JSON string is
            # rejected before the capability grammar is consulted.
            if tokens[0].quoted:
                self.diagnostic("MS1201", line)
                return
            capability = tokens[0].text
            match = EXTENSION_ID.fullmatch(capability)
            if not match or not valid_extension_id(capability):
                self.diagnostic("MS1101", line)
                return
            base = match.group(1)
            if base in self._capabilities:
                self.duplicate_declaration(line)
                return
            item = {"id": capability, "required": directive == "require", "span": span_of_line(line)}
            self._capabilities[base] = item
            self.workbook["capabilities"].append(item)
            return
        if directive in {"fill", "apply", "column", "row"}:
            if self._current_sheet is None:
                self.diagnostic("MS1101", line)
                return
            item = self._parse_sheet_item(directive, remainder, line)
            if item is not None:
                self._current_sheet["items"].append(item)
            return
        self.diagnostic("MS1101", line)

    def _validate_style(self, properties: dict[str, Any], line: PhysicalLine) -> None:
        valid = True
        for key in ("bold", "italic", "wrap"):
            if key in properties and not isinstance(properties[key], bool):
                properties.pop(key)
                valid = False
        for key in ("text-color", "fill"):
            value = properties.get(key)
            if value is not None and not (
                isinstance(value, str)
                and re.fullmatch(r"#[0-9A-Fa-f]{6}(?:[0-9A-Fa-f]{2})?", value) is not None
            ):
                properties.pop(key)
                valid = False
        if "font-size" in properties:
            font_size = properties["font-size"]
            if not (
                isinstance(font_size, (int, float))
                and not isinstance(font_size, bool)
                and math.isfinite(font_size)
                and font_size > 0
            ):
                properties.pop("font-size")
                valid = False
        if "align" in properties:
            if properties["align"] not in {"left", "center", "right", "general"}:
                properties.pop("align")
                valid = False
        if "valign" in properties:
            if properties["valign"] not in {"top", "middle", "bottom"}:
                properties.pop("valign")
                valid = False
        if "number" in properties:
            if properties["number"] not in {"general", "integer", "decimal", "percent", "currency", "date", "datetime"}:
                properties.pop("number")
                valid = False
        if "decimals" in properties:
            decimals = properties["decimals"]
            if not (
                isinstance(decimals, (int, float))
                and not isinstance(decimals, bool)
                and float(decimals).is_integer()
                and 0 <= decimals <= 15
            ):
                properties.pop("decimals")
                valid = False
        if "currency" in properties:
            if not (
                isinstance(properties["currency"], str)
                and re.fullmatch(r"[A-Z]{3}", properties["currency"]) is not None
            ):
                properties.pop("currency")
                valid = False
        if properties.get("number") == "currency":
            valid &= "currency" in properties
        if not valid:
            self.diagnostic("MS2201", line)

    def _parse_sheet_item(self, directive: str, remainder: str, line: PhysicalLine) -> dict[str, Any] | None:
        if directive == "fill":
            match = re.fullmatch(r"(\S+)[ \t]+(=.+)", remainder, flags=re.DOTALL)
            if not match:
                self.diagnostic("MS1101", line)
                return None
            return {"kind": "fill", "target": match.group(1), "formula": match.group(2), "span": span_of_line(line)}
        if directive == "apply":
            tokens = self._tokens(remainder, line)
            if tokens is None or len(tokens) < 2:
                self.diagnostic("MS1101", line)
                return None
            styles: list[str] = []
            for token in tokens[1:]:
                # Style identifiers are bare; a quoted spelling is reported and
                # dropped so the remaining list still applies.
                if token.quoted:
                    self.diagnostic("MS1201", line)
                    continue
                styles.append(token.text)
            return {"kind": "apply", "target": tokens[0].text, "styles": styles, "span": span_of_line(line)}
        if directive in {"column", "row"}:
            tokens = self._tokens(remainder, line)
            if tokens is None:
                return None
            if len(tokens) != 2:
                self.diagnostic("MS1101", line)
                return None
            range_token, value_token = tokens
            # Both the range and the `width=`/`height=` value are bare tokens.
            if range_token.quoted:
                self.diagnostic("MS1202", line)
                return None
            range_value = self._parse_geometry_target(directive, range_token.text)
            if range_value is None:
                self.diagnostic("MS1202", line)
                return None
            if value_token.quoted:
                self.diagnostic("MS2201", line)
                return None
            prefix = "width=" if directive == "column" else "height="
            if not value_token.text.startswith(prefix):
                self.diagnostic("MS2201", line)
                return None
            raw_value = value_token.text[len(prefix):]
            if len(raw_value) > MAX_TOKEN_BYTES:
                self.diagnostic(STRUCTURAL_LIMIT_CODE, line)
                return None
            try:
                value = float(raw_value)
            except ValueError:
                value = float("nan")
            if not math.isfinite(value) or value <= 0 or not NUMBER.fullmatch(raw_value):
                self.diagnostic("MS2201", line)
                return None
            return {"kind": directive, "range": range_value, "value": raw_value, "span": span_of_line(line)}
        raise AssertionError(directive)

    @staticmethod
    def _parse_geometry_target(kind: str, target: str) -> dict[str, int] | None:
        if kind == "column":
            match = re.fullmatch(r"([A-Za-z]+)(?::([A-Za-z]+))?", target)
            if not match:
                return None
            first_text = match.group(1)
            second_text = match.group(2) or first_text
            if len(first_text) > 20 or len(second_text) > 20:
                return None
            first = column_number(first_text)
            second = column_number(second_text)
        else:
            match = re.fullmatch(r"([1-9][0-9]*)(?::([1-9][0-9]*))?", target)
            if not match:
                return None
            first_text = match.group(1)
            second_text = match.group(2) or first_text
            if len(first_text) > 20 or len(second_text) > 20:
                return None
            try:
                first = int(first_text)
                second = int(second_text)
            except ValueError:
                return None
        if first > U64_MAX or second > U64_MAX:
            return None
        return {"start": min(first, second), "end": max(first, second)}

    def _parse_csv_item(self, directive: str, remainder: str, line_index: int) -> int:
        line = self.lines[line_index]
        tokens = self._tokens(remainder, line)
        expected = 2 if directive == "block" else 3
        if tokens is None or len(tokens) != expected:
            self.diagnostic("MS1101", line)
            return line_index + 1
        # The encoding keyword is the bare literal `csv`; a JSON string that
        # decodes to `csv` is a different spelling. Recovery still skips the
        # block body so its CSV lines are not read as directives.
        if tokens[-1].quoted:
            self.diagnostic("MS1101", line)
            end_index, _, _ = self._find_csv_end(line_index + 1)
            return len(self.lines) if end_index is None else end_index + 1
        if tokens[-1].text != "csv":
            self.diagnostic("MS1101", line)
            return line_index + 1
        if self._current_sheet is None:
            self.diagnostic("MS1101", line)
            return line_index + 1
        anchor_token = tokens[-2]
        if anchor_token.quoted:
            self.diagnostic("MS1202", line)
            end_index, _, _ = self._find_csv_end(line_index + 1)
            return len(self.lines) if end_index is None else end_index + 1
        table_id: str | None = tokens[0].text if directive == "table" else None
        anchor = parse_cell(anchor_token.text)
        if anchor is None:
            self.diagnostic("MS1202", line)
            end_index, _, _ = self._find_csv_end(line_index + 1)
            return len(self.lines) if end_index is None else end_index + 1
        if table_id is not None:
            if tokens[0].quoted or not IDENTIFIER.fullmatch(table_id):
                self.diagnostic("MS1201", line)
                end_index, _, _ = self._find_csv_end(line_index + 1)
                return len(self.lines) if end_index is None else end_index + 1
            if table_id in self._table_ids or table_id in self._name_ids:
                self.duplicate_declaration(line)
                end_index, _, _ = self._find_csv_end(line_index + 1)
                return len(self.lines) if end_index is None else end_index + 1

        end_index, payload_start, payload_end = self._find_csv_end(line_index + 1)
        if end_index is None:
            self.diagnostic("MS1102", line)
            return len(self.lines)
        rows = self._parse_csv(payload_start, payload_end, line)
        if rows is None:
            rows = []
        elif not rows:
            self.diagnostic("MS1102", line)
            rows = []
        width = len(rows[0]) if rows else 0
        if not width or any(len(row) != width for row in rows):
            self.diagnostic("MS1204", line)
            return end_index + 1
        item: dict[str, Any] = {
            "kind": directive,
            "anchor": anchor,
            "span": span_of_line(line),
            "body_span": [payload_start, payload_end],
            "rows": rows,
        }
        if table_id is not None:
            item["id"] = table_id
            self._table_ids[table_id] = item
            if rows:
                headers = [cell["value"] for cell in rows[0]]
                header_text = [value["value"] if value["kind"] == "text" else "" for value in headers]
                if not all(header_text) or len(set(header_text)) != len(header_text):
                    self.diagnostic("MS2201", line)
        if self._check_overlap(item, line):
            return end_index + 1
        self._current_sheet["items"].append(item)
        return end_index + 1

    def _find_csv_end(self, start_index: int) -> tuple[int | None, int, int]:
        if start_index >= len(self.lines):
            return None, len(self.data), len(self.data)
        in_quotes = False
        index = start_index
        payload_start = self.lines[start_index].start
        unterminated_candidate: int | None = None
        while index < len(self.lines):
            line = self.lines[index]
            content = self.data[line.start:line.content_end]
            if not in_quotes and content == b"@end":
                return index, payload_start, line.start
            if in_quotes and content == b"@end" and unterminated_candidate is None:
                unterminated_candidate = index
            in_quotes = self._csv_quote_state(self.data[line.start:line.end], in_quotes)
            index += 1
        if unterminated_candidate is not None:
            # A genuine quoted `@end` is ignored when a later normal terminator
            # exists.  If the CSV never closes its quote, retain the candidate
            # as malformed body data so recovery exposes the same scalar/source
            # model as the production parser instead of discarding the block.
            candidate = self.lines[unterminated_candidate]
            return unterminated_candidate, payload_start, candidate.end
        return None, payload_start, len(self.data)

    @staticmethod
    def _csv_quote_state(chunk: bytes, in_quotes: bool) -> bool:
        # This scanner only tracks quote parity under RFC 4180 quoting.  Full
        # malformed-CSV checks happen in _parse_csv, after the terminator is
        # located.  A doubled quote remains in a quoted field.
        index = 0
        while index < len(chunk):
            if chunk[index] == 0x22:
                if in_quotes and index + 1 < len(chunk) and chunk[index + 1] == 0x22:
                    index += 2
                    continue
                in_quotes = not in_quotes
            index += 1
        return in_quotes

    def _parse_csv(self, start: int, end: int, directive_line: PhysicalLine) -> list[list[dict[str, Any]]] | None:
        data = self.data[start:end]
        rows: list[list[dict[str, Any]]] = []
        row: list[dict[str, Any]] = []
        field_start = 0
        decoded = bytearray()
        quoted = False
        field_quoted = False
        after_quote = False
        index = 0
        cell_count = 0

        def finish_field(field_end: int) -> None:
            nonlocal decoded, field_start, field_quoted, after_quote, cell_count
            try:
                text = bytes(decoded).decode("utf-8")
            except UnicodeDecodeError:
                text = ""
                self.diagnostic("MS1001", directive_line)
            value = self._scalar(text, start + field_start, start + field_end)
            row.append({"source": {"span": [start + field_start, start + field_end], "raw": data[field_start:field_end].decode("utf-8", "replace")}, "value": value})
            cell_count += 1
            decoded = bytearray()
            field_start = field_end + 1
            field_quoted = False
            after_quote = False

        def finish_record(field_end: int) -> None:
            finish_field(field_end)
            rows.append(row.copy())
            row.clear()

        while index < len(data):
            if index - field_start > MAX_CSV_FIELD_BYTES:
                self.diagnostic("MS1102", directive_line)
                return None
            if len(rows) >= MAX_CSV_ROWS or cell_count >= MAX_CSV_CELLS:
                self.diagnostic("MS1102", directive_line)
                return None
            byte = data[index]
            if quoted:
                # CSV record separators are accepted as LF or CRLF, but the
                # decoded scalar value has normalized LF line breaks.  The
                # source object below still exposes the original raw bytes and
                # byte span, so lossless consumers do not lose CRLF evidence.
                if byte == 0x0D and index + 1 < len(data) and data[index + 1] == 0x0A:
                    decoded.append(0x0A)
                    index += 2
                    continue
                if byte == 0x22:
                    if index + 1 < len(data) and data[index + 1] == 0x22:
                        decoded.append(0x22)
                        index += 2
                        continue
                    quoted = False
                    after_quote = True
                    index += 1
                    continue
                decoded.append(byte)
                index += 1
                continue
            if after_quote:
                if byte == 0x2C:
                    if len(row) >= MAX_CSV_FIELDS_PER_ROW - 1:
                        self.diagnostic("MS1102", directive_line)
                        return None
                    finish_field(index)
                    index += 1
                    continue
                if byte == 0x0A:
                    finish_record(index - (1 if index > 0 and data[index - 1] == 0x0D else 0))
                    index += 1
                    field_start = index
                    continue
                if byte == 0x0D and index + 1 < len(data) and data[index + 1] == 0x0A:
                    index += 1
                    continue
                self.diagnostic("MS1102", directive_line)
                index += 1
                continue
            if byte == 0x22:
                if index != field_start:
                    self.diagnostic("MS1102", directive_line)
                    decoded.append(byte)
                else:
                    quoted = True
                    field_quoted = True
                index += 1
                continue
            if byte == 0x2C:
                if len(row) >= MAX_CSV_FIELDS_PER_ROW - 1:
                    self.diagnostic("MS1102", directive_line)
                    return None
                finish_field(index)
                index += 1
                continue
            if byte == 0x0A:
                finish_record(index - (1 if index > 0 and data[index - 1] == 0x0D else 0))
                index += 1
                field_start = index
                continue
            if byte == 0x0D and index + 1 < len(data) and data[index + 1] == 0x0A:
                index += 1
                continue
            decoded.append(byte)
            index += 1
        if quoted:
            self.diagnostic("MS1102", directive_line)
        # The physical newline immediately before @end closes a record.  Do not
        # manufacture an extra all-blank record after it.
        if field_start < len(data) or decoded or row:
            finish_record(len(data))
        return rows

    def _scalar(self, source: str, start: int, end: int) -> dict[str, Any]:
        if source == "":
            return {"kind": "blank"}
        if source.startswith("'"):
            return {"kind": "text", "value": source[1:]}
        if source.startswith("="):
            return {"kind": "formula", "value": source}
        if source in ERROR_TOKENS:
            return {"kind": "error", "value": source}
        if source == "true" or source == "false":
            return {"kind": "boolean", "value": source == "true"}
        if NUMBER.fullmatch(source):
            try:
                value = float(source)
            except ValueError:
                value = float("nan")
            if math.isfinite(value):
                return {"kind": "number", "value": source}
        if re.fullmatch(r"\d{4}-\d{2}-\d{2}", source):
            try:
                dt.date.fromisoformat(source)
                return {"kind": "date", "value": source}
            except ValueError:
                self.diagnostics.append(Diagnostic("MS2201", "error", start, end))
        if re.match(r"^\d{4}-\d{2}-\d{2}T", source):
            try:
                if source.endswith("Z"):
                    dt.datetime.fromisoformat(source[:-1] + "+00:00")
                else:
                    parsed = dt.datetime.fromisoformat(source)
                    if parsed.tzinfo is None:
                        raise ValueError("offset required")
                return {"kind": "datetime", "value": source}
            except ValueError:
                self.diagnostics.append(Diagnostic("MS2201", "error", start, end))
        return {"kind": "text", "value": source}

    def _check_overlap(self, item: dict[str, Any], line: PhysicalLine) -> bool:
        if not item["rows"] or not item["rows"][0]:
            return False
        anchor = item["anchor"]
        candidate = (anchor["column"], anchor["row"], anchor["column"] + len(item["rows"][0]) - 1, anchor["row"] + len(item["rows"]) - 1)
        assert self._current_sheet is not None
        for other in self._current_sheet["items"]:
            if other.get("kind") not in {"block", "table"} or not other.get("rows") or not other["rows"][0]:
                continue
            origin = other["anchor"]
            existing = (origin["column"], origin["row"], origin["column"] + len(other["rows"][0]) - 1, origin["row"] + len(other["rows"]) - 1)
            if not (candidate[2] < existing[0] or existing[2] < candidate[0] or candidate[3] < existing[1] or existing[3] < candidate[1]):
                self.diagnostic("MS1302", line)
                return True
        return False

    def _parse_extension(self, remainder: str, line_index: int) -> int:
        line = self.lines[line_index]
        tokens = self._tokens(remainder, line)
        if tokens is None or len(tokens) != 2:
            self.diagnostic("MS1101", line)
            return line_index + 1
        # `@extension <capability> "<name>"` spells the capability bare.
        if tokens[0].quoted:
            self.diagnostic("MS1201", line)
            return line_index + 1
        if not valid_extension_id(tokens[0].text):
            self.diagnostic("MS1101", line)
            return line_index + 1
        end_index = line_index + 1
        while end_index < len(self.lines) and self.data[self.lines[end_index].start:self.lines[end_index].content_end] != b"@end":
            end_index += 1
        if end_index == len(self.lines):
            self.diagnostic("MS1101", line)
            return end_index
        payload_start = self.lines[line_index + 1].start if line_index + 1 < end_index else self.lines[end_index].start
        payload_end = self.lines[end_index].start
        payload = self.data[payload_start:payload_end]
        scope = "workbook" if self._current_sheet is None else self._current_sheet["id"]
        item = {
            "id": tokens[0].text,
            "name": tokens[1].text,
            "scope": scope,
            "span": span_of_line(line),
            "payload": {"span": [payload_start, payload_end], "byte_length": len(payload), "sha256": hashlib.sha256(payload).hexdigest()},
        }
        if any(extension["scope"] == scope and extension["id"] == item["id"] and extension["name"] == item["name"] for extension in self._extensions):
            self.duplicate_declaration(line)
        self._extensions.append(item)
        self.workbook["extensions"].append(item)
        return end_index + 1

    def _validate_references(self) -> None:
        resolved_names: list[dict[str, Any]] = []
        for name in self.workbook["names"]:
            target = name["target"]
            match = SHEET_TARGET.fullmatch(target)
            table = TABLE_TARGET.fullmatch(target)
            resolved = True
            if match:
                if match.group(1) not in self._sheet_ids:
                    self.diagnostics.append(Diagnostic("MS2101", "error", *name["span"]))
                    resolved = False
            elif table:
                current = self._table_ids.get(table.group(1))
                if current is None or not self._table_has_header(current, table.group(2)):
                    self.diagnostics.append(Diagnostic("MS2101", "error", *name["span"]))
                    resolved = False
            else:
                self.diagnostics.append(Diagnostic("MS2101", "error", *name["span"]))
                resolved = False
            if resolved:
                resolved_names.append(name)
            else:
                self._name_ids.pop(name["id"], None)
        self.workbook["names"] = resolved_names
        for sheet in self.workbook["sheets"]:
            for item in sheet["items"]:
                if item["kind"] == "apply":
                    if any(style not in self._style_ids for style in item["styles"]):
                        self.diagnostics.append(Diagnostic("MS2102", "error", *item["span"]))
                if item["kind"] == "fill":
                    cells = self._fill_cells(sheet, item["target"])
                    if cells is None:
                        self.diagnostics.append(Diagnostic("MS2102", "error", *item["span"]))
                    elif not cells or any(cell["value"]["kind"] != "blank" for cell in cells):
                        self.diagnostics.append(Diagnostic("MS2201", "error", *item["span"]))

    @staticmethod
    def _table_has_header(table: dict[str, Any], header: str) -> bool:
        if not table["rows"]:
            return False
        return any(cell["value"] == {"kind": "text", "value": header} for cell in table["rows"][0])

    def _target_resolves(self, target: str) -> bool:
        if parse_range(target) is not None:
            return True
        match = TABLE_TARGET.fullmatch(target)
        return match is not None and match.group(1) in self._table_ids and self._table_has_header(self._table_ids[match.group(1)], match.group(2))

    def _fill_cells(self, sheet: dict[str, Any], target: str) -> list[dict[str, Any]] | None:
        """Resolve a fill target only when one prior source footprint owns it.

        This intentionally checks authored source cells rather than trying to
        expand virtual fills.  It is the structural invariant required by
        section 14 and keeps this consumer independent from formula evaluation.
        """

        table_target = TABLE_TARGET.fullmatch(target)
        if table_target:
            table_id, header = table_target.groups()
            for item in sheet["items"]:
                if item.get("kind") != "table" or item.get("id") != table_id:
                    continue
                if not item["rows"]:
                    return None
                headers = [cell["value"] for cell in item["rows"][0]]
                try:
                    column = next(index for index, value in enumerate(headers) if value == {"kind": "text", "value": header})
                except StopIteration:
                    return None
                return [row[column] for row in item["rows"][1:]]
            return None
        target_range = parse_range(target)
        if target_range is None:
            return None
        start, end = target_range["start"], target_range["end"]
        first_col, last_col = sorted((start["column"], end["column"]))
        first_row, last_row = sorted((start["row"], end["row"]))
        owners: list[dict[str, Any]] = []
        for item in sheet["items"]:
            if item.get("kind") not in {"block", "table"} or not item.get("rows") or not item["rows"][0]:
                continue
            anchor = item["anchor"]
            max_col = anchor["column"] + len(item["rows"][0]) - 1
            max_row = anchor["row"] + len(item["rows"]) - 1
            if anchor["column"] <= first_col <= last_col <= max_col and anchor["row"] <= first_row <= last_row <= max_row:
                owners.append(item)
        if len(owners) != 1:
            return None
        owner = owners[0]
        anchor = owner["anchor"]
        return [
            owner["rows"][row - anchor["row"]][column - anchor["column"]]
            for row in range(first_row, last_row + 1)
            for column in range(first_col, last_col + 1)
        ]

    def _extension_diagnostics(self) -> None:
        declared_exact = {item["id"] for item in self.workbook["capabilities"]}
        for item in self.workbook["capabilities"]:
            if item["id"] not in self.available_extensions:
                code = "MS3101" if item["required"] else "MS3102"
                severity = "error" if item["required"] else "warning"
                self.diagnostics.append(Diagnostic(code, severity, *item["span"]))
        for item in self._extensions:
            if item["id"] not in declared_exact:
                self.diagnostics.append(Diagnostic("MS3103", "warning", *item["span"]))


def project_bytes(data: bytes, available_extensions: Iterable[str] = ()) -> dict[str, Any]:
    """Return the stable structural projection for one source byte sequence."""

    return MarksheetProjection(data, available_extensions).parse()


def dump_projection(projection: dict[str, Any]) -> str:
    """Serialize one projection deterministically with a final newline."""

    return json.dumps(projection, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="Marksheet source file")
    parser.add_argument("--extension", action="append", default=[], help="available exact extension ID")
    parser.add_argument("--output", type=Path, help="write projection instead of stdout")
    args = parser.parse_args(argv)
    result = dump_projection(project_bytes(args.source.read_bytes(), args.extension))
    if args.output:
        args.output.write_text(result, encoding="utf-8", newline="\n")
    else:
        sys.stdout.write(result)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
