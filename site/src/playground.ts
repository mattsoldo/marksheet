import { MarksheetWorkerClient, WorkerProtocolError } from "../../bindings/wasm/web/client.js";
import { columnLabel, formatCoordinate } from "../../viewer/src/a1";
import {
  applyResolvedStyle,
  buildViewportStyleMap,
  columnTrackCss,
  formatPresentedCell,
  presentedValueKind,
  rowHeightCss,
} from "../../viewer/src/presentation";
import type {
  A1Range,
  ByteSpan,
  Diagnostic,
  PresentedCell,
  VisibleRegion,
  WorkbookSnapshot,
  WorkerResponseEnvelope,
} from "../../viewer/src/protocol";
import { resolveWorkerAssetUrl } from "../../viewer/src/worker-adapter";
import { escapeHtml, highlightLines } from "./highlight";

/** The playground shows a reading view, never an unbounded sheet. */
const MAX_COLUMNS = 26;
const MAX_ROWS = 120;
const EDIT_DELAY_MS = 160;

interface Client {
  open(source: Uint8Array): Promise<WorkerResponseEnvelope>;
  replaceSource(source: Uint8Array): Promise<WorkerResponseEnvelope>;
  visibleRegion(sheet: string, range: A1Range): Promise<WorkerResponseEnvelope>;
  calculate(sheet: string, range: A1Range): Promise<WorkerResponseEnvelope>;
  dispose(): void;
}

interface Rendered {
  bytes: Uint8Array;
  snapshot: WorkbookSnapshot;
  region: VisibleRegion | undefined;
}

export class Playground {
  readonly #root: HTMLElement;
  readonly #editor: HTMLTextAreaElement;
  readonly #highlight: HTMLElement;
  readonly #tabs: HTMLElement;
  readonly #grid: HTMLElement;
  readonly #status: HTMLElement;
  readonly #cellRef: HTMLElement;
  readonly #cellSource: HTMLElement;
  readonly #diagnostics: HTMLElement;
  readonly #encoder = new TextEncoder();
  readonly #decoder = new TextDecoder();
  #client: Client | undefined;
  #queue: Promise<void> = Promise.resolve();
  #generation = 0;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #activeSheet: string | undefined;
  #rendered: Rendered | undefined;
  #cells = new Map<string, PresentedCell>();
  #selected: string | undefined;
  #markedLines: number[] = [];
  #errorLines = new Set<number>();

