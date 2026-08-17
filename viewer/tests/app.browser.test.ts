import { describe, expect, it, vi } from "vitest";
import { ViewerApp } from "../src/app";
import { LocalFileSession } from "../src/local-file";
import type {
  A1Range,
  AuthoredValue,
  Diagnostic,
  EditTransaction,
  ExtensionSupportSummary,
  ScalarValue,
  StyledRegion,
  StyleProperties,
  ViewCompleteness,
  VisibleRegion,
  WorkbookSnapshot,
  WorkerResponseEnvelope,
} from "../src/protocol";
import type { WorkbenchAdapter } from "../src/worker-adapter";

const encoder = new TextEncoder();

function styleProperties(overrides: Partial<StyleProperties> = {}): StyleProperties {
  return {
    bold: null, italic: null, wrap: null, text_color: null, fill: null, font_size: null,
    align: null, valign: null, number: null, decimals: null, currency: null,
    ...overrides,
  };
}

function diagnostic(code: string, message: string, start = 0): Diagnostic {
  return {
    code,
    severity: "error",
    message,
    primary: { span: { start, end: start + 1 }, label: null },
    related: [],
    context: null,
    suggestion: null,
  };
}

const emptyStats = {
  dirty_cells: [], evaluated_cells: [], dirty_cell_count: 0, evaluated_cell_count: 0,
  evaluation_steps: 0, range_cells: 0, text_bytes: 0,
};

function response(payload: WorkerResponseEnvelope["response"], revision = 1): WorkerResponseEnvelope {
  return { protocol: "marksheet-worker@1", request_id: crypto.randomUUID(), revision, response: payload };
}

const defaultSheets = [
  { id: "inputs", label: "Inputs", authored_cell_count: 1, table_count: 0 },
  { id: "summary", label: "Summary", authored_cell_count: 1, table_count: 0 },
];

type ExtensionState = Pick<
  WorkbookSnapshot,
  "extension_declarations" | "extension_instances" | "extension_support"
>;

function completeExtensions(
  overrides: Partial<ExtensionSupportSummary> = {},
): ExtensionState {
  return {
    extension_declarations: [],
    extension_instances: [],
    extension_support: {
      supported_capabilities: ["assertions@1"],
      capabilities_complete: true,
      calculation_complete: true,
      rendering_complete: true,
      validation_complete: true,
      valid: true,
      ...overrides,
    },
  };
}

function snapshot(
  revision = 1,
  editable = true,
  diagnostics: Diagnostic[] = [],
  sheets = defaultSheets,
  diagnosticsOmitted = 0,
  extensions = completeExtensions(),
): WorkbookSnapshot {
  return {
    revision,
    diagnostics,
    diagnostics_omitted: diagnosticsOmitted,
    editable,
    locale: "en-US",
    timezone: "UTC",
    formula_profile: "portable-v1",
    sheets,
    names: [{
      id: "first_input",
      target: { Cell: { sheet: "inputs", coordinate: { column: 1, row: 1 } } },
    }],
    style_count: 1,
    name_count: 1,
    ...extensions,
  };
}

function region(
  sheet: string,
  range: A1Range,
  completeness: ViewCompleteness = { calculation_complete: true, rendering_complete: true },
  cellValue: AuthoredValue = { kind: "formula", value: "=1+1" },
  calculated: ScalarValue = { kind: "number", value: 2 },
): VisibleRegion {
  return {
    sheet: {
      id: sheet,
      label: sheet === "inputs" ? "Inputs" : "Summary",
      authored_cell_count: 1,
      virtual_cell_count: 0,
      footprint_count: 1,
      source_span: null,
    },
    range,
    completeness,
    cells: [{
      coordinate: { column: 1, row: 1 },
      source: { Authored: { value: cellValue, source_span: { start: 10, end: 14 } } },
      calculated,
      style: {
        properties: styleProperties({ bold: true }),
        layers: [{ id: "headline", style_source_span: null, application_source_span: null }],
      },
      column: { size: null, source_span: null },
      row: { size: null, source_span: null },
    }],
    style_regions: [],
    columns: [],
    rows: [],
    diagnostics: [],
  };
}

