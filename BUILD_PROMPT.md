# Build Marksheet

Use this prompt with a coding agent working at the root of the Marksheet
repository.

---

You are building **Marksheet: Markdown for spreadsheets**.

Marksheet is a plain-text, Git-friendly spreadsheet format for people, coding
agents, and applications. A `.ms` file can contain multiple sparse sheets,
blocks, named tables, formulas, named ranges, basic formatting, and declarative
extensions. It must remain useful as source text while rendering and editing
like a conventional spreadsheet.

Your job is to build the open-source reference implementation described in
this repository. Work autonomously through tested, usable vertical slices. Do
not stop after planning or scaffolding while a safe, concrete implementation
step remains.

## Read first

Read these files completely before making architectural or syntax decisions:

1. `README.md`
2. `PRODUCT.md`
3. `SPEC.md`
4. `IMPLEMENTATION.md`
5. `examples/budget.ms`
6. `LICENSE` and `ATTRIBUTION.md`

Treat `SPEC.md` as normative for format behavior. Treat `IMPLEMENTATION.md` as
the reference architecture. If they conflict, follow `SPEC.md`, document the
conflict, and make the smallest coherent correction to the documents and code.
Do not silently invent alternate syntax.

The format is Draft 0.1. You may refine underspecified behavior when an
implementation forces a decision, but every such decision must be:

- simple for people and coding agents;
- deterministic across implementations;
- compatible with lossless editing and focused Git diffs;
- tested with valid and invalid fixtures; and
- recorded in the relevant specification in the same change.

## Product invariants

Preserve these invariants throughout the build:

1. The source file is authoritative.
2. A workbook is readable and writable without proprietary software or a cloud
   service.
3. Ordinary edits produce small, meaningful text diffs.
4. Parsing and rendering must support arbitrary sparse coordinates without
   allocating a dense grid to the furthest cell.
5. Formulas and named ranges are core features.
6. Unknown optional extensions survive lossless edits.
7. Unsupported required extensions are reported visibly and never ignored.
8. A workbook cannot execute code, access the network, or install plugins.
9. Canonical formatting is explicit; opening and saving is not permission to
   rewrite the whole file.
10. Importers and exporters report every approximation and omission.

## Technology and repository direction

Build the core in Rust. Organize it toward this workspace shape, adjusting only
when implementation evidence supports a better boundary:

```text
crates/
  marksheet-syntax/
  marksheet-model/
  marksheet-calc/
  marksheet-edit/
  marksheet-convert/
  marksheet-cli/
bindings/
  wasm/
apps/
  viewer/
integrations/
  skill/
  mcp/
  harnesses/
tests/
  conformance/
  roundtrip/
  differential/
  harness/
```

Keep dependencies conservative. Prefer small, established libraries for
commodity concerns, but write the outer Marksheet scanner deliberately: it
must retain exact source spans and understand CSV quote state, multiline
fields, and `@end` termination. A conventional deserializing CSV reader alone
is not enough for lossless patching.

Expose calculation through a `CalcEngine` boundary. Prototype Formualizer
behind that boundary and compare its behavior with the `portable-a1@1` tests.
Do not allow an external engine's private workbook model or Excel-specific
behavior to become the Marksheet public API. If the engine cannot implement the
portable profile predictably, implement the compact profile locally and retain
the adapter for broader compatibility.

The CLI executable is `marksheet`; `.ms` is the workbook extension. Do not name
the executable `ms`.

All original project code and documentation must remain compatible with the
repository's MIT License.

## Build sequence

Implement the milestones in order. Keep the repository buildable and tested at
the end of every milestone.

### Milestone 1: Parser proof

Deliver a useful native tool for inspecting and validating Marksheet source.

Implement:

- a Cargo workspace and the syntax, model, and CLI crates;
- UTF-8 and version-header validation;
- a lossless scanner with byte spans and line/column positions;
- comments, blank lines, directives, CSV bodies, multiline quoted fields, and
  opaque extension bodies;
- a concrete syntax tree retaining every input byte;
- semantic parsing for workbook settings, sheets, blocks, tables, scalar
  values, names, fills, styles, geometry, and extensions;
- sparse coordinates and rectangular-footprint overlap detection;
- reference resolution after the full workbook is parsed;
- stable structured diagnostics with codes, severity, primary spans, and
  related spans;
- canonical serialization; and
- `marksheet check` and `marksheet fmt`.

Required tests:

- valid fixtures for every core construct;
- invalid fixtures for malformed versions, CSV, directives, coordinates,
  duplicate identifiers, unresolved names, nonrectangular data, overlaps, and
  unsupported required extensions;
- byte-identical no-op round trips;
- idempotent canonical formatting;
- CRLF input to LF canonical output;
- multiline CSV containing commas, quotes, newlines, and a quoted `@end`;
- distant sparse blocks that do not cause dense allocation; and
- successful parsing of `examples/budget.ms`.

Milestone 1 is complete only when this works:

```text
cargo test --workspace
cargo run -p marksheet-cli -- check examples/budget.ms
cargo run -p marksheet-cli -- fmt --check examples/budget.ms
```

Update the README with exact build and CLI usage.

### Milestone 2: Calculation proof

Implement:

- a formula lexer, parser, and typed AST for `portable-a1@1`;
- A1, absolute, cross-sheet, named-range, table, and current-row references;
- operator precedence and core errors;
- the required portable function surface;
- dependency graph construction, dirty propagation, and cycle detection;
- virtual formulas produced by `@fill` without rewriting source cells;
- deterministic calculation with volatile behavior disabled;
- a calculation adapter and a time-boxed Formualizer integration spike;
- `marksheet calc` with JSON, CSV, and readable text output; and
- formula diagnostics connected to source and cell locations.

Write formula fixtures before finalizing any coercion or edge-case behavior.
Record those semantics in `SPEC.md`. Differentially test the chosen engine
against the portable corpus. Never silently inherit an engine behavior that the
Marksheet profile has not defined.

Milestone 2 is complete when the example workbook calculates the expected
values, cycles produce `#CIRC!`, incremental edits recalculate only affected
dependencies, and the formula corpus passes deterministically.

### Milestone 3: Editing proof

Implement:

- transactional semantic edits;
- minimal ordered, nonoverlapping byte patches;
- replacement of individual CSV fields with correct quoting;
- table-row insertion before the owning `@end`;
- block movement by anchor edit;
- atomic sheet-ID and named-range reference updates;
- formula reference adjustment for structural edits;
- style reuse and insertion of focused `@apply` directives;
- inverse transactions for undo and redo;
- external-file-change detection and safe transaction rebasing; and
- a semantic `marksheet diff` command.

Required round-trip tests:

- editing one scalar changes only its CSV field;
- editing a formula preserves surrounding source spelling;
- renaming a label does not rewrite references;
- renaming an ID updates all references atomically;
- an untouched unknown extension remains byte-identical; and
- a no-op edit emits no source patch.

### Milestone 4: Browser and GUI proof

Implement:

- a WebAssembly binding with generated TypeScript declarations;
- batched workbook, visible-region, calculation, and edit APIs;
- parsing and calculation in a worker with cancellation;
- a lightweight browser spreadsheet application;
- ordered sheet tabs;
- a virtualized sparse grid;
- authored value, formula, calculated value, and style layers;
- formula bar and name box;
- editing for cells, formulas, names, basic styles, row heights, and column
  widths;
- source-connected diagnostics;
- a synchronized optional source view; and
- local-file open/save with external-change protection.

Do not allocate a dense matrix to the used sheet extent. Verify this with a
fixture containing blocks separated by very large coordinate distances.

The GUI is complete for this milestone when a user can open
`examples/budget.ms`, see both sheets with calculated values and formatting,
edit a cell, save, and observe a focused Git diff.

### Milestone 5: Interoperability and extensions

Implement:

- converter interfaces and structured conversion reports;
- Marksheet-to-XLSX and XLSX-to-Marksheet conversion for supported core
  features;
- explicit reporting for unsupported formulas, macros, links, pivots, charts,
  and advanced formatting;
- selected-table and selected-range CSV conversion;
- the trusted extension registry described by `IMPLEMENTATION.md`;
- one small demonstration extension, preferably validation or assertions; and
- a second parser or independently implemented conformance consumer.

A conversion with any approximation or omission must not be labeled lossless.
A CSV export must require an explicit table or sheet range.

### Milestone 6: Coding-harness integration

Build a **Marksheet Integration Kit** for coding agents. This is not a workbook
plugin: it is a portable skill and local tooling layer that teaches harnesses
how to use Marksheet without adding harness-specific content to `.ms` files.

Implement:

- stable versioned JSON output and exit codes for automation;
- `marksheet inspect` for workbook structure, names, tables, extensions, and
  diagnostics;
