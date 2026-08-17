import { describe, expect, it } from "vitest";
import type { AuthoredValue } from "../src/protocol";
import {
  applyStyleTransaction,
  authoredCellText,
  defineStyleTransaction,
  escapeAuthoredText,
  parseCellValue,
  setCellTransaction,
  setColumnWidthTransaction,
  setNameTargetTransaction,
  setRowHeightTransaction,
} from "../src/transactions";

describe("semantic transaction construction", () => {
  it("keeps a cell edit focused on one authored coordinate", () => {
    expect(setCellTransaction("inputs", { column: 7, row: 2 }, "0.25")).toEqual({
      operations: [{
        kind: "set_cell",
        sheet: "inputs",
        coordinate: { column: 7, row: 2 },
        value: { kind: "number", value: 0.25 },
      }],
    });
  });

  it("uses source-aware operations for names, style, and geometry", () => {
    const range = { start: { column: 2, row: 3 }, end: { column: 4, row: 5 } };
    expect(setNameTargetTransaction("focus", "main", range).operations[0]).toMatchObject({
      kind: "set_name_target",
      target: { Range: { sheet: "main", range } },
    });
    expect(applyStyleTransaction("main", range, "money").operations[0]).toMatchObject({
      kind: "apply_style",
      target: { Range: range },
      style: "money",
    });
    expect(defineStyleTransaction("money", {
      bold: true,
      fill: "#123456",
      number: "Currency",
      currency: "USD",
    })).toMatchObject({
      operations: [{
        kind: "define_style",
        style: "money",
        properties: { bold: true, fill: "#123456", number: "Currency", currency: "USD" },
      }],
    });
    expect(setColumnWidthTransaction("main", 2, 18).operations[0]).toMatchObject({
      kind: "set_column_width",
      columns: { start: 2, end: 2 },
    });
    expect(setRowHeightTransaction("main", 3, 24).operations[0]).toMatchObject({
      kind: "set_row_height",
      rows: { start: 3, end: 3 },
    });
  });
});

/**
 * Fields the authoritative Rust parser (`Value::from_csv_field`) reads as
 * something other than plain text. The viewer's mirror must agree, or a
 * formula-bar commit retypes the cell.
 */
describe("parseCellValue mirrors the source-format scalar precedence", () => {
  it.each<[string, AuthoredValue]>([
    ["", { kind: "blank" }],
    ["=SUM(A1)", { kind: "formula", value: "=SUM(A1)" }],
    ["true", { kind: "boolean", value: true }],
    ["false", { kind: "boolean", value: false }],
    ["42", { kind: "number", value: 42 }],
    ["-3.5", { kind: "number", value: -3.5 }],
    ["1e10", { kind: "number", value: 1e10 }],
    ["-12.5e+2", { kind: "number", value: -1250 }],
    ["001", { kind: "text", value: "001" }],
    ["1.", { kind: "text", value: "1." }],
    ["2024-01-01", { kind: "date", value: "2024-01-01" }],
    ["2024-02-29", { kind: "date", value: "2024-02-29" }],
    ["0000-01-01", { kind: "date", value: "0000-01-01" }],
    ["9999-12-31", { kind: "date", value: "9999-12-31" }],
    ["2024-01-01T12:30:00Z", { kind: "date_time", value: "2024-01-01T12:30:00Z" }],
    ["2024-01-01T12:30:00-05:00", { kind: "date_time", value: "2024-01-01T12:30:00-05:00" }],
    ["2024-01-01T12:30:00.5Z", { kind: "date_time", value: "2024-01-01T12:30:00.5Z" }],
    ["#REF!", { kind: "error", value: "#REF!" }],
    ["#DIV/0!", { kind: "error", value: "#DIV/0!" }],
    ["#N/A", { kind: "error", value: "#N/A" }],
    ["#NAME?", { kind: "error", value: "#NAME?" }],
    ["#NUM!", { kind: "error", value: "#NUM!" }],
    ["#VALUE!", { kind: "error", value: "#VALUE!" }],
    ["#CIRC!", { kind: "error", value: "#CIRC!" }],
    ["hello", { kind: "text", value: "hello" }],
    ["'42", { kind: "text", value: "42" }],
    ["'=SUM(A1)", { kind: "text", value: "=SUM(A1)" }],
    ["'2024-01-01", { kind: "text", value: "2024-01-01" }],
    ["'#REF!", { kind: "text", value: "#REF!" }],
    // ISO-shaped but not real instants: the Rust parser falls back to text.
    ["2024-02-30", { kind: "text", value: "2024-02-30" }],
    ["2024-00-01", { kind: "text", value: "2024-00-01" }],
    ["2024-13-01", { kind: "text", value: "2024-13-01" }],
    ["2023-02-29", { kind: "text", value: "2023-02-29" }],
    ["2100-02-29", { kind: "text", value: "2100-02-29" }],
    ["2024-01-01T12:30:00", { kind: "text", value: "2024-01-01T12:30:00" }],
    ["2024-01-01T25:00:00Z", { kind: "text", value: "2024-01-01T25:00:00Z" }],
    ["2024-01-01T12:60:00Z", { kind: "text", value: "2024-01-01T12:60:00Z" }],
    ["2024-01-01T12:30:60Z", { kind: "text", value: "2024-01-01T12:30:60Z" }],
    ["2024-01-01T12:30:00+25:00", { kind: "text", value: "2024-01-01T12:30:00+25:00" }],
    ["#OOPS!", { kind: "text", value: "#OOPS!" }],
  ])("parses %j as %j", (source, expected) => {
    expect(parseCellValue(source)).toEqual(expected);
  });
});

