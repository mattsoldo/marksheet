import type {
  A1Range,
  AuthoredValue,
  CellError,
  Coordinate,
  EditTransaction,
  StyleProperties,
} from "./protocol";

/**
 * The scalar tokens the source format reserves for cell errors, in the same
 * precedence position `Value::from_csv_field` gives them.
 */
const CELL_ERROR_TOKENS: readonly CellError[] = [
  "#DIV/0!",
  "#N/A",
  "#NAME?",
  "#NUM!",
  "#REF!",
  "#VALUE!",
  "#CIRC!",
];

const NUMBER_PATTERN = /^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?$/;
/** Shape-only ISO date check, mirroring `looks_like_date`. */
const DATE_PATTERN = /^(\d{4})-(\d{2})-(\d{2})$/;
/** Shape-only RFC 3339 check, mirroring `looks_like_datetime` (offset optional). */
const DATE_TIME_PATTERN =
  /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.\d+)?(Z|[+-]\d{2}:\d{2})?$/;

/**
 * Reconstructs the authored value a source field denotes.
 *
 * This is the browser-side mirror of `Value::from_csv_field` in
 * `crates/marksheet-model/src/lib.rs`, which is the authoritative parser used
 * when a `.ms` workbook is opened. The precedence below (escape marker,
 * formula, error token, boolean, number, date, datetime, text) must stay in
 * step with that function: any field the Rust parser reads as a date, a
 * datetime, or an error must not be downgraded to text here, or committing an
 * unedited formula bar would silently retype the cell.
 */
export function parseCellValue(source: string): AuthoredValue {
  if (source === "") return { kind: "blank" };
  if (source.startsWith("'")) return { kind: "text", value: source.slice(1) };
  if (source.startsWith("=")) return { kind: "formula", value: source };
  const error = CELL_ERROR_TOKENS.find((token) => token === source);
  if (error) return { kind: "error", value: error };
  if (source === "true" || source === "false") return { kind: "boolean", value: source === "true" };
  if (NUMBER_PATTERN.test(source)) {
    const value = Number(source);
    if (Number.isFinite(value)) return { kind: "number", value };
  }
  if (isIsoDate(source)) return { kind: "date", value: source };
  if (isRfc3339DateTime(source)) return { kind: "date_time", value: source };
  return { kind: "text", value: source };
}

/**
 * Renders an authored value as the source field that denotes it, so that
 * `parseCellValue(authoredCellText(value))` reproduces `value` for every
 * authored kind. The formula bar shows this text, which is what makes
 * selecting a cell and committing it unchanged an identity edit rather than a
 * silent retype.
 */
export function authoredCellText(value: AuthoredValue): string {
  switch (value.kind) {
    case "blank":
      return "";
    case "text":
      return escapeAuthoredText(value.value);
    case "boolean":
      return value.value ? "true" : "false";
    case "number":
      return authoredNumberText(value.value);
    case "date":
    case "date_time":
    case "error":
    case "formula":
      return value.value;
  }
}

/**
 * Inverse of {@link parseCellValue} for authored text: escapes `text` with a
 * leading apostrophe whenever it would otherwise be reparsed as something
 * other than that same literal text (a number, a boolean, a formula, an error
 * token, a date, a datetime, or text that already starts with the escape
 * marker itself). ISO-shaped text that is not a real date — `2024-02-30` —
 * is escaped too, matching the canonical serializer's use of `parse_strict`,
 * so the formula bar shows the same field the saved source holds.
 */
export function escapeAuthoredText(text: string): string {
  const reparsed = parseCellValue(text);
  const literal = reparsed.kind === "text"
    && reparsed.value === text
    && !DATE_PATTERN.test(text)
    && !DATE_TIME_PATTERN.test(text);
  return literal ? text : `'${text}`;
}

/** `String(-0)` is `"0"`, which would reparse as positive zero. */
function authoredNumberText(value: number): string {
  return Object.is(value, -0) ? "-0" : String(value);
}

function isIsoDate(source: string): boolean {
  const match = DATE_PATTERN.exec(source);
  if (!match) return false;
  const [, year, month, day] = match;
  return isCalendarDate(Number(year), Number(month), Number(day));
}

function isRfc3339DateTime(source: string): boolean {
  const match = DATE_TIME_PATTERN.exec(source);
  if (!match) return false;
  const [, year, month, day, hour, minute, second, offset] = match;
  // RFC 3339 requires an explicit offset; a bare local datetime stays text.
  if (offset === undefined) return false;
  if (!isCalendarDate(Number(year), Number(month), Number(day))) return false;
  // Leap seconds are rejected by the Rust parser, so `60` stays text here too.
  if (Number(hour) > 23 || Number(minute) > 59 || Number(second) > 59) return false;
  if (offset === "Z") return true;
  return Number(offset.slice(1, 3)) <= 23 && Number(offset.slice(4, 6)) <= 59;
}

function isCalendarDate(year: number, month: number, day: number): boolean {
  if (month < 1 || month > 12 || day < 1) return false;
  return day <= daysInMonth(year, month);
}

function daysInMonth(year: number, month: number): number {
  if (month === 2) return isLeapYear(year) ? 29 : 28;
  return month === 4 || month === 6 || month === 9 || month === 11 ? 30 : 31;
}

function isLeapYear(year: number): boolean {
  return year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
}

export function setCellTransaction(
  sheet: string,
  coordinate: Coordinate,
  source: string,
): EditTransaction {
  return {
    operations: [{ kind: "set_cell", sheet, coordinate, value: parseCellValue(source) }],
  };
}

export function setNameTargetTransaction(
  name: string,
  sheet: string,
  target: A1Range,
): EditTransaction {
  const single = target.start.column === target.end.column && target.start.row === target.end.row;
  return {
    operations: [{
      kind: "set_name_target",
      name,
      target: single
        ? { Cell: { sheet, coordinate: target.start } }
        : { Range: { sheet, range: target } },
    }],
  };
}

export function applyStyleTransaction(
  sheet: string,
  target: A1Range,
  style: string,
): EditTransaction {
  return {
    operations: [{ kind: "apply_style", sheet, target: { Range: target }, style }],
  };
}

export interface BasicStyleInput {
  bold?: boolean;
  fill?: string;
  number?: "General" | "Integer" | "Decimal" | "Percent" | "Currency" | "Date" | "DateTime";
  currency?: string;
}

export function defineStyleTransaction(style: string, input: BasicStyleInput): EditTransaction {
  const properties: StyleProperties = {
    bold: null,
    italic: null,
    wrap: null,
    text_color: null,
    fill: null,
    font_size: null,
    align: null,
    valign: null,
    number: null,
    decimals: null,
    currency: null,
  };
  if (input.bold !== undefined) properties.bold = input.bold;
  if (input.fill) properties.fill = input.fill;
  if (input.number) properties.number = input.number;
  if (input.currency) properties.currency = input.currency;
  return { operations: [{ kind: "define_style", style, properties }] };
}

export function setColumnWidthTransaction(
  sheet: string,
  column: number,
  width: number,
): EditTransaction {
  return {
    operations: [{ kind: "set_column_width", sheet, columns: { start: column, end: column }, width }],
  };
}

export function setRowHeightTransaction(
  sheet: string,
  row: number,
  height: number,
): EditTransaction {
  return {
    operations: [{ kind: "set_row_height", sheet, rows: { start: row, end: row }, height }],
  };
}
