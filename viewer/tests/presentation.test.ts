import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import {
  applyResolvedStyle,
  columnTrackCss,
  emptyStyleProperties,
  formatPresentedCell,
  rowHeightCss,
} from "../src/presentation";
import type { PresentedCell, ScalarValue, StyleProperties } from "../src/protocol";

interface PresentationFixtureCell {
  calculated: ScalarValue;
  style: Record<string, unknown>;
  geometry: { column_width: number; row_height: number };
}

interface PresentationFixture {
  operations: Array<{ expect_cells: Record<string, PresentationFixtureCell> }>;
}

const budgetFixture = JSON.parse(readFileSync(
  resolve(process.cwd(), "../tests/view/budget_open.json"),
  "utf8",
)) as PresentationFixture;
const layersFixture = JSON.parse(readFileSync(
  resolve(process.cwd(), "../tests/view/layers_geometry.json"),
  "utf8",
)) as PresentationFixture;

function projectedCell(value: ScalarValue | null, overrides: Partial<StyleProperties>): PresentedCell {
  return {
    coordinate: { column: 2, row: 2 },
    source: { Authored: { value: { kind: "formula", value: "=A1" }, source_span: null } },
    calculated: value,
    style: { properties: { ...emptyStyleProperties(), ...overrides }, layers: [] },
    column: { size: null, source_span: null },
    row: { size: null, source_span: null },
  };
}

describe("deterministic core presentation", () => {
  it("formats the Budget currency and percent projections", () => {
    const expectedCells = budgetFixture.operations[1]!.expect_cells;
    for (const [coordinate, formatted] of [
      ["B2", "$2,060.00"],
      ["B3", "$686.67"],
      ["B4", "$1,648.00"],
    ] as const) {
      const expected = expectedCells[coordinate]!;
      const currency: Partial<StyleProperties> = {
        number: "Currency",
        currency: String(expected.style.currency),
        decimals: Number(expected.style.decimals),
        align: "Right",
      };
      expect(formatPresentedCell(projectedCell(expected.calculated, currency), "en-US")).toBe(formatted);
    }
    expect(formatPresentedCell(
      projectedCell({ kind: "number", value: 0.2 }, { number: "Percent", decimals: 0 }),
      "en-US",
    )).toBe("20%");
  });

  it("uses character units for columns and points for rows", () => {
    const expectedCells = layersFixture.operations[0]!.expect_cells;
    expect(columnTrackCss(expectedCells.A2!.geometry.column_width)).toBe("max(56px, 10ch)");
    expect(columnTrackCss(expectedCells.B2!.geometry.column_width)).toBe("max(56px, 24ch)");
    expect(rowHeightCss(expectedCells.C2!.geometry.row_height)).toBe("max(22px, 30pt)");
  });

  it("applies vertical and general alignment plus point-sized fonts", () => {
    const number = document.createElement("button");
    applyResolvedStyle(number, {
      ...emptyStyleProperties(), valign: "Bottom", align: "General", font_size: 12,
    }, "number");
    expect(number.style.alignItems).toBe("flex-end");
    expect(number.style.justifyContent).toBe("flex-end");
    expect(number.style.fontSize).toBe("12pt");

    const text = document.createElement("button");
    applyResolvedStyle(text, { ...emptyStyleProperties(), valign: "Top", align: "General" }, "text");
    expect(text.style.alignItems).toBe("flex-start");
    expect(text.style.justifyContent).toBe("flex-start");
  });

  it("visibly distinguishes authored empty text from Blank", () => {
    const blank = projectedCell(null, {});
    blank.source = { Authored: { value: { kind: "blank" }, source_span: null } };
    const emptyText = projectedCell(null, {});
    emptyText.source = { Authored: { value: { kind: "text", value: "" }, source_span: null } };
    expect(formatPresentedCell(blank, "en-US")).toBe("");
    expect(formatPresentedCell(emptyText, "en-US")).toBe('""');
  });
});
