import { describe, expect, it } from "vitest";
import {
  FallbackRecentStore,
  MAX_RECENT_WORKBOOKS,
  MemoryRecentStore,
  ensureHandlePermission,
  formatRelativeTime,
  originLock,
  type RecentFileHandle,
  type RecentWorkbookStore,
} from "../src/recent";
import { defaultPreferences, loadPreferences, savePreferences } from "../src/preferences";

const encoder = new TextEncoder();

function fakeHandle(name: string, identity: object): RecentFileHandle {
  return {
    name,
    identity,
    async getFile() { return { arrayBuffer: async () => new ArrayBuffer(0) }; },
    async createWritable() { return { write: async () => undefined, close: async () => undefined }; },
    async isSameEntry(other: unknown) { return (other as { identity?: object }).identity === identity; },
  } as RecentFileHandle;
}

describe("recent workbooks", () => {
  it("orders by recency and reuses an entry for the same handle-less file name", async () => {
    let now = 1_000;
    const store = new MemoryRecentStore(() => now);
    const first = await store.remember({ name: "budget.ms", sheetCount: 2, source: encoder.encode("a") });
    now += 1;
    await store.remember({ name: "invoice.ms", sheetCount: 1, source: encoder.encode("b") });
    now += 1;
    const again = await store.remember({ name: "budget.ms", sheetCount: 2, source: encoder.encode("abc") });

    expect(again.id).toBe(first.id);
    expect((await store.list()).map((entry) => entry.name)).toEqual(["budget.ms", "invoice.ms"]);
    expect(new TextDecoder().decode(await store.source(first.id))).toBe("abc");
  });

  it("keeps handles instead of bytes and matches the same file entry", async () => {
    const store = new MemoryRecentStore();
    const identity = {};
    const first = await store.remember({ name: "a.ms", sheetCount: 1, source: encoder.encode("x"), handle: fakeHandle("a.ms", identity) });
    const second = await store.remember({ name: "a.ms", sheetCount: 1, source: encoder.encode("x"), handle: fakeHandle("a.ms", identity) });
    const other = await store.remember({ name: "a.ms", sheetCount: 1, source: encoder.encode("x"), handle: fakeHandle("a.ms", {}) });

    expect(second.id).toBe(first.id);
    expect(other.id).not.toBe(first.id);
    expect(await store.source(first.id)).toBeUndefined();
    await store.updateSource(first.id, encoder.encode("ignored"));
    expect(await store.source(first.id)).toBeUndefined();
  });

  it("evicts the oldest entries beyond the cap and supports forget and clear", async () => {
    let now = 0;
    const store = new MemoryRecentStore(() => (now += 1));
    for (let index = 0; index < MAX_RECENT_WORKBOOKS + 3; index += 1) {
      await store.remember({ name: `w${index}.ms`, sheetCount: 1, source: encoder.encode(String(index)) });
    }
    const listed = await store.list();
    expect(listed).toHaveLength(MAX_RECENT_WORKBOOKS);
    expect(listed.at(-1)?.name).toBe("w3.ms");

    await store.remove(listed[0]!.id);
    expect(await store.list()).toHaveLength(MAX_RECENT_WORKBOOKS - 1);
    await store.clear();
    expect(await store.list()).toEqual([]);
  });

  it("falls back to memory for the session when the primary store fails at runtime", async () => {
    const failing: RecentWorkbookStore = {
      list: async () => { throw new Error("open blocked"); },
      source: async () => { throw new Error("open blocked"); },
      remember: async () => { throw new Error("open blocked"); },
      updateSource: async () => { throw new Error("open blocked"); },
      remove: async () => { throw new Error("open blocked"); },
      clear: async () => { throw new Error("open blocked"); },
    };
    const store = new FallbackRecentStore(failing);
    expect(await store.list()).toEqual([]);
    const entry = await store.remember({ name: "a.ms", sheetCount: 1, source: encoder.encode("a") });
    expect((await store.list()).map((item) => item.id)).toEqual([entry.id]);
    expect(new TextDecoder().decode(await store.source(entry.id))).toBe("a");
  });

  it("retries every overlapping primary failure on the fallback", async () => {
    const reject = () => new Promise<never>((_, fail) => setTimeout(() => fail(new Error("blocked")), 0));
    const failing: RecentWorkbookStore = {
      list: reject, source: reject, remember: reject, updateSource: reject, remove: reject, clear: reject,
    };
    const store = new FallbackRecentStore(failing);
    const [listed, remembered] = await Promise.all([
      store.list(),
      store.remember({ name: "first.ms", sheetCount: 1, source: encoder.encode("a") }),
    ]);
    expect(listed).toEqual([]);
    expect((await store.list()).map((entry) => entry.id)).toEqual([remembered.id]);
  });

  it("takes the origin-wide Web Lock for read-modify-write updates when available", async () => {
    const requests: string[] = [];
    const locks = { request: async (name: string, task: () => Promise<unknown>) => { requests.push(name); return task(); } };
    const original = Object.getOwnPropertyDescriptor(navigator, "locks");
    Object.defineProperty(navigator, "locks", { configurable: true, value: locks });
    try {
      expect(await originLock(async () => 7)).toBe(7);
      expect(requests).toEqual(["marksheet-viewer-recent"]);
    } finally {
      if (original) Object.defineProperty(navigator, "locks", original);
      else Reflect.deleteProperty(navigator, "locks");
    }
  });

  it("asks only for read access when reopening a handle", async () => {
    const modes: string[] = [];
    const handle = {
      ...fakeHandle("a.ms", {}),
      queryPermission: async ({ mode }: { mode: string }) => { modes.push(`query:${mode}`); return "prompt" as PermissionState; },
      requestPermission: async ({ mode }: { mode: string }) => { modes.push(`request:${mode}`); return "granted" as PermissionState; },
    } as RecentFileHandle;
    expect(await ensureHandlePermission(handle)).toBe(true);
    expect(modes).toEqual(["query:read", "request:read"]);
  });

  it("formats relative times without locale dependence", () => {
    const now = Date.UTC(2026, 9, 10, 12);
    expect(formatRelativeTime(now - 10_000, now)).toBe("Just now");
    expect(formatRelativeTime(now - 5 * 60_000, now)).toBe("5m ago");
    expect(formatRelativeTime(now - 3 * 3_600_000, now)).toBe("3h ago");
    expect(formatRelativeTime(now - 26 * 3_600_000, now)).toBe("Yesterday");
    expect(formatRelativeTime(now - 30 * 86_400_000, now)).toBe("2026-09-10");
  });
});

