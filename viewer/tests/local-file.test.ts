import { describe, expect, it, vi } from "vitest";
import { ExternalFileChangeError, LocalFileSession } from "../src/local-file";
import type { WorkerResponseEnvelope } from "../src/protocol";
import type { WorkbenchAdapter } from "../src/worker-adapter";

function envelope(response: WorkerResponseEnvelope["response"], revision = 1): WorkerResponseEnvelope {
  return { protocol: "marksheet-worker@1", request_id: "test", revision, response };
}

function adapter(sessionSource: Uint8Array): WorkbenchAdapter {
  return {
    currentRevision: 1,
    lastAcceptedSource: sessionSource,
    open: vi.fn(),
    replaceSource: vi.fn(async () => envelope({
      kind: "replaced",
      snapshot: snapshot(2),
    }, 2)),
    snapshot: vi.fn(),
    visibleRegion: vi.fn(),
    calculate: vi.fn(),
    edit: vi.fn(),
    sourceBytes: vi.fn(async () => envelope({ kind: "source_bytes", source: [...sessionSource] })),
    cancelAndRestart: vi.fn(),
    dispose: vi.fn(),
  } as WorkbenchAdapter;
}

function snapshot(revision: number) {
  return {
    revision,
    diagnostics: [],
    diagnostics_omitted: 0,
    editable: true,
    locale: "en-US",
    timezone: "UTC",
    formula_profile: "portable-v1",
    sheets: [],
    names: [],
    style_count: 0,
    name_count: 0,
  };
}

describe("local-file external-change guard", () => {
  it("reparses drift and never obtains a writable", async () => {
    const base = new TextEncoder().encode("base");
    const external = new TextEncoder().encode("outside");
    const createWritable = vi.fn();
    const handle = {
      getFile: vi.fn(async () => ({ arrayBuffer: async () => external.buffer })),
      createWritable,
    };
    const worker = adapter(new TextEncoder().encode("inside"));
    const session = new LocalFileSession(handle, base);

    await expect(session.save(worker)).rejects.toBeInstanceOf(ExternalFileChangeError);
    expect(worker.replaceSource).toHaveBeenCalledWith(external);
    expect(createWritable).not.toHaveBeenCalled();
  });

  it("does not write an unchanged document", async () => {
    const base = new TextEncoder().encode("same");
    const createWritable = vi.fn();
    const handle = {
      getFile: vi.fn(async () => ({ arrayBuffer: async () => base.buffer })),
      createWritable,
    };
    const session = new LocalFileSession(handle, base);

    await expect(session.save(adapter(base))).resolves.toBe("unchanged");
    expect(createWritable).not.toHaveBeenCalled();
  });

  it("preserves invalid external bytes when reparsing fails", async () => {
    const base = new TextEncoder().encode("base");
    const invalid = new TextEncoder().encode("\xff invalid external source");
    const createWritable = vi.fn();
    const worker = adapter(base);
    const parseError = Object.assign(new Error("invalid source"), {
      diagnostics: [{ severity: "error", code: "MS1001", message: "invalid source" }],
    });
    worker.replaceSource = vi.fn(async () => { throw parseError; });
    const session = new LocalFileSession({
      getFile: vi.fn(async () => ({ arrayBuffer: async () => invalid.buffer })),
      createWritable,
    }, base);

    const failure = await session.save(worker).catch((error: unknown) => error);
    expect(failure).toBeInstanceOf(ExternalFileChangeError);
    expect((failure as ExternalFileChangeError).externalSource).toEqual(invalid);
    expect((failure as ExternalFileChangeError).reparseError).toBe(parseError);
    expect((failure as ExternalFileChangeError).workerReplaced).toBe(false);
    expect(session.baseSource).toEqual(base);
    expect(createWritable).not.toHaveBeenCalled();
  });
});
