import { describe, expect, it } from "vitest";
import { computeViewport, viewportCellCount } from "../src/viewport";

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
