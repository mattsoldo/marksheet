export type SaveResult =
  | { kind: "saved"; source: Uint8Array }
  | { kind: "unchanged"; source: Uint8Array }
  | {
      kind: "external_drift";
      currentSource: Uint8Array;
      rebase: { baseSource: Uint8Array; proposedSource: Uint8Array };
    };

export function saveWithExternalChangeGuard(input: {
  readCurrentBytes: () => Promise<Uint8Array>;
  writeBytes: (source: Uint8Array) => Promise<void>;
  expectedBase: Uint8Array;
  proposedSource: Uint8Array;
}): Promise<SaveResult>;