  constructor(root: HTMLElement) {
    this.#root = root;
    this.#editor = this.#part<HTMLTextAreaElement>("[data-editor]");
    this.#highlight = this.#part("[data-highlight]");
    this.#tabs = this.#part("[data-tabs]");
    this.#grid = this.#part("[data-grid]");
    this.#status = this.#part("[data-status]");
    this.#cellRef = this.#part("[data-cell-ref]");
    this.#cellSource = this.#part("[data-cell-source]");
    this.#diagnostics = this.#part("[data-diagnostics]");

    this.#editor.addEventListener("input", () => {
      this.#paint();
      this.#schedule();
    });
    this.#editor.addEventListener("scroll", () => this.#syncScroll());
    for (const event of ["click", "keyup", "select"] as const) {
      this.#editor.addEventListener(event, () => this.#followCaret());
    }
    this.#tabs.addEventListener("click", (event) => {
      const tab = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-sheet]");
      if (!tab?.dataset.sheet || tab.dataset.sheet === this.#activeSheet) return;
      this.#activeSheet = tab.dataset.sheet;
      this.#selected = undefined;
      this.#schedule(0);
    });
    this.#tabs.addEventListener("keydown", (event) => this.#moveTab(event));
    this.#grid.addEventListener("click", (event) => {
      const cell = (event.target as HTMLElement).closest<HTMLElement>("[data-coordinate]");
      if (cell?.dataset.coordinate) this.#select(cell.dataset.coordinate, true);
    });
  }

  /** Replaces the editor text, as when a visitor picks an example. */
  load(source: string): void {
    this.#editor.value = source;
    this.#editor.scrollTop = 0;
    this.#activeSheet = undefined;
    this.#selected = undefined;
    this.#paint();
    this.#schedule(0);
  }

  /** Ends the worker; the next update opens the editor's source in a fresh one. */
  #resetClient(): void {
    this.#client?.dispose();
    this.#client = undefined;
    this.#rendered = undefined;
  }

  #part<T extends HTMLElement = HTMLElement>(selector: string): T {
    const element = this.#root.querySelector<T>(selector);
    if (!element) throw new Error(`playground is missing ${selector}`);
    return element;
  }

  #schedule(delay = EDIT_DELAY_MS): void {
    clearTimeout(this.#timer);
    const generation = ++this.#generation;
    this.#timer = setTimeout(() => {
      // Requests are serialized: each one is sent against the revision the
      // previous one established, and only the newest edit is rendered.
      this.#queue = this.#queue.then(() => this.#update(generation));
    }, delay);
  }

  async #update(generation: number): Promise<void> {
    if (generation !== this.#generation) return;
    const started = performance.now();
    const bytes = this.#encoder.encode(this.#editor.value);
    this.#setStatus("busy", "Calculating…");
    try {
      const client = this.#ensureClient();
      const opened = await (this.#rendered ? client.replaceSource(bytes) : client.open(bytes));
      const response = opened.response;
      if (response.kind !== "opened" && response.kind !== "replaced") throw new Error(`unexpected ${response.kind}`);
      const snapshot = response.snapshot;
      const sheet = snapshot.sheets.find((candidate) => candidate.id === this.#activeSheet) ?? snapshot.sheets[0];
      this.#activeSheet = sheet?.id;
      const reading = sheet ? await this.#readingRegion(client, sheet.id) : undefined;
      const region = reading?.region;
      if (generation !== this.#generation) return;
      this.#rendered = { bytes, snapshot, region };
      this.#renderTabs(snapshot);
      this.#renderGrid(region, reading?.calculated ?? false);
      const diagnostics = uniqueDiagnostics([...snapshot.diagnostics, ...(region?.diagnostics ?? [])]);
      // Each response caps its own list, and their scopes overlap (a cycle is reported by the
      // workbook and by its calculation), so the largest single report is a safe lower bound.
      const truncated = snapshot.diagnostics_omitted > 0 || Boolean(reading?.truncated);
      const total = Math.max(
        diagnostics.length,
        snapshot.diagnostics.length + snapshot.diagnostics_omitted,
        ...(reading?.totals ?? []),
      );
      this.#renderDiagnostics(bytes, diagnostics, total - diagnostics.length, undefined, truncated);
      const elapsed = Math.max(1, Math.round(performance.now() - started));
      const counted = `${truncated ? "at least " : ""}${total} ${total === 1 ? "diagnostic" : "diagnostics"}`;
      if (reading && !reading.calculated) {
        this.#setStatus("warn", `Not calculated · showing authored values · ${counted}`);
      } else {
        this.#setStatus(
          total ? "warn" : "ok",
          total ? `Calculated with ${counted} · ${elapsed} ms` : `Parsed and calculated locally in ${elapsed} ms`,
        );
      }
    } catch (error) {
      // A rejected source leaves the worker usable; anything else (a crashed or
      // cancelled worker) leaves the client without one, so start over next time.
      if (!isDocumentError(error)) this.#resetClient();
      if (generation !== this.#generation) return;
      if (isDocumentError(error)) {
        // The last good grid stays on screen; the source explains what broke.
        this.#renderDiagnostics(bytes, error.diagnostics, error.diagnostics_omitted, error.message);
        this.#setStatus("error", "The source has an error · showing the last good result");
      } else {
        this.#setStatus("error", `The engine could not run: ${error instanceof Error ? error.message : String(error)}`);
      }
    }
  }

  #ensureClient(): Client {
    this.#client ??= new MarksheetWorkerClient(() => new Worker(
      resolveWorkerAssetUrl(import.meta.env.BASE_URL, window.location.href),
      { type: "module" },
    )) as unknown as Client;
    return this.#client;
  }

  /** Fits the request to the sheet's content, plus a little breathing room. */
  async #readingRegion(
    client: Client,
    sheet: string,
  ): Promise<{ region: VisibleRegion; totals: number[]; truncated: boolean; calculated: boolean }> {
    const probe = await this.#region(client, sheet, { start: { column: 1, row: 1 }, end: { column: 1, row: 1 } });
    const extent = probe.region.sheet.extent;
    const end = {
      column: Math.min(MAX_COLUMNS, Math.max(4, (extent?.end.column ?? 0) + 1)),
      row: Math.min(MAX_ROWS, Math.max(6, (extent?.end.row ?? 0) + 2)),
    };
    const range = { start: { column: 1, row: 1 }, end };
    const [{ region, omitted }, calculation] = await Promise.all([
      this.#region(client, sheet, range),
      // A workbook can be viewable but not calculable (a required extension is
      // unavailable): keep the projection and show authored values instead.
      client.calculate(sheet, range).catch((error: unknown) => {
        if (isDocumentError(error)) return error;
        throw error;
      }),
    ]);
    if (calculation instanceof WorkerProtocolError) {
      return {
        region: { ...region, diagnostics: [...region.diagnostics, ...calculation.diagnostics] },
        totals: [
          region.diagnostics.length + omitted,
          calculation.diagnostics.length + calculation.diagnostics_omitted,
        ],
        truncated: omitted > 0 || calculation.diagnostics_omitted > 0,
        calculated: false,
      };
    }
    if (calculation.response.kind !== "calculation") throw new Error(`unexpected ${calculation.response.kind}`);
    // Projection and calculation are separate requests; join them by coordinate.
    const values = new Map(calculation.response.calculation.cells
      .filter((entry) => entry.cell.sheet === sheet)
      .map((entry) => [formatCoordinate(entry.cell.coordinate), entry.value]));
    return {
      region: {
        ...region,
        cells: region.cells.map((cell) => {
          const value = values.get(formatCoordinate(cell.coordinate));
          return value ? { ...cell, calculated: value } : cell;
        }),
        diagnostics: [...region.diagnostics, ...calculation.response.calculation.diagnostics],
      },
      totals: [
        region.diagnostics.length + omitted,
        calculation.response.calculation.diagnostics.length + calculation.response.diagnostics_omitted,
      ],
      truncated: omitted > 0 || calculation.response.diagnostics_omitted > 0,
      calculated: true,
    };
  }


  async #region(client: Client, sheet: string, range: A1Range): Promise<{ region: VisibleRegion; omitted: number }> {
    const envelope = await client.visibleRegion(sheet, range);
    if (envelope.response.kind !== "visible_region") throw new Error(`unexpected ${envelope.response.kind}`);
    return { region: envelope.response.region, omitted: envelope.response.diagnostics_omitted };
  }

  #renderTabs(snapshot: WorkbookSnapshot): void {
    const hadFocus = this.#tabs.contains(document.activeElement);
    this.#tabs.replaceChildren(...snapshot.sheets.map((sheet) => {
      const tab = document.createElement("button");
      const active = sheet.id === this.#activeSheet;
      tab.type = "button";
      tab.className = "sheet-tab";
      tab.dataset.sheet = sheet.id;
      tab.textContent = sheet.label || sheet.id;
      tab.setAttribute("role", "tab");
      tab.setAttribute("aria-selected", String(active));
      tab.setAttribute("aria-controls", this.#grid.id);
      tab.tabIndex = active ? 0 : -1;
      return tab;
    }));
    // Rebuilding the buttons would otherwise drop keyboard focus to the page.
    if (hadFocus) this.#tabs.querySelector<HTMLElement>('[aria-selected="true"]')?.focus();
  }

  #moveTab(event: KeyboardEvent): void {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    const tabs = [...this.#tabs.querySelectorAll<HTMLButtonElement>("[data-sheet]")];
    const index = tabs.findIndex((tab) => tab.dataset.sheet === this.#activeSheet);
    const next = tabs[(index + (event.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length];
    if (!next) return;
    event.preventDefault();
    next.focus();
    next.click();
  }

  #renderGrid(region: VisibleRegion | undefined, calculated: boolean): void {
    this.#cells.clear();
    if (!region) {
      this.#grid.replaceChildren(emptyMessage("This workbook has no sheets yet. Add a line such as @sheet main."));
      this.#select(undefined);
      return;
    }
    const { start, end } = region.range;
    const locale = this.#rendered?.snapshot.locale ?? "en-US";
    const columns = new Map(region.columns.map((column) => [column.column, column.geometry.size]));
    const rows = new Map(region.rows.map((row) => [row.row, row.geometry.size]));
    const blankStyles = buildViewportStyleMap(region.style_regions, region.range);
    for (const cell of region.cells) this.#cells.set(formatCoordinate(cell.coordinate), cell);

    const table = document.createElement("table");
    table.className = "sheet";
    const caption = document.createElement("caption");
    caption.className = "sr-only";
    caption.textContent = `${region.sheet.label || region.sheet.id}, ${calculated ? "calculated" : "authored values, not calculated"}`;
    const colgroup = document.createElement("colgroup");
    colgroup.append(Object.assign(document.createElement("col"), { className: "row-number" }));
    const head = document.createElement("tr");
    head.append(headerCell("", "corner"));
    for (let column = start.column; column <= end.column; column += 1) {
      const col = document.createElement("col");
      col.style.width = columnTrackCss(columns.get(column));
      colgroup.append(col);
      head.append(headerCell(columnLabel(column), "column-header"));
    }
    const thead = document.createElement("thead");
    thead.append(head);
    const tbody = document.createElement("tbody");
    for (let row = start.row; row <= end.row; row += 1) {
      const tr = document.createElement("tr");
      tr.style.height = rowHeightCss(rows.get(row));
      const label = headerCell(String(row), "row-header");
      label.scope = "row";
      tr.append(label);
      for (let column = start.column; column <= end.column; column += 1) {
        const coordinate = formatCoordinate({ column, row });
        const cell = this.#cells.get(coordinate);
        const td = document.createElement("td");
        td.dataset.coordinate = coordinate;
        const content = document.createElement("span");
        content.className = "cell";
        content.textContent = formatPresentedCell(cell, locale);
        td.append(content);
        const style = cell?.style ?? blankStyles.get(`${column}:${row}`);
        if (style) applyResolvedStyle(content, style.properties, cell ? presentedValueKind(cell) : "blank");
        // Fills belong to the whole table cell, not just its text.
        if (content.style.backgroundColor) {
          td.style.backgroundColor = content.style.backgroundColor;
          content.style.backgroundColor = "";
        }
        if (cell) {
          const formula = "VirtualFill" in cell.source
            || ("Authored" in cell.source && cell.source.Authored.value.kind === "formula");
          td.classList.toggle("is-formula", formula);
          td.classList.toggle("is-error", cell.calculated?.kind === "error");
        }
        tr.append(td);
      }
      tbody.append(tr);
    }
    table.append(caption, colgroup, thead, tbody);

    const extent = region.sheet.extent;
    const clipped = extent && (extent.end.column > end.column || extent.end.row > end.row);
    this.#grid.replaceChildren(table);
    if (clipped) {
      this.#grid.append(emptyMessage(
        `Showing ${formatCoordinate(start)}:${formatCoordinate(end)}. Open the viewer to scroll the whole sheet.`,
      ));
    }
    const keep = this.#selected && this.#grid.querySelector(`[data-coordinate="${this.#selected}"]`);
    this.#select(keep ? this.#selected : firstFormula(region) ?? "A1");
  }

  #select(coordinate: string | undefined, fromGrid = false): void {
    this.#selected = coordinate;
    for (const element of this.#grid.querySelectorAll(".is-selected")) element.classList.remove("is-selected");
    if (!coordinate) {
      this.#cellRef.textContent = "";
      this.#cellSource.textContent = "";
      delete this.#cellSource.dataset.note;
      this.#markLines([]);
      return;
    }
    this.#grid.querySelector(`[data-coordinate="${coordinate}"]`)?.classList.add("is-selected");
    const cell = this.#cells.get(coordinate);
    this.#cellRef.textContent = coordinate;
    this.#cellSource.textContent = cellSourceText(cell);
    if (cell && "VirtualFill" in cell.source) this.#cellSource.dataset.note = "filled down the column";
    else delete this.#cellSource.dataset.note;
    const span = cell ? sourceSpan(cell) : undefined;
    // While an edit is rejected, the grid is from older source whose lines may have moved.
    const current = this.#rendered && this.#editor.value === this.#decoder.decode(this.#rendered.bytes);
    const lines = span && current && this.#rendered ? this.#linesForSpan(this.#rendered.bytes, span) : [];
    this.#markLines(lines);
    if (fromGrid && lines[0] !== undefined) this.#revealLine(lines[0]);
  }

  /** Selecting source text selects the cell it authored, when it is on screen. */
  #followCaret(): void {
    if (!this.#rendered || this.#editor.value !== this.#decoder.decode(this.#rendered.bytes)) return;
    const caret = this.#encoder.encode(this.#editor.value.slice(0, this.#editor.selectionStart)).length;
    let best: { coordinate: string; size: number } | undefined;
    for (const [coordinate, cell] of this.#cells) {
      const span = sourceSpan(cell);
      if (!span || caret < span.start || caret > span.end) continue;
      const size = span.end - span.start;
      if (!best || size < best.size) best = { coordinate, size };
    }
    if (best && best.coordinate !== this.#selected) this.#select(best.coordinate);
  }

  #linesForSpan(bytes: Uint8Array, span: ByteSpan): number[] {
    const first = lineOf(this.#decoder.decode(bytes.subarray(0, span.start)));
    const last = lineOf(this.#decoder.decode(bytes.subarray(0, Math.max(span.start, span.end - 1))));
    return Array.from({ length: last - first + 1 }, (_, index) => first + index);
  }

  #markLines(lines: number[]): void {
    this.#markedLines = lines;
    this.#paintMarks();
  }

  #revealLine(line: number): void {
    const element = this.#highlight.children[line] as HTMLElement | undefined;
    if (!element) return;
    const top = element.offsetTop;
    const view = this.#editor;
    if (top < view.scrollTop + 24 || top > view.scrollTop + view.clientHeight - 48) {
      view.scrollTo({ top: Math.max(0, top - view.clientHeight / 3), behavior: "smooth" });
    }
  }

  #renderDiagnostics(
    bytes: Uint8Array,
    diagnostics: Diagnostic[],
    omitted: number,
    fallback?: string,
    truncated = omitted > 0,
  ): void {
    this.#errorLines = new Set(diagnostics.map((diagnostic) => (
      lineOf(this.#decoder.decode(bytes.subarray(0, diagnostic.primary.span.start)))
    )));
    this.#paintMarks();
    if (!diagnostics.length && !fallback) {
      this.#diagnostics.hidden = true;
      this.#diagnostics.replaceChildren();
      return;
    }
    const items = diagnostics.slice(0, 4).map((diagnostic) => {
      const item = document.createElement("li");
      item.className = `diagnostic diagnostic-${diagnostic.severity}`;
      const line = lineOf(this.#decoder.decode(bytes.subarray(0, diagnostic.primary.span.start))) + 1;
      item.innerHTML = `<span class="diagnostic-code">${escapeHtml(diagnostic.code)}</span>`
        + `<span class="diagnostic-line">line ${line}</span>`
        + `<span class="diagnostic-message">${escapeHtml(diagnostic.message)}</span>`;
      return item;
    });
    if (!items.length && fallback) {
      const item = document.createElement("li");
      item.className = "diagnostic diagnostic-error";
      item.textContent = fallback;
      items.push(item);
    }
    const more = diagnostics.length - items.length + omitted;
    if (more > 0) {
      const item = document.createElement("li");
      item.className = "diagnostic diagnostic-more";
      item.textContent = `and ${truncated ? "at least " : ""}${more} more`;
      items.push(item);
    }
    this.#diagnostics.hidden = false;
    this.#diagnostics.replaceChildren(...items);
  }

  #paint(): void {
    this.#highlight.innerHTML = highlightLines(this.#editor.value)
      .map((html) => `<span class="line">${html || " "}</span>`)
      .join("");
    this.#paintMarks();
    this.#syncScroll();
  }

  #paintMarks(): void {
    const lines = this.#highlight.children;
    for (const line of lines) line.classList.remove("is-marked", "has-error");
    for (const index of this.#markedLines) lines[index]?.classList.add("is-marked");
    for (const index of this.#errorLines) lines[index]?.classList.add("has-error");
  }

  #syncScroll(): void {
    this.#highlight.style.transform = `translate(${-this.#editor.scrollLeft}px, ${-this.#editor.scrollTop}px)`;
  }

  #setStatus(state: "busy" | "ok" | "warn" | "error", message: string): void {
    this.#status.dataset.state = state;
    this.#status.textContent = message;
  }

  /** Paints the initial source before the engine loads. */
  start(): void {
    this.#paint();
    this.#schedule(0);
  }
}