class MockAdapter implements WorkbenchAdapter {
  currentRevision = 0;
  lastAcceptedSource: Uint8Array | undefined;
  source: Uint8Array<ArrayBufferLike> = encoder.encode("@marksheet 0.1\n@sheet inputs \"Inputs\"\n1\n");
  calculationMakesViewOnly = false;
  calculationError: unknown;
  calculated = false;
  diagnostics: Diagnostic[] = [];
  diagnosticsOmitted = 0;
  regionDiagnosticsOmitted = 0;
  calculationDiagnosticsOmitted = 0;
  editable = true;
  extensionState = completeExtensions();
  regionCompleteness: ViewCompleteness = { calculation_complete: true, rendering_complete: true };
  sheets = defaultSheets;
  styleRegions: StyledRegion[] = [];
  cellValue: AuthoredValue = { kind: "formula", value: "=1+1" };
  cellCalculated: ScalarValue = { kind: "number", value: 2 };
  edit = vi.fn(async (_transaction: EditTransaction) => {
    this.currentRevision += 1;
    this.source = encoder.encode("@marksheet 0.1\n@sheet inputs \"Inputs\"\n2\n");
    return response({
      kind: "edited",
      changed: true,
      patches: [{ span: { start: 42, end: 43 }, replacement: [50] }],
      snapshot: snapshot(
        this.currentRevision,
        this.editable,
        this.diagnostics,
        this.sheets,
        this.diagnosticsOmitted,
        this.extensionState,
      ),
    }, this.currentRevision);
  });

  async open(source: Uint8Array) {
    const repeated = this.currentRevision > 0;
    this.currentRevision = repeated ? this.currentRevision + 1 : 1;
    this.source = source.slice();
    this.lastAcceptedSource = source.slice();
    this.calculated = false;
    this.sheets = new TextDecoder().decode(source) === "second"
      ? [{ id: "other", label: "Other", authored_cell_count: 0, table_count: 0 }]
      : defaultSheets;
    const kind: "opened" | "replaced" = repeated ? "replaced" : "opened";
    return response({
      kind,
      snapshot: snapshot(
        this.currentRevision,
        this.editable,
        this.diagnostics,
        this.sheets,
        this.diagnosticsOmitted,
        this.extensionState,
      ),
    }, this.currentRevision);
  }

  async replaceSource(source: Uint8Array) {
    this.source = source;
    this.currentRevision += 1;
    return response({
      kind: "replaced",
      snapshot: snapshot(
        this.currentRevision,
        this.editable,
        this.diagnostics,
        this.sheets,
        this.diagnosticsOmitted,
        this.extensionState,
      ),
    }, this.currentRevision);
  }

  async snapshot() {
    const viewOnly = this.calculationMakesViewOnly && this.calculated;
    return response({
      kind: "snapshot",
      snapshot: snapshot(
        this.currentRevision,
        this.editable && !viewOnly,
        viewOnly ? [diagnostic("MS2303", "formula cycle")] : this.diagnostics,
        this.sheets,
        this.diagnosticsOmitted,
        this.extensionState,
      ),
    }, this.currentRevision);
  }

  async visibleRegion(sheet: string, range: A1Range) {
    const visible = region(sheet, range, this.regionCompleteness, this.cellValue, this.cellCalculated);
    visible.style_regions = this.styleRegions;
    const payload = {
      kind: "visible_region" as const,
      region: visible,
      diagnostics_omitted: this.regionDiagnosticsOmitted,
    };
    return response(payload, this.currentRevision);
  }

  async calculate(_sheet: string, _range: A1Range) {
    if (this.calculationError) throw this.calculationError;
    this.calculated = true;
    return response({
      kind: "calculation",
      calculation: {
        cells: [{
          cell: { sheet: _sheet, coordinate: { column: 1, row: 1 } },
          value: { kind: "number", value: 2 },
        }],
        diagnostics: [],
        revision: this.currentRevision,
        stats: emptyStats,
      },
      diagnostics_omitted: this.calculationDiagnosticsOmitted,
    }, this.currentRevision);
  }

  async sourceBytes() {
    return response({ kind: "source_bytes", source: [...this.source] }, this.currentRevision);
  }

