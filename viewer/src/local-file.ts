import type { WorkbenchAdapter } from "./worker-adapter";
import { responsePayload } from "./worker-adapter";

export interface LocalFileLike {
  arrayBuffer(): Promise<ArrayBuffer>;
}

export interface WritableLike {
  write(data: Uint8Array): Promise<void>;
  close(): Promise<void>;
  abort?(): Promise<void>;
}

export interface LocalFileHandleLike {
  name?: string;
  getFile(): Promise<LocalFileLike>;
  createWritable(): Promise<WritableLike>;
}

export class ExternalFileChangeError extends Error {
  readonly reparseError: unknown | undefined;
  readonly workerReplaced: boolean;

  constructor(
    readonly externalSource: Uint8Array,
    reparseError: unknown | undefined = undefined,
    workerReplaced = true,
  ) {
    super(workerReplaced
      ? "The local file changed outside Marksheet. The external version was opened; no bytes were written."
      : "The local file changed outside Marksheet and could not be parsed. Its exact bytes are shown; no bytes were written.");
    this.name = "ExternalFileChangeError";
    this.reparseError = reparseError;
    this.workerReplaced = workerReplaced;
  }
}

export class LocalFileSession {
  #baseSource: Uint8Array;

  constructor(
    readonly handle: LocalFileHandleLike,
    baseSource: Uint8Array,
  ) {
    this.#baseSource = baseSource.slice();
  }

  get baseSource(): Uint8Array {
    return this.#baseSource.slice();
  }

  /**
   * Checks the exact on-disk bytes before obtaining a writable. External drift
   * is reparsed in the worker and surfaced as a conflict; it is never hidden by
   * a whole-document overwrite.
   */
  async save(adapter: WorkbenchAdapter): Promise<"saved" | "unchanged"> {
    const diskSource = new Uint8Array(await (await this.handle.getFile()).arrayBuffer());
    if (!bytesEqual(diskSource, this.#baseSource)) {
      try {
        await adapter.replaceSource(diskSource);
      } catch (error) {
        throw new ExternalFileChangeError(diskSource, error, false);
      }
      this.#baseSource = diskSource.slice();
      throw new ExternalFileChangeError(diskSource);
    }

    const sourceResponse = responsePayload(await adapter.sourceBytes(), "source_bytes");
    const sessionSource = Uint8Array.from(sourceResponse.source);
    if (bytesEqual(sessionSource, this.#baseSource)) return "unchanged";

    const writable = await this.handle.createWritable();
    try {
      await writable.write(sessionSource);
      await writable.close();
    } catch (error) {
      await writable.abort?.();
      throw error;
    }
    this.#baseSource = sessionSource.slice();
    return "saved";
  }
}

export function bytesEqual(left: Uint8Array, right: Uint8Array): boolean {
  if (left.byteLength !== right.byteLength) return false;
  for (let index = 0; index < left.byteLength; index += 1) {
    if (left[index] !== right[index]) return false;
  }
  return true;
}

export function downloadSource(source: Uint8Array, filename: string): void {
  const copy = new Uint8Array(source);
  const url = URL.createObjectURL(new Blob([copy], { type: "text/plain;charset=utf-8" }));
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  anchor.click();
  URL.revokeObjectURL(url);
}
