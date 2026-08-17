# Marksheet Implementation Specification

**Status:** Draft 0.1

**Applies to:** Marksheet format 0.1

This document describes the architecture of the reference implementation. It
is not a requirement for independent Marksheet implementations. Normative file
behavior lives in [`SPEC.md`](SPEC.md).

## 1. Implementation goals

The reference implementation must prove five things:

1. Marksheet can be parsed with a small, dependable core.
2. The same core can run in a CLI, browser, desktop application, and libraries.
3. Real spreadsheets can calculate without making the format dependent on
   Excel or a cloud service.
4. A GUI edit can produce a focused source patch rather than a regenerated
   document.
5. Plugins can add useful behavior without allowing a workbook to execute code.

The implementation should remain useful as infrastructure. The GUI is one
client of the library, not the library's reason for existing.

## 2. Architectural shape

```text
Marksheet source
      |
      v
lossless syntax tree -----> diagnostics
      |
      v
semantic workbook IR -----> extension registry
      |
      +-----> calculation adapter -----> calculated values
      |
      +-----> renderer / GUI
      |
      +-----> converters
      |
edits + source spans
      |
      v
minimal source patches or explicit canonical serialization
```

The syntax and semantic layers are intentionally separate:

- The lossless syntax tree owns source spelling, comments, whitespace, CSV
  quoting, and opaque extension bytes.
- The workbook intermediate representation owns resolved sheets, cells,
  tables, names, formulas, styles, and dependencies.

Discarding either layer would break an important use case. A syntax tree alone
cannot calculate a workbook; a semantic model alone cannot save one without
rewriting unrelated source.

## 3. Technology direction

The reference core should be written in **Rust** and exposed through narrow
bindings.

Rust is a good fit because it provides:

- predictable memory and performance for large files;
- strong types for source spans, coordinates, values, and diagnostics;
- native binaries for the CLI and desktop host;
- WebAssembly output for browser use; and
- mature Python and JavaScript binding paths.

The first supported surfaces should be:

| Surface | Delivery |
| --- | --- |
| Rust | Native crates |
| Browser and Node.js | WebAssembly with TypeScript declarations |
| CLI | Single native `marksheet` executable |
| Python | Binding after the Rust API stabilizes |
| Desktop | Thin host around the browser GUI and native core |

The format itself is language-neutral. No Rust-specific type or serialization
detail may leak into the public Marksheet syntax.

## 4. Proposed repository layout

```text
crates/
  marksheet-syntax/     Lossless scanner, parser, CST, formatter
  marksheet-model/      Workbook IR, coordinates, names, styles
  marksheet-calc/       Formula profile and calculation adapter
  marksheet-edit/       Transactions and source patch generation
  marksheet-convert/    Converter interfaces and reports
  marksheet-cli/        check, inspect, get, set, fmt, calc, diff, and convert
bindings/
  wasm/                 Browser and Node.js API
  python/               Python API, added after the core stabilizes
apps/
  viewer/               Spreadsheet GUI
integrations/
  skill/                Canonical portable coding-agent skill
  mcp/                  Optional local structured-tool server
  harnesses/            Thin harness-specific packaging and adapters
tests/
  conformance/          Format and formula fixtures
  roundtrip/            Lossless and canonical round-trip fixtures
  differential/         Cross-engine and cross-implementation results
  harness/              End-to-end coding-agent task fixtures
```

This is a target layout, not scaffolding that must exist before the parser
prototype validates the design.

## 5. Parsing pipeline

### 5.1 Scanner

The scanner recognizes the version header, comments, blank lines, directives,
CSV bodies, and opaque extension bodies. It must understand CSV quote state so
that an `@end` inside a multiline quoted field is data rather than a terminator.

The scanner records byte offsets and line/column positions. UTF-8 byte offsets
are authoritative for patching; line and Unicode-column positions are for
diagnostics and editor protocols.

### 5.2 Concrete syntax tree

The CST stores every byte of the input through tokens and trivia. Important
nodes include:

```text
Document
  Header
  BookDirective?
  WorkbookDeclaration*
  SheetSection+

SheetSection
  SheetDirective
  SheetItem*

SheetItem
  Block | Table | Fill | Apply | Column | Row | Extension | Comment | Blank
```

A CSV field node stores:

- its raw byte span;
- decoded value;
- whether it was quoted;
- record and field indexes; and
- newline and delimiter trivia needed for a local rewrite.

Extension payloads remain opaque byte slices unless a trusted, installed
extension parser claims them.

### 5.3 Semantic construction

The semantic pass:

1. validates versions and workbook properties;
2. creates sheets in source order;
3. maps block and table fields to coordinates;
4. detects footprint overlaps;
5. parses scalar values and formula ASTs;
6. resolves tables and named ranges after the complete file is known;
7. expands fills into virtual formula cells;
8. resolves style and geometry applications; and
9. builds diagnostics without losing the CST.

Parsing should recover after local errors so an editor can display as much of a
damaged workbook as possible. A recovered document must not be presented as
valid.

## 6. Workbook intermediate representation

The workbook IR should use explicit IDs and sparse storage:

```text
Workbook
  version
  settings
  sheets: ordered list<Sheet>
  tables: map<TableId, Table>
  names: map<NameId, NamedRange>
  styles: map<StyleId, Style>
  extensions: list<ExtensionInstance>

Sheet
  id
  label
  blocks: ordered list<Block>
  occupied: spatial index<Footprint>
  cells: sparse map<Coordinate, Cell>
  fills
  style applications
  row geometry
  column geometry
```

Blank cells inside a block footprint are distinguishable from cells outside
all blocks. The distinction matters for overlap validation, table shape, source
edits, and blank-versus-empty-string behavior.

Virtual cells created by `@fill` should not be materialized into source fields.
They may be materialized in a calculation cache, but retain an origin link to
the fill directive and destination coordinate.

### 6.1 Coordinates

Internally, coordinates should use unsigned row and column indexes with checked
conversion from A1 text. A single packed integer may be used as a map key, but
the public API should expose a typed coordinate rather than an implementation
integer.

The format has no maximum coordinate. Any implementation limit belongs in a
configuration object and produces a diagnostic when exceeded.

### 6.2 Values

Use a tagged value representation matching the format:

```text
Blank
Text(string)
Number(f64)
Boolean(bool)
Date(civil date)
DateTime(offset datetime)
Formula(formula AST)
Error(core error)
```

Do not use an empty string as the internal representation of a blank.

## 7. Formula implementation

Marksheet should own the `portable-a1@1` formula profile and its conformance
tests, but it should not build a complete spreadsheet engine from scratch
before validating existing engines.

The calculation layer therefore uses an adapter:

```text
trait CalcEngine {
  load(workbook_view)
  apply_changes(change_set)
  evaluate(dirty_set) -> calculation_result
  register_functions(namespace, functions)
}
```

The adapter is responsible for translating stable sheet IDs, named ranges,
table references, dates, errors, and fill formulas without leaking an engine's
private workbook model into Marksheet APIs.

### 7.1 Initial engine spike

