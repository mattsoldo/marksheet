# Marksheet browser viewer

This standalone package is the completed Milestone 4 browser proof for the
sparse, source-aware Marksheet workbench. It is a small TypeScript and DOM
application rather than a second spreadsheet engine: parsing, formula
preparation and calculation, sparse projection, and semantic edits cross the
versioned `marksheet-worker@1` boundary in the independent
[`bindings/wasm`](../bindings/wasm) Cargo workspace. The worker delegates its
renderer-neutral viewport model to the pure-Rust `marksheet-view` crate.
Viewer types are re-exported directly from the binding's generated
`protocol.d.ts`; this package does not maintain a second wire model.

The viewer provides:

- local `.ms` open, guarded File System Access saves when a browser supplies a
  file handle, and an explicit download fallback otherwise;
- source-order sheet tabs and a finite 30×12 viewport with three-cell overscan;
- separate authored, formula, calculated, virtual-fill, resolved-style, and
  geometry layers;
- a formula bar, A1/range/declared-name box, semantic name/style/width/height controls;
- source-linked cells and diagnostics plus an exact, synchronized source view;
- stale-response suppression and worker-restart cancellation; and
- view-only rendering when error-level formula diagnostics make transactional
  edits unsafe.

The grid never derives a dense allocation from the furthest authored cell.
Jumping to a distant coordinate requests and realizes at most 648 cells.
Resolved style regions also decorate blank viewport coordinates. Currency,
percent, decimal, integer, date, horizontal, and vertical presentation is
deterministic; column geometry uses CSS `ch`, while row and font geometry use
CSS `pt`, matching the format's authored units.

## Prerequisites and development

The viewer uses a current stable Rust toolchain (the workspace currently
requires Rust 1.85 or newer), Node.js with npm, the `wasm32-unknown-unknown`
Rust target, and the `wasm-bindgen` CLI. Install the Wasm prerequisites once:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.127 --locked
```

Then install JavaScript dependencies and run the verification sequence:

```sh
cd viewer
npm install
npm test
npm run smoke:wasm
npm run build
```

`npm run dev` and `npm run build` first compile the real Rust binding, run
`wasm-bindgen`, and stage the canonical worker under the ignored
`public/marksheet-wasm/` directory. Vite copies the worker, protocol, JavaScript
glue, and Wasm module into `dist/`; the build fails if any one is missing. Thus
`npm run preview` serves a self-contained worker build with no manual copying.
The worker URL is resolved beneath Vite's `BASE_URL`, including project-site
deployments such as `/marksheet/`.

The generated module comes from
`bindings/wasm/target/wasm32-unknown-unknown/release/marksheet_wasm.wasm`.

## Default worker admission limits

The browser's `WasmWorkbench` accepts at most **5 MiB (5,242,880 bytes)** of
source. Before parsing an open or source replacement, and again after an edit
while rebuilding its view, it rejects a source with more than 4,096
newline-delimited records, 4,096 lines beginning with `@`, 4,096 `=` bytes, or
4,096 `,` bytes. These are raw preflight scans rather than a partial CSV or
Marksheet parser: `=` and `,` in quoted text or comments, and data lines
beginning with `@`, still count. That means a valid but unusually shaped source
can be refused with a worker `limit` error. The false positives are intentional:
they prevent malformed quoting or comments from evading the worker's bounds
before parsing, diagnostics, or CSV lowering can consume unbounded resources.

The same default worker caps visible rectangles at 250,000 cells, calculation
rectangles at 100,000 cells, and sparse projected cells at 100,000. See the
[binding README](../bindings/wasm/README.md#default-admission-and-resource-limits)
for the full response, calculation-work, styling, and browser-number limits.

`npm test` uses an in-memory protocol adapter in `happy-dom`. It covers bounded
viewport realization, source-order tabs, focused semantic edits and patches,
stale-response gating, and the external-change no-write invariant. Formula
diagnostic view-only behavior is covered across both the DOM adapter and Rust
worker conformance tests. The viewer tests do not exercise an actual browser's File
System Access write permission or writable stream.

Diagnostic rendering is deduplicated and capped at 100 DOM rows per refresh;
the panel reports the total unique count and an overflow summary. This keeps a
pathological diagnostic set bounded just like the sparse viewport.

`npm run smoke:wasm` invokes the generated ABI directly: it opens the real
Budget workbook, verifies ordered sheets and `summary!B4 = 1648`, applies one
focused `inputs!G2` edit, and verifies the dependent recalculation to `1545`.
`npm run build` additionally proves that the distributable worker, protocol,
JavaScript glue, and Wasm files are present.

## Manual browser proof

Run `npm run dev`, open the displayed local URL in a browser, and choose
`../examples/budget.ms`. Confirm the `Inputs` and `Summary` tabs remain in
source order, select the declared name `tax_rate` using the name box, change
`0.2` to `0.25`,
then select `summary!B4` and confirm the calculated value is `1545`. The source
view should show only the focused source change.

In a browser that supports the File System Access API, this flow opens through
the file picker and Save rereads the selected file's exact bytes before it
creates a writable. To prove external-change protection, modify that file with
another program between opening it and saving it: the viewer reloads the
external source and writes no bytes. A no-op Save also creates no writable. If
the browser does not provide a file handle, the fallback picker can open the
workbook but Save downloads the current source; it does not overwrite the
selected file. These are real-browser checks, intentionally separate from the
headless DOM tests.

## Known binding seam

Name navigation uses the binding's bounded typed name summaries. Cell and range
targets navigate directly. Table-column targets require the optional resolved
sheet/range included by the browser binding; if it is unavailable, the viewer
reports the unsupported target without changing selection.
