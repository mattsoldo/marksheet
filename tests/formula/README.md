# `portable-a1@1` formula conformance corpus

This directory is the executable semantic contract for Marksheet formulas. The
prose rules live in `SPEC.md`; every edge rule adopted by the profile should
also have a small fixture here.

The corpus has two layers:

- `parser/*.json` checks formula tokenization, precedence, references, and
  malformed syntax without constructing a workbook.
- `eval/*.json` evaluates one formula against an optional typed cell map.
- `format/*.json` checks canonical formula serialization after successful
  parsing. It is a semantic normalization, never a lossless source rewrite.
- `scenarios/*.ms` and matching `.calc.json` sidecars exercise container
  integration, resolution, cycles, dependencies, and virtual fills.

All JSON documents declare `marksheet.formula-conformance@1` or
`marksheet.calculation-scenario@1` and the `portable-a1@1` profile. Case IDs are
stable, globally unique, lowercase dotted identifiers. Adding behavior under a
new profile requires new cases rather than changing the expected result of an
old profile case.

## Typed values

Expected values use the public scalar vocabulary, with ISO strings for temporal
values:

```json
{ "kind": "blank" }
{ "kind": "text", "value": "" }
{ "kind": "number", "value": 1.5 }
{ "kind": "boolean", "value": true }
{ "kind": "date", "value": "2026-08-16" }
{ "kind": "datetime", "value": "2026-08-16T10:30:00-04:00" }
{ "kind": "error", "value": "#DIV/0!" }
```

Blank and empty text are deliberately different. JSON numbers in this initial
corpus are ordinary finite binary64 values. A future boundary case that cannot
be stated unambiguously as a JSON number must use a separately versioned
bit-pattern representation; it must not use a string that looks numeric.

Evaluation cases may provide a `cells` object. An unqualified key addresses the
case's current sheet (default `main`); a qualified key uses a stable sheet ID.
Absent cells are blank. Cell inputs are scalar values, never formula source.
Workbook behavior belongs in a scenario fixture.

## Parser projection

Parser cases use a compact S-expression solely as a normalized test projection;
it is not a required Rust API. Nodes are `number`, `text`, `boolean`, `error`,
`cell`, `range`, `name`, `table-column`, `table-region`, `current-row`, `unary`,
`binary`, and `call`. Cell and range nodes retain authored `$` markers and use
`-` for the current sheet. Function names and A1 columns are normalized to
uppercase in the projection. Header arguments containing whitespace are quoted
using JSON string spelling. For example:

```text
(binary + (number 1) (binary * (number 2) (number 3)))
(range inputs A1 B3)
(call SUM (table-column costs Cost))
(table-column costs "Unit Cost")
```

Malformed parser cases expect `MS2202`. Resolution errors such as an unknown
sheet or name require workbook context, so they are evaluation or scenario
cases rather than parser failures.

## Canonical formula projection

Canonical-format cases use an exclusive `expect.canonical` string. The parser
accepts ordinary profile whitespace and casing, while canonical output uses the
shortest round-trippable finite number spelling, uppercase function names,
boolean literals, and A1 columns, and no optional whitespace. It removes only
parentheses that are unnecessary to preserve the parsed AST. Text quotes and
structured-header closing brackets are escaped as `""` and `]]` respectively;
header content, including leading or trailing spaces, is preserved exactly.

For example:

```json
{
  "id": "format.whitespace.function-boolean-cell-case",
  "formula": "= sum ( tRuE , a1 )",
  "expect": { "canonical": "=SUM(TRUE,A1)" }
}
```

## Scenario sidecars

A `.calc.json` sidecar names the exact cells that must be observed. Unlisted
cells are not assertions. Diagnostics are stable codes plus their affected
cells; byte spans remain implementation-level assertions. Fill scenarios also
assert `virtual_formulas_only`, meaning calculation must not expand `@fill`
into authored CSV formula fields.

## Validation

Run the structural validator from the repository root:

```text
tests/formula/validate.sh
```

The script requires `jq`, rejects malformed JSON, unknown top-level shapes,
duplicate case IDs, missing scenario partners, and malformed typed values. The
calculation crate must additionally discover every corpus file and fail when a
case is skipped. Formula evaluation errors are typed results, not diagnostics;
malformed syntax and unresolved references are diagnostics.
