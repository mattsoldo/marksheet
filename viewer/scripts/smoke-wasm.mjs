import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const viewerDirectory = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repositoryDirectory = resolve(viewerDirectory, "..");
const packageDirectory = resolve(viewerDirectory, "public/marksheet-wasm/pkg");
const { default: initialize, WasmWorkbench } = await import(
  new URL("../public/marksheet-wasm/pkg/marksheet_wasm.js", import.meta.url)
);
const wasmBytes = await readFile(resolve(packageDirectory, "marksheet_wasm_bg.wasm"));
await initialize({ module_or_path: wasmBytes });

const source = await readFile(resolve(repositoryDirectory, "examples/budget.ms"));
const workbench = new WasmWorkbench();
let revision = 0;
let nextRequest = 1;

function dispatch(request) {
  const requestId = `node-smoke-${nextRequest++}`;
  const result = JSON.parse(workbench.dispatch_json(JSON.stringify({
    protocol: "marksheet-worker@1",
    request_id: requestId,
    revision,
    request,
  })));
  assert.equal(result.protocol, "marksheet-worker@1");
  assert.equal(result.request_id, requestId);
  assert.notEqual(result.response.kind, "error", JSON.stringify(result.response));
  revision = result.revision;
  return result.response;
}

const opened = dispatch({ kind: "open", source: [...source] });
assert.equal(opened.kind, "opened");
assert.deepEqual(opened.snapshot.sheets.map(({ id }) => id), ["inputs", "summary"]);
assert.equal(opened.snapshot.editable, true);

const b4 = {
  start: { column: 2, row: 4 },
  end: { column: 2, row: 4 },
};
const before = dispatch({ kind: "calculate", sheet: "summary", range: b4 });
assert.equal(before.kind, "calculation");
assert.equal(before.calculation.cells[0]?.value?.kind, "number");
assert.equal(before.calculation.cells[0]?.value?.value, 1648);

const edited = dispatch({
  kind: "edit",
  transaction: {
    operations: [{
      kind: "set_cell",
      sheet: "inputs",
      coordinate: { column: 7, row: 2 },
      value: { kind: "number", value: 0.25 },
    }],
  },
});
assert.equal(edited.kind, "edited");
assert.equal(edited.changed, true);
assert.equal(edited.patches.length, 1);
assert.ok(edited.patches[0].span.end - edited.patches[0].span.start <= 4, "edit patch was not focused");

const after = dispatch({ kind: "calculate", sheet: "summary", range: b4 });
assert.equal(after.kind, "calculation");
assert.equal(after.calculation.cells[0]?.value?.kind, "number");
assert.equal(after.calculation.cells[0]?.value?.value, 1545);

workbench.free();
console.log("Wasm smoke passed: ordered sheets, B4 1648→1545, one focused edit patch.");
