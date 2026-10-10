import type { LocalFileHandleLike } from "./local-file";

/**
 * A remembered workbook. Entries opened through the File System Access API
 * keep only their handle and are reread from disk; entries opened without a
 * handle keep a browser-local copy of the last opened or saved bytes.
 */
export interface RecentWorkbook {
  id: string;
  name: string;
  openedAt: number;
  byteLength: number;
  sheetCount: number;
  handle?: RecentFileHandle;
}

export interface RecentFileHandle extends LocalFileHandleLike {
  name: string;
  isSameEntry?(other: unknown): Promise<boolean>;
  queryPermission?(descriptor: { mode: "read" | "readwrite" }): Promise<PermissionState>;
  requestPermission?(descriptor: { mode: "read" | "readwrite" }): Promise<PermissionState>;
}

export interface RememberRequest {
  name: string;
  sheetCount: number;
  source: Uint8Array;
  handle?: RecentFileHandle;
  /** Reuses an existing entry, for example when a recent workbook is reopened. */
  id?: string;
}

export interface RecentWorkbookStore {
  list(): Promise<RecentWorkbook[]>;
  /** Returns the stored copy for handle-less entries. */
  source(id: string): Promise<Uint8Array | undefined>;
  remember(request: RememberRequest): Promise<RecentWorkbook>;
  /** Replaces the stored copy of a handle-less entry, for example after a download save. */
  updateSource(id: string, source: Uint8Array): Promise<void>;
  remove(id: string): Promise<void>;
  clear(): Promise<void>;
}

export const MAX_RECENT_WORKBOOKS = 12;

interface StoredRecord {
  entry: RecentWorkbook;
  source?: Uint8Array;
}

/** The persistence-free store used by tests and browsers without IndexedDB. */
export class MemoryRecentStore implements RecentWorkbookStore {
  #records = new Map<string, StoredRecord>();
  #clock: () => number;

  constructor(clock: () => number = Date.now) {
    this.#clock = clock;
  }