/** Rejections of this document; the worker that reported them is still healthy and in sync. */
const DOCUMENT_ERRORS = new Set(["invalid_source", "limit", "calculation", "edit"]);

/**
 * True when the worker rejected the document but can keep serving it. Anything
 * else (a session that failed to load, a protocol or revision mismatch, or a
 * code this page doesn't know) means the worker must be replaced.
 */
function isDocumentError(error: unknown): error is WorkerProtocolError {
  return error instanceof WorkerProtocolError && DOCUMENT_ERRORS.has(error.code);
}

function headerCell(text: string, className: string): HTMLTableCellElement {
  const cell = document.createElement("th");
  cell.className = className;
  cell.textContent = text;
  if (className === "column-header") cell.scope = "col";
  if (className === "corner") {
    const label = document.createElement("span");
    label.className = "sr-only";
    label.textContent = "Row";
    cell.append(label);
  }
  return cell;
}

function emptyMessage(text: string): HTMLElement {
  const message = document.createElement("p");
  message.className = "grid-note";
  message.textContent = text;
  return message;
}

function sourceSpan(cell: PresentedCell): ByteSpan | undefined {
  if ("Authored" in cell.source) return cell.source.Authored.source_span ?? undefined;
  return cell.source.VirtualFill.fill_source_span ?? undefined;
}