  async cancelAndRestart() {}
  dispose() {}
}

describe("viewer browser shell", () => {
  it("keeps source-order tabs and realizes only the bounded viewport", async () => {
    const root = document.createElement("main");
    document.body.append(root);
    const app = new ViewerApp(root, new MockAdapter());

    await app.openSource(encoder.encode("fixture"), "fixture.ms");

    expect([...root.querySelectorAll(".sheet-tab")].map((tab) => tab.textContent)).toEqual(["Inputs", "Summary"]);
    expect(root.querySelectorAll(".grid-cell")).toHaveLength(648);
    expect(root.querySelector<HTMLElement>("[data-coordinate='1:1']")?.textContent).toBe("2");
    expect((root.querySelector("#formula-input") as HTMLInputElement).value).toBe("=1+1");
    app.dispose();
    root.remove();
  });

  it("supports arrow-key focus movement within the ARIA grid", async () => {
    const root = document.createElement("main");
    document.body.append(root);
    const app = new ViewerApp(root, new MockAdapter());
    await app.openSource(encoder.encode("fixture"), "fixture.ms");
    const a1 = root.querySelector<HTMLElement>("[data-coordinate='1:1']")!;
    a1.focus();
    a1.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    await vi.waitFor(() => expect(root.querySelector(".cell-selected")?.getAttribute("data-coordinate")).toBe("2:1"));
    expect((document.activeElement as HTMLElement).dataset.coordinate).toBe("2:1");
    app.dispose();
    root.remove();
  });

  it("materially renders a style region on an un-authored blank cell", async () => {
    const root = document.createElement("main");
    document.body.append(root);
    const adapter = new MockAdapter();
    adapter.styleRegions = [{
      range: { start: { column: 3, row: 3 }, end: { column: 3, row: 3 } },
      source_order: 4,
      style: {
        properties: styleProperties({ italic: true, fill: "#123456", valign: "Bottom" }),
        layers: [{ id: "blank_note", style_source_span: null, application_source_span: null }],
      },
    }];
    const app = new ViewerApp(root, adapter);
    await app.openSource(encoder.encode("fixture"), "blank-style.ms");
    const c3 = root.querySelector<HTMLElement>("[data-coordinate='3:3']")!;
    expect(c3.classList.contains("cell-styled-blank")).toBe(true);
    expect(c3.style.backgroundColor).toBe("#123456");
    expect(c3.style.fontStyle).toBe("italic");
    expect(c3.style.alignItems).toBe("flex-end");
    app.dispose();
    root.remove();
  });

  it("keeps distant virtual-grid ARIA indices internally bounded", async () => {
    const root = document.createElement("main");
    document.body.append(root);
    const app = new ViewerApp(root, new MockAdapter());
    await app.openSource(encoder.encode("fixture"), "fixture.ms");
    const nameBox = root.querySelector("#name-box") as HTMLInputElement;
    nameBox.value = "BDW1000000";
    nameBox.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await vi.waitFor(() => expect(root.querySelector("#viewport-status")?.textContent).toContain("999997"));
    const grid = root.querySelector("#grid")!;
    expect(grid.getAttribute("aria-rowcount")).toBe("37");
    expect(grid.getAttribute("aria-colcount")).toBe("19");
    expect(grid.querySelectorAll(":scope > [role='row']")).toHaveLength(37);
    for (const cell of grid.querySelectorAll("[role='gridcell']")) {
      expect(cell.parentElement?.getAttribute("role")).toBe("row");
      expect(Number(cell.getAttribute("aria-colindex"))).toBeLessThanOrEqual(19);
    }
    app.dispose();
    root.remove();
  });

  it("deduplicates and caps diagnostic DOM rows", async () => {
    const root = document.createElement("main");
    document.body.append(root);
    const adapter = new MockAdapter();
    adapter.diagnostics = Array.from({ length: 150 }, (_, index) => ({
      ...diagnostic(`W${index}`, `warning ${index}`, index),
      severity: "warning" as const,
    }));
    adapter.diagnostics.push(adapter.diagnostics[0]!);
    const app = new ViewerApp(root, adapter);
    await app.openSource(encoder.encode("fixture"), "fixture.ms");
    expect(root.querySelectorAll(".diagnostic:not(.diagnostic-overflow)")).toHaveLength(100);
    expect(root.querySelector(".diagnostic-overflow")?.textContent).toContain("50 additional");
    expect(root.querySelector("#diagnostic-count")?.textContent).toBe("100 of 150");
    app.dispose();
    root.remove();
  });

  it("reports worker-level diagnostic truncation separately from the DOM cap", async () => {
    const root = document.createElement("main");
    document.body.append(root);
    const adapter = new MockAdapter();
    adapter.diagnostics = [diagnostic("MS1000", "first"), diagnostic("MS1001", "second", 2)];
    adapter.diagnosticsOmitted = 17;
    adapter.regionDiagnosticsOmitted = 19;
    adapter.calculationDiagnosticsOmitted = 23;
    const app = new ViewerApp(root, adapter);

    await app.openSource(encoder.encode("fixture"), "truncated.ms");

    expect(root.querySelector("#diagnostic-count")?.textContent)
      .toBe("2 rendered · document +17 · viewport +19 · calculation +23");
    expect([...root.querySelectorAll(".diagnostic-overflow")].map((row) => row.textContent)).toEqual([
      "17 additional document diagnostics were omitted by the worker resource cap.",
      "19 additional viewport diagnostics were omitted by the worker resource cap.",
      "23 additional calculation diagnostics were omitted by the worker resource cap.",
    ]);
    expect(root.querySelectorAll(".diagnostic:not(.diagnostic-overflow)")).toHaveLength(2);
    app.dispose();
    root.remove();
  });

  it("accepts a repeat-open replacement and keeps UI/source coherent", async () => {
    const root = document.createElement("main");
    document.body.append(root);
    const adapter = new MockAdapter();
    const app = new ViewerApp(root, adapter);
    await app.openSource(encoder.encode("fixture"), "first.ms");
    await app.openSource(encoder.encode("second"), "second.ms");
    expect([...root.querySelectorAll(".sheet-tab")].map((tab) => tab.textContent)).toEqual(["Other"]);
    expect((root.querySelector("#source-view") as HTMLTextAreaElement).value).toBe("second");
    expect(adapter.lastAcceptedSource).toEqual(encoder.encode("second"));
    app.dispose();
    root.remove();
  });

  it("does not send overlapping open requests", async () => {
    const root = document.createElement("main");
    document.body.append(root);
    const adapter = new MockAdapter();
    const originalOpen = adapter.open.bind(adapter);
    let releaseOpen: (() => void) | undefined;
    const gate = new Promise<void>((resolve) => { releaseOpen = resolve; });
    adapter.open = vi.fn(async (source: Uint8Array) => {
      await gate;
      return originalOpen(source);
    });
    const app = new ViewerApp(root, adapter);
    const first = app.openSource(encoder.encode("fixture"), "first.ms");
    await vi.waitFor(() => expect(adapter.open).toHaveBeenCalledTimes(1));
    await expect(app.openSource(encoder.encode("second"), "second.ms"))
      .rejects.toThrow("another open, save, or edit is already in progress");
    expect(adapter.open).toHaveBeenCalledTimes(1);
    releaseOpen?.();
    await first;
    expect((root.querySelector("#source-view") as HTMLTextAreaElement).value).toBe("fixture");
    app.dispose();
    root.remove();
  });

  it("commits one semantic edit and reports its focused source patch", async () => {
    const root = document.createElement("main");
    document.body.append(root);
    const adapter = new MockAdapter();
    const app = new ViewerApp(root, adapter);
    await app.openSource(encoder.encode("fixture"), "fixture.ms");
    const formula = root.querySelector("#formula-input") as HTMLInputElement;
    formula.value = "=2+2";
    formula.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await vi.waitFor(() => expect(adapter.edit).toHaveBeenCalledTimes(1));
    await vi.waitFor(() => expect(root.querySelector("#status")?.textContent).toContain("42..43"));

    expect(adapter.edit).toHaveBeenCalledWith({
      operations: [{
        kind: "set_cell",
        sheet: "inputs",
        coordinate: { column: 1, row: 1 },
        value: { kind: "formula", value: "=2+2" },
      }],
    });
    app.dispose();
    root.remove();
  });

  const roundTripCases: Array<{
    label: string;
    value: AuthoredValue;
    calculated: ScalarValue;
    expectedSource: string;
  }> = [
    { label: "blank", value: { kind: "blank" }, calculated: { kind: "blank" }, expectedSource: "" },
    {
      label: "text that looks like a number",
      value: { kind: "text", value: "42" },
      calculated: { kind: "text", value: "42" },
      expectedSource: "'42",
    },
    {
      label: "text that looks like a boolean",
      value: { kind: "text", value: "true" },
      calculated: { kind: "text", value: "true" },
      expectedSource: "'true",
    },
    {
      label: "text that looks like a formula",
      value: { kind: "text", value: "=SUM(A1)" },
      calculated: { kind: "text", value: "=SUM(A1)" },
      expectedSource: "'=SUM(A1)",
    },
    {
      label: "text that looks like a date",
      value: { kind: "text", value: "2024-01-01" },
      calculated: { kind: "text", value: "2024-01-01" },
      expectedSource: "2024-01-01",
    },
    {
      label: "plain text",
      value: { kind: "text", value: "hello" },
      calculated: { kind: "text", value: "hello" },
      expectedSource: "hello",
    },
    {
      label: "boolean",
      value: { kind: "boolean", value: true },
      calculated: { kind: "boolean", value: true },
      expectedSource: "true",
    },
    {
      label: "number",
      value: { kind: "number", value: 42 },
      calculated: { kind: "number", value: 42 },
      expectedSource: "42",
    },
    {
      label: "formula",
      value: { kind: "formula", value: "=SUM(A1)" },
      calculated: { kind: "number", value: 4 },
      expectedSource: "=SUM(A1)",
    },
  ];

  it.each(roundTripCases)(
    "round-trips an authored $label value through the formula bar unchanged",
    async ({ value, calculated, expectedSource }) => {
      const root = document.createElement("main");
      document.body.append(root);
      const adapter = new MockAdapter();
      adapter.cellValue = value;
      adapter.cellCalculated = calculated;
      const app = new ViewerApp(root, adapter);
      await app.openSource(encoder.encode("fixture"), "fixture.ms");

      const formula = root.querySelector("#formula-input") as HTMLInputElement;
      expect(formula.value).toBe(expectedSource);

      // Recommit the formula bar's own displayed text, unmodified by the user.
      formula.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
      await vi.waitFor(() => expect(adapter.edit).toHaveBeenCalledTimes(1));

      expect(adapter.edit).toHaveBeenCalledWith({
        operations: [{
          kind: "set_cell",
          sheet: "inputs",
          coordinate: { column: 1, row: 1 },
          value,
        }],
      });
      app.dispose();
      root.remove();
    },
  );

  it("keeps a cell authored as text \"42\" as text when recommitted unchanged", async () => {
    const root = document.createElement("main");
    document.body.append(root);
    const adapter = new MockAdapter();
    adapter.cellValue = { kind: "text", value: "42" };
    adapter.cellCalculated = { kind: "text", value: "42" };
    const app = new ViewerApp(root, adapter);
    await app.openSource(encoder.encode("fixture"), "fixture.ms");

    const formula = root.querySelector("#formula-input") as HTMLInputElement;
    // The formula bar must show the escaped form, not the bare "42" that
    // would reparse as Number 42 on the next commit.
    expect(formula.value).toBe("'42");

    formula.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await vi.waitFor(() => expect(adapter.edit).toHaveBeenCalledTimes(1));

    expect(adapter.edit).toHaveBeenCalledWith({
      operations: [{
        kind: "set_cell",
        sheet: "inputs",
        coordinate: { column: 1, row: 1 },
        value: { kind: "text", value: "42" },
      }],
    });
    app.dispose();
    root.remove();
  });

  it("does not overwrite an incomplete post-edit refresh with a success status", async () => {
    const root = document.createElement("main");
    document.body.append(root);
    const adapter = new MockAdapter();
    const edit = adapter.edit;
    adapter.edit = vi.fn(async (transaction: EditTransaction) => {
      const result = await edit(transaction);
      adapter.regionCompleteness = { calculation_complete: false, rendering_complete: false };
      adapter.calculationError = Object.assign(new Error("workbook capabilities are incomplete"), {
        diagnostics: [diagnostic("MS3101", "required extension unavailable")],
        diagnostics_omitted: 0,
      });
      return result;
    });
    const app = new ViewerApp(root, adapter);
    await app.openSource(encoder.encode("fixture"), "fixture.ms");

    const formula = root.querySelector("#formula-input") as HTMLInputElement;
    formula.value = "=2+2";
    formula.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));

    await vi.waitFor(() => expect(root.querySelector("#status")?.textContent).toContain("Incomplete workbook view"));
    expect(root.querySelector("#status")?.className).toBe("status-error");
    expect(root.querySelector("#status")?.textContent).not.toContain("Committed");
    app.dispose();
    root.remove();
  });

  it("switches to view-only controls after calculation discovers formula errors", async () => {
    const root = document.createElement("main");
    document.body.append(root);
    const adapter = new MockAdapter();
    adapter.calculationMakesViewOnly = true;
    const app = new ViewerApp(root, adapter);

    await app.openSource(encoder.encode("fixture"), "cycles.ms");

    expect((root.querySelector("#formula-input") as HTMLInputElement).disabled).toBe(true);
    expect((root.querySelector("#apply-style") as HTMLButtonElement).disabled).toBe(true);
    expect((root.querySelector("#column-width") as HTMLInputElement).disabled).toBe(true);
    expect(root.querySelector("#diagnostic-list")?.textContent).toContain("formula cycle");
    app.dispose();
    root.remove();
  });

  it("keeps the viewport usable while surfacing a truncated calculation failure", async () => {
    const root = document.createElement("main");
    document.body.append(root);
    const adapter = new MockAdapter();
    adapter.calculationError = Object.assign(new Error("unsupported formula function"), {
      diagnostics: [diagnostic("MS2401", "unsupported formula")],
      diagnostics_omitted: 7,
    });
    const app = new ViewerApp(root, adapter);

    await app.openSource(encoder.encode("fixture"), "unsupported.ms");

    expect(root.querySelector("[data-coordinate='1:1']")?.textContent).toBe("2");
    expect(root.querySelector("#status")?.textContent)
      .toBe("Calculation failed: unsupported formula function");
    expect(root.querySelector("#status")?.className).toBe("status-error");
    expect(root.querySelector("#diagnostic-count")?.textContent).toBe("1 rendered · calculation +7");
    expect(root.querySelector("#diagnostic-list")?.textContent).toContain("unsupported formula");
    expect(root.querySelector("#diagnostic-list")?.textContent)
      .toContain("7 additional calculation diagnostics were omitted by the worker resource cap.");
    app.dispose();
    root.remove();
  });

  it("marks a required-extension workbook as incomplete without presenting calculated rendering as complete", async () => {
    const root = document.createElement("main");
    document.body.append(root);
    const adapter = new MockAdapter();
    adapter.editable = false;
    adapter.diagnostics = [diagnostic("MS3101", "required extension assertions@2 is unavailable")];
    adapter.extensionState = completeExtensions({
      capabilities_complete: false,
      calculation_complete: false,
      rendering_complete: false,
      valid: false,
    });
    adapter.extensionState.extension_declarations = [{
      capability: "assertions@2",
      required: true,
      availability: "unavailable_required",
    }];
    adapter.extensionState.extension_instances = [{
      capability: "assertions@2",
      declared: true,
      name: "checks",
      outcome: "skipped_unavailable",
      scope: { kind: "workbook" },
      supported: false,
    }];
    adapter.regionCompleteness = { calculation_complete: false, rendering_complete: false };
    adapter.calculationError = Object.assign(new Error("workbook capabilities are incomplete"), {
      diagnostics: adapter.diagnostics,
      diagnostics_omitted: 0,
    });
    const app = new ViewerApp(root, adapter);

    await app.openSource(encoder.encode("fixture"), "required.ms");

    expect(root.querySelector("#grid")?.hasAttribute("hidden")).toBe(false);
    expect(root.querySelector("#status")?.textContent).toBe(
      "Incomplete workbook view: required extension support is unavailable (assertions@2); "
      + "the viewer cannot provide calculated values or complete rendering.",
    );
    expect(root.querySelector("#status")?.className).toBe("status-error");
    expect(root.querySelector("#diagnostic-list")?.textContent).toContain("required extension assertions@2");
    expect((root.querySelector("#formula-input") as HTMLInputElement).disabled).toBe(true);
    expect((root.querySelector("#apply-style") as HTMLButtonElement).disabled).toBe(true);
    app.dispose();
    root.remove();
  });

  it("keeps optional and undeclared extension warnings usable", async () => {
    const root = document.createElement("main");
    document.body.append(root);
    const adapter = new MockAdapter();
    adapter.diagnostics = [
      { ...diagnostic("MS3102", "optional assertions@2 unavailable"), severity: "warning" },
      { ...diagnostic("MS3103", "undeclared vendor instance skipped", 2), severity: "warning" },
    ];
    adapter.extensionState.extension_declarations = [{
      capability: "assertions@2",
      required: false,
      availability: "unavailable_optional",
    }];
    adapter.extensionState.extension_instances = [{
      capability: "vendor_data@1",
      declared: false,
      name: "opaque-secret",
      outcome: "skipped_undeclared",
      scope: { kind: "workbook" },
      supported: false,
    }];
    const app = new ViewerApp(root, adapter);

    await app.openSource(encoder.encode("fixture"), "warnings.ms");

    expect(root.querySelector("#status")?.textContent).toBe(
      "Opened warnings.ms with extension warnings (optional capability unavailable; undeclared instance skipped); "
      + "calculation and rendering remain complete",
    );
    expect(root.querySelector("#status")?.textContent).not.toContain("opaque-secret");
    expect(root.querySelector("#status")?.className).toBe("status-warning");
    expect(root.querySelector("#diagnostic-list")?.textContent).toContain("optional assertions@2 unavailable");
    expect(root.querySelector("#diagnostic-list")?.textContent).toContain("undeclared vendor instance skipped");
    expect((root.querySelector("#formula-input") as HTMLInputElement).disabled).toBe(false);
    expect((root.querySelector("#apply-style") as HTMLButtonElement).disabled).toBe(false);
    app.dispose();
    root.remove();
  });

  it("keeps failed extension assertions editable for repair", async () => {
    const root = document.createElement("main");
    document.body.append(root);
    const adapter = new MockAdapter();
    adapter.diagnostics = [diagnostic("MS3201", "assertion failed")];
    adapter.extensionState = completeExtensions({ valid: false });
    adapter.extensionState.extension_declarations = [{
      capability: "assertions@1",
      required: true,
      availability: "available",
    }];
    adapter.extensionState.extension_instances = [{
      capability: "assertions@1",
      declared: true,
      name: "checks",
      outcome: "processed",
      scope: { kind: "workbook" },
      supported: true,
    }];
    const app = new ViewerApp(root, adapter);

    await app.openSource(encoder.encode("fixture"), "assertions.ms");

    expect(root.querySelector("#status")?.textContent).toBe(
      "Opened assertions.ms with extension validation failures; the workbook remains editable for repair",
    );
    expect(root.querySelector("#diagnostic-list")?.textContent).toContain("assertion failed");
    const formula = root.querySelector("#formula-input") as HTMLInputElement;
    expect(formula.disabled).toBe(false);
    expect((root.querySelector("#apply-style") as HTMLButtonElement).disabled).toBe(false);
    formula.value = "=2+2";
    formula.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await vi.waitFor(() => expect(adapter.edit).toHaveBeenCalledTimes(1));
    app.dispose();
    root.remove();
  });

  it("serializes a deferred local save against semantic edits", async () => {
    const root = document.createElement("main");
    document.body.append(root);
    const adapter = new MockAdapter();
    const base = encoder.encode("fixture");
    let releaseWritable: (() => void) | undefined;
    const write = vi.fn(async (_source: Uint8Array) => undefined);
    const writable = { write, close: vi.fn(async () => undefined), abort: vi.fn(async () => undefined) };
    const createWritable = vi.fn(() => new Promise<typeof writable>((resolve) => {
      releaseWritable = () => resolve(writable);
    }));
    const session = new LocalFileSession({
      getFile: vi.fn(async () => ({ arrayBuffer: async () => base.buffer })),
      createWritable,
    }, base);
    const app = new ViewerApp(root, adapter);
    await app.openSource(base, "fixture.ms", session);
    adapter.source = encoder.encode("changed");

    (root.querySelector("#save-file") as HTMLButtonElement).click();
    await vi.waitFor(() => expect(createWritable).toHaveBeenCalledTimes(1));
    expect((root.querySelector("#formula-input") as HTMLInputElement).disabled).toBe(true);
    expect((root.querySelector("#save-file") as HTMLButtonElement).disabled).toBe(true);
    releaseWritable?.();
    await vi.waitFor(() => expect(write).toHaveBeenCalledWith(adapter.source));
    await vi.waitFor(() => expect((root.querySelector("#formula-input") as HTMLInputElement).disabled).toBe(false));
    app.dispose();
    root.remove();
  });

  it("shows invalid external bytes and structured diagnostics without writing", async () => {
    const root = document.createElement("main");
    document.body.append(root);
    const adapter = new MockAdapter();
    const base = encoder.encode("fixture");
    const external = Uint8Array.of(0xff, 0x20, 0x62, 0x61, 0x64);
    adapter.replaceSource = vi.fn(async () => {
      throw Object.assign(new Error("cannot parse external source"), {
        diagnostics: [diagnostic("MS1001", "invalid directive")],
        diagnostics_omitted: 3,
      });
    });
    const createWritable = vi.fn();
    const session = new LocalFileSession({
      getFile: vi.fn(async () => ({ arrayBuffer: async () => external.buffer })),
      createWritable,
    }, base);
    const app = new ViewerApp(root, adapter);
    await app.openSource(base, "fixture.ms", session);
    (root.querySelector("#save-file") as HTMLButtonElement).click();
    await vi.waitFor(() => expect(root.querySelector("#status")?.textContent).toContain("could not be parsed"));
    expect((root.querySelector("#source-view") as HTMLTextAreaElement).value).toBe("ff 20 62 61 64");
    expect((root.querySelector("#source-view") as HTMLTextAreaElement).dataset.encoding).toBe("hex");
    expect(root.querySelector("#source-title")?.textContent).toContain("invalid UTF-8");
    expect(root.querySelector("#diagnostic-list")?.textContent).toContain("invalid directive");
    expect(root.querySelector("#diagnostic-count")?.textContent).toBe("1 rendered · error +3");
    expect(root.querySelector("#diagnostic-list")?.textContent)
      .toContain("3 additional error diagnostics were omitted by the worker resource cap.");
    expect(createWritable).not.toHaveBeenCalled();
    app.dispose();
    root.remove();
  });

  it("requires and emits an ISO code for a Currency style", async () => {
    const root = document.createElement("main");
    document.body.append(root);
    const adapter = new MockAdapter();
    const app = new ViewerApp(root, adapter);
    await app.openSource(encoder.encode("fixture"), "fixture.ms");
    (root.querySelector("#style-id") as HTMLInputElement).value = "money2";
    (root.querySelector("#style-number") as HTMLSelectElement).value = "Currency";
    (root.querySelector("#style-currency") as HTMLInputElement).value = "";
    (root.querySelector("#define-style") as HTMLButtonElement).click();
    expect(root.querySelector("#status")?.textContent).toContain("three-letter ISO code");
    expect(adapter.edit).not.toHaveBeenCalled();

    (root.querySelector("#style-currency") as HTMLInputElement).value = "usd";
    (root.querySelector("#define-style") as HTMLButtonElement).click();
    await vi.waitFor(() => expect(adapter.edit).toHaveBeenCalled());
    expect(adapter.edit).toHaveBeenCalledWith(expect.objectContaining({
      operations: [expect.objectContaining({
        kind: "define_style",
        style: "money2",
        properties: expect.objectContaining({ number: "Currency", currency: "USD" }),
      })],
    }));
    app.dispose();
    root.remove();
  });
});
