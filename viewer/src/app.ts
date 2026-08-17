import { formatCoordinate, parseRange } from "./a1";
import { ExternalFileChangeError, LocalFileSession, downloadSource } from "./local-file";
import type {
  A1Range,
  AuthoredValue,
  ByteSpan,
  Coordinate,
  Diagnostic,
  PresentedCell,
  ResolvedStyle,
  VisibleRegion,
  WorkbookSnapshot,
} from "./protocol";
import {
  applyResolvedStyle,
  buildViewportStyleMap,
  columnTrackCss,
  formatPresentedCell,
  presentedValueKind,
  rowHeightCss,
} from "./presentation";
import {
  applyStyleTransaction,
  defineStyleTransaction,
  setCellTransaction,
  setColumnWidthTransaction,
  setNameTargetTransaction,
  setRowHeightTransaction,
} from "./transactions";
import { computeViewport, viewportCellCount } from "./viewport";
import { formatSourceBytes, sourceSelectionOffsets } from "./source-view";
import type { WorkbenchAdapter } from "./worker-adapter";
import { StaleResponseGate, responsePayload } from "./worker-adapter";

interface FilePickerWindow extends Window {
  showOpenFilePicker?: (options?: unknown) => Promise<Array<{
    name: string;
    getFile(): Promise<File>;
    createWritable(): Promise<{
      write(data: Uint8Array): Promise<void>;
      close(): Promise<void>;
      abort(): Promise<void>;
    }>;
  }>>;
}

type DiagnosticScope = "document" | "viewport" | "calculation" | "error";
type DiagnosticOmissions = Partial<Record<DiagnosticScope, number>>;

export class ViewerApp {
  static readonly MAX_RENDERED_DIAGNOSTICS = 100;
  readonly #regionGate = new StaleResponseGate();
  #snapshot?: WorkbookSnapshot;
  #region?: VisibleRegion;
  #activeSheet: string | undefined;
  #selected: Coordinate = { column: 1, row: 1 };
  #fileName = "workbook.ms";
  #fileSession: LocalFileSession | undefined;
  #source: Uint8Array<ArrayBufferLike> = new Uint8Array();
  #disposed = false;
  #mutationBusy = false;

  constructor(
    private readonly root: HTMLElement,
    private readonly adapter: WorkbenchAdapter,
  ) {
    this.renderShell();
    this.bindEvents();
  }

  dispose(): void {
    this.#disposed = true;
    this.#regionGate.invalidate();
    this.adapter.dispose();
  }

  async openSource(source: Uint8Array, fileName = "workbook.ms", session?: LocalFileSession): Promise<void> {
    if (!this.beginMutation("Another open, save, or edit is already in progress")) {
      throw new Error("another open, save, or edit is already in progress");
    }
    this.setBusy(true, `Opening ${fileName}…`);
    this.#regionGate.invalidate();
    try {
      const envelope = await this.adapter.open(source);
      if (envelope.response.kind !== "opened" && envelope.response.kind !== "replaced") {
        throw new Error(`expected worker response opened or replaced, received ${envelope.response.kind}`);
      }
      const opened = envelope.response;
      this.#snapshot = opened.snapshot;
      this.#source = source.slice();
      this.#fileName = fileName;
      this.#fileSession = session;
      this.#activeSheet = opened.snapshot.sheets[0]?.id;
      this.#selected = { column: 1, row: 1 };
      this.updateWorkbookChrome();
      const refreshed = await this.refreshVisibleRegion();
      if (refreshed) {
        const snapshot = this.#snapshot ?? opened.snapshot;
        const extensionNotice = extensionOpenNotice(snapshot);
        if (!snapshot.editable) {
          this.setStatus(
            `Opened ${fileName} view-only; resolve formula diagnostics before editing`,
            "warning",
          );
        } else if (extensionNotice) {
          this.setStatus(`Opened ${fileName} ${extensionNotice}`, "warning");
        } else {
          this.setStatus(`Opened ${fileName} at revision ${snapshot.revision}`, "ok");
        }
      }
    } catch (error) {
      this.setError(error);
      throw error;
    } finally {
      this.setBusy(false);
      this.endMutation();
    }
  }