function cellSourceText(cell: PresentedCell | undefined): string {
  if (!cell) return "blank";
  if ("VirtualFill" in cell.source) return cell.source.VirtualFill.formula;
  const value = cell.source.Authored.value;
  switch (value.kind) {
    case "blank":
      return "blank";
    case "formula":
      return value.value.startsWith("=") ? value.value : `=${value.value}`;
    case "text":
      return JSON.stringify(value.value);
    default:
      return String(value.value);
  }
}

function firstFormula(region: VisibleRegion): string | undefined {
  const formula = region.cells.find((cell) => (
    "Authored" in cell.source && cell.source.Authored.value.kind === "formula"
  )) ?? region.cells.find((cell) => "VirtualFill" in cell.source);
  return formula ? formatCoordinate(formula.coordinate) : undefined;
}

/** The snapshot and the region can report the same source problem. */
function uniqueDiagnostics(diagnostics: Diagnostic[]): Diagnostic[] {
  const seen = new Set<string>();
  return diagnostics.filter((diagnostic) => {
    const key = `${diagnostic.code}:${diagnostic.primary.span.start}:${diagnostic.primary.span.end}:${diagnostic.message}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function lineOf(prefix: string): number {
  let count = 0;
  for (const character of prefix) if (character === "\n") count += 1;
  return count;
}