const roundTripValues: AuthoredValue[] = [
  { kind: "blank" },
  { kind: "boolean", value: true },
  { kind: "boolean", value: false },
  { kind: "number", value: 0 },
  { kind: "number", value: 42 },
  { kind: "number", value: -3.5 },
  { kind: "number", value: 1e21 },
  { kind: "number", value: 1e-7 },
  { kind: "number", value: -0 },
  { kind: "date", value: "2024-01-01" },
  { kind: "date", value: "0000-01-01" },
  { kind: "date", value: "9999-12-31" },
  { kind: "date_time", value: "2024-01-01T12:30:00Z" },
  { kind: "date_time", value: "2024-01-01T12:30:00-05:00" },
  { kind: "date_time", value: "2024-01-01T12:30:00.123456789Z" },
  { kind: "formula", value: "=SUM(A1)" },
  { kind: "formula", value: "=1+1" },
  { kind: "error", value: "#DIV/0!" },
  { kind: "error", value: "#N/A" },
  { kind: "error", value: "#NAME?" },
  { kind: "error", value: "#NUM!" },
  { kind: "error", value: "#REF!" },
  { kind: "error", value: "#VALUE!" },
  { kind: "error", value: "#CIRC!" },
  ...[
    "",
    "hello",
    "  padded  ",
    "0",
    "-0",
    "42",
    "-3.5",
    "1e10",
    "Infinity",
    "NaN",
    "true",
    "false",
    "=SUM(A1)",
    "'",
    "''",
    "'quoted",
    "2024-01-01",
    "2024-02-30",
    "2024-01-01T12:30:00Z",
    "2024-01-01T12:30:00",
    "#REF!",
    "#DIV/0!",
    "multi\nline",
  ].map((value): AuthoredValue => ({ kind: "text", value })),
];

describe("authoredCellText round-trips every authored value kind", () => {
  it("covers all eight authored kinds", () => {
    expect(new Set(roundTripValues.map((value) => value.kind))).toEqual(
      new Set(["blank", "text", "number", "boolean", "date", "date_time", "formula", "error"]),
    );
  });

  it.each(roundTripValues.map((value): [string, AuthoredValue] => [
    `${value.kind} ${"value" in value ? JSON.stringify(value.value) : ""}`,
    value,
  ]))("reproduces %s", (_label, value) => {
    const reparsed = parseCellValue(authoredCellText(value));
    expect(reparsed).toEqual(value);
    if (value.kind === "number") {
      // toEqual treats -0 and 0 as equal; the sign must survive too.
      expect(Object.is((reparsed as { value: number }).value, value.value)).toBe(true);
    }
  });
});

describe("escapeAuthoredText", () => {
  it.each([
    ["", "'"],
    ["hello", "hello"],
    ["  padded  ", "  padded  "],
    ["42", "'42"],
    ["-3.5", "'-3.5"],
    ["true", "'true"],
    ["false", "'false"],
    ["=SUM(A1)", "'=SUM(A1)"],
    ["'", "''"],
    ["'quoted", "''quoted"],
    ["2024-01-01", "'2024-01-01"],
    ["2024-01-01T12:30:00Z", "'2024-01-01T12:30:00Z"],
    ["#REF!", "'#REF!"],
    // ISO-shaped text is escaped even when it is not a real instant, matching
    // the canonical serializer, which escapes on `Value::parse_strict`.
    ["2024-02-30", "'2024-02-30"],
    ["2024-01-01T12:30:00", "'2024-01-01T12:30:00"],
  ])("escapes %j as %j", (text, expected) => {
    expect(escapeAuthoredText(text)).toBe(expected);
  });
});
