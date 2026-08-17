import { describe, expect, it } from "vitest";
import { StaleResponseGate, resolveWorkerAssetUrl } from "../src/worker-adapter";

describe("stale response suppression", () => {
  it("accepts only the newest request generation", () => {
    const gate = new StaleResponseGate();
    const initial = gate.begin();
    const replacement = gate.begin();

    expect(gate.isCurrent(initial)).toBe(false);
    expect(gate.isCurrent(replacement)).toBe(true);

    gate.invalidate();
    expect(gate.isCurrent(replacement)).toBe(false);
  });
});

describe("worker asset deployment", () => {
  it("keeps the worker under a project-site base path", () => {
    expect(resolveWorkerAssetUrl("/marksheet/", "https://example.test/marksheet/").href)
      .toBe("https://example.test/marksheet/marksheet-wasm/web/worker.js");
  });
});
