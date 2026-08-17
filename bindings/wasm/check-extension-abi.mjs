import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { resolve } from "node:path";

const modulePath = process.argv[2];
if (!modulePath) {
  throw new Error("usage: node check-extension-abi.mjs <generated-node-module>");
}
const require = createRequire(import.meta.url);
const { WasmWorkbench } = require(resolve(modulePath));
const encoder = new TextEncoder();

function request(workbench, revision, requestId, requestBody) {
  return JSON.parse(workbench.dispatch_json(JSON.stringify({
    protocol: "marksheet-worker@1",
    request_id: requestId,
    revision,
    request: requestBody,
  })));
}

function open(source) {
  const workbench = new WasmWorkbench();
  const response = request(workbench, 0, "open", {
    kind: "open",
    source: [...encoder.encode(source)],
  });
  assert.equal(response.response.kind, "opened");
  assert.equal(response.revision, 1);
  return { workbench, snapshot: response.response.snapshot };
}

function diagnosticCodes(snapshot) {
  return snapshot.diagnostics.map(({ code }) => code);
}

{
  const source = "#!marksheet 0.1\n@require assertions@1\n@sheet s \"S\"\n@block A1 csv\nValue\n2\n@end\n@extension assertions@1 \"checks\"\nassert A2 = 2\n@end\n";
  const { workbench, snapshot } = open(source);
  assert.deepEqual(snapshot.extension_support.supported_capabilities, ["assertions@1"]);
  assert.equal(snapshot.extension_support.calculation_complete, true);
  assert.equal(snapshot.extension_support.rendering_complete, true);
  assert.equal(snapshot.extension_support.valid, true);
  assert.equal(snapshot.extension_declarations[0].availability, "available");
  assert.equal(snapshot.extension_instances[0].outcome, "processed");
  assert.equal(JSON.stringify(snapshot).includes("assert A2 = 2"), false);

  const edited = request(workbench, 1, "edit", {
    kind: "edit",
    transaction: {
      operations: [{
        kind: "rename_sheet_label",
        sheet: "s",
        label: "Renamed",
      }],
    },
  });
  assert.equal(edited.response.kind, "edited");
  assert.equal(edited.response.snapshot.sheets[0].label, "Renamed");
  assert.equal(edited.response.snapshot.extension_support.valid, true);
}

{
  const source = "#!marksheet 0.1\n@use assertions@1\n@sheet s \"S\"\n@block A1 csv\nValue\n2\n@end\n@extension assertions@1 \"checks\"\nassert A2 = 3\n@end\n";
  const { workbench, snapshot } = open(source);
  assert.deepEqual(diagnosticCodes(snapshot), ["MS3201"]);
  assert.equal(snapshot.editable, true);
  assert.equal(snapshot.extension_support.calculation_complete, true);
  assert.equal(snapshot.extension_support.rendering_complete, true);
  assert.equal(snapshot.extension_support.valid, false);
  const calculation = request(workbench, 1, "calculate-after-assertion", {
    kind: "calculate",
    sheet: "s",
    range: { start: { column: 1, row: 2 }, end: { column: 1, row: 2 } },
  });
  assert.equal(calculation.response.kind, "calculation");
  assert.equal(calculation.response.calculation.cells.length, 1);
}

{
  const { snapshot } = open("#!marksheet 0.1\n@use assertions@2\n@sheet s \"S\"\n");
  assert.deepEqual(diagnosticCodes(snapshot), ["MS3102"]);
  assert.equal(snapshot.extension_declarations[0].availability, "unavailable_optional");
  assert.equal(snapshot.extension_support.calculation_complete, true);
  assert.equal(snapshot.extension_support.rendering_complete, true);
}

{
  const source = "#!marksheet 0.1\n@require assertions@2\n@sheet s \"S\"\n@block A1 csv\n=1+1\n@end\n";
  const { workbench, snapshot } = open(source);
  assert.deepEqual(diagnosticCodes(snapshot), ["MS3101"]);
  assert.equal(snapshot.extension_declarations[0].availability, "unavailable_required");
  assert.equal(snapshot.extension_support.calculation_complete, false);
  assert.equal(snapshot.extension_support.rendering_complete, false);

  const calculation = request(workbench, 1, "calculate", {
    kind: "calculate",
    sheet: "s",
    range: { start: { column: 1, row: 1 }, end: { column: 1, row: 1 } },
  });
  assert.equal(calculation.response.kind, "error");
  assert.equal(calculation.response.error.code, "calculation");

  const visible = request(workbench, 1, "visible", {
    kind: "visible_region",
    sheet: "s",
    range: { start: { column: 1, row: 1 }, end: { column: 1, row: 1 } },
  });
  assert.equal(visible.response.kind, "visible_region");
  assert.equal(visible.response.region.completeness.calculation_complete, false);
  assert.equal(visible.response.region.completeness.rendering_complete, false);
  assert.equal(visible.response.region.cells[0].calculated, null);
}

{
  const source = "#!marksheet 0.1\n@extension vendor_data@1 \"opaque\"\nprivate-payload\n@end\n@sheet s \"S\"\n";
  const { snapshot } = open(source);
  assert.deepEqual(diagnosticCodes(snapshot), ["MS3103"]);
  assert.equal(snapshot.extension_support.calculation_complete, true);
  assert.equal(snapshot.extension_support.rendering_complete, true);
  assert.equal(snapshot.extension_instances[0].declared, false);
  assert.equal(snapshot.extension_instances[0].supported, false);
  assert.equal(snapshot.extension_instances[0].outcome, "skipped_undeclared");
  assert.equal(JSON.stringify(snapshot).includes("private-payload"), false);
}
