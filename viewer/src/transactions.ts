import type { A1Range, AuthoredValue, Coordinate, EditTransaction, StyleProperties } from "./protocol";

export function parseCellValue(source: string): AuthoredValue {
  if (source === "") return { kind: "blank" };
  if (source.startsWith("=")) return { kind: "formula", value: source };
  if (source === "true" || source === "false") return { kind: "boolean", value: source === "true" };
  if (/^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?$/.test(source)) {
    const value = Number(source);
    if (Number.isFinite(value)) return { kind: "number", value };
  }
  return { kind: "text", value: source.startsWith("'") ? source.slice(1) : source };
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
