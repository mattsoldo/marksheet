import { describe, expect, it } from "vitest";
import {
  applyStyleTransaction,
  defineStyleTransaction,
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
