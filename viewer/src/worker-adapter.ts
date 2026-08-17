import type {
  A1Range,
  EditTransaction,
  WorkerResponseEnvelope,
} from "./protocol";
import { MarksheetWorkerClient } from "../../bindings/wasm/web/client.js";

/** The UI depends only on this batched, revision-aware boundary. */
export interface WorkbenchAdapter {
  readonly currentRevision: number;
  readonly lastAcceptedSource: Uint8Array | undefined;
  open(source: Uint8Array): Promise<WorkerResponseEnvelope>;
  replaceSource(source: Uint8Array): Promise<WorkerResponseEnvelope>;
  snapshot(): Promise<WorkerResponseEnvelope>;
  visibleRegion(sheet: string, range: A1Range): Promise<WorkerResponseEnvelope>;
  calculate(sheet: string, range: A1Range): Promise<WorkerResponseEnvelope>;
  edit(transaction: EditTransaction): Promise<WorkerResponseEnvelope>;
  sourceBytes(): Promise<WorkerResponseEnvelope>;
  cancelAndRestart(): Promise<void>;
  dispose(): void;
}

/**
 * Creates the checked-in revision-aware binding client. The Worker itself is
 * created only after the UI is ready; generated Wasm remains a runtime asset.
 * A deployment must serve the compiled binding worker at `workerUrl`.
 */
export function createBindingAdapter(
  workerFactory: () => Worker = () => new Worker(
    resolveWorkerAssetUrl(import.meta.env.BASE_URL, window.location.href),
    { type: "module" },
  ),
): WorkbenchAdapter {
  return new MarksheetWorkerClient(workerFactory) as unknown as WorkbenchAdapter;
}

/** Resolves public worker assets under Vite's configured deployment base. */
export function resolveWorkerAssetUrl(
  baseUrl: string,
  pageUrl: string,
): URL {
  const normalizedBase = baseUrl.endsWith("/") ? baseUrl : `${baseUrl}/`;
  return new URL(`${normalizedBase}marksheet-wasm/web/worker.js`, pageUrl);
}

export class StaleResponseGate {
  #generation = 0;

  begin(): number {
    this.#generation += 1;
    return this.#generation;
  }

  isCurrent(generation: number): boolean {
    return generation === this.#generation;
  }

  invalidate(): void {
    this.#generation += 1;
  }
}

export function responsePayload<T extends WorkerResponseEnvelope["response"]["kind"]>(
  envelope: WorkerResponseEnvelope,
  kind: T,
): ResponsePayload<T> {
  if (envelope.response.kind !== kind) {
    throw new Error(`expected worker response ${kind}, received ${envelope.response.kind}`);
  }
  return envelope.response as ResponsePayload<T>;
}

type SnapshotResponse = Extract<
  WorkerResponseEnvelope["response"],
  { kind: "opened" | "replaced" | "snapshot" }
>;

type ResponsePayload<T extends WorkerResponseEnvelope["response"]["kind"]> =
  T extends "opened" | "replaced" | "snapshot"
    ? SnapshotResponse
    : Extract<WorkerResponseEnvelope["response"], { kind: T }>;