- `marksheet get` for explicit cells, ranges, names, and tables, with authored
  and calculated values;
- source-aware `marksheet set` for one explicit value or formula edit;
- a table-row append operation that patches the owning CSV block locally;
- `integrations/skill/SKILL.md` as the canonical portable agent skill;
- concise skill references for syntax, editing workflows, diagnostics, and
  conversion safety;
- small valid and invalid example workbooks for skill use;
- an optional local structured-tool server backed by the same Rust core;
- thin harness-specific packages that reuse the canonical guidance; and
- an end-to-end task corpus exercised through at least two coding-agent
  environments.

The skill must teach agents when direct source editing is simplest and when to
use structured tools. It must emphasize stable identifiers, named ranges,
tables, formula fills, CSV quoting, minimal diffs, validation after edits, and
honest conversion reports.

The tool server should expose operations equivalent to `check`, `inspect`,
`get`, `set`, `append_table_row`, `calculate`, `format`, `convert`, and
`semantic_diff`. Mutating calls return exact source patches and diagnostics.
Limit file access to the configured workspace. Do not expose general shell,
network, or workbook-directed plugin-installation capabilities.

Milestone 6 is complete when both harnesses can use the same integration kit to
create a workbook, add a sheet, append a table row, change a named input,
diagnose and repair an invalid workbook, calculate selected outputs, and report
losses during conversion.

## API quality

Keep public APIs typed, documented, and narrow. The conceptual surface is:

```text
parse(source, options) -> ParsedDocument
validate(document) -> Diagnostic[]
workbook(document) -> WorkbookView
calculate(workbook, options) -> CalculationResult
edit(document, transaction) -> EditResult
format(document, options) -> string
convert(workbook, target, options) -> ConversionResult
```

The exact Rust names may follow language conventions, but preserve these
boundaries. Parsed results should include recoverable structure and diagnostics
rather than discarding the document at the first error.

## Performance and safety requirements

- Use sparse maps, interval indexes, and block footprints rather than dense
  sheet matrices.
- Provide configurable limits for file bytes, field sizes, coordinate values,
  formula depth, dependency count, and evaluation work.
- Report limit violations; never truncate silently.
- Fuzz the scanner, CSV termination, formula parser, and source patcher.
- Never allow core formulas to access files, processes, environment variables,
  clocks, randomness, or the network.
- Treat extension payloads and imported workbooks as untrusted input.
- Escape cell content in HTML and other rendered output.
- Never automatically install or fetch an extension requested by a workbook.

## Working method

For each milestone:

1. Inspect the current repository and preserve unrelated work.
2. State a short implementation plan.
3. Add fixtures or failing tests for the next behavior.
4. Implement the smallest coherent vertical slice.
5. Run focused tests while iterating, then the full workspace checks.
6. Run formatters and linters.
7. Exercise the real CLI or GUI path, not only unit tests.
8. Review the resulting Git diff for accidental rewrites and generated noise.
9. Update specifications and public usage documentation in the same change.
10. Report completed behavior, verification, known limitations, and the next
    milestone.

Do not:

- weaken or delete a test simply to make a build pass;
- silently discard unsupported source;
- regenerate whole workbooks for local edits;
- add a broad framework before a vertical slice needs it;
- commit build products, dependency caches, or generated noise;
- introduce telemetry or network dependencies; or
- claim support that has not been exercised through a user-facing path.

## Definition of done

The initial reference implementation is done when:

- every core construct in `SPEC.md` has valid and invalid conformance coverage;
- no-op lossless round trips are byte-identical;
- canonical formatting is deterministic and idempotent;
- the portable formula corpus agrees across two implementations;
- the CLI validates, formats, calculates, diffs, and converts workbooks;
- the CLI exposes stable structured inspect, query, and minimal-edit operations
  for automation;
- the GUI opens, calculates, edits, and minimally saves the example workbook;
- large sparse sheets do not cause dense memory allocation;
- unknown optional extensions survive edits;
- unsupported required extensions visibly block complete interpretation;
- XLSX and CSV conversions emit honest reports;
- a portable skill and optional local tool server work through at least two
  coding-agent harnesses;
- public APIs and commands are documented; and
- all tests, formatting checks, linters, and end-to-end verification pass from
  a clean checkout.

Begin with Milestone 1. Inspect the repository, identify any specification
contradictions that block implementation, and then build the parser proof into
a working `marksheet check` and `marksheet fmt` vertical slice.

---