  private renderShell(): void {
    this.root.innerHTML = `
      <div class="app-shell">
        <header class="topbar">
          <div class="brand"><span class="brand-mark" aria-hidden="true">M</span> Marksheet</div>
          <span class="file-name" id="file-name">No workbook open</span>
          <div class="topbar-actions">
            <input class="sr-only" id="file-input" type="file" accept=".ms,text/plain" />
            <button id="open-file" type="button">Open local file</button>
            <button id="save-file" class="primary" type="button" disabled>Save</button>
            <button id="cancel-work" type="button" disabled>Cancel work</button>
          </div>
        </header>
        <div class="toolbar" aria-label="Editing controls">
          <button id="pan-up" type="button" title="Move viewport up">↑</button>
          <button id="pan-left" type="button" title="Move viewport left">←</button>
          <button id="pan-right" type="button" title="Move viewport right">→</button>
          <button id="pan-down" type="button" title="Move viewport down">↓</button>
          <label>Style <input id="style-id" size="9" placeholder="money" /></label>
          <label><input id="style-bold" type="checkbox" /> Bold</label>
          <label>Fill <input id="style-fill" type="color" value="#1f6feb" /></label>
          <label>Number <select id="style-number"><option value="">Unspecified</option><option>General</option><option>Integer</option><option>Decimal</option><option>Percent</option><option>Currency</option><option>Date</option><option>DateTime</option></select></label>
          <label>Currency <input id="style-currency" size="4" maxlength="3" value="USD" aria-label="ISO currency code" /></label>
          <button id="define-style" type="button">Define</button>
          <button id="apply-style" type="button">Apply</button>
          <label>Name <input id="name-id" size="9" placeholder="tax_rate" /></label>
          <button id="set-name" type="button">Set target</button>
          <label>Width <input id="column-width" type="number" min="1" step="0.5" size="5" /></label>
          <label>Height <input id="row-height" type="number" min="1" step="0.5" size="5" /></label>
          <span class="toolbar-spacer"></span>
          <button id="toggle-source" type="button" aria-pressed="true">Source</button>
        </div>
        <section class="workspace">
          <section class="sheet-workspace" aria-label="Workbook">
            <nav class="sheet-tabs" id="sheet-tabs" aria-label="Sheets"></nav>
            <div class="formula-strip">
              <label class="sr-only" for="name-box">Name box</label>
              <input class="cell-address" id="name-box" value="A1" aria-label="Coordinate, range, or declared name" />
              <label class="sr-only" for="formula-input">Formula or cell value</label>
              <input id="formula-input" placeholder="Select an authored cell to edit" disabled />
            </div>
            <div class="grid-shell" id="grid-shell">
              <div class="empty-state" id="grid-empty">Open a .ms file to inspect its sparse workbook.</div>
              <div class="grid" id="grid" role="grid" aria-label="Workbook cells" hidden></div>
            </div>
          </section>
          <aside class="inspector-panel" id="inspector">
            <section class="source-panel">
              <div class="panel-title" id="source-title">Exact source bytes</div>
              <textarea id="source-view" readonly spellcheck="false" aria-label="Exact workbook source"></textarea>
            </section>
            <section class="diagnostics" aria-live="polite">
              <div class="diagnostics-header"><span class="panel-title">Diagnostics</span><span id="diagnostic-count">0</span></div>
              <div id="diagnostic-list"><span class="file-name">No diagnostics.</span></div>
            </section>
          </aside>
        </section>
        <footer class="statusbar" aria-live="polite">
          <span id="status" class="status-ok">Ready</span>
          <span id="viewport-status">No viewport</span>
        </footer>
      </div>`;
  }

