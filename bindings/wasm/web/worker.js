// Browser module-worker entry point. Build the Rust cdylib with wasm-bindgen
// into `bindings/wasm/pkg` before bundling this module; see README.md.
import init, { WasmWorkbench } from "../pkg/marksheet_wasm.js";

import {
  MAX_SOURCE_BYTES,
  PROTOCOL_VERSION,
  assertRequestJsonSize,
  assertRequestStructureBudget,
} from "./protocol.js";

const ready = init().then(() => new WasmWorkbench());

self.addEventListener("message", async (event) => {
  const request = event.data;
  const requestId = typeof request?.request_id === "string" ? request.request_id : "invalid";
  if (!request || request.protocol !== PROTOCOL_VERSION) {
    self.postMessage({
      protocol: PROTOCOL_VERSION,
      request_id: requestId,
      revision: 0,
      response: {
        kind: "error",
        error: {
          code: "protocol",
          message: "expected marksheet-worker@1 request envelope",
          diagnostics: [],
          diagnostics_omitted: 0,
        },
      },
    });
    return;
  }

  if ((request.request?.kind === "open" || request.request?.kind === "replace_source")
      && (!Array.isArray(request.request.source) || request.request.source.length > MAX_SOURCE_BYTES)) {
    self.postMessage({
      protocol: PROTOCOL_VERSION,
      request_id: requestId,
      revision: Number.isSafeInteger(request.revision) ? request.revision : 0,
      response: { kind: "error", error: {
        code: "limit",
        message: `source exceeds the ${MAX_SOURCE_BYTES} byte worker limit`,
        diagnostics: [],
        diagnostics_omitted: 0,
      } },
    });
    return;
  }

  try {
    // Bound the same exact UTF-8 JSON payload the worker passes to Wasm. This
    // produces a correlated protocol error rather than allowing serde to
    // allocate an oversized request or leaving the browser promise pending.
    assertRequestStructureBudget(request);
    assertRequestJsonSize(JSON.stringify(request));
  } catch (error) {
    self.postMessage({
      protocol: PROTOCOL_VERSION,
      request_id: requestId,
      revision: Number.isSafeInteger(request.revision) ? request.revision : 0,
      response: { kind: "error", error: {
        code: "limit",
        message: error instanceof Error ? error.message : String(error),
        diagnostics: [],
        diagnostics_omitted: 0,
      } },
    });
    return;
  }

  try {
    const workbench = await ready;
    const response = JSON.parse(workbench.dispatch_json(JSON.stringify(request)));
    self.postMessage(response);
  } catch (error) {
    self.postMessage({
      protocol: PROTOCOL_VERSION,
      request_id: requestId,
      revision: 0,
      response: {
        kind: "error",
        error: {
          code: "session",
          message: error instanceof Error ? error.message : String(error),
          diagnostics: [],
          diagnostics_omitted: 0,
        },
      },
    });
  }
});
