import type { A1Range, Coordinate } from "./protocol";

export function columnLabel(column: number): string {
  if (!Number.isSafeInteger(column) || column < 1) throw new RangeError("column must be positive");
  let value = column;
  let result = "";
  while (value > 0) {
    value -= 1;
    result = String.fromCharCode(65 + (value % 26)) + result;
    value = Math.floor(value / 26);
  }
  return result;
}

export function formatCoordinate(coordinate: Coordinate): string {
  return `${columnLabel(coordinate.column)}${coordinate.row}`;
}

export function parseCoordinate(input: string): Coordinate | undefined {
  const match = /^([A-Za-z]+)([1-9][0-9]*)$/.exec(input.trim());
  if (!match) return undefined;
  const letters = match[1];
  const rowText = match[2];
  if (!letters || !rowText) return undefined;
  let column = 0;
  for (const character of letters.toUpperCase()) {
    column = column * 26 + character.charCodeAt(0) - 64;
    if (!Number.isSafeInteger(column)) return undefined;
  }
  const row = Number(rowText);
  return Number.isSafeInteger(row) ? { column, row } : undefined;
}

export function parseRange(input: string): A1Range | undefined {
  const parts = input.trim().split(":");
  if (parts.length > 2) return undefined;
  const first = parseCoordinate(parts[0] ?? "");
  const second = parseCoordinate(parts[1] ?? parts[0] ?? "");
  if (!first || !second) return undefined;
  return {
    start: { column: Math.min(first.column, second.column), row: Math.min(first.row, second.row) },
    end: { column: Math.max(first.column, second.column), row: Math.max(first.row, second.row) },
  };
}