  private bindEvents(): void {
    this.byId("open-file").addEventListener("click", () => void this.pickFile());
    this.byId<HTMLInputElement>("file-input").addEventListener("change", (event) => {
      const file = (event.currentTarget as HTMLInputElement).files?.[0];
      if (file) void this.openBrowserFile(file);
    });
    this.byId("save-file").addEventListener("click", () => void this.save());
    this.byId("cancel-work").addEventListener("click", () => void this.cancelWork());
    this.byId("toggle-source").addEventListener("click", () => this.toggleSource());
    this.byId("pan-up").addEventListener("click", () => void this.pan(0, -20));
    this.byId("pan-down").addEventListener("click", () => void this.pan(0, 20));
    this.byId("pan-left").addEventListener("click", () => void this.pan(-8, 0));
    this.byId("pan-right").addEventListener("click", () => void this.pan(8, 0));
    this.byId("name-box").addEventListener("keydown", (event) => {
      if (event.key === "Enter") void this.navigate((event.currentTarget as HTMLInputElement).value);
    });
    this.byId("formula-input").addEventListener("keydown", (event) => {
      if (event.key === "Enter") void this.commitCell((event.currentTarget as HTMLInputElement).value);
    });
    this.byId("apply-style").addEventListener("click", () => {
      const style = this.byId<HTMLInputElement>("style-id").value.trim();
      if (style) void this.commitTransaction(applyStyleTransaction(this.requireSheet(), this.selectionRange(), style));
    });
    this.byId("define-style").addEventListener("click", () => {
      const style = this.byId<HTMLInputElement>("style-id").value.trim();
      const numberValue = this.byId<HTMLSelectElement>("style-number").value;
      const allowedNumbers = ["General", "Integer", "Decimal", "Percent", "Currency", "Date", "DateTime"] as const;
      const number = allowedNumbers.find((value) => value === numberValue);
      const currency = this.byId<HTMLInputElement>("style-currency").value.trim().toUpperCase();
      if (number === "Currency" && !/^[A-Z]{3}$/.test(currency)) {
        this.setStatus("Currency styles require a three-letter ISO code", "error");
        return;
      }
      if (style) {
        void this.commitTransaction(defineStyleTransaction(style, {
          bold: this.byId<HTMLInputElement>("style-bold").checked,
          fill: this.byId<HTMLInputElement>("style-fill").value,
          ...(number ? { number } : {}),
          ...(number === "Currency" ? { currency } : {}),
        }));
      }
    });
    this.byId("set-name").addEventListener("click", () => {
      const name = this.byId<HTMLInputElement>("name-id").value.trim();
      if (name) void this.commitTransaction(setNameTargetTransaction(name, this.requireSheet(), this.selectionRange()));
    });
    this.byId("column-width").addEventListener("change", (event) => {
      const width = Number((event.currentTarget as HTMLInputElement).value);
      if (Number.isFinite(width)) {
        void this.commitTransaction(setColumnWidthTransaction(this.requireSheet(), this.#selected.column, width));
      }
    });
    this.byId("row-height").addEventListener("change", (event) => {
      const height = Number((event.currentTarget as HTMLInputElement).value);
      if (Number.isFinite(height)) {
        void this.commitTransaction(setRowHeightTransaction(this.requireSheet(), this.#selected.row, height));
      }
    });
  }

  private async pickFile(): Promise<void> {
    try {
      const picker = (window as FilePickerWindow).showOpenFilePicker;
      if (!picker) {
        this.byId<HTMLInputElement>("file-input").click();
        return;
      }
      const handles = await picker({
        multiple: false,
        types: [{ description: "Marksheet workbook", accept: { "text/plain": [".ms"] } }],
      });
      const handle = handles[0];
      if (!handle) return;
      const file = await handle.getFile();
      const bytes = new Uint8Array(await file.arrayBuffer());
      await this.openSource(bytes, handle.name, new LocalFileSession(handle, bytes));
    } catch (error) {
      if ((error as DOMException).name !== "AbortError") this.setError(error);
    }
  }

  private async openBrowserFile(file: File): Promise<void> {
    try {
      await this.openSource(new Uint8Array(await file.arrayBuffer()), file.name);
    } catch {
      // `openSource` already presents the structured worker error.
    }
  }

  private async save(): Promise<void> {
    if (!this.beginMutation("Saving is already in progress")) return;
    this.setBusy(true, "Checking local file…");
    try {
      if (this.#fileSession) {
        const result = await this.#fileSession.save(this.adapter);
        this.#source = this.#fileSession.baseSource;
        this.updateSourceView();
        this.setStatus(result === "saved" ? "Saved focused source patches" : "No changes to save", "ok");
      } else {
        const payload = responsePayload(await this.adapter.sourceBytes(), "source_bytes");
        const source = Uint8Array.from(payload.source);
        downloadSource(source, this.#fileName);
        this.#source = source;
        this.setStatus(`Downloaded ${this.#fileName}`, "ok");
      }
    } catch (error) {
      if (error instanceof ExternalFileChangeError) {
        this.#source = error.externalSource.slice();
        this.updateSourceView();
        if (error.workerReplaced) await this.afterSourceReplacement();
        else this.renderDiagnostics(errorDiagnostics(error.reparseError), {
          error: diagnosticsOmitted(error.reparseError),
        });
      }
      this.setError(error);
    } finally {
      this.setBusy(false);
      this.endMutation();
    }
  }

  private async afterSourceReplacement(): Promise<void> {
    const snapshot = responsePayload(await this.adapter.snapshot(), "snapshot").snapshot;
    this.#snapshot = snapshot;
    this.#activeSheet = snapshot.sheets.some((sheet) => sheet.id === this.#activeSheet)
      ? this.#activeSheet
      : snapshot.sheets[0]?.id;
    this.updateWorkbookChrome();
    await this.refreshVisibleRegion();
  }

  private async cancelWork(): Promise<void> {
    this.#regionGate.invalidate();
    this.setBusy(true, "Restarting worker…");
    try {
      await this.adapter.cancelAndRestart();
      if (this.#snapshot) await this.afterSourceReplacement();
      this.setStatus("Cancelled outstanding work and reopened the last accepted source", "warning");
    } catch (error) {
      this.setError(error);
    } finally {
      this.setBusy(false);
    }
  }

  private async refreshVisibleRegion(): Promise<boolean> {
    const sheet = this.#activeSheet;
    if (!sheet) return false;
    const generation = this.#regionGate.begin();
    const range = computeViewport({
      anchor: this.#selected,
      visibleRows: 30,
      visibleColumns: 12,
      overscan: 3,
    });
    this.setBusy(true, `Loading ${sheet}…`);
    try {
      const [envelope, calculation] = await Promise.all([
        this.adapter.visibleRegion(sheet, range),
        this.adapter.calculate(sheet, range).then(
          (response) => ({ response } as const),
          (error: unknown) => ({ error } as const),
        ),
      ]);
      if (!this.#regionGate.isCurrent(generation) || this.#disposed) return false;
      const refreshedSnapshot = responsePayload(await this.adapter.snapshot(), "snapshot").snapshot;
      if (!this.#regionGate.isCurrent(generation) || this.#disposed) return false;
      this.#snapshot = refreshedSnapshot;
      this.updateWorkbookChrome();
      let mergedRegion = responsePayload(envelope, "visible_region").region;
      const upstreamDiagnosticsOmitted: DiagnosticOmissions = {
        document: diagnosticsOmitted(refreshedSnapshot),
        viewport: diagnosticsOmitted(envelope.response),
      };
      const calculationError = "error" in calculation ? calculation.error : undefined;
      if ("response" in calculation) {
        const calculated = responsePayload(calculation.response, "calculation").calculation;
        upstreamDiagnosticsOmitted.calculation = diagnosticsOmitted(calculation.response.response);
        const byCoordinate = new Map(
          calculated.cells
            .filter((entry) => entry.cell.sheet === sheet)
            .map((entry) => [coordinateKey(entry.cell.coordinate), entry.value]),
        );
        mergedRegion = {
          ...mergedRegion,
          cells: mergedRegion.cells.map((cell) => ({
            ...cell,
            ...(byCoordinate.has(coordinateKey(cell.coordinate))
              ? { calculated: byCoordinate.get(coordinateKey(cell.coordinate))! }
              : {}),
          })),
          diagnostics: [...mergedRegion.diagnostics, ...calculated.diagnostics],
        };
      }
      this.#region = mergedRegion;
      this.renderGrid();
      this.renderDiagnostics(
        [
          ...(this.#snapshot?.diagnostics ?? []),
          ...mergedRegion.diagnostics,
          ...errorDiagnostics(calculationError),
        ],
        calculationError
          ? {
              ...upstreamDiagnosticsOmitted,
              calculation: diagnosticsOmitted(calculationError),
            }
          : upstreamDiagnosticsOmitted,
      );
      const incompleteMessage = incompleteViewMessage(this.#snapshot, mergedRegion);
      if (incompleteMessage) {
        this.setStatus(incompleteMessage, "error");
        return false;
      }
      if (calculationError) {
        const message = calculationError instanceof Error ? calculationError.message : String(calculationError);
        this.setStatus(`Calculation failed: ${message}`, "error");
        return false;
      }
      this.setStatus(`Ready at revision ${this.adapter.currentRevision}`, "ok");
      return true;
    } catch (error) {
      if (this.#regionGate.isCurrent(generation)) this.setError(error);
      return false;
    } finally {
      if (this.#regionGate.isCurrent(generation)) this.setBusy(false);
    }
  }

  private async commitCell(source: string): Promise<void> {
    const cell = this.selectedCell();
    if (cell && "VirtualFill" in cell.source) {
      this.setStatus("Fill-derived cells are virtual and cannot be directly edited", "error");
      return;
    }
    await this.commitTransaction(setCellTransaction(this.requireSheet(), this.#selected, source));
  }

  private async commitTransaction(transaction: ReturnType<typeof setCellTransaction>): Promise<void> {
    if (!this.#snapshot?.editable) {
      this.setStatus("This workbook is view-only until its formula diagnostics are resolved", "error");
      return;
    }
    if (!this.beginMutation("Another save or edit is already in progress")) return;
    this.setBusy(true, "Planning source-aware edit…");
    try {
      const edited = responsePayload(await this.adapter.edit(transaction), "edited");
      this.#snapshot = edited.snapshot;
      if (edited.changed) {
        const source = responsePayload(await this.adapter.sourceBytes(), "source_bytes");
        this.#source = Uint8Array.from(source.source);
        this.updateSourceView();
      }
      const refreshed = await this.refreshVisibleRegion();
      if (!refreshed) return;
      const patchSummary = edited.patches
        .map((patch) => `${patch.span.start}..${patch.span.end}`)
        .join(", ");
      this.setStatus(
        edited.changed
          ? `Committed ${edited.patches.length} focused patch${edited.patches.length === 1 ? "" : "es"}: ${patchSummary}`
          : "Edit was a semantic no-op",
        "ok",
      );
    } catch (error) {
      this.setError(error);
    } finally {
      this.setBusy(false);
      this.endMutation();
    }
  }

  private async navigate(target: string): Promise<void> {
    const parsed = parseRange(target);
    if (parsed) {
      this.#selected = parsed.start;
      await this.refreshVisibleRegion();
      return;
    }

    const matches = this.#snapshot?.names.filter((name) => name.id === target.trim()) ?? [];
    if (matches.length !== 1) {
      this.setStatus(matches.length > 1 ? `Ambiguous declared name: ${target}` : `Invalid coordinate, range, or declared name: ${target}`, "error");
      return;
    }
    const name = matches[0];
    if (!name) return;
    const resolved = name.resolved ?? nameTargetRange(name.target);
    if (!resolved) {
      this.setStatus(`Declared name “${name.id}” targets a table column whose resolved range is unavailable`, "error");
      return;
    }
    if (!this.#snapshot?.sheets.some((sheet) => sheet.id === resolved.sheet)) {
      this.setStatus(`Declared name “${name.id}” resolves to an unavailable sheet`, "error");
      return;
    }
    this.#activeSheet = resolved.sheet;
    this.#selected = resolved.range.start;
    this.updateWorkbookChrome();
    await this.refreshVisibleRegion();
  }

  private async pan(columns: number, rows: number): Promise<void> {
    this.#selected = {
      column: Math.max(1, this.#selected.column + columns),
      row: Math.max(1, this.#selected.row + rows),
    };
    this.byId<HTMLInputElement>("name-box").value = formatCoordinate(this.#selected);
    await this.refreshVisibleRegion();
  }

  private updateWorkbookChrome(): void {
    this.byId("file-name").textContent = this.#fileName;
    this.updateEditingState();
    const tabs = this.byId("sheet-tabs");
    tabs.replaceChildren();
    for (const sheet of this.#snapshot?.sheets ?? []) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "sheet-tab";
      button.textContent = sheet.label;
      button.title = `${sheet.id} · ${sheet.authored_cell_count} authored cells`;
      button.setAttribute("aria-selected", String(sheet.id === this.#activeSheet));
      button.addEventListener("click", () => {
        this.#activeSheet = sheet.id;
        this.#selected = { column: 1, row: 1 };
        this.updateWorkbookChrome();
        void this.refreshVisibleRegion();
      });
      tabs.append(button);
    }
    this.updateSourceView();
  }

  private updateEditingState(): void {
    const editable = this.#snapshot?.editable === true && !this.#mutationBusy;
    this.byId<HTMLButtonElement>("save-file").disabled = !this.#snapshot || this.#mutationBusy;
    this.byId<HTMLButtonElement>("open-file").disabled = this.#mutationBusy;
    this.byId<HTMLInputElement>("file-input").disabled = this.#mutationBusy;
    for (const id of ["define-style", "apply-style", "set-name", "column-width", "row-height", "style-id", "style-bold", "style-fill", "style-number", "style-currency", "name-id"]) {
      this.byId<HTMLInputElement | HTMLButtonElement | HTMLSelectElement>(id).disabled = !editable;
    }
    const formula = this.byId<HTMLInputElement>("formula-input");
    const selected = this.selectedCell();
    formula.disabled = !editable || Boolean(selected && "VirtualFill" in selected.source);
  }

  private renderGrid(): void {
    const region = this.#region;
    if (!region) return;
    const grid = this.byId("grid");
    const empty = this.byId("grid-empty");
    grid.hidden = false;
    empty.hidden = true;
    grid.replaceChildren();

    const columns = inclusiveNumbers(region.range.start.column, region.range.end.column);
    const rows = inclusiveNumbers(region.range.start.row, region.range.end.row);
    const columnGeometry = new Map(region.columns.map((column) => [column.column, column.geometry]));
    const rowGeometry = new Map(region.rows.map((row) => [row.row, row.geometry]));
    grid.style.gridTemplateColumns = `46px ${columns
      .map((column) => columnTrackCss(columnGeometry.get(column)?.size))
      .join(" ")}`;
    grid.setAttribute("aria-rowcount", String(rows.length + 1));
    grid.setAttribute("aria-colcount", String(columns.length + 1));

    const headerRow = this.gridElement("div", "grid-row", "", undefined, "row");
    headerRow.setAttribute("aria-rowindex", "1");
    const corner = this.gridElement("div", "corner", "", undefined, "columnheader");
    corner.setAttribute("aria-colindex", "1");
    headerRow.append(corner);
    for (const [columnIndex, column] of columns.entries()) {
      const header = this.gridElement("div", "column-header", columnName(column), undefined, "columnheader");
      header.setAttribute("aria-colindex", String(columnIndex + 2));
      headerRow.append(header);
    }
    grid.append(headerRow);

    const sparse = new Map(region.cells.map((cell) => [coordinateKey(cell.coordinate), cell]));
    const blankStyles = buildViewportStyleMap(region.style_regions, region.range);
    for (const [rowIndex, row] of rows.entries()) {
      const height = rowHeightCss(rowGeometry.get(row)?.size);
      const gridRow = this.gridElement("div", "grid-row", "", undefined, "row");
      gridRow.setAttribute("aria-rowindex", String(rowIndex + 2));
      const header = this.gridElement("div", "row-header", String(row), undefined, "rowheader");
      header.style.height = height;
      header.setAttribute("aria-colindex", "1");
      gridRow.append(header);
      for (const [columnIndex, column] of columns.entries()) {
        const coordinate = { column, row };
        const cell = sparse.get(coordinateKey(coordinate));
        const element = this.gridElement(
          "button",
          "grid-cell",
          formatPresentedCell(cell, this.#snapshot?.locale ?? "en-US"),
          coordinate,
          "gridcell",
        );
        element.style.height = height;
        element.setAttribute("aria-colindex", String(columnIndex + 2));
        this.decorateCell(element, cell, cell?.style ?? blankStyles.get(coordinateKey(coordinate)));
        gridRow.append(element);
      }
      grid.append(gridRow);
    }
    this.updateSelectionChrome();
    this.byId("viewport-status").textContent = `${region.sheet.label} · ${formatCoordinate(region.range.start)}:${formatCoordinate(region.range.end)} · ${region.cells.length} sparse / ${viewportCellCount(region.range)} rendered`;
  }

  private gridElement(
    tag: "button" | "div",
    className: string,
    text: string,
    coordinate?: Coordinate,
    role?: string,
  ): HTMLElement {
    const element = document.createElement(tag);
    if (tag === "button") (element as HTMLButtonElement).type = "button";
    element.className = className;
    element.textContent = text;
    if (role) element.setAttribute("role", role);
    if (coordinate) {
      element.dataset.coordinate = coordinateKey(coordinate);
      element.addEventListener("click", () => {
        this.#selected = coordinate;
        this.updateSelectionChrome();
      });
      element.addEventListener("dblclick", () => this.byId<HTMLInputElement>("formula-input").focus());
      element.addEventListener("keydown", (event) => {
        const keyboard = event as KeyboardEvent;
        if (["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(keyboard.key)) {
          keyboard.preventDefault();
          void this.moveGridSelection(coordinate, keyboard.key);
        }
      });
    }
    return element;
  }

  private decorateCell(element: HTMLElement, cell?: PresentedCell, style?: ResolvedStyle): void {
    if (!cell) {
      if (style) {
        element.classList.add("cell-styled-blank");
        element.title = `Un-authored blank\nStyle layers: ${style.layers.map((layer) => layer.id).join(", ") || "resolved region"}`;
        element.setAttribute("aria-label", "Un-authored blank with resolved style");
        applyResolvedStyle(element, style.properties, "blank");
      }
      return;
    }
    const isVirtual = "VirtualFill" in cell.source;
    const authored = "Authored" in cell.source ? cell.source.Authored.value : undefined;
    element.classList.add(isVirtual ? "cell-virtual" : "cell-authored");
    if (isVirtual || authored?.kind === "formula") element.classList.add("cell-formula");
    if (cell.calculated?.kind === "error" || authored?.kind === "error") element.classList.add("cell-error");
    if (authored?.kind === "blank") element.classList.add("cell-blank");
    if (authored?.kind === "text" && authored.value === "") element.classList.add("cell-empty-text");
    element.title = cellTitle(cell);
    element.setAttribute("aria-label", `${formatCoordinate(cell.coordinate)}: ${semanticCellLabel(cell)}`);
    element.dataset.layer = isVirtual ? "virtual" : "authored";
    applyResolvedStyle(element, cell.style.properties, presentedValueKind(cell));
  }

  private async moveGridSelection(origin: Coordinate, key: string): Promise<void> {
    const delta = key === "ArrowUp"
      ? { column: 0, row: -1 }
      : key === "ArrowDown"
        ? { column: 0, row: 1 }
        : key === "ArrowLeft"
          ? { column: -1, row: 0 }
          : { column: 1, row: 0 };
    const next = {
      column: Math.max(1, origin.column + delta.column),
      row: Math.max(1, origin.row + delta.row),
    };
    if (next.column === origin.column && next.row === origin.row) return;
    this.#selected = next;
    const range = this.#region?.range;
    const inViewport = range
      && next.column >= range.start.column
      && next.column <= range.end.column
      && next.row >= range.start.row
      && next.row <= range.end.row;
    if (inViewport) this.updateSelectionChrome();
    else await this.refreshVisibleRegion();
    this.root.querySelector<HTMLElement>(`.grid-cell[data-coordinate="${coordinateKey(next)}"]`)?.focus();
  }

  private updateSelectionChrome(): void {
    const key = coordinateKey(this.#selected);
    for (const element of this.root.querySelectorAll<HTMLElement>(".grid-cell")) {
      element.classList.toggle("cell-selected", element.dataset.coordinate === key);
      element.tabIndex = element.dataset.coordinate === key ? 0 : -1;
    }
    this.byId<HTMLInputElement>("name-box").value = formatCoordinate(this.#selected);
    const selected = this.selectedCell();
    const formula = this.byId<HTMLInputElement>("formula-input");
    formula.value = sourceText(selected);
    formula.disabled = !this.#snapshot?.editable || this.#mutationBusy || Boolean(selected && "VirtualFill" in selected.source);
    formula.placeholder = !this.#snapshot?.editable
      ? "View-only workbook"
      : formula.disabled
        ? "Virtual fill cell: edit the @fill source instead"
        : "Enter a value or formula";
    const span = cellSpan(selected);
    if (span) this.selectSourceSpan(span, false);
  }

  private renderDiagnostics(diagnostics: Diagnostic[], omissions: DiagnosticOmissions = {}): void {
    const unique = dedupeDiagnostics(diagnostics);
    const rendered = unique.slice(0, ViewerApp.MAX_RENDERED_DIAGNOSTICS);
    const truncatedScopes = diagnosticOmissionEntries(omissions);
    this.byId("diagnostic-count").textContent = truncatedScopes.length > 0
      ? `${rendered.length} rendered · ${truncatedScopes.map(([scope, count]) => `${scope} +${count}`).join(" · ")}`
      : unique.length > rendered.length
        ? `${rendered.length} of ${unique.length}`
        : String(unique.length);
    const list = this.byId("diagnostic-list");
    list.replaceChildren();
    if (rendered.length === 0 && truncatedScopes.length === 0) {
      const empty = document.createElement("span");
      empty.className = "file-name";
      empty.textContent = "No diagnostics in this viewport.";
      list.append(empty);
      return;
    }
    for (const diagnostic of rendered) {
      const row = document.createElement("div");
      const severity = diagnostic.severity ?? "info";
      row.className = `diagnostic ${severity}`;
      const level = document.createElement("span");
      level.className = "diagnostic-level";
      level.textContent = severity;
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = diagnostic.message ?? diagnostic.code ?? "Workbook diagnostic";
      const span = diagnosticSpan(diagnostic);
      button.disabled = !span;
      if (span) button.addEventListener("click", () => this.selectSourceSpan(span, true));
      row.append(level, button);
      list.append(row);
    }
    if (unique.length > rendered.length) {
      const overflow = document.createElement("div");
      overflow.className = "diagnostic diagnostic-overflow";
      overflow.textContent = `${unique.length - rendered.length} additional diagnostics are not rendered.`;
      list.append(overflow);
    }
    for (const [scope, count] of truncatedScopes) {
      const upstreamOverflow = document.createElement("div");
      upstreamOverflow.className = "diagnostic diagnostic-overflow";
      upstreamOverflow.textContent = `${count} additional ${scope} diagnostics were omitted by the worker resource cap.`;
      list.append(upstreamOverflow);
    }
  }

  private selectSourceSpan(span: ByteSpan, focus: boolean): void {
    const source = this.byId<HTMLTextAreaElement>("source-view");
    const offsets = sourceSelectionOffsets(
      this.#source,
      span,
      source.dataset.encoding === "hex" ? "hex" : "utf8",
    );
    source.setSelectionRange(offsets.start, offsets.end);
    if (focus) source.focus();
  }

  private updateSourceView(): void {
    const formatted = formatSourceBytes(this.#source);
    const view = this.byId<HTMLTextAreaElement>("source-view");
    view.value = formatted.text;
    view.dataset.encoding = formatted.encoding;
    this.byId("source-title").textContent = formatted.encoding === "utf8"
      ? "Exact source bytes"
      : "Exact source bytes (hex; invalid UTF-8)";
  }

  private toggleSource(): void {
    const inspector = this.byId("inspector");
    const button = this.byId("toggle-source");
    const hidden = inspector.hidden;
    inspector.hidden = !hidden;
    button.setAttribute("aria-pressed", String(hidden));
  }

  private selectionRange(): A1Range {
    const text = this.byId<HTMLInputElement>("name-box").value;
    return parseRange(text) ?? { start: this.#selected, end: this.#selected };
  }

  private selectedCell(): PresentedCell | undefined {
    return this.#region?.cells.find((cell) => coordinateKey(cell.coordinate) === coordinateKey(this.#selected));
  }

  private requireSheet(): string {
    if (!this.#activeSheet) throw new Error("Open a workbook before editing");
    return this.#activeSheet;
  }

  private setBusy(busy: boolean, message?: string): void {
    this.byId<HTMLButtonElement>("cancel-work").disabled = !busy;
    if (message) this.setStatus(message, "warning");
  }

  private beginMutation(message: string): boolean {
    if (this.#mutationBusy) {
      this.setStatus(message, "warning");
      return false;
    }
    this.#mutationBusy = true;
    this.updateEditingState();
    return true;
  }

  private endMutation(): void {
    this.#mutationBusy = false;
    this.updateEditingState();
  }

  private setStatus(message: string, kind: "ok" | "warning" | "error"): void {
    const status = this.byId("status");
    status.textContent = message;
    status.className = `status-${kind}`;
  }

  private setError(error: unknown): void {
    const diagnostics = errorDiagnostics(error);
    const omitted = diagnosticsOmitted(error);
    if (diagnostics.length > 0 || omitted > 0) this.renderDiagnostics(diagnostics, { error: omitted });
    this.setStatus(error instanceof Error ? error.message : String(error), "error");
  }

  private byId<T extends HTMLElement = HTMLElement>(id: string): T {
    const element = this.root.querySelector<T>(`#${id}`);
    if (!element) throw new Error(`viewer shell is missing #${id}`);
    return element;
  }
}

function coordinateKey(coordinate: Coordinate): string {
  return `${coordinate.column}:${coordinate.row}`;
}

function nameTargetRange(target: import("./protocol").NameTarget): { sheet: string; range: A1Range } | undefined {
  if ("Cell" in target) {
    return { sheet: target.Cell.sheet, range: { start: target.Cell.coordinate, end: target.Cell.coordinate } };
  }
  if ("Range" in target) return { sheet: target.Range.sheet, range: target.Range.range };
  return undefined;
}

function columnName(column: number): string {
  return formatCoordinate({ column, row: 1 }).replace(/1$/, "");
}

function inclusiveNumbers(start: number, end: number): number[] {
  const length = end - start + 1;
  return Array.from({ length }, (_, index) => start + index);
}

function sourceText(cell?: PresentedCell): string {
  if (!cell) return "";
  if ("VirtualFill" in cell.source) return cell.source.VirtualFill.formula;
  return authoredText(cell.source.Authored.value);
}

function authoredText(value: AuthoredValue): string {
  if (value.kind === "blank") return "";
  if (value.kind === "text" && value.value === "") return "'";
  if (value.kind === "boolean") return value.value ? "true" : "false";
  return String(value.value);
}

function cellTitle(cell: PresentedCell): string {
  const source = sourceText(cell);
  const calculated = cell.calculated ? formatPresentedCell(cell, "en-US") : "";
  const layer = "VirtualFill" in cell.source ? "Virtual fill" : "Authored";
  const styles = cell.style.layers.map((style) => style.id).join(", ") || "none";
  return `${semanticCellLabel(cell)}\n${layer} source: ${source || "(empty)"}\nCalculated: ${calculated || "(blank)"}\nStyle layers: ${styles}`;
}

function semanticCellLabel(cell: PresentedCell): string {
  if ("VirtualFill" in cell.source) return `Virtual formula ${cell.source.VirtualFill.formula}`;
  const value = cell.source.Authored.value;
  if (value.kind === "blank") return "Authored blank";
  if (value.kind === "text" && value.value === "") return "Authored empty text";
  return `Authored ${value.kind} ${authoredText(value)}`;
}

function cellSpan(cell?: PresentedCell): ByteSpan | undefined {
  if (!cell) return undefined;
  return ("VirtualFill" in cell.source
    ? cell.source.VirtualFill.fill_source_span
    : cell.source.Authored.source_span) ?? undefined;
}

function diagnosticSpan(diagnostic: Diagnostic): ByteSpan | undefined {
  return isSpan(diagnostic.primary.span) ? diagnostic.primary.span : undefined;
}

function isSpan(value: unknown): value is ByteSpan {
  return Boolean(
    value
      && typeof value === "object"
      && "start" in value
      && "end" in value
      && Number.isSafeInteger((value as ByteSpan).start)
      && Number.isSafeInteger((value as ByteSpan).end),
  );
}

function dedupeDiagnostics(diagnostics: Diagnostic[]): Diagnostic[] {
  const unique = new Map<string, Diagnostic>();
  for (const diagnostic of diagnostics) {
    const span = diagnosticSpan(diagnostic);
    const key = JSON.stringify([
      diagnostic.severity ?? "",
      diagnostic.code ?? "",
      diagnostic.message ?? "",
      span?.start ?? null,
      span?.end ?? null,
    ]);
    if (!unique.has(key)) unique.set(key, diagnostic);
  }
  return [...unique.values()];
}

function errorDiagnostics(error: unknown): Diagnostic[] {
  if (!error || typeof error !== "object" || !("diagnostics" in error)) return [];
  const diagnostics = error.diagnostics;
  return Array.isArray(diagnostics)
    ? diagnostics.filter((entry): entry is Diagnostic => Boolean(entry && typeof entry === "object"))
    : [];
}

/** Reads additive wire metadata without weakening the generated protocol types. */
function diagnosticsOmitted(value: unknown): number {
  if (!value || typeof value !== "object" || !("diagnostics_omitted" in value)) return 0;
  const omitted = value.diagnostics_omitted;
  return Number.isSafeInteger(omitted) && Number(omitted) > 0 ? Number(omitted) : 0;
}

function diagnosticOmissionEntries(omissions: DiagnosticOmissions): Array<[DiagnosticScope, number]> {
  return (Object.entries(omissions) as Array<[DiagnosticScope, number | undefined]>)
    .filter((entry): entry is [DiagnosticScope, number] => Number.isSafeInteger(entry[1]) && (entry[1] ?? 0) > 0);
}

function incompleteViewMessage(snapshot: WorkbookSnapshot, region: VisibleRegion): string | undefined {
  const support = snapshot.extension_support;
  const calculationComplete = support.calculation_complete && region.completeness.calculation_complete;
  const renderingComplete = support.rendering_complete && region.completeness.rendering_complete;
  if (calculationComplete && renderingComplete) return undefined;

  const required = snapshot.extension_declarations
    .filter((declaration) => declaration.availability === "unavailable_required")
    .map((declaration) => declaration.capability);
  const capability = required.length > 0 ? ` (${required.join(", ")})` : "";
  const unavailable = [
    !calculationComplete ? "calculated values" : undefined,
    !renderingComplete ? "complete rendering" : undefined,
  ].filter((value): value is string => Boolean(value));
  const reason = required.length > 0
    ? `required extension support is unavailable${capability}`
    : "workbook capabilities are incomplete";
  return `Incomplete workbook view: ${reason}; the viewer cannot provide ${unavailable.join(" or ")}.`;
}

function extensionOpenNotice(snapshot: WorkbookSnapshot): string | undefined {
  if (!snapshot.extension_support.valid) {
    return "with extension validation failures; the workbook remains editable for repair";
  }
  const optionalUnavailable = snapshot.extension_declarations.some(
    (declaration) => declaration.availability === "unavailable_optional",
  );
  const undeclaredInstance = snapshot.extension_instances.some(
    (instance) => instance.outcome === "skipped_undeclared",
  );
  const skippedUnavailable = snapshot.extension_instances.some(
    (instance) => instance.outcome === "skipped_unavailable",
  );
  const warnings = [
    optionalUnavailable ? "optional capability unavailable" : undefined,
    undeclaredInstance ? "undeclared instance skipped" : undefined,
    skippedUnavailable && !optionalUnavailable ? "unavailable instance skipped" : undefined,
  ].filter((value): value is string => Boolean(value));
  return warnings.length > 0
    ? `with extension warnings (${warnings.join("; ")}); calculation and rendering remain complete`
    : undefined;
}
