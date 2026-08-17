# Marksheet agent workflows

## Create or add a sheet

Choose stable lowercase IDs before labels. Add a new `@sheet` at the intended
source position, author only finite blocks/tables, then run `marksheet check`
and `marksheet inspect`. A new worksheet is source text; do not hide it in an
extension payload.

## Change a named input

Inspect the name, then use the semantic edit boundary:

```sh
marksheet get budget.ms tax_rate
marksheet set budget.ms tax_rate 0.25
marksheet get budget.ms 'summary!B2:B4'
```

The edit result lists exact byte patches. If the name is range-shaped or
virtual, stop and choose an explicit authored cell instead of broadening it.

## Append a repeated record

Inspect the table headers first. Supply one strict scalar string for every
header, including an empty string for a calculated-column blank:

```sh
marksheet append-table-row budget.ms costs \
  --value Transport --value 50 --value 2 --value ''
```

Never copy a calculated-column formula into the row when an existing `@fill`
owns it.

An edit may exit 1 with `status:"committed_invalid"`, `changed:true` when its
source patch committed successfully but an extension assertion fails afterward.
Do not retry an append in that state: doing so would duplicate the record.
Inspect the current workbook and repair the assertion or edited value from the
new source. A true refusal has `changed:false`.

## Repair invalid CSV

Use byte/line spans from `marksheet check --format json`. Check for:

- nonrectangular records;
- unescaped `"` inside a quoted field;
- an `@end` line that is still inside CSV quote state;
- text requiring a leading apostrophe; and
- malformed ISO-looking dates/datetimes.

Edit only the affected field or record, then validate again. Canonical
formatting is not a recovery substitute for malformed source.

## Explain a formula error

Run `marksheet get` on the formula cell and its direct inputs. Report both the
typed calculated error (`#NAME?`, `#REF!`, `#VALUE!`, `#DIV/0!`, `#CIRC!`) and
the stable diagnostic code/source span. Do not replace the formula merely to
silence the error unless the requested repair is unambiguous.

## Convert honestly

Select CSV explicitly and always read `marksheet-conversion@1`:

```sh
marksheet convert budget.ms --to xlsx --output budget.xlsx
marksheet convert budget.ms --to csv --sheet summary --range A1:B4 \
  --output summary.csv
```

`lossless` means every reported feature is exact. `lossy` means at least one
approximation/omission. `unsupported` writes no successful artifact. Preserve
the report with the artifact when fidelity matters.

## External changes and conflicts

Do not retry a rejected edit by overwriting the whole file. Re-inspect the
current bytes, re-evaluate the intended target, and submit a new semantic edit.
This is especially important when an editor, watcher, or another agent may be
changing the workbook concurrently.
