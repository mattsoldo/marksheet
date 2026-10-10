import { formatCoordinate, parseRange } from "./a1";
import { ExternalFileChangeError, LocalFileSession, downloadSource } from "./local-file";
import type {
  A1Range,
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
  authoredCellText,
  defineStyleTransaction,
  setCellTransaction,
  setColumnWidthTransaction,
  setNameTargetTransaction,
  setRowHeightTransaction,
} from "./transactions";
import {
  clipRange,
  computeViewport,
  readingLimit,
  viewportCellCount,
  viewportForContainer,
} from "./viewport";
import { formatSourceBytes, sourceSelectionOffsets } from "./source-view";
import {
  THEMES,
  browserStorage,
  loadPreferences,
  prefersDarkScheme,
  savePreferences,
  type ThemeId,
  type ViewerPreferences,
} from "./preferences";
import {
  createRecentStore,
  ensureHandlePermission,
  formatRelativeTime,
  type RecentFileHandle,
  type RecentWorkbook,
  type RecentWorkbookStore,
} from "./recent";
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

interface DroppedItem extends DataTransferItem {
  getAsFileSystemHandle?: () => Promise<{ kind: string } | null>;
}

export interface ViewerOptions {
  /** Remembered workbooks; defaults to IndexedDB with an in-memory fallback. */
  recentStore?: RecentWorkbookStore;
  /** Theme, sidebar, and details preferences; defaults to `localStorage`. */
  storage?: Storage;
  /** Asks before unsaved edits are replaced by another workbook; defaults to `window.confirm`. */
  confirmDiscard?: (message: string) => boolean;
}

type DiagnosticScope = "document" | "viewport" | "calculation" | "error";

/** After a render, either scroll the anchor to the top-left or keep what the user was looking at. */
type Reveal = "anchor" | "preserve";

interface RefreshOptions {
  reveal?: Reveal;
  /** Scroll-driven loads keep the current status message. */
  quiet?: boolean;
  /** Issued by a scroll shift itself; every other refresh makes shifts stand down. */
  fromShift?: boolean;
}

interface ScrollReference {
  row: number;
  top: number;
  column: number;
  left: number;
}
type DiagnosticOmissions = Partial<Record<DiagnosticScope, number>>;

export class ViewerApp {
  static readonly MAX_RENDERED_DIAGNOSTICS = 100;
  readonly #regionGate = new StaleResponseGate();
  #snapshot?: WorkbookSnapshot;
  #region?: VisibleRegion;
  #activeSheet: string | undefined;
  #selected: Coordinate = { column: 1, row: 1 };
  /** The top-left cell the rendered window is built around; independent of the selection. */
  #anchor: Coordinate = { column: 1, row: 1 };
  /** Last known content extent per sheet; `null` is an empty sheet, absence is unknown. */
  #extents = new Map<string, A1Range | null>();
  /** Where the last render should reveal: `#anchor`, clamped to the reading-view fit. */
  #revealTarget: Coordinate = { column: 1, row: 1 };
  /** Pending refreshes not issued by a scroll shift (open, navigate, edit, Details); shifts wait for these. */
  #pendingRefreshes = 0;
  /** Whether the rendered grid was clipped to the reading-view fit. */
  #renderedFitted = false;
  #scrollFrame = 0;
  #shifting = false;
  #rescanAfterShift = false;
  readonly #onResize = () => this.scheduleWindowCheck();
  /** Watches the grid area itself: the sidebar and Details change its size without a window resize. */
  #resizeObserver: ResizeObserver | undefined;
  #fileName = "workbook.ms";
  #fileSession: LocalFileSession | undefined;
  #source: Uint8Array<ArrayBufferLike> = new Uint8Array();
  #disposed = false;
  #mutationBusy = false;
  #dirty = false;
  #recentStore: RecentWorkbookStore;
  #recent: RecentWorkbook[] = [];
  #currentRecentId: string | undefined;
  #rememberGeneration = 0;
  /** Recent-store operations run one at a time, in request order. */
  #recentQueue: Promise<unknown> = Promise.resolve();
  #storage: Storage | undefined;
  #confirmDiscard: (message: string) => boolean;
  #preferences: ViewerPreferences;
  readonly #keydown = (event: KeyboardEvent) => this.handleShortcut(event);

  constructor(
    private readonly root: HTMLElement,
    private readonly adapter: WorkbenchAdapter,
    options: ViewerOptions = {},
  ) {
    this.#recentStore = options.recentStore ?? createRecentStore();
    this.#storage = options.storage ?? browserStorage();
    this.#confirmDiscard = options.confirmDiscard ?? defaultConfirm;
    this.#preferences = loadPreferences(this.#storage, prefersDarkScheme());
    this.renderShell();
    this.bindEvents();
    this.applyPreferences(isNarrowViewport() ? { sidebarOpen: false } : {});
    void this.refreshRecent();
  }

  /** Workbooks remembered on this device, most recent first. */
  get recentWorkbooks(): readonly RecentWorkbook[] {
    return this.#recent;
  }

