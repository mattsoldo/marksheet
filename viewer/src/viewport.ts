import type { A1Range, Coordinate } from "./protocol";

export const DEFAULT_VIEWPORT = Object.freeze({ rows: 30, columns: 12, overscan: 3 });

export interface ViewportSpec {
  anchor: Coordinate;
  visibleRows: number;
  visibleColumns: number;
  overscan: number;
  /** Overrides `overscan` on the row axis, e.g. a larger buffer for smooth vertical scrolling. */
  rowOverscan?: number;
  /** Overrides `overscan` on the column axis. */
  columnOverscan?: number;
}

/** The most rows and columns a window may show, however large the screen. */
export const MAX_VISIBLE = Object.freeze({ rows: 120, columns: 48 });

/**
 * Returns a finite request around the active cell. It is intentionally based
 * on viewport dimensions, never on a workbook's furthest authored coordinate.
 */
export function computeViewport(spec: ViewportSpec): A1Range {
  const visibleRows = positiveInteger(spec.visibleRows, "visibleRows");
  const visibleColumns = positiveInteger(spec.visibleColumns, "visibleColumns");
  const overscan = nonNegativeInteger(spec.overscan, "overscan");
  const rowOverscan = nonNegativeInteger(spec.rowOverscan ?? overscan, "rowOverscan");
  const columnOverscan = nonNegativeInteger(spec.columnOverscan ?? overscan, "columnOverscan");
  const startRow = Math.max(1, spec.anchor.row - rowOverscan);
  const startColumn = Math.max(1, spec.anchor.column - columnOverscan);
  return {
    start: { column: startColumn, row: startRow },
    end: {
      column: checkedAdd(startColumn, visibleColumns + columnOverscan * 2 - 1),
      row: checkedAdd(startRow, visibleRows + rowOverscan * 2 - 1),
    },
  };
}

/**
 * Sizes a window from the scroll container's pixels. Unknown sizes (a hidden
 * container or a DOM without layout) keep the historical 30×12 default.
 */
export function viewportForContainer(
  anchor: Coordinate,
  size: { width: number; height: number },
): ViewportSpec {
  if (!(size.width > 0) || !(size.height > 0)) {
    return {
      anchor,
      visibleRows: DEFAULT_VIEWPORT.rows,
      visibleColumns: DEFAULT_VIEWPORT.columns,
      overscan: DEFAULT_VIEWPORT.overscan,
    };
  }
  // Rows are at least 22px and columns at least 56px, so these never undercount.
  const visibleRows = Math.min(MAX_VISIBLE.rows, Math.max(10, Math.ceil(size.height / 22)));
  const visibleColumns = Math.min(MAX_VISIBLE.columns, Math.max(4, Math.ceil(size.width / 56)));
  return {
    anchor,
    visibleRows,
    visibleColumns,
    overscan: DEFAULT_VIEWPORT.overscan,
    rowOverscan: Math.ceil(visibleRows / 2),
    columnOverscan: 4,
  };
}

/** Clips a window to an inclusive limit; `undefined` when nothing remains. */
export function clipRange(range: A1Range, limit: Coordinate | undefined): A1Range | undefined {
  if (!limit) return range;
  const end = {
    column: Math.min(range.end.column, limit.column),
    row: Math.min(range.end.row, limit.row),
  };
  if (end.column < range.start.column || end.row < range.start.row) return undefined;
  return { start: range.start, end };
}

/**
 * The reading view's limit: the sheet's content extent plus a small margin, or
 * a small blank canvas for an empty sheet. `undefined` extent means unknown.
 */
export function readingLimit(extent: A1Range | null | undefined): Coordinate | undefined {
  if (extent === undefined) return undefined;
  return {
    column: Math.max(3, (extent?.end.column ?? 0) + 1),
    row: Math.max(5, (extent?.end.row ?? 0) + 2),
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
