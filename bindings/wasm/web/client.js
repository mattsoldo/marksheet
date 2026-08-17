import {
  PROTOCOL_VERSION,
  applySourcePatches,
  assertRequestJsonSize,
  assertRequestStructureBudget,
  assertSourceSize,
  isByteArray,
} from "./protocol.js";

/** A request was intentionally abandoned when its Worker was restarted. */
export class WorkerCancelledError extends Error {
  constructor() {
    super("Marksheet worker request was cancelled by a worker restart");
    this.name = "WorkerCancelledError";
  }
}

/** The worker returned a structured protocol error. */
export class WorkerProtocolError extends Error {
  /** @param {{code: string, message: string, diagnostics: import("../protocol.d.ts").Diagnostic[], diagnostics_omitted: number}} error */
  constructor(error) {
    super(error.message);
    this.name = "WorkerProtocolError";
    this.code = error.code;
    this.diagnostics = error.diagnostics;
    this.diagnostics_omitted = error.diagnostics_omitted;
  }
}

/**
 * @typedef {{
 *   postMessage(message: unknown): void,
 *   terminate(): void,
 *   onmessage: ((event: MessageEvent) => void) | null,
 *   onerror: ((event: ErrorEvent) => void) | null,
 * }} WorkerLike
 */

/**
 * A revision-aware browser client. The `workerFactory` normally creates
 * `new Worker(new URL("./worker.js", import.meta.url), { type: "module" })`.
 *
 * The client deliberately keeps an exact local source snapshot. This lets a
 * restart cancel outstanding work, construct a new Worker, and reopen only the
 * last accepted source without guessing which in-flight mutation won.
 */
export class MarksheetWorkerClient {
  /** @param {() => WorkerLike} workerFactory */
  constructor(workerFactory) {
    this.workerFactory = workerFactory;
    /** @type {WorkerLike | undefined} */
    this.worker = undefined;
    /** @type {Map<string, {resolve: (value: any) => void, reject: (reason: unknown) => void}>} */
    this.pending = new Map();
    this.nextRequestId = 1;
    this.revision = 0;
    /** @type {Uint8Array | undefined} */
    this.acceptedSource = undefined;
    /** @type {(response: any) => void | undefined} */
    this.onResponse = undefined;
    this.#startWorker();
  }

  /** @returns {number} */
  get currentRevision() {
    return this.revision;
  }

  /** @returns {Uint8Array | undefined} */
  get lastAcceptedSource() {
    return this.acceptedSource?.slice();
  }

  /** @param {(response: any) => void} listener */
  setResponseListener(listener) {
    this.onResponse = listener;
  }

  /** @param {Uint8Array} source */
  open(source) {
    if (!isByteArray(source)) throw new TypeError("open source must be Uint8Array");
    assertSourceSize(source);
    const snapshot = source.slice();
    // `open` establishes revision 1 only for a fresh Worker. Opening another
    // document is a source replacement against the current revision.
    if (this.acceptedSource) return this.replaceSource(snapshot);
    return this.#request({ kind: "open", source: Array.from(snapshot) }, 0, snapshot);
  }

  /** @param {Uint8Array} source */
  replaceSource(source) {
    if (!isByteArray(source)) throw new TypeError("replacement source must be Uint8Array");
    assertSourceSize(source);
    const snapshot = source.slice();
    return this.#request({ kind: "replace_source", source: Array.from(snapshot) }, this.revision, snapshot);
  }

  snapshot() {
    return this.#request({ kind: "workbook_snapshot" }, this.revision);
  }

  /** @param {string} sheet @param {any} range */
  visibleRegion(sheet, range) {
    return this.#request({ kind: "visible_region", sheet, range }, this.revision);
  }

  /** @param {string} sheet @param {any} range */
  calculate(sheet, range) {
    return this.#request({ kind: "calculate", sheet, range }, this.revision);
  }

  /** @param {any} transaction */
  edit(transaction) {
    return this.#request({ kind: "edit", transaction }, this.revision);
  }

  sourceBytes() {
    return this.#request({ kind: "source_bytes" }, this.revision);
  }

  /**
   * Cancels all outstanding promises by terminating the worker, then reopens
   * the exact last accepted document in a fresh worker. The returned promise
   * resolves only when that reopen is accepted.
   */
  async cancelAndRestart() {
    this.#stopWorker(new WorkerCancelledError());
    this.revision = 0;
    this.#startWorker();
    if (this.acceptedSource) await this.#reopenAcceptedSource();
  }

  dispose() {
    this.#stopWorker(new WorkerCancelledError());
  }

