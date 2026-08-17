import { bytesEqual } from "./protocol.js";

/**
 * Performs the exact-byte read/compare/write sequence required for a local
 * save. The browser adapter supplies a File System Access API reader/writer,
 * Electron bridge, or other user-authorized storage implementation.
 *
 * On external drift this function never calls `writeBytes`; instead it returns
 * the exact current bytes and base/proposed snapshots so the application can
 * hand them to Marksheet's source replacement/rebase flow.
 *
 * @param {{
 *   readCurrentBytes: () => Promise<Uint8Array>,
 *   writeBytes: (source: Uint8Array) => Promise<void>,
 *   expectedBase: Uint8Array,
 *   proposedSource: Uint8Array,
 * }} input
 */
export async function saveWithExternalChangeGuard(input) {
  const current = await input.readCurrentBytes();
  if (!bytesEqual(current, input.expectedBase)) {
    return {
      kind: "external_drift",
      currentSource: current,
      rebase: {
        baseSource: input.expectedBase.slice(),
        proposedSource: input.proposedSource.slice(),
      },
    };
  }
  if (bytesEqual(current, input.proposedSource)) {
    return { kind: "unchanged", source: current.slice() };
  }
  await input.writeBytes(input.proposedSource);
  return { kind: "saved", source: input.proposedSource.slice() };
}
