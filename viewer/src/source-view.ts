import type { ByteSpan } from "./protocol";

export type SourceViewEncoding = "utf8" | "hex";

export interface FormattedSourceView {
  text: string;
  encoding: SourceViewEncoding;
}

/** Uses exact UTF-8 text when valid and a reversible byte view otherwise. */
export function formatSourceBytes(source: Uint8Array): FormattedSourceView {
  try {
    return { text: new TextDecoder("utf-8", { fatal: true }).decode(source), encoding: "utf8" };
  } catch {
    return {
      text: [...source].map((byte) => byte.toString(16).padStart(2, "0")).join(" "),
      encoding: "hex",
    };
  }
}

export function sourceSelectionOffsets(
  source: Uint8Array,
  span: ByteSpan,
  encoding: SourceViewEncoding,
): { start: number; end: number } {
  const startByte = Math.min(span.start, source.length);
  const endByte = Math.min(span.end, source.length);
  if (encoding === "hex") {
    return {
      start: startByte * 3,
      end: Math.max(startByte * 3, endByte * 3 - (endByte > startByte ? 1 : 0)),
    };
  }
  const decoder = new TextDecoder();
  return {
    start: decoder.decode(source.slice(0, startByte)).length,
    end: decoder.decode(source.slice(0, endByte)).length,
  };
}