  /** @param {any} request @param {number} revision @param {Uint8Array | undefined} proposedSource */
  #request(request, revision, proposedSource = undefined) {
    const worker = this.worker;
    if (!worker) return Promise.reject(new WorkerCancelledError());
    const requestId = `marksheet-${this.nextRequestId++}`;
    const envelope = { protocol: PROTOCOL_VERSION, request_id: requestId, revision, request };
    // Match the Wasm raw-message admission limit before sending. Rejection is
    // immediate and tied to this newly allocated request id, so an oversized
    // local edit cannot leave an entry in `pending` waiting for a response the
    // worker will never receive.
    try {
      assertRequestStructureBudget(envelope);
      assertRequestJsonSize(JSON.stringify(envelope));
    } catch (error) {
      return Promise.reject(error);
    }
    return new Promise((resolve, reject) => {
      this.pending.set(requestId, { resolve, reject, proposedSource });
      worker.postMessage(envelope);
    });
  }

  #startWorker() {
    const worker = this.workerFactory();
    worker.onmessage = (event) => this.#handleResponse(event.data);
    worker.onerror = (event) => this.#handleWorkerError(event);
    this.worker = worker;
  }

  /** Reopens after a restart without treating the retained source as a replacement. */
  #reopenAcceptedSource() {
    const source = this.acceptedSource?.slice();
    if (!source) return Promise.resolve(undefined);
    return this.#request({ kind: "open", source: Array.from(source) }, 0, source);
  }

  /** @param {unknown} response */
  #handleResponse(response) {
    if (!response || typeof response.request_id !== "string") return;
    const pending = this.pending.get(response.request_id);
    // An earlier worker can still deliver a queued message after termination.
    // Unknown ids are stale by design. A structured error belongs to its
    // request even if Worker initialization reported revision zero after a
    // document had already been opened, so reject it before revision filtering.
    if (!pending) return;
    if (response.protocol !== PROTOCOL_VERSION || !Number.isSafeInteger(response.revision) || !response.response || typeof response.response.kind !== "string") {
      this.#rejectInvalidResponse(response.request_id, pending);
      return;
    }
    if (response.response?.kind === "error") {
      if (!isWorkerErrorPayload(response.response.error)) {
        this.#rejectInvalidResponse(response.request_id, pending);
        return;
      }
      this.pending.delete(response.request_id);
      pending.reject(new WorkerProtocolError(response.response.error));
      return;
    }
    if (response.revision < this.revision) {
      const error = new Error("worker returned a stale success response");
      this.pending.delete(response.request_id);
      pending.reject(error);
      // A known pending id with a stale success reply means this worker's
      // session no longer agrees with the accepted local source. Restart and
      // reopen the exact bytes instead of leaving the promise unresolved.
      this.#restartAfterProtocolFailure(error);
      return;
    }

    // Apply an edit response before advancing revision or swapping the restart
    // snapshot. A malformed patch response cannot leave the client claiming a
    // document revision whose exact source it does not possess.
    let nextAcceptedSource = this.acceptedSource;
    if (response.response?.kind === "opened" || response.response?.kind === "replaced") {
      nextAcceptedSource = pending.proposedSource?.slice();
    } else if (response.response?.kind === "edited" && response.response.changed) {
      try {
        nextAcceptedSource = applySourcePatches(this.acceptedSource ?? new Uint8Array(), response.response.patches);
      } catch (error) {
        this.pending.delete(response.request_id);
        pending.reject(error);
        this.#restartAfterProtocolFailure(error);
        return;
      }
    }

    this.pending.delete(response.request_id);
    this.revision = response.revision;
    this.acceptedSource = nextAcceptedSource;
    this.onResponse?.(response);
    pending.resolve(response);
  }

  /** @param {unknown} event */
  #handleWorkerError(event) {
    const error = event instanceof ErrorEvent ? event.error ?? new Error(event.message) : event;
    this.#stopWorker(error);
  }

  /** Rejects a matching malformed reply and restores the accepted source. */
  #rejectInvalidResponse(requestId, pending) {
    const error = new Error("worker returned an invalid protocol response");
    this.pending.delete(requestId);
    pending.reject(error);
    this.#restartAfterProtocolFailure(error);
  }

  /** @param {unknown} reason */
  #stopWorker(reason) {
    this.worker?.terminate();
    this.worker = undefined;
    for (const { reject } of this.pending.values()) reject(reason);
    this.pending.clear();
  }

  /** @param {unknown} reason */
  #restartAfterProtocolFailure(reason) {
    this.#stopWorker(reason);
    this.revision = 0;
    this.#startWorker();
    if (this.acceptedSource) this.#reopenAcceptedSource().catch(() => {});
  }
}

/** @param {unknown} value */
function isWorkerErrorPayload(value) {
  return Boolean(
    value
      && typeof value === "object"
      && typeof value.code === "string"
      && typeof value.message === "string"
      && Array.isArray(value.diagnostics)
      && Number.isSafeInteger(value.diagnostics_omitted)
      && value.diagnostics_omitted >= 0,
  );
}