The first spike should test
[Formualizer](https://github.com/psu3d0/formualizer) behind this adapter. It is a
Rust spreadsheet runtime with Rust, Python, and WebAssembly surfaces,
dependency-aware recalculation, sparse-oriented storage, deterministic mode,
and custom functions. Those properties align closely with Marksheet.

[IronCalc](https://github.com/ironcalc/IronCalc) should remain the comparison
engine. It is also Rust-based, supports WebAssembly and other bindings, includes
an XLSX reader/writer, and has a browser application.

Adoption is conditional, not automatic. The spike must verify:

- exact portable-profile formula parsing and coercion behavior;
- stable sheet-ID and named-range mapping;
- structured references or a safe lowering strategy;
- deterministic calculation with volatile functions disabled;
- incremental updates for GUI editing;
- WebAssembly size and startup cost;
- license compatibility; and
- the ability to report source-connected formula diagnostics.

If no engine passes, Marksheet should implement the small portable profile in
`marksheet-calc` and continue to use external engines only for broader import
and export compatibility.

## 8. Editing model

Every user action is an explicit transaction that produces both a semantic
change set and a source patch plan.

```text
EditTransaction
  semantic operations
  affected source nodes
  reference adjustments
  diagnostics before/after
  inverse transaction
```

Examples:

- Editing `inputs!B2` replaces one CSV field token.
- Appending a table row inserts one CSV record before that table's `@end`.
- Changing a block anchor edits one directive argument.
- Renaming a sheet label edits one quoted string.
- Renaming a sheet ID edits the declaration and all resolved formula/name
  tokens in one transaction.
- Styling a range reuses an existing style or inserts one `@apply` directive.

Edits should preserve the author's nearby conventions when safe. Canonical
formatting is a separate command.

### 8.1 Conflict avoidance

Before applying a source patch, the editor compares the original byte spans
with the current file. If the file changed externally, it must reparse and
rebase the semantic transaction or ask the user to resolve the conflict. It
must not overwrite newer source with an old full-document snapshot.

## 9. Public library API

The first API should be small and operation-oriented:

```text
parse(source, options) -> ParsedDocument
validate(document) -> Diagnostic[]
workbook(document) -> WorkbookView
calculate(workbook, options) -> CalculationResult
edit(document, transaction) -> EditResult
format(document, options) -> string
convert(workbook, target, options) -> ConversionResult
```

`ParsedDocument` contains the CST, semantic IR when available, and diagnostics.
No parse error should require throwing away successfully recovered structure.

`EditResult` contains minimal text edits as ordered nonoverlapping byte ranges,
the updated semantic view, and new diagnostics. Language-server and browser
clients can apply the same patch representation.

The WebAssembly API should expose owned handles and batched calls rather than a
fine-grained getter for every cell. Crossing the JavaScript/Wasm boundary once
per visible region is preferable to crossing it once per cell.

## 10. Command-line interface

The executable should be named `marksheet`, not `ms`, to avoid unrelated shell
command and file-format collisions.

Initial commands:

```text
marksheet check workbook.ms
marksheet fmt --check workbook.ms
marksheet fmt workbook.ms
marksheet inspect workbook.ms
marksheet get workbook.ms summary!A1:B20
marksheet set workbook.ms inputs!G2 0.22
marksheet calc workbook.ms --sheet summary --range A1:B20
marksheet diff old.ms new.ms
marksheet convert workbook.ms --to xlsx
marksheet convert workbook.xlsx --to marksheet
```

- `check` validates syntax, references, required extensions, and formulas.
- `fmt` performs explicit canonical serialization.
- `inspect` returns workbook structure, names, tables, extensions, and
  diagnostics as stable structured data.
- `get` returns authored and calculated values for an explicit cell, range,
  name, or table.
- `set` applies one source-aware value or formula edit and refuses ambiguous or
  invalid targets.
- `calc` emits calculated values as JSON by default, with CSV and text options.
- `diff` adds a semantic explanation alongside ordinary source diffs.
- `convert` always emits or writes a conversion report.

Diagnostics should support human text, JSON, and the standard language-server
diagnostic shape.

### 10.1 Coding-harness integration kit

Coding harness support is an integration layer, not a Marksheet workbook
extension. Workbook extensions add declarative spreadsheet semantics; the
harness integration teaches software how to author Marksheet and gives it safe
operations over local files.

The integration kit has three layers:

1. The `marksheet` CLI is the universal, process-level interface. Its JSON
   output and exit codes are versioned and stable enough for automation.
2. A portable skill contains concise instructions, examples, decision rules,
   and error-recovery guidance for coding agents.
3. An optional local tool server exposes structured operations to harnesses
   that support a tool protocol.

The canonical skill should live at:

```text
integrations/skill/SKILL.md
integrations/skill/references/format-cheatsheet.md
integrations/skill/references/workflows.md
integrations/skill/examples/
```

It should teach an agent to:

- recognize `.ms` files and the `#!marksheet` header;
- choose tables versus unnamed blocks;
- use stable sheet IDs, named ranges, structured references, and fills;
- preserve source locality and avoid canonicalizing unrelated content;
- validate after every material edit;
- inspect calculated results and diagnostics;
- use explicit text escapes and CSV quoting correctly;
- avoid unsupported required extensions; and
- request conversion reports rather than assume XLSX or CSV fidelity.

The skill is documentation and examples. It must not bundle a second parser or
teach syntax that differs from `SPEC.md`.

The optional local tool server should expose operations equivalent to:

```text
check(path)
inspect(path)
get(path, target, calculated=true)
set(path, target, value_or_formula)
append_table_row(path, table, values)
calculate(path, targets)
format(path, check_only=true)
convert(path, target_format, options)
semantic_diff(old_path, new_path)
```

Every mutating operation returns the exact source patches and diagnostics it
applied. The server is local-first, requires explicit file paths within its
configured workspace, and does not provide workbook-triggered network or
plugin installation.

Harness-specific packages should be thin wrappers around the canonical skill
and tool schema. They may adapt manifests and installation layout, but must not
fork the authoring guidance.

Harness integration is tested with end-to-end tasks rather than prompt snapshots
alone. The task corpus should cover creating a workbook, adding a sheet,
appending a table row, changing a named input, repairing invalid CSV, explaining
a formula error, and converting with an honest loss report. At least two
independent coding-agent environments should complete the corpus against the
same CLI or tool server before the integration is considered stable.

## 11. GUI architecture

The GUI should use the Rust core through WebAssembly in a browser and through
the same logical API in a desktop host.

Primary components:

- workbook and sheet navigation;
- a virtualized grid that requests only visible cell regions;
- formula bar and name box;
- style and geometry controls;
- source-connected diagnostics;
- an optional synchronized source view; and
- unsupported-extension indicators.

The grid model reads three layers:

1. authored scalar or formula value;
2. calculated display value and error state; and
3. resolved presentation style.

These layers must remain distinct. Editing a calculated display value should
never accidentally replace its source formula.

### 11.1 Large and sparse sheets

The GUI must not allocate a dense matrix to the furthest used coordinate.
Visible-region requests should query sparse block indexes, fill ranges, and
style interval indexes. Sheet extents are a viewport hint, not an allocation
instruction.

Table rendering may materialize a visible row window. Formula calculation may
materialize required dependencies independently of the viewport.

### 11.2 Worker boundary

Parsing, canonical formatting, large conversions, and recalculation should run
outside the UI thread. Browser builds should place the Wasm core in a worker and
communicate through versioned messages with cancellation support.

## 12. Plugin host

Draft 0.1 plugins are installed by an application or linked at build time. A
workbook can refer to a plugin but cannot install it.

The host registry maps one exact `id@major` to capabilities. It rejects a
duplicate exact registration as host configuration failure; it never falls back
to a different major or chooses by registration order:

```text
ExtensionPlugin
  id
  supported_major
  parse_payload(bytes) -> extension_model
  validate(extension_model, workbook_view) -> diagnostics
  formula_functions() -> functions
  renderers() -> renderers
  converters() -> converters
```

Plugins receive read-only workbook views unless an explicit user action invokes
a converter or edit command. Formula functions receive typed arguments and a
deterministic calculation context; they do not receive filesystem or network
handles by default.

The first plugin API is an in-process Rust trait plus a browser registry for
trusted compiled modules. Implementations are static or link-time host code;
there is no third-party dynamic-plugin ABI, workbook-directed fetch, automatic
installation, network capability, or subprocess capability. The registry owns
diagnostic and resource caps, orders extension diagnostics by source span and
code, and records explicit truncation rather than silently discarding results.

The first demonstration extension is `assertions@1`. Its payload consists of
newline-separated assertion lines, each `assert <target> <operator> <literal>`.
`target` is one concrete core A1 cell: unqualified in a sheet-scoped instance
and sheet-qualified in a workbook-scoped one. `operator` is one of `=`, `!=`,
`<`, `<=`, `>`, or `>=`, and `literal` is one section-11 scalar spelling with
JSON strings and the `blank` sentinel. It has no expression language,
identifiers, functions, or I/O. Assertions run after core calculation, report
failed assertions at the target, and cannot modify values. Hosts bound payload
bytes, physical lines, targets, and emitted diagnostics before parsing or
evaluation.

## 13. Import and export

Converters operate on the semantic IR, not the CST. Every conversion returns:

```text
ConversionResult
  artifact
  exact_features
  approximations
  omissions
  diagnostics with source/cell locations
```

The XLSX converter should initially target:

- sheets and their order;
- scalar cells and formulas in the portable profile;
- table regions and headers;
- named ranges;
- core styles; and
- row and column geometry.

Macros, external links, unsupported formula functions, advanced conditional
formatting, pivots, and charts must appear in the conversion report.

A defined name whose target Marksheet cannot express — a whole-column or
whole-row range, a multi-area target, a reference to an unknown sheet or table,
or any other non-finite A1 shape — is an omission of that one name, not a
package-level failure: the rest of the workbook still imports, and the omitted
name gets its own `named_ranges` omission entry. Because the portable evaluator
would resolve a reference to a name that no longer exists as `#NAME?`, and an
unresolved name reference cannot be written back to XLSX, a formula that reaches
an omitted name is replaced with `#NAME?` — a cell formula becomes the typed
error value and a fill becomes `=#NAME?` — with a `portable_formulas` replaced
outcome per affected cell, table column, or fill range. That replacement is the
only formula outcome the affected location keeps: names resolve after every
sheet has been read, so the substitution retracts the translation the first pass
recorded for the same formula rather than leaving the report claiming both.
Package-level defects such as malformed XML, duplicate case-insensitive names,
or a name that collides with a table identifier after normalization remain
conversion failures — the collision is a property of the identifier namespace,
so it is fatal even when that name's own target is unimportable.

Reaching a name at all requires rewriting the formula body. Excel spells sheet
labels and defined names case-insensitively, while portable-a1@1 requires the
exact lowercase identifier, so importing a formula translates both to the
identifiers the importer assigned — including for a name whose target was
omitted, which keeps owning its identifier so its callers can be recognized. A
word that opens a call or a structured selector is a function or a table, not a
name reference, and is left alone. A calculated-column body that still cannot be
parsed as portable-a1@1 costs that column its `@fill` and nothing else: the
column keeps the values Excel cached, and the import records a
`portable_formulas` replaced outcome, the same treatment as a body that parses
but leaves the portable profile.

CSV export requires the caller to select one sheet and range or one named table.
There is no honest default that flattens an arbitrary workbook into one CSV.

The concrete public report is `marksheet-conversion@1`. It distinguishes
`exact`, `approximated`, `omitted`, and `unsupported` feature outcomes and
derives `lossless`, `lossy`, or `unsupported` fidelity from them. A report never
states two outcomes for the same feature at the same location: a converter that
learns only in a later pass that an earlier decision no longer holds — a formula
first translated, then destroyed once a defined name turned out to be
unimportable — retracts the superseded outcome and its diagnostic before
recording the replacement, and fidelity is rederived from what survives. The
finalized report sorts `exact` ahead of `approximated`, so a stale claim left
next to a true one would be the first a consumer reads. XLSX package
writers use deterministic ZIP/XML construction (fixed order, timestamps,
relationship IDs, and attributes) so reproducibility is testable. Import
limits cover compressed and expanded archive size, members, XML nesting,
worksheets, cells, styles, shared strings, and formulas. The converter never
silently truncates an archive or chooses a CSV worksheet/range on the caller's
behalf.

## 14. Diagnostics

Every diagnostic contains:

```text
code
severity: error | warning | info
message
primary source span
related source spans
sheet and cell context, when available
suggested fix, when safe
```

Diagnostic codes are stable public identifiers such as:

```text
MS1001 unsupported_format_version
MS1204 non_rectangular_block
MS1302 overlapping_footprints
MS2101 unresolved_name
MS2303 formula_cycle
MS3101 required_extension_unavailable
MS3103 undeclared_extension_instance
MS3201 assertion_failed
MS3202 assertion_payload_invalid
MS3203 assertion_resource_limit
MS4101 conversion_resource_limit
MS4102 conversion_loss
MS4103 csv_selection_required
MS4104 csv_import_target_required
MS4105 conversion_rejected
```

Messages may improve without breaking clients; codes and structured fields are
the API.

## 15. Testing strategy

### 15.1 Format conformance

Each fixture includes source, expected diagnostics, and a normalized JSON
projection of the workbook IR. Both valid and invalid cases are required.

### 15.2 Formula conformance

Each formula case defines inputs, formula, expected typed result or error, and
profile version. Edge semantics must be fixtures before they become normative
prose.

### 15.3 Round-trip tests

For every valid fixture:

- parse then lossless-save with no edits is byte-identical;
- parse then canonicalize is idempotent;
- canonicalize then parse produces the same semantic projection; and
- a one-cell edit changes only the expected source span and necessary CSV
  quoting.

### 15.4 Property and fuzz tests

Use generated coordinates, CSV fields, formulas, Unicode strings, and opaque
extension payloads to test scanner and parser invariants. Fuzz inputs must cover
multiline quoted fields and adversarial `@end` placement.

### 15.5 Differential tests

Run the portable formula corpus through the reference engine adapter and at
least one independent implementation. Differences remain explicit test
failures until the profile defines the behavior.

### 15.6 Performance tests

Track:

- parse throughput and peak memory;
- time to first visible grid region;
- one-cell incremental recalculation latency;
- one-cell source patch latency;
- large sparse coordinate behavior; and
- Wasm bundle size and startup time.

Performance fixtures should include many small blocks, one large table, distant
sparse blocks, deep dependency chains, and wide fan-out formulas.

## 16. Delivery milestones

### Milestone 1: Parser proof

- lossless scanner and CST;
- semantic workbook IR;
- example and invalid fixtures;
- `marksheet check`; and
- canonical formatter.

### Milestone 2: Calculation proof

- formula parser/profile fixtures;
- calculation-engine adapter spike;
- named ranges, structured references, and fills; and
- incremental dependency updates.

### Milestone 3: Editing proof

- source patch API;
- transactional reference updates;
- byte-identical no-op round trips; and
- semantic diff prototype.

### Milestone 4: GUI proof

- Wasm binding;
- virtualized multi-sheet grid;
- formula and style editing; and
- local-file open/save with external-change protection.

### Milestone 5: Interoperability proof

- XLSX import/export with conversion reports;
- selected-table and selected-range CSV conversion;
- exact-ID trusted extension-host prototype with `assertions@1`; and
- second independent parser against the conformance corpus.

### Milestone 6: Coding-harness proof

- [x] stable versioned JSON output and exit codes for CLI automation;
- [x] `inspect`, `get`, source-aware `set`, and table-row append commands;
- [x] canonical portable skill with concise references and examples;
- [x] bounded workspace-local structured-tool server;
- [x] thin Codex and Claude Code packages referencing the canonical assets; and
- [x] one executable seven-task corpus shared by both harness profiles,
  covering authoring, editing, calculation, diagnosis, and conversion.

## 17. Decisions deferred by Draft 0.1

The following choices should be made through prototypes rather than embedded in
the format prematurely:

- final calculation engine;
- GUI framework and desktop shell;
- text-buffer or rope implementation;
- spatial and interval index libraries;
- dynamic plugin ABI;
- tool-server protocol SDK and transport;
- Python binding library and API shape; and
- packaging for directory-mode workbooks.

Deferring these choices does not defer the architectural boundaries they must
respect: lossless source, independent semantic IR, calculation adapter, safe
extension host, and minimal source patches.
