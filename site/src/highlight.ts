/**
 * A presentation-only highlighter for Marksheet source. It never decides
 * meaning: the Wasm engine owns parsing, and an unusual line simply renders
 * as plain text.
 */

type Body = { kind: "rows"; delimiter: "|" | ","; header: boolean; quoted: boolean } | { kind: "payload" };

const BODY_DIRECTIVES = new Set(["block", "table", "extension"]);

export function escapeHtml(text: string): string {
  return text.replace(/[&<>"]/g, (character) => (
    { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" } as Record<string, string>
  )[character] ?? character);
}

/** Returns one HTML string per physical source line. */
export function highlightLines(source: string): string[] {
  let body: Body | undefined;
  return source.split("\n").map((raw) => {
    const line = raw.endsWith("\r") ? raw.slice(0, -1) : raw;
    if (body) {
      // A quoted field may span lines, and `@end` inside one is data (SPEC §9).
      if (line === "@end" && !(body.kind === "rows" && body.quoted)) {
        body = undefined;
        return span("directive", line);
      }
      if (body.kind === "payload") return span("payload", line);
      const header = body.header;
      body.header = false;
      const row = highlightRow(line, body.delimiter, header, body.quoted);
      body.quoted = row.quoted;
      return row.html;
    }
    if (line.startsWith("#!")) return span("shebang", line);
    if (/^\s*#/.test(line)) return span("comment", line);
    const directive = /^@([a-z][a-z0-9-]*)/.exec(line);
    if (!directive?.[1]) return escapeHtml(line);
    const name = directive[1];
    if (BODY_DIRECTIVES.has(name)) {
      body = name === "extension"
        ? { kind: "payload" }
        : { kind: "rows", delimiter: /\bcsv\s*$/.test(line) ? "," : "|", header: name === "table", quoted: false };
    }
    return span("directive", `@${name}`) + highlightArguments(line.slice(name.length + 1));
  });
}

function highlightArguments(rest: string): string {
  let html = "";
  let index = 0;
  while (index < rest.length) {
    const tail = rest.slice(index);
    const string = /^"(?:[^"\\]|\\.)*"?/.exec(tail);
    const formula = /^=\s*\S.*$/.exec(tail);
    const attribute = /^([a-z][a-z0-9-]*)(=)/.exec(tail);
    const number = /^-?\d+(?:\.\d+)?\b/.exec(tail);
    const word = /^[^\s="]+/.exec(tail);
    if (string) {
      html += span("string", string[0]);
      index += string[0].length;
    } else if (formula && /(^|\s)$/.test(rest.slice(0, index))) {
      html += span("operator", "=") + span("formula", formula[0].slice(1));
      index += formula[0].length;
    } else if (attribute?.[1]) {
      html += span("attribute", attribute[1]) + span("operator", "=");
      index += attribute[0].length;
    } else if (number) {
      html += span("number", number[0]);
      index += number[0].length;
    } else if (word) {
      html += escapeHtml(word[0]);
      index += word[0].length;
    } else {
      html += escapeHtml(tail[0] ?? "");
      index += 1;
    }
  }
  return html;
}

function highlightRow(
  line: string,
  delimiter: "|" | ",",
  header: boolean,
  continued: boolean,
): { html: string; quoted: boolean } {
  const { fields, quoted } = splitFields(line, delimiter, continued);
  const html = fields
    .map((field, position) => {
      if (position === 0 && continued) return span("string", field);
      const separator = position === 0 ? "" : span("delimiter", delimiter);
      if (header) return separator + span("header", field);
      if (field.startsWith("=")) return separator + span("formula", field);
      if (/^-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?$/.test(field)) return separator + span("number", field);
      if (field.startsWith('"')) return separator + span("string", field);
      return separator + escapeHtml(field);
    })
    .join("");
  return { html, quoted };
}

/**
 * Splits on delimiters outside RFC 4180 quotes, keeping each field's spelling.
 * `quoted` carries a field that is still open from the previous line.
 */
function splitFields(line: string, delimiter: string, quoted: boolean): { fields: string[]; quoted: boolean } {
  const fields: string[] = [];
  let current = "";
  for (const character of line) {
    if (character === '"') quoted = !quoted;
    if (character === delimiter && !quoted) {
      fields.push(current);
      current = "";
    } else {
      current += character;
    }
  }
  fields.push(current);
  return { fields, quoted };
}

function span(kind: string, text: string): string {
  return text ? `<span class="tok-${kind}">${escapeHtml(text)}</span>` : "";
}
