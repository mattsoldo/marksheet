import type { A1Range, Coordinate } from "./protocol";

export const DEFAULT_VIEWPORT = Object.freeze({ rows: 30, columns: 12, overscan: 3 });

export interface ViewportSpec {
  anchor: Coordinate;
  visibleRows: number;
  visibleColumns: number;
  overscan: number;
}

/**
 * Returns a finite request around the active cell. It is intentionally based
 * on viewport dimensions, never on a workbook's furthest authored coordinate.
 */
export function computeViewport(spec: ViewportSpec): A1Range {
  const visibleRows = positiveInteger(spec.visibleRows, "visibleRows");
  const visibleColumns = positiveInteger(spec.visibleColumns, "visibleColumns");
  const overscan = nonNegativeInteger(spec.overscan, "overscan");
  const startRow = Math.max(1, spec.anchor.row - overscan);
  const startColumn = Math.max(1, spec.anchor.column - overscan);
  return {
    start: { column: startColumn, row: startRow },
    end: {
      column: checkedAdd(startColumn, visibleColumns + overscan * 2 - 1),
      row: checkedAdd(startRow, visibleRows + overscan * 2 - 1),
    },
  };
}

export function viewportCellCount(range: A1Range): number {
  return (range.end.column - range.start.column + 1) * (range.end.row - range.start.row + 1);
}

function positiveInteger(value: number, name: string): number {
  if (!Number.isSafeInteger(value) || value < 1) throw new RangeError(`${name} must be positive`);
  return value;
}

function nonNegativeInteger(value: number, name: string): number {
  if (!Number.isSafeInteger(value) || value < 0) throw new RangeError(`${name} must not be negative`);
  return value;
}

function checkedAdd(left: number, right: number): number {
  const result = left + right;
  if (!Number.isSafeInteger(result)) throw new RangeError("viewport coordinate overflow");
  return result;
}
