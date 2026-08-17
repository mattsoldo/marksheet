import { describe, expect, it } from "vitest";
import { formatSourceBytes, sourceSelectionOffsets } from "../src/source-view";

describe("exact source view", () => {
  it("uses a reversible hex representation for invalid UTF-8", () => {
    const source = Uint8Array.of(0x40, 0xff, 0x0a);
    expect(formatSourceBytes(source)).toEqual({ text: "40 ff 0a", encoding: "hex" });
    expect(sourceSelectionOffsets(source, { start: 1, end: 2 }, "hex")).toEqual({ start: 3, end: 5 });
  });

  it("keeps byte spans aligned in valid multibyte UTF-8", () => {
    const source = new TextEncoder().encode("éx");
    const formatted = formatSourceBytes(source);
    expect(formatted).toEqual({ text: "éx", encoding: "utf8" });
    expect(sourceSelectionOffsets(source, { start: 0, end: 2 }, "utf8")).toEqual({ start: 0, end: 1 });
  });
});