describe("viewer preferences", () => {
  function memoryStorage(): Storage {
    const values = new Map<string, string>();
    return {
      get length() { return values.size; },
      clear: () => values.clear(),
      getItem: (key) => values.get(key) ?? null,
      key: (index) => [...values.keys()][index] ?? null,
      removeItem: (key) => { values.delete(key); },
      setItem: (key, value) => { values.set(key, value); },
    };
  }

  it("defaults to the reading view and follows the OS color scheme", () => {
    expect(defaultPreferences(false)).toEqual({ theme: "paper", sidebarOpen: true, detailsOpen: false });
    expect(loadPreferences(undefined, true).theme).toBe("graphite");
  });

  it("round-trips valid choices and ignores malformed storage", () => {
    const storage = memoryStorage();
    savePreferences(storage, { theme: "ledger", sidebarOpen: false, detailsOpen: true });
    expect(loadPreferences(storage)).toEqual({ theme: "ledger", sidebarOpen: false, detailsOpen: true });
    storage.setItem("marksheet.viewer.preferences", JSON.stringify({ theme: "neon", sidebarOpen: "yes" }));
    expect(loadPreferences(storage)).toEqual(defaultPreferences());
    storage.setItem("marksheet.viewer.preferences", "{");
    expect(loadPreferences(storage)).toEqual(defaultPreferences());
  });
});
