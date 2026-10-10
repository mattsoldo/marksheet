import { describe, expect, it } from "vitest";
import {
  MAX_VISIBLE,
  clipRange,
  computeViewport,
  readingLimit,
  viewportCellCount,
  viewportForContainer,
} from "../src/viewport";

describe("bounded viewport projection", () => {
  it("allocates only visible cells plus finite overscan", () => {
    const range = computeViewport({
      anchor: { column: 1_000_000, row: 1_000_000 },
      visibleRows: 30,
      visibleColumns: 12,
      overscan: 3,
    });

    expect(range).toEqual({
      start: { column: 999_997, row: 999_997 },
      end: { column: 1_000_014, row: 1_000_032 },
    });
    expect(viewportCellCount(range)).toBe(648);
  });

  it("clips overscan at the one-based origin", () => {
    const range = computeViewport({
      anchor: { column: 1, row: 1 },
      visibleRows: 2,
      visibleColumns: 3,
      overscan: 2,
    });

    expect(range.start).toEqual({ column: 1, row: 1 });
    expect(viewportCellCount(range)).toBe(42);
  });
});

describe("container-sized windows and the reading-view fit", () => {
  it("keeps the 30×12 default when the container has no layout", () => {
    expect(viewportForContainer({ column: 1, row: 1 }, { width: 0, height: 0 })).toEqual({
      anchor: { column: 1, row: 1 }, visibleRows: 30, visibleColumns: 12, overscan: 3,
    });
  });

  it("sizes to the screen with a larger row buffer, capped for huge screens", () => {
    const spec = viewportForContainer({ column: 5, row: 100 }, { width: 1120, height: 880 });
    expect(spec).toMatchObject({ visibleRows: 40, visibleColumns: 20, rowOverscan: 20, columnOverscan: 4 });
    expect(computeViewport(spec)).toEqual({ start: { column: 1, row: 80 }, end: { column: 28, row: 159 } });
    const huge = viewportForContainer({ column: 1, row: 1 }, { width: 100_000, height: 100_000 });
    expect(huge).toMatchObject({ visibleRows: MAX_VISIBLE.rows, visibleColumns: MAX_VISIBLE.columns });
  });

  it("fits the reading view to the extent plus a margin, with a small canvas for empty sheets", () => {
    expect(readingLimit(undefined)).toBeUndefined();
    expect(readingLimit(null)).toEqual({ column: 3, row: 5 });
    expect(readingLimit({ start: { column: 1, row: 1 }, end: { column: 7, row: 4 } })).toEqual({ column: 8, row: 6 });
  });

  it("clips a window to a limit and reports a window entirely past it", () => {
    const window = { start: { column: 2, row: 3 }, end: { column: 20, row: 40 } };
    expect(clipRange(window, undefined)).toBe(window);
    expect(clipRange(window, { column: 8, row: 6 })).toEqual({ start: { column: 2, row: 3 }, end: { column: 8, row: 6 } });
    expect(clipRange(window, { column: 1, row: 6 })).toBeUndefined();
  });
});
