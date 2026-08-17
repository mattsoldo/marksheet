# Marksheet

**Markdown for spreadsheets.**

Marksheet is a plain-text, Git-friendly spreadsheet format designed to be easy
for people and coding agents to read, write, review, and generate.

A Marksheet workbook can contain multiple sparse sheets, formulas, named
ranges, named tables, and basic presentation formatting. It is intended to
open in a spreadsheet GUI while remaining useful in any text editor and in
ordinary source-control workflows.

```marksheet
#!marksheet 0.1
@book locale="en-US" timezone="UTC" formula-profile="portable-a1@1"

@style money number=currency currency="USD" decimals=2
@name tax_rate = inputs!G2

@sheet inputs "Inputs"

@table costs A1 csv
Item,Cost,Quantity,Subtotal
Rent,1500,1,
Utilities,200,1,
Groceries,360,1,
@end

@fill costs[Subtotal] =[@Cost]*[@Quantity]
@apply costs[Cost] money
@apply costs[Subtotal] money

@block F1 csv
Setting,Value
Tax rate,0.2
@end

@sheet summary "Summary"

@block A1 csv
Metric,Value
Total,=SUM(costs[Subtotal])
After tax,=B2*(1-tax_rate)
@end

@apply B2:B3 money
```

## Why Marksheet?

Markdown gave documents a durable source format that works for humans, tools,
and Git. Spreadsheet data still moves between binary workbooks, cloud services,
CSV fragments, and Markdown tables. Marksheet aims to provide the missing
source format: small enough for an agent to author, expressive enough for a
real workbook, and predictable enough for independent implementations.

The `.ms` extension is proposed for Marksheet workbooks. Every file also begins
with a self-identifying and versioned `#!marksheet` header, so tools do not need
to trust the extension.

## Documents

- [Product specification](PRODUCT.md) — the problem, product promise, scope,
  users, principles, and success criteria.
- [Format specification](SPEC.md) — the normative syntax, data model, formula
  profile, formatting model, extensions, and conformance requirements.
- [Implementation specification](IMPLEMENTATION.md) — the reference parser,
  workbook model, calculation adapter, editing architecture, GUI, CLI, and test
  strategy.
- [Build prompt](BUILD_PROMPT.md) — a master prompt for a coding agent to build
  the reference implementation in tested vertical slices.
- [Example workbook](examples/budget.ms) — a small workbook exercising the
  draft core.
- [Attribution](ATTRIBUTION.md) — how the license handles copies, forks, and
  derivative works.

## Project status

Marksheet is at **Draft 0.1**. The format is being designed in public and is not
yet stable. Files written during the `0.x` period may require migration as the
core is refined. Stability rules become strict at `1.0`.

The reference implementation has completed the Milestone 5 interoperability
and extensions proof. It includes lossless parsing, canonical formatting,
deterministic `portable-a1@1` calculation, source-aware semantic transactions,
exact inverse patches, undo/redo, conservative external-change rebasing,
semantic diff, and the following interoperable layers:

- `marksheet-view`, a sparse, pure-Rust, renderer-neutral projection layer. It
  returns bounded viewports and keeps authored values, virtual fills,
  calculation, resolved presentation, geometry, and source links distinct; it
  never expands a sheet to the distance between populated coordinates.
- `bindings/wasm`, an independent Cargo workspace that exposes the revisioned,
  batched `marksheet-worker@1` worker protocol without per-cell getters or a
  dense used range.
- `viewer`, a standalone TypeScript/DOM workbench that packages the real Wasm
  worker and displays an exact synchronized source view alongside a bounded
  grid.
- `marksheet-convert`, a bounded semantic-IR converter for deterministic XLSX
  import/export and explicitly selected CSV ranges or tables. Every conversion
  returns a versioned report that distinguishes exact, approximated, omitted,
  and unsupported features.
- `marksheet-extensions`, a trusted static registry with exact `id@major`
  matching and the declarative `assertions@1` demonstration extension.
- `conformance/python`, a standard-library-only independent parser and checked
  projection consumer for the byte-level conformance corpus.

Formula diagnostics do not erase the source: a workbook that can be projected
remains viewable with source-linked diagnostics, but semantic edits are refused
until error-level formula diagnostics are resolved. For a local file opened
through the File System Access API, the viewer compares the current exact bytes
with its opening snapshot before it obtains a writable. Any drift is reopened
as the external source and produces no write; an unchanged document also does
not create a writable. Browsers without a file handle use an explicit download
instead of overwriting the selected file.

Required unavailable extensions keep source and core structure viewable but
make calculation and rendering explicitly incomplete. Optional unavailable and
undeclared opaque extensions remain visible and preserved. Workbooks never
select code, URLs, package paths, or installation behavior.

Milestone 6 adds the coding-harness integration kit: versioned automation JSON,
source-aware query/edit commands, a canonical portable skill, a bounded local
tool server, and one executable task corpus shared by Codex and Claude Code
package profiles.

## Build and CLI usage

Install a current stable Rust toolchain, then run the verification
commands from the repository root:

```sh
cargo test --workspace
cargo run -p marksheet-cli -- check examples/budget.ms
cargo run -p marksheet-cli -- inspect examples/budget.ms
cargo run -p marksheet-cli -- get examples/budget.ms tax_rate
cargo run -p marksheet-cli -- set examples/budget.ms tax_rate 0.25
cargo run -p marksheet-cli -- fmt --check examples/budget.ms
cargo run -p marksheet-cli -- calc examples/budget.ms \
  --sheet summary --range A1:B4 --format json
cargo run -p marksheet-cli -- diff old.ms new.ms
cargo run -p marksheet-cli -- convert examples/budget.ms \
  --to xlsx --output budget.xlsx
cargo run -p marksheet-cli -- convert examples/budget.ms \
  --to csv --sheet summary --range A1:B4 --output summary.csv
```

The canonical coding-agent guidance is
[`integrations/skill/SKILL.md`](integrations/skill/SKILL.md). The optional
workspace-local JSON-lines server and stable tool schema live under
[`integrations/mcp`](integrations/mcp), including checked request and response
schemas, with thin harness packages under
[`integrations/harnesses`](integrations/harnesses). Run both reference harness
profiles with:

```sh
cargo build -p marksheet-cli
python3 integrations/mcp/test_tool_server.py
python3 tests/harness/run.py
```

To build the standalone `marksheet` executable:

```sh
cargo build --release -p marksheet-cli
./target/release/marksheet check examples/budget.ms
```

`marksheet check <workbook.ms>` validates syntax, workbook structure, formulas,
and references. `marksheet fmt --check <workbook.ms>` verifies canonical
formatting; `marksheet fmt <workbook.ms>` explicitly rewrites a valid workbook.
`marksheet check --format json` emits machine-readable diagnostics.

`marksheet calc <workbook.ms> --sheet <id> --range <A1:B2>` calculates one
explicit rectangle. JSON is the default stable typed output; `--format csv`
and `--format text` are also available. Core calculation is deterministic:
volatile functions, I/O, clocks, randomness, and network access are not part of
`portable-a1@1`.

`marksheet diff <old.ms> <new.ms>` compares workbook meaning rather than source
spelling. Human output is concise; `--format json` emits the versioned
`marksheet-diff@1` envelope. Equivalent workbooks exit `0`, semantic
differences exit `1`, and operational failures exit `2`.

`marksheet convert <source> --to <marksheet|xlsx|csv>` writes its destination
atomically and emits a `marksheet-conversion@1` JSON report on standard output.
XLSX conversion records unsupported Office features instead of silently
discarding them. CSV export requires exactly one `--table` or explicit
`--sheet` plus `--range`; CSV import likewise requires an explicit target sheet
and range or table anchor. An unsupported conversion does not write a partial
artifact.

The `marksheet-edit` crate exposes typed transactions for authored-cell edits,
table-row appends, sheet and name renames, complete block movement, and focused
style application. Successful transactions return ordered source-bound byte
patches and exact inverse patches. A structural block move is deliberately a
single-operation transaction so newly authored formulas cannot bypass its
reference-adjustment pass. `EditSession` adds undo, redo, and safe
semantic rebasing after unrelated external changes; conflicts never expose a
partial patch plan. Cell, table-append, and identifier edits can be rebased
when their semantic preconditions still hold. Structural moves and style
applications currently return an explicit conflict after external drift rather
than guessing at a rebase.

Successful JSON calculation output uses the versioned
`marksheet-calc@1` envelope. It contains the formula profile, explicit
selection, row-major cells with tagged values, diagnostics, revision, and
bounded work statistics. New optional fields may be added within an envelope
version; a breaking shape or meaning change requires a new version string.
Fatal parse, validation, selection, or resource errors produce no partial
calculation document on stdout. Exit status `0` means calculation completed,
`1` means the workbook or calculation is incomplete (including a selected
cycle), and `2` means an I/O, usage, or serialization failure.

The optional Formualizer adapter under `crates/marksheet-calc-formualizer` is a
time-boxed compatibility spike and reference comparison, not the profile's
semantic authority or the default calculator.

## Browser viewer (Milestone 4)

The viewer is a standalone package; it builds the independent Wasm binding and
stages the generated worker assets itself. Install the Rust target and
`wasm-bindgen` CLI once, then run its checks from the repository root:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.127 --locked

cd viewer
npm install
npm test
npm run smoke:wasm
npm run build
```

`npm test` runs the viewer and local-file guard tests in a DOM test environment.
`npm run smoke:wasm` loads the generated Rust Wasm ABI and verifies the Budget
workbook's ordered sheets, calculation, focused edit patch, and recalculation.
`npm run build` requires and verifies the packaged worker, protocol glue,
JavaScript glue, and Wasm artifact. The standalone manual browser workflow,
including the File System Access behavior that a headless DOM test cannot
prove, is documented in [viewer/README.md](viewer/README.md).

## Design boundaries

The core intentionally includes formulas and named ranges because they make
workbooks substantially easier to understand. Features such as charts,
validation, assertions, schemas, conditional formatting, and external data
connections belong in declarative extensions.

A workbook never downloads or executes a plugin. Applications decide which
plugins they trust and install.

## License

Copyright (c) 2026 Marksheet project contributors.

Marksheet is distributed under the permissive [MIT License](LICENSE). Commercial
use, modification, redistribution, and private use are allowed. Copies and
substantial derivative works must retain the copyright and permission notice;
there are no royalties, source-disclosure requirements, or other commercial
obligations.
