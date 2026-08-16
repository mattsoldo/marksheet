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

The reference implementation is currently being built through Milestone 1:
parsing, validation, lossless source retention, and explicit canonical
formatting. Formula parsing, reference validation, and calculation are
Milestone 2 work; Milestone 1 retains formula source without changing its
spelling.

## Build and CLI usage

Install a current stable Rust toolchain, then run the Milestone 1 verification
commands from the repository root:

```sh
cargo test --workspace
cargo run -p marksheet-cli -- check examples/budget.ms
cargo run -p marksheet-cli -- fmt --check examples/budget.ms
```

To build the standalone `marksheet` executable:

```sh
cargo build --release -p marksheet-cli
./target/release/marksheet check examples/budget.ms
```

`marksheet check <workbook.ms>` validates the available Milestone 1 syntax and
workbook structure. `marksheet fmt --check <workbook.ms>` verifies that a file
already matches canonical formatting. `marksheet fmt <workbook.ms>` is the
explicit canonical-formatting command, and `marksheet check --format json`
emits machine-readable diagnostics. Formula calculation and `marksheet calc`
begin in Milestone 2.

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
