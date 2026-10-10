import { describe, expect, it } from "vitest";
import { MAX_RECENT_WORKBOOKS, MemoryRecentStore, formatRelativeTime, type RecentFileHandle } from "../src/recent";
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