  async list(): Promise<RecentWorkbook[]> {
    return sortRecent([...this.#records.values()].map((record) => ({ ...record.entry })));
  }

  async source(id: string): Promise<Uint8Array | undefined> {
    return this.#records.get(id)?.source?.slice();
  }

  async remember(request: RememberRequest): Promise<RecentWorkbook> {
    const entries = [...this.#records.values()].map((record) => record.entry);
    const id = request.id && this.#records.has(request.id)
      ? request.id
      : await matchingEntryId(entries, request) ?? createId();
    const record = buildRecord(id, request, this.#clock());
    this.#records.set(id, record);
    for (const stale of evicted(sortRecent([...this.#records.values()].map((value) => value.entry)))) {
      this.#records.delete(stale.id);
    }
    return { ...record.entry };
  }

  async updateSource(id: string, source: Uint8Array): Promise<void> {
    const record = this.#records.get(id);
    if (!record || record.entry.handle) return;
    record.source = source.slice();
    record.entry = { ...record.entry, byteLength: source.byteLength };
  }

  async remove(id: string): Promise<void> {
    this.#records.delete(id);
  }

  async clear(): Promise<void> {
    this.#records.clear();
  }
}

const DATABASE = "marksheet-viewer";
const ENTRIES = "recent";
const SOURCES = "recent-sources";

/** Browser-local persistence. Nothing leaves the device. */
/** Runs a task exclusively; used to make read-modify-write updates atomic. */
export type ExclusiveRunner = <T>(task: () => Promise<T>) => Promise<T>;

/**
 * Every same-origin tab shares one IndexedDB database, so every write takes an
 * origin-wide Web Lock where the browser provides one.
 */
export async function originLock<T>(task: () => Promise<T>): Promise<T> {
  const locks = typeof navigator === "undefined" ? undefined : navigator.locks;
  if (!locks) return task();
  return await locks.request("marksheet-viewer-recent", task) as T;
}

export class IndexedDbRecentStore implements RecentWorkbookStore {
  #database: Promise<IDBDatabase> | undefined;

  constructor(
    private readonly factory: IDBFactory,
    private readonly exclusive: ExclusiveRunner = originLock,
  ) {}

  async list(): Promise<RecentWorkbook[]> {
    const entries = await this.#request<RecentWorkbook[]>(ENTRIES, "readonly", (store) => store.getAll());
    return sortRecent(entries);
  }

  async source(id: string): Promise<Uint8Array | undefined> {
    const value = await this.#request<unknown>(SOURCES, "readonly", (store) => store.get(id));
    return value instanceof Uint8Array ? value.slice() : undefined;
  }

  remember(request: RememberRequest): Promise<RecentWorkbook> {
    return this.exclusive(() => this.#remember(request));
  }

  async #remember(request: RememberRequest): Promise<RecentWorkbook> {
    const entries = await this.list();
    const id = request.id && entries.some((entry) => entry.id === request.id)
      ? request.id
      : await matchingEntryId(entries, request) ?? createId();
    const record = buildRecord(id, request, Date.now());
    const stale = evicted(sortRecent([record.entry, ...entries.filter((entry) => entry.id !== id)]));
    const database = await this.#open();
    await transactionDone(database, [ENTRIES, SOURCES], (transaction) => {
      const entryStore = transaction.objectStore(ENTRIES);
      const sourceStore = transaction.objectStore(SOURCES);
      entryStore.put(record.entry);
      if (record.source) sourceStore.put(record.source, id);
      else sourceStore.delete(id);
      for (const entry of stale) {
        entryStore.delete(entry.id);
        sourceStore.delete(entry.id);
      }
    });
    return record.entry;
  }

  updateSource(id: string, source: Uint8Array): Promise<void> {
    return this.exclusive(async () => {
      const entry = (await this.list()).find((candidate) => candidate.id === id);
      if (!entry || entry.handle) return;
      const database = await this.#open();
      await transactionDone(database, [ENTRIES, SOURCES], (transaction) => {
        transaction.objectStore(ENTRIES).put({ ...entry, byteLength: source.byteLength });
        transaction.objectStore(SOURCES).put(source.slice(), id);
      });
    });
  }

  // Destructive writes share the lock so they order against other tabs' updates.
  remove(id: string): Promise<void> {
    return this.exclusive(async () => {
      const database = await this.#open();
      await transactionDone(database, [ENTRIES, SOURCES], (transaction) => {
        transaction.objectStore(ENTRIES).delete(id);
        transaction.objectStore(SOURCES).delete(id);
      });
    });
  }

  clear(): Promise<void> {
    return this.exclusive(async () => {
      const database = await this.#open();
      await transactionDone(database, [ENTRIES, SOURCES], (transaction) => {
        transaction.objectStore(ENTRIES).clear();
        transaction.objectStore(SOURCES).clear();
      });
    });
  }

  #open(): Promise<IDBDatabase> {
    this.#database ??= new Promise((resolve, reject) => {
      const request = this.factory.open(DATABASE, 1);
      request.onupgradeneeded = () => {
        const database = request.result;
        if (!database.objectStoreNames.contains(ENTRIES)) database.createObjectStore(ENTRIES, { keyPath: "id" });
        if (!database.objectStoreNames.contains(SOURCES)) database.createObjectStore(SOURCES);
      };
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error ?? new Error("could not open recent workbook storage"));
    });
    return this.#database;
  }

  async #request<T>(
    storeName: string,
    mode: IDBTransactionMode,
    action: (store: IDBObjectStore) => IDBRequest,
  ): Promise<T> {
    const database = await this.#open();
    return new Promise<T>((resolve, reject) => {
      const request = action(database.transaction(storeName, mode).objectStore(storeName));
      request.onsuccess = () => resolve(request.result as T);
      request.onerror = () => reject(request.error ?? new Error("recent workbook storage request failed"));
    });
  }
}

/** Prefers IndexedDB and degrades to a session-only store when it is unavailable. */
export function createRecentStore(): RecentWorkbookStore {
  try {
    if (typeof indexedDB !== "undefined" && indexedDB) {
      return new FallbackRecentStore(new IndexedDbRecentStore(indexedDB));
    }
  } catch {
    // Some privacy modes throw on access; remembering is a convenience only.
  }
  return new MemoryRecentStore();
}

/**
 * Uses the primary store until any operation fails, then switches to memory
 * for the rest of the session. Restricted storage modes can expose
 * `indexedDB` but reject `open()` or later writes.
 */
export class FallbackRecentStore implements RecentWorkbookStore {
  #active: RecentWorkbookStore;
  #fallback: RecentWorkbookStore | undefined;

  constructor(primary: RecentWorkbookStore, private readonly createFallback = () => new MemoryRecentStore()) {
    this.#active = primary;
  }

  list() { return this.#run((store) => store.list()); }
  source(id: string) { return this.#run((store) => store.source(id)); }
  remember(request: RememberRequest) { return this.#run((store) => store.remember(request)); }
  updateSource(id: string, source: Uint8Array) { return this.#run((store) => store.updateSource(id, source)); }
  remove(id: string) { return this.#run((store) => store.remove(id)); }
  clear() { return this.#run((store) => store.clear()); }

  async #run<T>(operation: (store: RecentWorkbookStore) => Promise<T>): Promise<T> {
    const store = this.#active;
    try {
      return await operation(store);
    } catch (error) {
      // Only a failure of the fallback itself is final; overlapping primary failures retry there.
      if (store === this.#fallback) throw error;
      this.#fallback ??= this.createFallback();
      this.#active = this.#fallback;
      return operation(this.#fallback);
    }
  }
}

/**
 * Ensures access to a handle; the request must follow a user gesture. Reopening
 * asks only for `read`; Save asks for `readwrite` from its own click, because
 * `createWritable()` fails rather than prompting when write access is not granted.
 */
export async function ensureHandlePermission(
  handle: RecentFileHandle,
  mode: "read" | "readwrite" = "read",
): Promise<boolean> {
  const descriptor = { mode };
  if (!handle.queryPermission) return true;
  if (await handle.queryPermission(descriptor) === "granted") return true;
  return (await handle.requestPermission?.(descriptor)) === "granted";
}

export function formatRelativeTime(timestamp: number, now: number = Date.now()): string {
  const seconds = Math.max(0, Math.round((now - timestamp) / 1000));
  if (seconds < 45) return "Just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  if (days === 1) return "Yesterday";
  if (days < 7) return `${days}d ago`;
  return new Date(timestamp).toISOString().slice(0, 10);
}

function buildRecord(id: string, request: RememberRequest, openedAt: number): StoredRecord {
  const entry: RecentWorkbook = {
    id,
    name: request.name,
    openedAt,
    byteLength: request.source.byteLength,
    sheetCount: request.sheetCount,
    ...(request.handle ? { handle: request.handle } : {}),
  };
  return request.handle ? { entry } : { entry, source: request.source.slice() };
}

async function matchingEntryId(entries: RecentWorkbook[], request: RememberRequest): Promise<string | undefined> {
  for (const entry of entries) {
    if (request.handle && entry.handle) {
      try {
        if (await request.handle.isSameEntry?.(entry.handle)) return entry.id;
      } catch {
        // A revoked or detached handle cannot match.
      }
    } else if (!request.handle && !entry.handle && entry.name === request.name) {
      return entry.id;
    }
  }
  return undefined;
}

function sortRecent(entries: RecentWorkbook[]): RecentWorkbook[] {
  return [...entries].sort((left, right) => right.openedAt - left.openedAt);
}

function evicted(sorted: RecentWorkbook[]): RecentWorkbook[] {
  return sorted.slice(MAX_RECENT_WORKBOOKS);
}

function createId(): string {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

function transactionDone(
  database: IDBDatabase,
  stores: string[],
  body: (transaction: IDBTransaction) => void,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const transaction = database.transaction(stores, "readwrite");
    transaction.oncomplete = () => resolve();
    transaction.onerror = () => reject(transaction.error ?? new Error("recent workbook storage write failed"));
    transaction.onabort = () => reject(transaction.error ?? new Error("recent workbook storage write aborted"));
    body(transaction);
  });
}
