---
name: marksheet
description: Create, inspect, calculate, repair, convert, and minimally edit portable Marksheet .ms workbooks.
---

# Marksheet workbook workflow

Use this skill for `.ms` files beginning with `#!marksheet 0.1`.

Marksheet is source-first. Prefer a focused source edit for ordinary authoring,
and use the CLI's structured commands whenever you need semantic resolution,
calculation, or a safe edit of existing source.

## Required workflow

1. Inspect an unfamiliar workbook before editing:

   ```sh
   marksheet inspect workbook.ms
   ```

2. Query names, tables, or explicit stable-ID ranges with calculated values:

   ```sh
   marksheet get workbook.ms tax_rate
   marksheet get workbook.ms 'summary!A1:B4'
   ```

3. Use source-aware edits for an existing authored cell or one-cell name:

   ```sh
   marksheet set workbook.ms tax_rate 0.25
   marksheet set workbook.ms 'summary!B4' '=B2*(1-tax_rate)'
   ```

4. Append table records through the semantic edit boundary:

   ```sh
   marksheet append-table-row workbook.ms costs \
     --value Transport --value 50 --value 2 --value ''
   ```

5. After every material direct-source edit, validate and inspect the affected
   result:

   ```sh
   marksheet check --format json workbook.ms
   marksheet get workbook.ms 'summary!B2:B4'
   ```

6. Before conversion, request and read the conversion report. Never infer
   XLSX or CSV fidelity from a successful file write alone.

## Decision rules

- Use a named table for repeated records, headers, structured references, or a
  calculated column. Use an unnamed `@block` for a fixed rectangular island.
- Treat stable sheet, table, name, and style IDs as API identities. Labels are
  presentation and may contain spaces.
- Prefer workbook names for important inputs and table-column references for
  repeated data. Prefer `@fill` over copying formulas into every row.
- `set` only edits an existing authored cell. It refuses ranges, absent cells,
  and virtual fill cells instead of guessing where source should be inserted.
  A cell can be both: a blank field inside a column owned by an `@fill` reports
  `source:"authored"` from `get` but is still refused, because the fill owns its
  value. Treat a non-null `virtual_formula` as not settable whatever `source`
  says; to change it, edit the `@fill` directive in source and re-run
  `marksheet check`. No command edits directives.
- Preserve comments, blank lines, directive order, quoted CSV spelling, and
  opaque extension payloads. Do not run `fmt` merely to tidy an unrelated edit.
- Use a leading apostrophe for text that would otherwise parse as a number,
  boolean, date, error, or formula. Follow RFC 4180 quoting inside block/table
  bodies.
- Do not add `@require` for an extension that the active host does not support.
  Workbooks never authorize installation, network access, or executable code.
- Exit 0 is success, exit 1 is a semantic refusal/difference/diagnostic result,
  and exit 2 is an operational I/O or serialization failure. For `set` and
  `append-table-row`, always inspect the JSON `status` and `changed` fields:
  `committed_invalid` means the edit was applied but trusted assertions now
  fail. Never retry that mutation; repair from the current source instead.

Read [format-cheatsheet.md](references/format-cheatsheet.md) for syntax and
[workflows.md](references/workflows.md) for repair, diagnosis, and conversion
playbooks. Use the files in `examples/` as small authoring patterns; the full
language authority remains `SPEC.md`.