  dispose(): void {
    this.#disposed = true;
    document.removeEventListener("keydown", this.#keydown);
    this.#resizeObserver?.disconnect();
    window.removeEventListener("resize", this.#onResize);
    if (this.#scrollFrame && typeof cancelAnimationFrame === "function") cancelAnimationFrame(this.#scrollFrame);
    this.#regionGate.invalidate();
    this.adapter.dispose();
  }

  async openSource(
    source: Uint8Array,
    fileName = "workbook.ms",
    session?: LocalFileSession,
    recentId?: string,
  ): Promise<void> {
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
      this.#anchor = { column: 1, row: 1 };
      this.#extents.clear();
      this.#dirty = false;
      this.updateWorkbookChrome();
      // Until the store answers, a download save must not update another entry's copy.
      this.#currentRecentId = recentId;
      // Remembering is a convenience and must never delay or fail an open.
      void this.rememberWorkbook(source, fileName, opened.snapshot.sheets.length, session, recentId);
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
    const themeOptions = THEMES.map((theme) => `
      <button class="theme-option" type="button" role="radio" data-theme-option="${theme.id}" aria-checked="false" title="${theme.description}">
        <span class="theme-swatch theme-swatch-${theme.id}" aria-hidden="true"><span></span><span></span><span></span></span>
        <span>${theme.label}</span>
      </button>`).join("");
    this.root.innerHTML = `
      <div class="app-shell" id="app-shell" data-sidebar="open" data-details="closed">
        <aside class="sidebar" id="sidebar" aria-label="Workbooks">
          <div class="sidebar-header">
            <div class="brand"><span class="brand-mark" aria-hidden="true">${ICONS.mark}</span><span>Marksheet</span></div>
            <button id="close-sidebar" class="icon-button sidebar-close" type="button" aria-label="Close workbook list">${ICONS.close}</button>
          </div>
          <input class="sr-only" id="file-input" type="file" accept=".ms,text/plain" tabindex="-1" aria-label="Marksheet workbook file" />
          <button id="open-file" class="sidebar-open" type="button">${ICONS.plus}<span>Open workbook</span><kbd>${MOD_KEY}O</kbd></button>
          <section class="sidebar-section" aria-labelledby="recent-heading">
            <div class="sidebar-label">
              <span id="recent-heading">Recent</span>
              <button id="clear-recent" class="link-button" type="button" hidden>Clear</button>
            </div>
            <ul class="recent-list" id="recent-list"></ul>
            <p class="recent-empty" id="recent-empty">Workbooks you open are remembered here, on this device only.</p>
          </section>
          <section class="sidebar-footer" aria-labelledby="theme-heading">
            <div class="sidebar-label"><span id="theme-heading">Appearance</span></div>
            <div class="theme-picker" role="radiogroup" aria-labelledby="theme-heading">${themeOptions}</div>
          </section>
        </aside>
        <div class="sidebar-scrim" id="sidebar-scrim" aria-hidden="true"></div>
        <div class="main-column">
          <header class="topbar">
            <button id="toggle-sidebar" class="icon-button" type="button" aria-controls="sidebar" aria-expanded="true" title="Workbooks (${MOD_KEY}\\)">${ICONS.sidebar}<span class="sr-only">Toggle workbook list</span></button>
            <div class="title-group">
              <h1 class="file-name" id="file-name">No workbook open</h1>
              <span class="file-badge" id="file-badge" hidden></span>
            </div>
            <div class="topbar-actions">
              <button id="cancel-work" class="ghost" type="button" disabled>Cancel</button>
              <button id="toggle-details" class="ghost details-toggle" type="button" aria-pressed="false" aria-controls="details-bar inspector" title="Formula bar, formatting, source, and diagnostics (${MOD_KEY}/)">${ICONS.details}<span>Details</span><span class="count-badge" id="details-badge" hidden></span></button>
              <button id="save-file" class="primary" type="button" disabled>Save</button>
            </div>
          </header>
          <nav class="sheet-tabs" id="sheet-tabs" aria-label="Sheets" hidden></nav>
          <div class="details-bar" id="details-bar" hidden>
            <div class="formula-strip">
              <label class="sr-only" for="name-box">Name box</label>
              <input class="cell-address" id="name-box" value="A1" aria-label="Coordinate, range, or declared name" spellcheck="false" />
              <span class="formula-glyph" aria-hidden="true">fx</span>
              <label class="sr-only" for="formula-input">Formula or cell value</label>
              <input id="formula-input" placeholder="Select an authored cell to edit" disabled spellcheck="false" />
            </div>
            <div class="toolbar" aria-label="Editing controls">
              <div class="tool-group" aria-label="Style">
                <label class="field"><span>Style</span><input id="style-id" size="9" placeholder="money" /></label>
                <label class="check"><input id="style-bold" type="checkbox" /> <span>Bold</span></label>
                <label class="field color"><span>Fill</span><input id="style-fill" type="color" value="#1f6feb" /></label>
                <label class="field"><span>Number</span><select id="style-number"><option value="">Unspecified</option><option>General</option><option>Integer</option><option>Decimal</option><option>Percent</option><option>Currency</option><option>Date</option><option>DateTime</option></select></label>
                <label class="field"><span>Currency</span><input id="style-currency" size="4" maxlength="3" value="USD" aria-label="ISO currency code" /></label>
                <button id="define-style" type="button">Define</button>
                <button id="apply-style" type="button">Apply</button>
              </div>
              <div class="tool-group" aria-label="Name">
                <label class="field"><span>Name</span><input id="name-id" size="9" placeholder="tax_rate" /></label>
                <button id="set-name" type="button">Set target</button>
              </div>
              <div class="tool-group" aria-label="Geometry">
                <label class="field"><span>Width</span><input id="column-width" type="number" min="1" step="0.5" size="5" /></label>
                <label class="field"><span>Height</span><input id="row-height" type="number" min="1" step="0.5" size="5" /></label>
              </div>
            </div>
          </div>
          <section class="workspace">
            <section class="sheet-workspace" aria-label="Workbook">
              <div class="progress" aria-hidden="true"></div>
              <div class="grid-shell" id="grid-shell">
                <div class="empty-state" id="grid-empty">
                  <div class="empty-card">
                    <div class="empty-art" aria-hidden="true">${ICONS.emptyGrid}</div>
                    <h2>Open a Marksheet workbook</h2>
                    <p>Drop a <code>.ms</code> file here or choose one from this device. Workbooks are read and calculated locally.</p>
                    <button id="open-file-empty" class="primary" type="button">Choose a file</button>
                  </div>
                </div>
                <div class="grid" id="grid" role="grid" aria-label="Workbook cells" tabindex="-1" hidden></div>
              </div>
            </section>
            <aside class="inspector-panel" id="inspector" aria-label="Source and diagnostics" hidden>
              <section class="source-panel">
                <div class="panel-title" id="source-title">Exact source bytes</div>
                <textarea id="source-view" readonly spellcheck="false" aria-label="Exact workbook source"></textarea>
              </section>
              <section class="diagnostics" aria-live="polite">
                <div class="diagnostics-header"><span class="panel-title">Diagnostics</span><span class="count-pill" id="diagnostic-count">0</span></div>
                <div id="diagnostic-list"><span class="file-name">No diagnostics.</span></div>
              </section>
            </aside>
          </section>
          <footer class="statusbar" aria-live="polite">
            <span id="status" class="status-ok">Ready</span>
            <div class="pan-controls" aria-label="Move viewport">
              <button id="pan-left" class="icon-button" type="button" title="Move viewport left">${ICONS.left}</button>
              <button id="pan-up" class="icon-button" type="button" title="Move viewport up">${ICONS.up}</button>
              <button id="pan-down" class="icon-button" type="button" title="Move viewport down">${ICONS.down}</button>
              <button id="pan-right" class="icon-button" type="button" title="Move viewport right">${ICONS.right}</button>
            </div>
            <span id="viewport-status">No viewport</span>
          </footer>
          <div class="drop-overlay" aria-hidden="true"><div>Drop to open workbook</div></div>
        </div>
      </div>`;
  }

  private bindEvents(): void {
    this.byId("open-file").addEventListener("click", () => void this.pickFile());
    this.byId("open-file-empty").addEventListener("click", () => void this.pickFile());
    this.byId("toggle-sidebar").addEventListener("click", () => {
      this.applyPreferences({ sidebarOpen: !this.#preferences.sidebarOpen }, !isNarrowViewport());
    });
    this.byId("close-sidebar").addEventListener("click", () => this.applyPreferences({ sidebarOpen: false }, !isNarrowViewport()));
    this.byId("sidebar-scrim").addEventListener("click", () => this.applyPreferences({ sidebarOpen: false }, false));
    this.byId("toggle-details").addEventListener("click", () => {
      this.applyPreferences({ detailsOpen: !this.#preferences.detailsOpen }, true);
    });
    this.byId("clear-recent").addEventListener("click", () => void this.clearRecent());
    for (const option of this.root.querySelectorAll<HTMLButtonElement>("[data-theme-option]")) {
      option.addEventListener("click", () => {
        const theme = THEMES.find((candidate) => candidate.id === option.dataset.themeOption);
        if (theme) this.applyPreferences({ theme: theme.id }, true);
      });
    }
    this.bindFileDrop();
    document.addEventListener("keydown", this.#keydown);
    // Arrow keys are handled on the grid, so they still work after scrolling evicts the
    // focused cell (focus then rests on the grid itself; see renderGrid).
    this.byId("grid").addEventListener("keydown", (event) => {
      if (["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(event.key)) {
        event.preventDefault();
        void this.moveGridSelection(event.key);
      }
    });
    const shell = this.byId("grid-shell");
    shell.addEventListener("scroll", () => this.scheduleWindowCheck(), { passive: true });
    if (typeof ResizeObserver === "function") {
      this.#resizeObserver = new ResizeObserver(this.#onResize);
      this.#resizeObserver.observe(shell);
    } else {
      window.addEventListener("resize", this.#onResize);
    }
    this.byId<HTMLInputElement>("file-input").addEventListener("change", (event) => {
      const input = event.currentTarget as HTMLInputElement;
      const file = input.files?.[0];
      // Clear the selection so choosing the same file again (e.g. after declining) fires `change`.
      input.value = "";
      if (file) void this.openBrowserFile(file);
    });
    this.byId("save-file").addEventListener("click", () => void this.save());
    this.byId("cancel-work").addEventListener("click", () => void this.cancelWork());
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
      if (!this.confirmReplace(handle.name)) return;
      await this.openSource(bytes, handle.name, new LocalFileSession(handle, bytes));
    } catch (error) {
      if ((error as DOMException).name !== "AbortError") this.setError(error);
    }
  }

  private async openBrowserFile(file: File): Promise<void> {
    let bytes: Uint8Array;
    try {
      bytes = new Uint8Array(await file.arrayBuffer());
    } catch (error) {
      this.setError(error);
      return;
    }
    // Confirm after the read so no edit can land between the decision and the replacement.
    if (!this.confirmReplace(file.name)) return;
    try {
      await this.openSource(bytes, file.name);
    } catch {
      // `openSource` already presents the structured worker error.
    }
  }

  private async save(): Promise<void> {
    if (!this.beginMutation("Saving is already in progress")) return;
    this.setBusy(true, "Checking local file…");
    try {
      if (this.#fileSession) {
        // Request write access first, while the Save click still grants user activation.
        if (!await this.ensureWritePermission()) return;
        const result = await this.#fileSession.save(this.adapter);
        this.#source = this.#fileSession.baseSource;
        this.updateSourceView();
        this.#dirty = false;
        this.updateFileBadge();
        this.setStatus(result === "saved" ? "Saved focused source patches" : "No changes to save", "ok");
      } else {
        const payload = responsePayload(await this.adapter.sourceBytes(), "source_bytes");
        const source = Uint8Array.from(payload.source);
        downloadSource(source, this.#fileName);
        this.#source = source;
        this.#dirty = false;
        this.updateFileBadge();
        // Queued behind any pending registration, so the entry id is known when this runs.
        await this.withRecentStore(async (store) => {
          if (this.#currentRecentId) await store.updateSource(this.#currentRecentId, source);
        }).catch(() => undefined);
        this.setStatus(`Downloaded ${this.#fileName}`, "ok");
      }
    } catch (error) {
      if (error instanceof ExternalFileChangeError) {
        this.#source = error.externalSource.slice();
        // Unparseable external bytes leave the edited session in the worker, still unsaved.
        if (error.workerReplaced) {
          this.#dirty = false;
          this.updateFileBadge();
        }
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
    await this.refreshVisibleRegion({ reveal: "preserve" });
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

  private async refreshVisibleRegion(options: RefreshOptions = {}): Promise<boolean> {
    const { reveal = "anchor", quiet = false, fromShift = false } = options;
    const sheet = this.#activeSheet;
    if (!sheet) return false;
    const generation = this.#regionGate.begin();
    const limit = this.fitLimit();
    // Clamp only this request, so reopening Details returns to the unclamped position. At
    // most the last screenful of the fit is shown, never the last row alone at the top.
    const size = this.gridShellSize();
    const visible = viewportForContainer(this.#anchor, size);
    const anchor = limit
      ? {
          column: Math.min(this.#anchor.column, Math.max(1, limit.column - visible.visibleColumns + 1)),
          row: Math.min(this.#anchor.row, Math.max(1, limit.row - visible.visibleRows + 1)),
        }
      : this.#anchor;
    const range = computeViewport({ ...visible, anchor });
    this.setBusy(true, quiet ? undefined : `Loading ${sheet}…`);
    if (!fromShift) this.#pendingRefreshes += 1;
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
      this.#revealTarget = anchor;
      // Workers without the additive `extent` field leave the extent unknown, so nothing is clipped.
      if ("extent" in mergedRegion.sheet && mergedRegion.sheet.extent !== undefined) {
        this.#extents.set(sheet, mergedRegion.sheet.extent);
      }
      this.renderGrid(reveal);
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
      if (!quiet) this.setStatus(`Ready at revision ${this.adapter.currentRevision}`, "ok");
      return true;
    } catch (error) {
      if (this.#regionGate.isCurrent(generation)) this.setError(error);
      return false;
    } finally {
      if (!fromShift) this.#pendingRefreshes -= 1;
      if (this.#regionGate.isCurrent(generation)) this.setBusy(false);
    }
  }

  /** Returns whether the worker accepted the edit. */
  private async commitCell(source: string): Promise<boolean> {
    const cell = this.selectedCell();
    if (cell && "VirtualFill" in cell.source) {
      this.setStatus("Fill-derived cells are virtual and cannot be directly edited", "error");
      return false;
    }
    return this.commitTransaction(setCellTransaction(this.requireSheet(), this.#selected, source));
  }

  private async commitTransaction(transaction: ReturnType<typeof setCellTransaction>): Promise<boolean> {
    if (!this.#snapshot?.editable) {
      this.setStatus("This workbook is view-only until its formula diagnostics are resolved", "error");
      return false;
    }
    if (!this.beginMutation("Another save or edit is already in progress")) return false;
    this.setBusy(true, "Planning source-aware edit…");
    try {
      const edited = responsePayload(await this.adapter.edit(transaction), "edited");
      this.#snapshot = edited.snapshot;
      if (edited.changed) {
        // The worker has committed the edit even if reading its bytes back fails.
        this.#dirty = true;
        this.updateFileBadge();
        const source = responsePayload(await this.adapter.sourceBytes(), "source_bytes");
        this.#source = Uint8Array.from(source.source);
        this.updateSourceView();
      }
      const refreshed = await this.refreshVisibleRegion({ reveal: "preserve" });
      if (!refreshed) return true;
      const patchSummary = edited.patches
        .map((patch) => `${patch.span.start}..${patch.span.end}`)
        .join(", ");
      this.setStatus(
        edited.changed
          ? `Committed ${edited.patches.length} focused patch${edited.patches.length === 1 ? "" : "es"}: ${patchSummary}`
          : "Edit was a semantic no-op",
        "ok",
      );
      return true;
    } catch (error) {
      this.setError(error);
      return false;
    } finally {
      this.setBusy(false);
      this.endMutation();
    }
  }

  private async navigate(target: string): Promise<void> {
    const parsed = parseRange(target);
    if (parsed) {
      this.#selected = parsed.start;
      this.#anchor = parsed.start;
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
    this.#anchor = resolved.range.start;
    this.updateWorkbookChrome();
    await this.refreshVisibleRegion();
  }

  private async pan(columns: number, rows: number): Promise<void> {
    // Like the arrow keys, panning stays inside the reading view's fit.
    const limit = this.fitLimit();
    const move = (from: Coordinate): Coordinate => ({
      column: Math.min(Math.max(1, from.column + columns), limit?.column ?? Number.MAX_SAFE_INTEGER),
      row: Math.min(Math.max(1, from.row + rows), limit?.row ?? Number.MAX_SAFE_INTEGER),
    });
    this.#anchor = move(this.#anchor);
    this.#selected = move(this.#selected);
    this.byId<HTMLInputElement>("name-box").value = formatCoordinate(this.#selected);
    await this.refreshVisibleRegion();
  }

  private updateWorkbookChrome(): void {
    this.byId("file-name").textContent = this.#fileName;
    this.updateFileBadge();
    this.updateEditingState();
    const tabs = this.byId("sheet-tabs");
    tabs.hidden = !this.#snapshot;
    tabs.replaceChildren();
    tabs.setAttribute("role", "tablist");
    for (const sheet of this.#snapshot?.sheets ?? []) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "sheet-tab";
      button.textContent = sheet.label;
      button.setAttribute("role", "tab");
      button.title = `${sheet.id} · ${sheet.authored_cell_count} authored cells`;
      button.setAttribute("aria-selected", String(sheet.id === this.#activeSheet));
      button.addEventListener("click", () => {
        this.#activeSheet = sheet.id;
        this.#selected = { column: 1, row: 1 };
        this.#anchor = { column: 1, row: 1 };
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

  private renderGrid(reveal: Reveal = "anchor"): void {
    const region = this.#region;
    if (!region) return;
    const grid = this.byId("grid");
    const empty = this.byId("grid-empty");
    const reference = reveal === "preserve" && !grid.hidden ? this.scrollReference() : undefined;
    // Re-rendering replaces every cell; keep keyboard focus on the same coordinate.
    const focused = grid.contains(document.activeElement)
      ? (document.activeElement as HTMLElement).dataset.coordinate
      : undefined;
    const limit = this.fitLimit();
    // The reading view stops at the content extent; the window itself stays bounded either way.
    const range = clipRange(region.range, limit);
    if (!range) {
      // The request was clamped to an extent that has since shrunk (an edit or an external
      // change), so this window lies wholly past the fit; ask again from the new clamp.
      void this.refreshVisibleRegion({ quiet: true });
      return;
    }
    grid.hidden = false;
    empty.hidden = true;
    grid.classList.toggle("grid-fitted", Boolean(limit));
    this.#renderedFitted = Boolean(limit);
    grid.replaceChildren();

    const columns = inclusiveNumbers(range.start.column, range.end.column);
    const rows = inclusiveNumbers(range.start.row, range.end.row);
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
    const cornerLabel = document.createElement("span");
    cornerLabel.className = "sr-only";
    cornerLabel.textContent = "Row";
    corner.append(cornerLabel);
    corner.setAttribute("aria-colindex", "1");
    headerRow.append(corner);
    for (const [columnIndex, column] of columns.entries()) {
      const header = this.gridElement("div", "column-header", columnName(column), undefined, "columnheader");
      header.setAttribute("aria-colindex", String(columnIndex + 2));
      header.dataset.column = String(column);
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
      header.dataset.row = String(row);
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
        // Every cell is a button, so blank ones still need a name a screen reader can announce.
        if (!element.hasAttribute("aria-label")) element.setAttribute("aria-label", `${formatCoordinate(coordinate)}, blank`);
        gridRow.append(element);
      }
      grid.append(gridRow);
    }
    this.updateSelectionChrome();
    if (reference) this.restoreScroll(reference);
    else this.revealAnchor();
    if (focused !== undefined || document.activeElement === grid) {
      const cell = focused ? grid.querySelector<HTMLElement>(`.grid-cell[data-coordinate="${focused}"]`) : null;
      (cell ?? grid).focus({ preventScroll: true });
    }
    this.byId("viewport-status").textContent = `${region.sheet.label} · ${formatCoordinate(range.start)}:${formatCoordinate(range.end)} · ${region.cells.length} sparse / ${viewportCellCount(range)} rendered`;
  }

  /** The reading view's clip, or `undefined` in the detailed view or before the extent is known. */
  private fitLimit() {
    const sheet = this.#activeSheet;
    if (this.#preferences.detailsOpen || !sheet || !this.#extents.has(sheet)) return undefined;
    return readingLimit(this.#extents.get(sheet));
  }

  private gridShellSize(): { width: number; height: number } {
    const shell = this.byId("grid-shell");
    // Leave out the row-number column and the column-letter row.
    return { width: shell.clientWidth - 44, height: shell.clientHeight - 28 };
  }

  private headerMetrics() {
    const grid = this.byId("grid");
    return {
      shell: this.byId("grid-shell"),
      headerHeight: grid.querySelector<HTMLElement>(".column-header")?.offsetHeight ?? 0,
      rowHeaderWidth: grid.querySelector<HTMLElement>(".row-header")?.offsetWidth ?? 0,
      rows: [...grid.querySelectorAll<HTMLElement>(".row-header")],
      columns: [...grid.querySelectorAll<HTMLElement>(".column-header")],
    };
  }

  /** The first row and column the user can currently see, and where they sit on screen. */
  private scrollReference(): ScrollReference | undefined {
    const { shell, headerHeight, rowHeaderWidth, rows, columns } = this.headerMetrics();
    const row = rows.find((header) => header.offsetTop + header.offsetHeight > shell.scrollTop + headerHeight);
    const column = columns.find((header) => header.offsetLeft + header.offsetWidth > shell.scrollLeft + rowHeaderWidth);
    if (!row || !column) return undefined;
    return {
      row: Number(row.dataset.row),
      top: row.offsetTop - shell.scrollTop,
      column: Number(column.dataset.column),
      left: column.offsetLeft - shell.scrollLeft,
    };
  }

  private restoreScroll(reference: ScrollReference): void {
    const grid = this.byId("grid");
    const shell = this.byId("grid-shell");
    const row = grid.querySelector<HTMLElement>(`.row-header[data-row="${reference.row}"]`);
    const column = grid.querySelector<HTMLElement>(`.column-header[data-column="${reference.column}"]`);
    if (row) shell.scrollTop = row.offsetTop - reference.top;
    if (column) shell.scrollLeft = column.offsetLeft - reference.left;
  }

  private revealAnchor(): void {
    const { shell, headerHeight, rowHeaderWidth } = this.headerMetrics();
    const grid = this.byId("grid");
    const row = grid.querySelector<HTMLElement>(`.row-header[data-row="${this.#revealTarget.row}"]`);
    const column = grid.querySelector<HTMLElement>(`.column-header[data-column="${this.#revealTarget.column}"]`);
    shell.scrollTop = row ? row.offsetTop - headerHeight : 0;
    shell.scrollLeft = column ? column.offsetLeft - rowHeaderWidth : 0;
  }

  private scheduleWindowCheck(): void {
    if (this.#scrollFrame || this.#disposed) return;
    const schedule = typeof requestAnimationFrame === "function"
      ? requestAnimationFrame
      : (callback: FrameRequestCallback) => setTimeout(() => callback(0), 16) as unknown as number;
    this.#scrollFrame = schedule(() => {
      this.#scrollFrame = 0;
      void this.shiftWindowIfNeeded();
    });
  }

  /**
   * Moves the bounded window when the user scrolls near its rendered edge, so
   * scrolling feels continuous without ever rendering more than one window.
   */
  private async shiftWindowIfNeeded(): Promise<void> {
    if (this.#shifting) {
      this.#rescanAfterShift = true;
      return;
    }
    const region = this.#region;
    if (!region || !this.#activeSheet || this.byId("grid").hidden) return;
    // A jump, sheet switch, or Details toggle is loading; its own render decides the window.
    if (this.#pendingRefreshes > 0 || region.sheet.id !== this.#activeSheet) return;
    // The grid on screen was drawn for the other mode; its rows say nothing about this one.
    if (this.#renderedFitted !== Boolean(this.fitLimit())) return;
    const { shell, headerHeight, rowHeaderWidth, rows, columns } = this.headerMetrics();
    const firstRowHeader = rows[0];
    const lastRowHeader = rows.at(-1);
    const firstColumnHeader = columns[0];
    const lastColumnHeader = columns.at(-1);
    if (!firstRowHeader || !lastRowHeader || !firstColumnHeader || !lastColumnHeader) return;

    const top = shell.scrollTop + headerHeight;
    const bottom = shell.scrollTop + shell.clientHeight;
    const left = shell.scrollLeft + rowHeaderWidth;
    const right = shell.scrollLeft + shell.clientWidth;
    const firstRow = Number(rows.find((header) => header.offsetTop + header.offsetHeight > top)?.dataset.row);
    const lastRow = Number([...rows].reverse().find((header) => header.offsetTop < bottom)?.dataset.row);
    const firstColumn = Number(columns.find((header) => header.offsetLeft + header.offsetWidth > left)?.dataset.column);
    const lastColumn = Number([...columns].reverse().find((header) => header.offsetLeft < right)?.dataset.column);
    if (![firstRow, lastRow, firstColumn, lastColumn].every(Number.isSafeInteger)) return;

    const rendered = {
      start: { row: Number(firstRowHeader.dataset.row), column: Number(firstColumnHeader.dataset.column) },
      end: { row: Number(lastRowHeader.dataset.row), column: Number(lastColumnHeader.dataset.column) },
    };
    const spec = viewportForContainer(this.#anchor, this.gridShellSize());
    const limit = this.fitLimit();
    const anchor = { ...this.#anchor };
    if (lastRow >= rendered.end.row - 2 && (!limit || rendered.end.row < limit.row)) anchor.row = firstRow;
    // Either way, the new window has `rowOverscan` rows above and below what is visible.
    else if (firstRow <= rendered.start.row + 2 && rendered.start.row > 1) anchor.row = firstRow;
    if (lastColumn >= rendered.end.column - 1 && (!limit || rendered.end.column < limit.column)) anchor.column = firstColumn;
    else if (firstColumn <= rendered.start.column + 1 && rendered.start.column > 1) anchor.column = firstColumn;
    const next = computeViewport({ ...spec, anchor });
    const current = region.range;
    if (next.start.row === current.start.row && next.start.column === current.start.column
      && next.end.row === current.end.row && next.end.column === current.end.column) return;

    this.#anchor = anchor;
    this.#shifting = true;
    try {
      await this.refreshVisibleRegion({ reveal: "preserve", quiet: true, fromShift: true });
    } finally {
      this.#shifting = false;
    }
    if (this.#rescanAfterShift) {
      this.#rescanAfterShift = false;
      this.scheduleWindowCheck();
    }
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
      element.addEventListener("dblclick", () => {
        if (!this.#preferences.detailsOpen) this.applyPreferences({ detailsOpen: true }, true);
        this.byId<HTMLInputElement>("formula-input").focus();
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

  private async moveGridSelection(key: string): Promise<void> {
    // Move from the selection, not the key's target: while a window loads, held keys
    // keep landing on the previous cell and would otherwise collapse into one step.
    const origin = this.#selected;
    const delta = key === "ArrowUp"
      ? { column: 0, row: -1 }
      : key === "ArrowDown"
        ? { column: 0, row: 1 }
        : key === "ArrowLeft"
          ? { column: -1, row: 0 }
          : { column: 1, row: 0 };
    const limit = this.fitLimit();
    const next = {
      column: Math.min(Math.max(1, origin.column + delta.column), limit?.column ?? Number.MAX_SAFE_INTEGER),
      row: Math.min(Math.max(1, origin.row + delta.row), limit?.row ?? Number.MAX_SAFE_INTEGER),
    };
    if (next.column === origin.column && next.row === origin.row) return;
    this.#selected = next;
    const rendered = this.root.querySelector(`.grid-cell[data-coordinate="${coordinateKey(next)}"]`);
    if (rendered) this.updateSelectionChrome();
    else {
      // Bring the new cell into the window: at the top or left edge when moving back, the far edge otherwise.
      const spec = viewportForContainer(this.#anchor, this.gridShellSize());
      this.#anchor = {
        column: delta.column > 0 ? Math.max(1, next.column - spec.visibleColumns + 1) : delta.column < 0 ? next.column : this.#anchor.column,
        row: delta.row > 0 ? Math.max(1, next.row - spec.visibleRows + 1) : delta.row < 0 ? next.row : this.#anchor.row,
      };
      await this.refreshVisibleRegion();
    }
    // Further keys may have moved the selection during the load; focus where it is now.
    this.root.querySelector<HTMLElement>(`.grid-cell[data-coordinate="${coordinateKey(this.#selected)}"]`)?.focus();
  }

  private updateSelectionChrome(): void {
    const key = coordinateKey(this.#selected);
    for (const element of this.root.querySelectorAll<HTMLElement>(".grid-cell")) {
      element.classList.toggle("cell-selected", element.dataset.coordinate === key);
      element.tabIndex = element.dataset.coordinate === key ? 0 : -1;
    }
    // Keep one cell reachable with Tab even when the selection has scrolled out of the window.
    if (!this.root.querySelector(`.grid-cell[data-coordinate="${key}"]`)) {
      const first = this.root.querySelector<HTMLElement>(".grid-cell");
      if (first) first.tabIndex = 0;
    }
    for (const header of this.root.querySelectorAll<HTMLElement>(".column-header, .row-header")) {
      header.classList.toggle(
        "header-active",
        header.dataset.column === String(this.#selected.column) || header.dataset.row === String(this.#selected.row),
      );
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
    this.updateDetailsBadge(unique, truncatedScopes.reduce((total, [, count]) => total + count, 0));
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

  private applyPreferences(changes: Partial<ViewerPreferences>, persist = false): void {
    this.#preferences = { ...this.#preferences, ...changes };
    if (persist) savePreferences(this.#storage, this.#preferences);
    const { theme, sidebarOpen, detailsOpen } = this.#preferences;
    const shell = this.byId("app-shell");
    shell.dataset.theme = theme;
    shell.dataset.sidebar = sidebarOpen ? "open" : "closed";
    shell.dataset.details = detailsOpen ? "open" : "closed";
    document.documentElement.dataset.theme = theme;
    this.byId("sidebar").toggleAttribute("inert", !sidebarOpen);
    this.byId("toggle-sidebar").setAttribute("aria-expanded", String(sidebarOpen));
    this.byId("toggle-details").setAttribute("aria-pressed", String(detailsOpen));
    const detailsChanged = this.byId("details-bar").hidden === detailsOpen;
    this.byId("details-bar").hidden = !detailsOpen;
    this.byId("inspector").hidden = !detailsOpen;
    // The reading view clips to the content; the detailed view shows the full window. The
    // window itself changes, so return to the remembered anchor (clamped while reading).
    if (detailsChanged && this.#region) void this.refreshVisibleRegion({ quiet: true });
    for (const option of this.root.querySelectorAll<HTMLElement>("[data-theme-option]")) {
      option.setAttribute("aria-checked", String(option.dataset.themeOption === theme));
    }
  }

  /** The active theme, exposed for embedding hosts and tests. */
  get theme(): ThemeId {
    return this.#preferences.theme;
  }

  private handleShortcut(event: KeyboardEvent): void {
    if (!(event.metaKey || event.ctrlKey) || event.altKey) return;
    const key = event.key.toLowerCase();
    if (key === "o") {
      event.preventDefault();
      if (!this.#mutationBusy) void this.pickFile();
    } else if (key === "s" && this.#snapshot) {
      event.preventDefault();
      void this.saveFromShortcut();
    } else if (key === "\\") {
      event.preventDefault();
      this.applyPreferences({ sidebarOpen: !this.#preferences.sidebarOpen }, !isNarrowViewport());
    } else if (key === "/") {
      event.preventDefault();
      this.applyPreferences({ detailsOpen: !this.#preferences.detailsOpen }, true);
    }
  }

  /** Ctrl/⌘+S from the formula bar first commits the typed value, as Enter would. */
  private async saveFromShortcut(): Promise<void> {
    const formula = this.byId<HTMLInputElement>("formula-input");
    const pending = document.activeElement === formula
      && !formula.disabled
      && formula.value !== sourceText(this.selectedCell());
    // Ask for write access before the edit's awaits can outlive the key press's user activation.
    if (pending && !await this.ensureWritePermission().catch((error: unknown) => {
      this.setError(error);
      return false;
    })) return;
    if (pending && !await this.commitCell(formula.value)) return;
    await this.save();
  }

  /** True when there is no local file handle, or write access is granted. */
  private async ensureWritePermission(): Promise<boolean> {
    if (!this.#fileSession) return true;
    const handle = this.#fileSession.handle as RecentFileHandle;
    if (await ensureHandlePermission(handle, "readwrite")) return true;
    this.setStatus(`Permission to save ${this.#fileName} was not granted; no bytes were written`, "error");
    return false;
  }

  private bindFileDrop(): void {
    const shell = this.byId("app-shell");
    let depth = 0;
    const carriesFiles = (event: DragEvent) => Boolean(event.dataTransfer?.types.includes("Files"));
    shell.addEventListener("dragenter", (event) => {
      if (!carriesFiles(event)) return;
      event.preventDefault();
      depth += 1;
      shell.toggleAttribute("data-dropping", true);
    });
    shell.addEventListener("dragover", (event) => {
      if (!carriesFiles(event)) return;
      event.preventDefault();
      if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
    });
    shell.addEventListener("dragleave", () => {
      depth = Math.max(0, depth - 1);
      if (depth === 0) shell.toggleAttribute("data-dropping", false);
    });
    shell.addEventListener("drop", (event) => {
      if (!carriesFiles(event)) return;
      event.preventDefault();
      depth = 0;
      shell.toggleAttribute("data-dropping", false);
      void this.openDrop(event.dataTransfer);
    });
  }

  private async openDrop(transfer: DataTransfer | null): Promise<void> {
    if (!transfer || this.#mutationBusy) return;
    const item = [...transfer.items].find((candidate) => candidate.kind === "file") as DroppedItem | undefined;
    const file = transfer.files[0];
    // Request the handle synchronously: the transfer is only readable during the drop event.
    const handlePromise = item?.getAsFileSystemHandle?.().catch(() => null);
    try {
      const handle = await handlePromise;
      if (handle && handle.kind === "file") {
        const fileHandle = handle as unknown as RecentFileHandle;
        const bytes = new Uint8Array(await (await fileHandle.getFile()).arrayBuffer());
        if (!this.confirmReplace(fileHandle.name)) return;
        await this.openSource(bytes, fileHandle.name, new LocalFileSession(fileHandle, bytes));
        return;
      }
      if (file) await this.openBrowserFile(file);
    } catch (error) {
      this.setError(error);
    }
  }

  private async rememberWorkbook(
    source: Uint8Array,
    name: string,
    sheetCount: number,
    session: LocalFileSession | undefined,
    recentId: string | undefined,
  ): Promise<void> {
    const generation = ++this.#rememberGeneration;
    const handle = session?.handle as RecentFileHandle | undefined;
    await this.withRecentStore(async (store) => {
      try {
        const entry = await store.remember({
          name,
          sheetCount,
          source,
          ...(handle ? { handle } : {}),
          ...(recentId ? { id: recentId } : {}),
        });
        if (generation === this.#rememberGeneration) this.#currentRecentId = entry.id;
      } catch {
        // Remembering is best effort: storage may be full or disabled.
        if (generation === this.#rememberGeneration) this.#currentRecentId = undefined;
      }
    });
    await this.refreshRecent();
  }

  /**
   * Serializes every recent-store operation, so a lookup, insert, eviction,
   * save, forget, or clear never interleaves with another one.
   */
  private withRecentStore<T>(task: (store: RecentWorkbookStore) => Promise<T>): Promise<T> {
    const run = this.#recentQueue.then(() => task(this.#recentStore));
    this.#recentQueue = run.catch(() => undefined);
    return run;
  }

  private async refreshRecent(): Promise<void> {
    try {
      this.#recent = await this.withRecentStore((store) => store.list());
    } catch {
      this.#recent = [];
    }
    if (!this.#disposed) this.renderRecent();
  }

  private renderRecent(): void {
    const list = this.byId("recent-list");
    list.replaceChildren();
    this.byId("recent-empty").hidden = this.#recent.length > 0;
    this.byId("clear-recent").hidden = this.#recent.length === 0;
    const now = Date.now();
    for (const entry of this.#recent) {
      const item = document.createElement("li");
      item.className = "recent-item";
      item.dataset.recentId = entry.id;
      const current = entry.id === this.#currentRecentId && Boolean(this.#snapshot);
      item.classList.toggle("active", current);

      const open = document.createElement("button");
      open.type = "button";
      open.className = "recent-open";
      if (current) open.setAttribute("aria-current", "page");
      open.title = entry.handle
        ? `${entry.name}\nReopens the file on disk`
        : `${entry.name}\nReopens the copy stored in this browser`;
      const icon = document.createElement("span");
      icon.className = "recent-icon";
      icon.setAttribute("aria-hidden", "true");
      icon.innerHTML = ICONS.file;
      const text = document.createElement("span");
      text.className = "recent-text";
      const name = document.createElement("span");
      name.className = "recent-name";
      name.textContent = entry.name;
      const meta = document.createElement("span");
      meta.className = "recent-meta";
      const sheets = `${entry.sheetCount} sheet${entry.sheetCount === 1 ? "" : "s"}`;
      meta.textContent = `${formatRelativeTime(entry.openedAt, now)} · ${sheets}`;
      text.append(name, meta);
      open.append(icon, text);
      open.addEventListener("click", () => void this.openRecent(entry));

      const forget = document.createElement("button");
      forget.type = "button";
      forget.className = "recent-forget icon-button";
      forget.setAttribute("aria-label", `Forget ${entry.name}`);
      forget.title = "Remove from recent";
      forget.innerHTML = ICONS.close;
      forget.addEventListener("click", () => void this.forgetRecent(entry.id));

      item.append(open, forget);
      list.append(item);
    }
  }

  private async openRecent(entry: RecentWorkbook): Promise<void> {
    if (this.#mutationBusy) {
      this.setStatus("Another open, save, or edit is already in progress", "warning");
      return;
    }
    if (isNarrowViewport()) this.applyPreferences({ sidebarOpen: false });
    let opening = false;
    try {
      if (entry.handle) {
        if (!await ensureHandlePermission(entry.handle)) {
          this.setStatus(`Permission to open ${entry.name} was not granted`, "error");
          return;
        }
        let bytes: Uint8Array;
        try {
          bytes = new Uint8Array(await (await entry.handle.getFile()).arrayBuffer());
        } catch {
          this.setStatus(`${entry.name} could not be read; it may have been moved or deleted`, "error");
          return;
        }
        if (!this.confirmReplace(entry.name)) return;
        opening = true;
        await this.openSource(bytes, entry.name, new LocalFileSession(entry.handle, bytes), entry.id);
        return;
      }
      const bytes = await this.withRecentStore((store) => store.source(entry.id));
      if (!bytes) {
        this.setStatus(`The stored copy of ${entry.name} is no longer available`, "error");
        await this.forgetRecent(entry.id);
        return;
      }
      if (!this.confirmReplace(entry.name)) return;
      opening = true;
      await this.openSource(bytes, entry.name, undefined, entry.id);
    } catch (error) {
      // `openSource` already presents its structured worker error; earlier failures are ours to show.
      if (!opening) this.setError(error);
    }
  }

  private async forgetRecent(id: string): Promise<void> {
    // Queued after any registration in flight, so it cannot recreate what is removed.
    await this.withRecentStore(async (store) => {
      await store.remove(id);
      if (this.#currentRecentId === id) this.#currentRecentId = undefined;
    }).catch(() => undefined);
    await this.refreshRecent();
  }

  private async clearRecent(): Promise<void> {
    await this.withRecentStore(async (store) => {
      await store.clear();
      this.#currentRecentId = undefined;
    }).catch(() => undefined);
    await this.refreshRecent();
  }

  /** Unsaved edits are only replaced after an explicit discard decision. */
  private confirmReplace(nextName: string): boolean {
    if (!this.#dirty || !this.#snapshot) return true;
    const confirmed = this.#confirmDiscard(
      `${this.#fileName} has unsaved edits. Discard them and open ${nextName}?`,
    );
    if (!confirmed) this.setStatus(`Kept ${this.#fileName}; save it before opening another workbook`, "warning");
    return confirmed;
  }

  private updateFileBadge(): void {
    const badge = this.byId("file-badge");
    const label = !this.#snapshot
      ? ""
      : !this.#snapshot.editable
        ? "View only"
        : this.#dirty
          ? "Edited"
          : "";
    badge.textContent = label;
    badge.hidden = label === "";
    badge.dataset.kind = this.#snapshot && !this.#snapshot.editable ? "readonly" : "edited";
    // The browser tab names the workbook and marks unsaved edits, like a desktop editor.
    document.title = this.#snapshot
      ? `${this.#dirty ? "• " : ""}${this.#fileName} — Marksheet`
      : "Marksheet Viewer";
  }

  private updateDetailsBadge(diagnostics: Diagnostic[], omitted: number): void {
    const badge = this.byId("details-badge");
    const errors = diagnostics.filter((diagnostic) => diagnostic.severity === "error").length;
    const total = diagnostics.length + omitted;
    badge.hidden = total === 0;
    badge.textContent = String(total);
    badge.dataset.severity = errors > 0 ? "error" : "warning";
    badge.title = `${total} diagnostic${total === 1 ? "" : "s"}`;
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
    this.byId("app-shell").toggleAttribute("data-busy", busy);
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
  return authoredCellText(cell.source.Authored.value);
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
  return `Authored ${value.kind} ${authoredCellText(value)}`;
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

function defaultConfirm(message: string): boolean {
  return typeof window === "undefined" || typeof window.confirm !== "function" || window.confirm(message);
}

function isNarrowViewport(): boolean {
  try {
    return typeof matchMedia === "function" && matchMedia("(max-width: 760px)").matches;
  } catch {
    return false;
  }
}

const MOD_KEY = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform ?? "") ? "⌘" : "Ctrl+";

const svg = (body: string, size = 16) =>
  `<svg width="${size}" height="${size}" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">${body}</svg>`;

const ICONS = {
  mark: `<svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" focusable="false"><rect x="1.5" y="1.5" width="13" height="13" rx="3" fill="none" stroke="currentColor" stroke-width="1.5"/><path d="M1.5 6h13M6 6v8.5" stroke="currentColor" stroke-width="1.5"/></svg>`,
  plus: svg(`<path d="M8 3.5v9M3.5 8h9"/>`),
  close: svg(`<path d="M4.5 4.5l7 7M11.5 4.5l-7 7"/>`),
  sidebar: svg(`<rect x="2" y="2.5" width="12" height="11" rx="2"/><path d="M6.5 2.5v11"/>`),
  details: svg(`<rect x="2" y="2.5" width="12" height="11" rx="2"/><path d="M9.5 2.5v11M11.25 5.5h.5M11.25 8h.5"/>`),
  file: svg(`<rect x="2.5" y="2" width="11" height="12" rx="2"/><path d="M2.5 6h11M2.5 10h11M6.5 6v8"/>`),
  left: svg(`<path d="M10 3.5L5.5 8l4.5 4.5"/>`),
  right: svg(`<path d="M6 3.5L10.5 8 6 12.5"/>`),
  up: svg(`<path d="M3.5 10L8 5.5l4.5 4.5"/>`),
  down: svg(`<path d="M3.5 6L8 10.5 12.5 6"/>`),
  emptyGrid: `<svg width="120" height="84" viewBox="0 0 120 84" fill="none" aria-hidden="true" focusable="false"><rect x="1" y="1" width="118" height="82" rx="10" class="art-frame"/><path d="M1 21h118M1 41h118M1 61h118M33 1v82M62 1v82M91 1v82" class="art-line"/><rect x="34" y="22" width="27" height="18" class="art-cell"/><path d="M70 50h14M41 70h14M99 30h12" class="art-ink"/></svg>`,
} as const;
