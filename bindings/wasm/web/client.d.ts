import type { A1Range, Diagnostic, EditTransaction, WorkerResponseEnvelope } from "../protocol.d.ts";

export class WorkerCancelledError extends Error {}
export class WorkerProtocolError extends Error {
  code: string;
  diagnostics: Diagnostic[];
  diagnostics_omitted: number;
}

export interface WorkerLike {
  postMessage(message: unknown): void;
  terminate(): void;
  onmessage: ((event: MessageEvent) => void) | null;
  onerror: ((event: ErrorEvent) => void) | null;
}

export class MarksheetWorkerClient {
  constructor(workerFactory: () => WorkerLike);
  readonly currentRevision: number;
  readonly lastAcceptedSource: Uint8Array | undefined;
  setResponseListener(listener: (response: WorkerResponseEnvelope) => void): void;
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
